//! Statement fingerprints and in-memory statement statistics (ADR-0036 §1).
//!
//! Every statement a backend runs is reduced to a *fingerprint*: its text with
//! literals, placeholders and comments removed, whitespace collapsed, keywords
//! lower-cased and `IN (?, ?, ?)` lists reduced to one placeholder. Two
//! statements that differ only in values share a fingerprint, so
//! `SELECT * FROM t WHERE id = 7` and `... id = 'x'` and `... id = ?1` are one
//! row of statistics. The text kept per fingerprint (and listed with a slow
//! statement) is that reduced text, so it can never hold a literal value.
//!
//! The hot path is [`StatementStats::record`]: one pass over the SQL that
//! hashes the reduced text without allocating, a lookup in one of 16 sharded
//! read-locked maps, and a handful of relaxed atomic additions, one of them
//! into a latency sketch ([`forgebase_core::sketch::AtomicSketch`]). Nothing
//! on that path waits for another statement or touches the database. The
//! reduced text is built once per fingerprint, on its first statement.
//!
//! A background task (in `forgebase-insights`) calls [`StatementStats::drain`]
//! every flush interval and writes the aggregates; the in-memory entries are
//! bounded ([`MAX_FINGERPRINTS`], then one overflow entry) and idle ones are
//! dropped.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::time::Duration;

use forgebase_core::sketch::{AtomicSketch, Sketch};

use crate::backend::QueryLabel;

/// Fingerprints kept in memory; further ones count in [`OVERFLOW_TEXT`].
pub const MAX_FINGERPRINTS: usize = 4000;
/// Reduced text kept per fingerprint, in bytes.
pub const MAX_TEXT_BYTES: usize = 2048;
/// Slow statements kept between two drains; further ones are counted as dropped.
pub const MAX_SLOW_PER_DRAIN: i64 = 200;
/// The text of the entry that counts statements past [`MAX_FINGERPRINTS`].
pub const OVERFLOW_TEXT: &str = "<other statements>";
/// Drains an entry may stay unused before it is dropped.
const IDLE_DRAINS: u32 = 5;
const SHARDS: usize = 16;

// ---------------------------------------------------------------------------
// Fingerprinting
// ---------------------------------------------------------------------------

/// Receives the reduced text byte by byte.
trait Sink {
    fn put(&mut self, b: u8);
}

/// Hashes the reduced text (FxHash-style mixing, 64 bits).
struct Hasher(u64);

impl Sink for Hasher {
    #[inline]
    fn put(&mut self, b: u8) {
        self.0 = (self.0.rotate_left(5) ^ u64::from(b)).wrapping_mul(0x517c_c1b7_2722_0a95);
    }
}

/// Builds the reduced text.
struct Text(Vec<u8>);

impl Sink for Text {
    #[inline]
    fn put(&mut self, b: u8) {
        self.0.push(b);
    }
}

#[inline]
fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b >= 0x80
}

#[inline]
fn is_word_start(b: u8) -> bool {
    b.is_ascii_alphabetic() || b == b'_' || b >= 0x80
}

/// Output state of the scanner. Spacing is decided from the tokens, not from
/// the original whitespace, so `a=1` and `a = 1` agree; and `?, ?, ?`
/// collapses to `?`.
struct Emitter<'a, S: Sink> {
    sink: &'a mut S,
    started: bool,
    /// Last byte written.
    prev: u8,
    /// The last token was a placeholder.
    last_param: bool,
    /// A `,` after a placeholder is held back: dropped when another placeholder follows.
    held_comma: bool,
    /// The last token when it was a short word (lower case), for `in (` against `count(`.
    word: [u8; 10],
    word_len: usize,
}

/// Words after which `(` opens a group or a list, not a call or a column list.
fn is_keyword(w: &[u8]) -> bool {
    const KEYWORDS: &[&str] = &[
        "as",
        "in",
        "on",
        "and",
        "or",
        "not",
        "exists",
        "from",
        "join",
        "where",
        "select",
        "union",
        "all",
        "any",
        "values",
        "using",
        "over",
        "between",
        "then",
        "else",
        "when",
        "limit",
        "offset",
        "by",
        "set",
        "returning",
        "except",
        "intersect",
        "case",
        "with",
        "having",
        "distinct",
        "conflict",
        "lateral",
    ];
    KEYWORDS.iter().any(|k| k.as_bytes() == w)
}

#[inline]
fn is_op(b: u8) -> bool {
    matches!(
        b,
        b'=' | b'<' | b'>' | b'!' | b'|' | b'&' | b'+' | b'*' | b'/' | b'%' | b'^' | b'~' | b'@' | b'#'
    )
}

impl<S: Sink> Emitter<'_, S> {
    fn flush_comma(&mut self) {
        if self.held_comma {
            self.held_comma = false;
            self.sink.put(b',');
            self.prev = b',';
        }
    }

    fn token(&mut self, bytes: &[u8]) {
        self.flush_comma();
        self.write(bytes);
        self.last_param = false;
        if bytes.len() <= self.word.len() && bytes.iter().all(|b| b.is_ascii_alphabetic()) {
            self.word[..bytes.len()].copy_from_slice(bytes);
            self.word_len = bytes.len();
        }
    }

    fn space_before(&self, first: u8) -> bool {
        let p = self.prev;
        if !self.started || matches!(first, b')' | b',' | b';' | b'.' | b':') || matches!(p, b'(' | b'.' | b':') {
            return false;
        }
        if first == b'(' {
            // `count(`, `t(`: a call or a column list; `in (`, `values (`: a group.
            if is_word(p) {
                return self.word_len > 0 && is_keyword(&self.word[..self.word_len]);
            }
            return !matches!(p, b'"' | b'?' | b'`');
        }
        true
    }

    fn write(&mut self, bytes: &[u8]) {
        let Some(&first) = bytes.first() else { return };
        if self.space_before(first) {
            self.sink.put(b' ');
        }
        self.started = true;
        self.word_len = 0;
        for &b in bytes {
            self.sink.put(b);
        }
        self.prev = bytes[bytes.len() - 1];
    }

    fn param(&mut self) {
        if self.held_comma && self.last_param {
            // `?, ?`: one placeholder stands for the whole list.
            self.held_comma = false;
            return;
        }
        self.flush_comma();
        self.write(b"?");
        self.last_param = true;
    }

    fn comma(&mut self) {
        if self.last_param && !self.held_comma {
            self.held_comma = true;
        } else {
            self.token(b",");
        }
    }

    fn finish(&mut self) {
        self.flush_comma();
    }
}

fn skip_string(sql: &[u8], mut i: usize, backslash: bool) -> usize {
    // `i` is just past the opening quote.
    while i < sql.len() {
        match sql[i] {
            b'\\' if backslash => i += 2,
            b'\'' if sql.get(i + 1) == Some(&b'\'') => i += 2,
            b'\'' => return i + 1,
            _ => i += 1,
        }
    }
    sql.len()
}

fn scan<S: Sink>(sql: &[u8], sink: &mut S) {
    let mut e = Emitter {
        sink,
        started: false,
        prev: 0,
        last_param: false,
        held_comma: false,
        word: [0; 10],
        word_len: 0,
    };
    let n = sql.len();
    let mut i = 0;
    while i < n {
        let b = sql[i];
        match b {
            b' ' | b'\t' | b'\n' | b'\r' | 0x0c => i += 1,
            b'-' if sql.get(i + 1) == Some(&b'-') => {
                while i < n && sql[i] != b'\n' {
                    i += 1;
                }
            }
            b'/' if sql.get(i + 1) == Some(&b'*') => {
                i += 2;
                while i < n && !(sql[i] == b'*' && sql.get(i + 1) == Some(&b'/')) {
                    i += 1;
                }
                i = (i + 2).min(n);
            }
            b'\'' => {
                i = skip_string(sql, i + 1, false);
                e.param();
            }
            b'"' => {
                let start = i;
                i += 1;
                while i < n {
                    if sql[i] == b'"' {
                        if sql.get(i + 1) == Some(&b'"') {
                            i += 2;
                            continue;
                        }
                        i += 1;
                        break;
                    }
                    i += 1;
                }
                e.token(&sql[start..i]);
            }
            b'`' => {
                let start = i;
                i += 1;
                while i < n && sql[i] != b'`' {
                    i += 1;
                }
                i = (i + 1).min(n);
                e.token(&sql[start..i]);
            }
            b'?' => {
                i += 1;
                while i < n && sql[i].is_ascii_digit() {
                    i += 1;
                }
                e.param();
            }
            b'$' => {
                let mut j = i + 1;
                if j < n && sql[j].is_ascii_digit() {
                    while j < n && sql[j].is_ascii_digit() {
                        j += 1;
                    }
                    i = j;
                    e.param();
                } else {
                    // `$$ ... $$` or `$tag$ ... $tag$`.
                    while j < n && (is_word(sql[j]) && sql[j] != b'$') {
                        j += 1;
                    }
                    if j < n && sql[j] == b'$' && !sql[i + 1..j].first().is_some_and(u8::is_ascii_digit) {
                        let tag = &sql[i..=j];
                        let mut k = j + 1;
                        let mut end = n;
                        while k + tag.len() <= n {
                            if &sql[k..k + tag.len()] == tag {
                                end = k + tag.len();
                                break;
                            }
                            k += 1;
                        }
                        i = end;
                        e.param();
                    } else {
                        e.token(b"$");
                        i += 1;
                    }
                }
            }
            b':' if sql.get(i + 1) == Some(&b':') => {
                e.token(b"::");
                i += 2;
            }
            b':' if sql.get(i + 1).is_some_and(|c| is_word_start(*c)) && !is_word(e.prev) => {
                i += 1;
                while i < n && is_word(sql[i]) {
                    i += 1;
                }
                e.param();
            }
            b'-' if sql.get(i + 1) == Some(&b'>') => {
                let start = i;
                i += 1;
                while i < n && is_op(sql[i]) {
                    i += 1;
                }
                e.token(&sql[start..i]);
            }
            b'-' | b'0'..=b'9' => {
                if b == b'-' {
                    // A sign belongs to the number after an operator, `(`, `,` or the start.
                    let operand_before = e.started && (is_word(e.prev) || matches!(e.prev, b')' | b'?' | b'"'));
                    if !sql.get(i + 1).is_some_and(u8::is_ascii_digit) || operand_before {
                        e.token(b"-");
                        i += 1;
                        continue;
                    }
                    i += 1;
                }
                // A number (identifiers consume their own digits).
                if b == b'0' && matches!(sql.get(i + 1), Some(b'x' | b'X')) {
                    i += 2;
                    while i < n && sql[i].is_ascii_hexdigit() {
                        i += 1;
                    }
                } else {
                    while i < n && (sql[i].is_ascii_digit() || sql[i] == b'.') {
                        i += 1;
                    }
                    if i < n && matches!(sql[i], b'e' | b'E') {
                        let mut j = i + 1;
                        if j < n && matches!(sql[j], b'+' | b'-') {
                            j += 1;
                        }
                        if j < n && sql[j].is_ascii_digit() {
                            while j < n && sql[j].is_ascii_digit() {
                                j += 1;
                            }
                            i = j;
                        }
                    }
                }
                e.param();
            }
            b',' => {
                e.comma();
                i += 1;
            }
            _ if is_op(b) => {
                let start = i;
                while i < n && is_op(sql[i]) {
                    i += 1;
                }
                e.token(&sql[start..i]);
            }
            _ if is_word_start(b) => {
                let start = i;
                while i < n && (is_word(sql[i]) || sql[i] == b'$') {
                    i += 1;
                }
                let word = &sql[start..i];
                // `E'..'`, `X'..'`, `B'..'`, `N'..'`: a prefixed literal.
                if word.len() == 1 && sql.get(i) == Some(&b'\'') && matches!(word[0] | 0x20, b'e' | b'x' | b'b' | b'n')
                {
                    i = skip_string(sql, i + 1, word[0] | 0x20 == b'e');
                    e.param();
                    continue;
                }
                let mut lower = [0u8; 64];
                if word.len() <= lower.len() {
                    for (d, s) in lower.iter_mut().zip(word) {
                        *d = s.to_ascii_lowercase();
                    }
                    e.token(&lower[..word.len()]);
                } else {
                    let v: Vec<u8> = word.iter().map(u8::to_ascii_lowercase).collect();
                    e.token(&v);
                }
            }
            _ => {
                e.token(&sql[i..=i]);
                i += 1;
            }
        }
    }
    e.finish();
}

/// The fingerprint of `sql`: a 64-bit hash of its reduced text. No allocation.
pub fn fingerprint_hash(sql: &str) -> u64 {
    let mut h = Hasher(0xcbf2_9ce4_8422_2325);
    scan(sql.as_bytes(), &mut h);
    h.0
}

/// The key a statement of `backend` has in the statistics: its fingerprint
/// hash, salted per backend.
pub fn fingerprint_key(backend: &str, sql: &str) -> u64 {
    fingerprint_hash(sql)
        ^ if backend == "postgres" {
            0x9e37_79b9_7f4a_7c15
        } else {
            0
        }
}

/// The 16-hex-digit id of a statement in the statistics and the API.
pub fn fingerprint_id(backend: &str, sql: &str) -> String {
    format!("{:016x}", fingerprint_key(backend, sql))
}

/// The reduced text of `sql` (no literal values, whitespace collapsed,
/// keywords lower-cased, value lists reduced), cut to [`MAX_TEXT_BYTES`].
pub fn normalize(sql: &str) -> String {
    let mut t = Text(Vec::with_capacity(sql.len().min(MAX_TEXT_BYTES)));
    scan(sql.as_bytes(), &mut t);
    let mut s = String::from_utf8_lossy(&t.0).into_owned();
    if s.len() > MAX_TEXT_BYTES {
        let mut end = MAX_TEXT_BYTES;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        s.truncate(end);
        s.push_str("...");
    }
    s
}

/// The area (subsystem) a statement belongs to, from the first ForgeBase
/// system table in its reduced text, else from the label.
pub fn area_of(text: &str, label: Option<&QueryLabel>) -> &'static str {
    const PREFIXES: &[(&str, &str)] = &[
        ("_forgebase_stmt_", "insights"),
        ("_forgebase_slow_", "insights"),
        ("_forgebase_fn_stats", "insights"),
        ("_forgebase_tenant_usage", "insights"),
        ("_forgebase_billing", "billing"),
        ("_forgebase_credit", "credits"),
        ("_forgebase_usage", "entitlements"),
        ("_forgebase_entitlement", "entitlements"),
        ("_forgebase_plan", "entitlements"),
        ("_forgebase_quota", "entitlements"),
        ("_forgebase_feature", "entitlements"),
        ("_forgebase_addon", "entitlements"),
        ("_forgebase_subscription", "entitlements"),
        ("_forgebase_job", "jobs"),
        ("_forgebase_queue", "queue"),
        ("_forgebase_kv", "kv"),
        ("_forgebase_vector", "vector"),
        ("_forgebase_graph", "graph"),
        ("_forgebase_storage", "storage"),
        ("_forgebase_function", "functions"),
        ("_forgebase_auth", "auth"),
        ("_forgebase_session", "auth"),
        ("_forgebase_realm_identity", "auth"),
        ("_forgebase_authz", "policy"),
        ("_forgebase_org", "policy"),
        ("_forgebase_role", "policy"),
        ("_forgebase_project", "policy"),
        ("_forgebase_environment", "policy"),
        ("_forgebase_realm", "policy"),
        ("_forgebase_acl", "policy"),
        ("_forgebase_team", "policy"),
        ("_forgebase_member", "policy"),
        ("_forgebase_outbox", "realtime"),
        ("_forgebase_realtime", "realtime"),
        ("_forgebase_customize", "customize"),
        ("_forgebase_domain", "customize"),
        ("_forgebase_branding", "customize"),
        ("_forgebase_email", "customize"),
        ("_forgebase_legal", "customize"),
        ("_forgebase_db_", "database"),
        ("_forgebase_migrations", "system"),
    ];
    if let Some(at) = text.find("_forgebase_") {
        let rest = &text[at..];
        for (p, area) in PREFIXES {
            if rest.starts_with(p) {
                return area;
            }
        }
        return "system";
    }
    if label.is_some() { "data" } else { "other" }
}

// ---------------------------------------------------------------------------
// Aggregation
// ---------------------------------------------------------------------------

struct Entry {
    key: u64,
    text: Box<str>,
    area: &'static str,
    backend: &'static str,
    /// Statements of ForgeBase's own insight tables: not counted.
    ignored: bool,
    calls: AtomicU64,
    errors: AtomicU64,
    writes: AtomicU64,
    slow: AtomicU64,
    total_us: AtomicU64,
    max_us: AtomicU64,
    min_us: AtomicU64,
    rows: AtomicU64,
    sketch: AtomicSketch,
    idle: AtomicU64,
}

impl Entry {
    fn new(key: u64, text: String, area: &'static str, backend: &'static str) -> Self {
        Self {
            key,
            ignored: area == "insights",
            text: text.into_boxed_str(),
            area,
            backend,
            calls: AtomicU64::new(0),
            errors: AtomicU64::new(0),
            writes: AtomicU64::new(0),
            slow: AtomicU64::new(0),
            total_us: AtomicU64::new(0),
            max_us: AtomicU64::new(0),
            min_us: AtomicU64::new(u64::MAX),
            rows: AtomicU64::new(0),
            sketch: AtomicSketch::new(),
            idle: AtomicU64::new(0),
        }
    }

    #[inline]
    fn observe(&self, us: u64, rows: u64, error: bool, write: bool, slow: bool) {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.total_us.fetch_add(us, Ordering::Relaxed);
        self.rows.fetch_add(rows, Ordering::Relaxed);
        self.sketch.record(us);
        self.max_us.fetch_max(us, Ordering::Relaxed);
        self.min_us.fetch_min(us, Ordering::Relaxed);
        if error {
            self.errors.fetch_add(1, Ordering::Relaxed);
        }
        if write {
            self.writes.fetch_add(1, Ordering::Relaxed);
        }
        if slow {
            self.slow.fetch_add(1, Ordering::Relaxed);
        }
    }

    fn take(&self, clear: bool) -> Option<StatementAggregate> {
        let calls = if clear {
            self.calls.swap(0, Ordering::Relaxed)
        } else {
            self.calls.load(Ordering::Relaxed)
        };
        if calls == 0 {
            return None;
        }
        let get = |a: &AtomicU64| {
            if clear {
                a.swap(0, Ordering::Relaxed)
            } else {
                a.load(Ordering::Relaxed)
            }
        };
        let min = if clear {
            self.min_us.swap(u64::MAX, Ordering::Relaxed)
        } else {
            self.min_us.load(Ordering::Relaxed)
        };
        Some(StatementAggregate {
            fingerprint: format!("{:016x}", self.key),
            text: self.text.to_string(),
            area: self.area,
            backend: self.backend,
            calls,
            errors: get(&self.errors),
            writes: get(&self.writes),
            slow: get(&self.slow),
            total_us: get(&self.total_us),
            max_us: get(&self.max_us),
            min_us: if min == u64::MAX { 0 } else { min },
            rows: get(&self.rows),
            sketch: if clear { self.sketch.drain() } else { self.sketch.peek() },
        })
    }
}

/// What the statistics hold for one fingerprint over some period.
#[derive(Debug, Clone)]
pub struct StatementAggregate {
    /// 16 hex digits identifying the fingerprint.
    pub fingerprint: String,
    /// The reduced statement text.
    pub text: String,
    /// The subsystem (see [`area_of`]).
    pub area: &'static str,
    /// `sqlite` or `postgres`.
    pub backend: &'static str,
    /// Statements run.
    pub calls: u64,
    /// Statements that failed.
    pub errors: u64,
    /// Statements run inside a write transaction.
    pub writes: u64,
    /// Statements at or above the slow threshold.
    pub slow: u64,
    /// Total wall time in microseconds.
    pub total_us: u64,
    /// Longest statement in microseconds.
    pub max_us: u64,
    /// Shortest statement in microseconds.
    pub min_us: u64,
    /// Rows returned or changed.
    pub rows: u64,
    /// Latency distribution in microseconds.
    pub sketch: Sketch,
}

/// One statement at or above the slow threshold.
#[derive(Debug, Clone)]
pub struct SlowStatement {
    /// Unix milliseconds the statement finished.
    pub at_ms: i64,
    /// Fingerprint id.
    pub fingerprint: String,
    /// The reduced text: no literal values.
    pub text: String,
    /// The subsystem.
    pub area: &'static str,
    /// `sqlite` or `postgres`.
    pub backend: &'static str,
    /// Wall time in microseconds.
    pub duration_us: u64,
    /// Rows returned or changed.
    pub rows: u64,
    /// Whether the statement failed.
    pub error: bool,
    /// Whether it ran inside a write transaction.
    pub write: bool,
}

/// Everything [`StatementStats::drain`] took out of memory.
#[derive(Debug, Default)]
pub struct Drained {
    /// Aggregates per fingerprint, busiest first (by total time).
    pub aggregates: Vec<StatementAggregate>,
    /// Slow statements, oldest first.
    pub slow: Vec<SlowStatement>,
    /// Slow statements not kept because the per-drain budget was spent.
    pub slow_dropped: u64,
}

/// The statement statistics of the process.
pub struct StatementStats {
    enabled: AtomicBool,
    shards: [RwLock<HashMap<u64, Entry>>; SHARDS],
    count: AtomicU64,
    overflow: [Entry; 2],
    slow: Mutex<VecDeque<SlowStatement>>,
    slow_budget: AtomicI64,
    slow_dropped: AtomicU64,
}

impl std::fmt::Debug for StatementStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("StatementStats")
            .field("enabled", &self.enabled.load(Ordering::Relaxed))
            .field("fingerprints", &self.count.load(Ordering::Relaxed))
            .finish_non_exhaustive()
    }
}

impl Default for StatementStats {
    fn default() -> Self {
        Self::new()
    }
}

impl StatementStats {
    /// An empty collector, enabled.
    pub fn new() -> Self {
        Self {
            enabled: AtomicBool::new(true),
            shards: std::array::from_fn(|_| RwLock::new(HashMap::new())),
            count: AtomicU64::new(0),
            overflow: [
                Entry::new(1, OVERFLOW_TEXT.into(), "other", "sqlite"),
                Entry::new(2, OVERFLOW_TEXT.into(), "other", "postgres"),
            ],
            slow: Mutex::new(VecDeque::new()),
            slow_budget: AtomicI64::new(MAX_SLOW_PER_DRAIN),
            slow_dropped: AtomicU64::new(0),
        }
    }

    /// The collector of this process, shared by every database handle.
    pub fn global() -> &'static Arc<StatementStats> {
        static GLOBAL: OnceLock<Arc<StatementStats>> = OnceLock::new();
        GLOBAL.get_or_init(|| Arc::new(StatementStats::new()))
    }

    /// Turns the aggregation on or off (`[observability] query_stats`).
    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
    }

    /// Whether statements are aggregated.
    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Fingerprints held in memory.
    pub fn fingerprints(&self) -> u64 {
        self.count.load(Ordering::Relaxed)
    }

    /// Records one finished statement. This is the hot path: see the module
    /// docs. `slow` is whether it reached the slow threshold.
    #[allow(clippy::too_many_arguments)]
    pub fn record(
        &self,
        backend: &'static str,
        sql: &str,
        label: Option<&QueryLabel>,
        elapsed: Duration,
        rows: u64,
        error: bool,
        write: bool,
        slow: bool,
    ) {
        if !self.enabled.load(Ordering::Relaxed) {
            return;
        }
        let us = u64::try_from(elapsed.as_micros()).unwrap_or(u64::MAX);
        let key = fingerprint_key(backend, sql);
        let shard = &self.shards[(key as usize) % SHARDS];
        {
            let map = shard.read().unwrap_or_else(|p| p.into_inner());
            if let Some(e) = map.get(&key) {
                if !e.ignored {
                    e.observe(us, rows, error, write, slow);
                    if slow {
                        self.note_slow(e, us, rows, error, write);
                    }
                }
                return;
            }
        }
        // First statement of this fingerprint (or the table is full).
        if self.count.load(Ordering::Relaxed) as usize >= MAX_FINGERPRINTS {
            let o = &self.overflow[usize::from(backend == "postgres")];
            o.observe(us, rows, error, write, slow);
            return;
        }
        let text = normalize(sql);
        let area = area_of(&text, label);
        let mut map = shard.write().unwrap_or_else(|p| p.into_inner());
        let created = !map.contains_key(&key);
        let e = map.entry(key).or_insert_with(|| Entry::new(key, text, area, backend));
        if created {
            self.count.fetch_add(1, Ordering::Relaxed);
        }
        if !e.ignored {
            e.observe(us, rows, error, write, slow);
            if slow {
                self.note_slow(e, us, rows, error, write);
            }
        }
    }

    #[cold]
    fn note_slow(&self, e: &Entry, us: u64, rows: u64, error: bool, write: bool) {
        metrics::counter!("forgebase_db_slow_statements_total", "area" => e.area).increment(1);
        if self.slow_budget.fetch_sub(1, Ordering::Relaxed) <= 0 {
            self.slow_dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let at_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX));
        let s = SlowStatement {
            at_ms,
            fingerprint: format!("{:016x}", e.key),
            text: e.text.to_string(),
            area: e.area,
            backend: e.backend,
            duration_us: us,
            rows,
            error,
            write,
        };
        self.slow.lock().unwrap_or_else(|p| p.into_inner()).push_back(s);
    }

    /// Takes the aggregates and slow statements out of memory, resetting
    /// the counters. Fingerprints idle for [`IDLE_DRAINS`] drains are dropped.
    pub fn drain(&self) -> Drained {
        let mut aggregates = Vec::new();
        for shard in &self.shards {
            let mut dropped = 0u64;
            {
                let mut map = shard.write().unwrap_or_else(|p| p.into_inner());
                map.retain(|_, e| match e.take(true) {
                    Some(a) => {
                        e.idle.store(0, Ordering::Relaxed);
                        aggregates.push(a);
                        true
                    }
                    None => {
                        let keep = e.idle.fetch_add(1, Ordering::Relaxed) < u64::from(IDLE_DRAINS);
                        if !keep {
                            dropped += 1;
                        }
                        keep
                    }
                });
            }
            if dropped > 0 {
                self.count.fetch_sub(dropped, Ordering::Relaxed);
            }
        }
        for o in &self.overflow {
            if let Some(a) = o.take(true) {
                aggregates.push(a);
            }
        }
        aggregates.sort_by_key(|a| std::cmp::Reverse(a.total_us));
        let slow = std::mem::take(&mut *self.slow.lock().unwrap_or_else(|p| p.into_inner()))
            .into_iter()
            .collect();
        self.slow_budget.store(MAX_SLOW_PER_DRAIN, Ordering::Relaxed);
        Drained {
            aggregates,
            slow,
            slow_dropped: self.slow_dropped.swap(0, Ordering::Relaxed),
        }
    }

    /// The aggregates in memory (since the last drain), without clearing them.
    pub fn peek(&self) -> Vec<StatementAggregate> {
        let mut out = Vec::new();
        for shard in &self.shards {
            let map = shard.read().unwrap_or_else(|p| p.into_inner());
            out.extend(map.values().filter_map(|e| e.take(false)));
        }
        for o in &self.overflow {
            out.extend(o.take(false));
        }
        out.sort_by_key(|a| std::cmp::Reverse(a.total_us));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(sql: &str) -> String {
        normalize(sql)
    }

    #[test]
    fn literals_and_placeholders_share_a_fingerprint() {
        let a = "SELECT id, name FROM users WHERE id = 7 AND name = 'bob' LIMIT 10";
        let b = "select id,name  from users\nWHERE id=?1 and name = $2 limit ?";
        let c = "SELECT id, name FROM users WHERE id = 99.5e3 AND name = 'it''s' LIMIT 1 -- who\n";
        assert_eq!(fp(a), fp(b));
        assert_eq!(fp(a), fp(c));
        assert_eq!(fingerprint_hash(a), fingerprint_hash(b));
        assert_eq!(fingerprint_hash(a), fingerprint_hash(c));
        assert_eq!(fp(a), "select id, name from users where id = ? and name = ? limit ?");
        assert_ne!(
            fingerprint_hash(a),
            fingerprint_hash("SELECT id FROM users WHERE id = 7")
        );
    }

    #[test]
    fn in_lists_and_comments_reduce() {
        assert_eq!(
            fp("SELECT 1 FROM t WHERE a IN (1, 2, 3)"),
            fp("SELECT 1 FROM t WHERE a IN (?)")
        );
        assert_eq!(
            fp("SELECT 1 FROM t WHERE a IN (?, ?, ?, ?) AND b = 1"),
            "select ? from t where a in (?) and b = ?"
        );
        assert_eq!(fp("SELECT /* hint */ 1 -- x"), "select ?");
        assert_eq!(fp("select a,b from t"), "select a, b from t");
    }

    #[test]
    fn strings_dollar_quotes_casts_and_identifiers() {
        // A secret in a literal never reaches the text.
        for sql in [
            "INSERT INTO t (a) VALUES ('hunter2')",
            "INSERT INTO t (a) VALUES (E'hunter2\\'x')",
            "INSERT INTO t (a) VALUES ($$hunter2$$)",
            "INSERT INTO t (a) VALUES ($tag$hunter2$tag$)",
            "INSERT INTO t (a) VALUES (X'68756e74657232')",
        ] {
            let n = fp(sql);
            assert!(!n.contains("hunter2") && !n.contains("6875"), "{sql} -> {n}");
            assert_eq!(n, "insert into t(a) values (?)", "{sql}");
        }
        assert_eq!(
            fp("SELECT a::int FROM t WHERE b = :name"),
            "select a::int from t where b = ?"
        );
        assert_eq!(
            fp("SELECT col1, \"Mixed Case\" FROM t2"),
            "select col1, \"Mixed Case\" from t2"
        );
        assert_eq!(
            fp("UPDATE t SET a = a + 1 WHERE b = -5"),
            "update t set a = a + ? where b = ?"
        );
    }

    #[test]
    fn areas_follow_the_first_system_table() {
        assert_eq!(area_of("select ? from _forgebase_billing_prices", None), "billing");
        assert_eq!(area_of("select ? from _forgebase_queue_messages", None), "queue");
        assert_eq!(area_of("select ? from _forgebase_kv_entries", None), "kv");
        assert_eq!(area_of("select ? from _forgebase_something_new", None), "system");
        assert_eq!(area_of("select ? from posts", Some(&QueryLabel::default())), "data");
        assert_eq!(area_of("select ? from posts", None), "other");
        assert_eq!(
            area_of("insert into _forgebase_stmt_stats(a)values(?)", None),
            "insights"
        );
    }

    #[test]
    fn aggregates_count_and_drain() {
        let s = StatementStats::new();
        let ms = Duration::from_millis;
        s.record(
            "sqlite",
            "SELECT * FROM t WHERE id = 1",
            None,
            ms(1),
            1,
            false,
            false,
            false,
        );
        s.record(
            "sqlite",
            "SELECT * FROM t WHERE id = 2",
            None,
            ms(3),
            1,
            false,
            false,
            false,
        );
        s.record(
            "sqlite",
            "select * from t where id = ?",
            None,
            ms(9),
            0,
            true,
            false,
            true,
        );
        s.record(
            "sqlite",
            "INSERT INTO _forgebase_stmt_stats (a) VALUES (1)",
            None,
            ms(1),
            1,
            false,
            true,
            false,
        );
        let live = s.peek();
        assert_eq!(live.len(), 1, "insight tables are not counted: {live:?}");
        let d = s.drain();
        assert_eq!(d.aggregates.len(), 1);
        let a = &d.aggregates[0];
        assert_eq!((a.calls, a.errors, a.slow, a.rows), (3, 1, 1, 2));
        assert_eq!((a.total_us, a.max_us, a.min_us), (13_000, 9_000, 1_000));
        assert_eq!(a.sketch.count(), 3);
        assert_eq!(d.slow.len(), 1);
        assert_eq!(d.slow[0].text, "select * from t where id = ?");
        assert!(d.slow[0].error);
        assert!(s.drain().aggregates.is_empty(), "the drain cleared the counters");
    }

    #[test]
    fn memory_is_bounded() {
        let s = StatementStats::new();
        for i in 0..(MAX_FINGERPRINTS + 500) {
            s.record(
                "sqlite",
                &format!("SELECT c{i} FROM t"),
                None,
                Duration::from_micros(5),
                0,
                false,
                false,
                false,
            );
        }
        assert!(s.fingerprints() as usize <= MAX_FINGERPRINTS);
        let d = s.drain();
        let other = d
            .aggregates
            .iter()
            .find(|a| a.text == OVERFLOW_TEXT)
            .expect("overflow entry");
        assert_eq!(other.calls, 500);
    }

    #[test]
    fn slow_statements_are_sampled_per_drain() {
        let s = StatementStats::new();
        for _ in 0..(MAX_SLOW_PER_DRAIN as usize + 30) {
            s.record(
                "sqlite",
                "SELECT 1",
                None,
                Duration::from_secs(1),
                0,
                false,
                false,
                true,
            );
        }
        let d = s.drain();
        assert_eq!(d.slow.len() as i64, MAX_SLOW_PER_DRAIN);
        assert_eq!(d.slow_dropped, 30);
        assert_eq!(
            d.aggregates[0].slow as i64,
            MAX_SLOW_PER_DRAIN + 30,
            "the counter still sees them all"
        );
    }
}

//! Replication on the primary side (ADR-0029 §2.2, §2.3, §2.8; FB-168):
//! the `forgebase_replicator` role, physical slots and their WAL limits,
//! LSNs, cluster facts, `pg_controldata`, the `pg_hba.conf` entries
//! replication needs (as data, for FB-153's generator), exact engine
//! identities, and the topology service ([`service`]) the admin API
//! (FB-175) and `forgebase node` call.

use std::borrow::Cow;
use std::fmt;
use std::net::IpAddr;
use std::path::Path;
use std::process::Stdio;
use std::str::FromStr;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use sqlx::{Connection, PgConnection, Row};

use crate::discover::EngineDir;
use crate::error::{EngineError, Result};
use crate::secret::Role;

pub mod service;

/// Prefix of the physical slot of a managed replica (`fbr_<node>`).
pub const SLOT_PREFIX: &str = "fbr_";

/// The physical slot of node `name` on its primary: `fbr_<name>` with `-`
/// written as `_` (slot names allow `a-z`, `0-9` and `_`).
pub fn slot_name(name: &str) -> String {
    let mut s = String::with_capacity(SLOT_PREFIX.len() + name.len());
    s.push_str(SLOT_PREFIX);
    for c in name.chars() {
        s.push(match c {
            'a'..='z' | '0'..='9' | '_' => c,
            'A'..='Z' => c.to_ascii_lowercase(),
            _ => '_',
        });
    }
    s
}

/// How much WAL the primary keeps for slots (ADR-0029 §2.2).
///
/// PostgreSQL keeps WAL for a slot without limit by default, so one
/// stopped replica can fill the primary's disk. ForgeBase bounds it:
/// `max_slot_wal_keep_size` (8 GB) and, on PostgreSQL 18 and later,
/// `idle_replication_slot_timeout` (1 day). A slot past either limit is
/// invalidated; its replica shows `needs_rebuild`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotLimits {
    /// `max_slot_wal_keep_size` (`[database.replicas] slot_wal_keep_size`), e.g. `"8GB"`.
    pub wal_keep_size: Cow<'static, str>,
    /// `idle_replication_slot_timeout`; `None` leaves it off.
    pub idle_timeout: Option<Duration>,
}

impl SlotLimits {
    /// 8 GB and one day.
    pub const DEFAULT: SlotLimits = SlotLimits {
        wal_keep_size: Cow::Borrowed("8GB"),
        idle_timeout: Some(Duration::from_secs(86_400)),
    };

    /// The settings for an engine of `major`.
    pub fn settings(&self, major: u32) -> Vec<(&'static str, String)> {
        let mut out = vec![("max_slot_wal_keep_size", self.wal_keep_size.to_string())];
        if major >= 18
            && let Some(t) = self.idle_timeout
        {
            out.push(("idle_replication_slot_timeout", duration_setting(t)));
        }
        out
    }

    /// Checks `wal_keep_size` (a PostgreSQL size: digits and an optional
    /// `kB`, `MB`, `GB` or `TB`; `-1` for unlimited).
    ///
    /// # Errors
    /// A value PostgreSQL would refuse at start.
    pub fn check(&self) -> Result<()> {
        let v = self.wal_keep_size.trim();
        let digits = v.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        let unit = &v[digits.len()..];
        let ok = v == "-1"
            || (!digits.is_empty()
                && digits.bytes().all(|b| b.is_ascii_digit())
                && ["", "kB", "MB", "GB", "TB"].contains(&unit));
        if ok {
            Ok(())
        } else {
            Err(EngineError::Config(format!(
                "slot_wal_keep_size {v:?} is not a size like \"8GB\" (kB, MB, GB, TB) or -1"
            )))
        }
    }
}

impl Default for SlotLimits {
    fn default() -> Self {
        Self::DEFAULT
    }
}

/// `1d`, `6h`, `30min`, `45s` (PostgreSQL time units).
fn duration_setting(d: Duration) -> String {
    let s = d.as_secs();
    if s == 0 {
        "0".into()
    } else if s % 86_400 == 0 {
        format!("{}d", s / 86_400)
    } else if s % 3_600 == 0 {
        format!("{}h", s / 3_600)
    } else if s % 60 == 0 {
        format!("{}min", s / 60)
    } else {
        format!("{s}s")
    }
}

/// A write-ahead log position (`pg_lsn`), shown as PostgreSQL does (`16/B374D848`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct Lsn(pub u64);

impl FromStr for Lsn {
    type Err = EngineError;

    fn from_str(s: &str) -> Result<Self> {
        let bad = || EngineError::Sql(format!("not an LSN: {s:?}"));
        let (hi, lo) = s.trim().split_once('/').ok_or_else(bad)?;
        let hi = u64::from_str_radix(hi, 16).map_err(|_| bad())?;
        let lo = u64::from_str_radix(lo, 16).map_err(|_| bad())?;
        if hi > u64::from(u32::MAX) || lo > u64::from(u32::MAX) {
            return Err(bad());
        }
        Ok(Lsn((hi << 32) | lo))
    }
}

impl fmt::Display for Lsn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:X}/{:X}", self.0 >> 32, self.0 & 0xFFFF_FFFF)
    }
}

impl Serialize for Lsn {
    fn serialize<S: serde::Serializer>(&self, s: S) -> std::result::Result<S::Ok, S::Error> {
        s.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for Lsn {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> std::result::Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

fn opt_lsn(v: Option<String>) -> Result<Option<Lsn>> {
    v.map(|s| s.parse()).transpose()
}

fn sql_err(what: &str) -> impl FnOnce(sqlx::Error) -> EngineError + '_ {
    move |e| EngineError::Sql(format!("{what}: {e}"))
}

/// Connects to `url` (a URL from [`crate::EngineHandle::url`] or built for a
/// remote node).
///
/// # Errors
/// The connection fails.
pub async fn connect(url: &crate::Secret) -> Result<PgConnection> {
    PgConnection::connect(url.expose())
        .await
        .map_err(|e| EngineError::Sql(format!("connect: {e}")))
}

// ---------------------------------------------------------------------------
// The replicator role.
// ---------------------------------------------------------------------------

/// Functions `pg_rewind` calls on its source (PostgreSQL docs, pg_rewind
/// "Notes"): granted to `forgebase_replicator` so a remote old primary can
/// rewind from the new one without a superuser. The role can already copy
/// the whole cluster with `pg_basebackup`, so this reveals nothing more.
pub const REWIND_FUNCTIONS: &[&str] = &[
    "pg_catalog.pg_ls_dir(text, boolean, boolean)",
    "pg_catalog.pg_stat_file(text, boolean)",
    "pg_catalog.pg_read_binary_file(text)",
    "pg_catalog.pg_read_binary_file(text, bigint, bigint, boolean)",
];

/// `GRANT EXECUTE` of [`REWIND_FUNCTIONS`] to the replicator (bootstrap,
/// database `postgres`, which `pg_rewind` connects to).
pub(crate) fn rewind_grants_sql() -> String {
    REWIND_FUNCTIONS
        .iter()
        .map(|f| format!("GRANT EXECUTE ON FUNCTION {f} TO {};", Role::Replicator.name()))
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------------------
// Slots and senders.
// ---------------------------------------------------------------------------

/// One row of `pg_replication_slots`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotInfo {
    /// Slot name.
    pub name: String,
    /// A walsender uses it now.
    pub active: bool,
    /// Oldest WAL the slot keeps.
    pub restart_lsn: Option<Lsn>,
    /// `reserved`, `extended`, `unreserved` or `lost`.
    pub wal_status: Option<String>,
    /// Why PostgreSQL invalidated it (`wal_removed`, `idle_timeout`, …).
    pub invalidation_reason: Option<String>,
}

impl SlotInfo {
    /// The replica can no longer stream from this slot: re-clone or rewind it.
    pub fn needs_rebuild(&self) -> bool {
        self.wal_status.as_deref() == Some("lost") || self.invalidation_reason.is_some()
    }
}

/// Creates the physical slot `name` (reserving WAL now) unless it exists.
/// Returns whether it was created.
///
/// # Errors
/// SQL errors (no more `max_replication_slots`, not a primary).
pub async fn ensure_slot(conn: &mut PgConnection, name: &str) -> Result<bool> {
    let exists: Option<i32> = sqlx::query_scalar("SELECT 1 FROM pg_replication_slots WHERE slot_name = $1")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await
        .map_err(sql_err("read slots"))?;
    if exists.is_some() {
        return Ok(false);
    }
    sqlx::query("SELECT pg_create_physical_replication_slot($1, true)")
        .bind(name)
        .execute(&mut *conn)
        .await
        .map_err(sql_err("create the replication slot"))?;
    Ok(true)
}

/// Drops slot `name` if it exists, ending the walsender that uses it first.
/// Returns whether a slot was dropped.
///
/// # Errors
/// SQL errors.
pub async fn drop_slot(conn: &mut PgConnection, name: &str) -> Result<bool> {
    for _ in 0..50 {
        let row: Option<(bool, Option<i32>)> =
            sqlx::query_as("SELECT active, active_pid FROM pg_replication_slots WHERE slot_name = $1")
                .bind(name)
                .fetch_optional(&mut *conn)
                .await
                .map_err(sql_err("read slots"))?;
        match row {
            None => return Ok(false),
            Some((true, pid)) => {
                if let Some(pid) = pid {
                    let _ = sqlx::query("SELECT pg_terminate_backend($1)")
                        .bind(pid)
                        .execute(&mut *conn)
                        .await;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Some((false, _)) => {
                sqlx::query("SELECT pg_drop_replication_slot($1)")
                    .bind(name)
                    .execute(&mut *conn)
                    .await
                    .map_err(sql_err("drop the replication slot"))?;
                return Ok(true);
            }
        }
    }
    Err(EngineError::State(format!("replication slot {name} stays in use")))
}

/// Every physical slot.
///
/// # Errors
/// SQL errors.
pub async fn slots(conn: &mut PgConnection) -> Result<Vec<SlotInfo>> {
    let rows = sqlx::query(
        "SELECT slot_name::text, active, restart_lsn::text, wal_status, invalidation_reason::text \
         FROM pg_replication_slots WHERE slot_type = 'physical' ORDER BY slot_name",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(sql_err("read slots"))?;
    rows.into_iter()
        .map(|r| {
            Ok(SlotInfo {
                name: r.try_get(0).map_err(sql_err("slot_name"))?,
                active: r.try_get(1).map_err(sql_err("active"))?,
                restart_lsn: opt_lsn(r.try_get(2).map_err(sql_err("restart_lsn"))?)?,
                wal_status: r.try_get(3).map_err(sql_err("wal_status"))?,
                invalidation_reason: r.try_get(4).map_err(sql_err("invalidation_reason"))?,
            })
        })
        .collect()
}

/// One row of `pg_stat_replication` (the primary's view of a standby).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Sender {
    /// `application_name` of the standby (ForgeBase sets the node name).
    pub application_name: String,
    /// `startup`, `catchup`, `streaming`, …
    pub state: String,
    /// Last position sent.
    pub sent_lsn: Option<Lsn>,
    /// Last position the standby replayed.
    pub replay_lsn: Option<Lsn>,
    /// The standby's address (`None` over a Unix socket).
    pub client_addr: Option<String>,
}

/// Every walsender of physical replication (needs a superuser or
/// `pg_monitor` to see other roles' rows).
///
/// # Errors
/// SQL errors.
pub async fn senders(conn: &mut PgConnection) -> Result<Vec<Sender>> {
    let rows = sqlx::query(
        "SELECT application_name::text, state::text, sent_lsn::text, replay_lsn::text, host(client_addr) \
         FROM pg_stat_replication ORDER BY application_name",
    )
    .fetch_all(&mut *conn)
    .await
    .map_err(sql_err("read pg_stat_replication"))?;
    rows.into_iter()
        .map(|r| {
            Ok(Sender {
                application_name: r.try_get::<Option<String>, _>(0).map_err(sql_err("application_name"))?.unwrap_or_default(),
                state: r.try_get::<Option<String>, _>(1).map_err(sql_err("state"))?.unwrap_or_default(),
                sent_lsn: opt_lsn(r.try_get(2).map_err(sql_err("sent_lsn"))?)?,
                replay_lsn: opt_lsn(r.try_get(3).map_err(sql_err("replay_lsn"))?)?,
                client_addr: r.try_get(4).map_err(sql_err("client_addr"))?,
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Facts of a running cluster.
// ---------------------------------------------------------------------------

/// What one running cluster says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClusterFacts {
    /// `pg_control_system().system_identifier`: equal on a primary and its replicas.
    pub system_identifier: String,
    /// `server_version_num`.
    pub version_num: u32,
    /// `pg_is_in_recovery()`.
    pub in_recovery: bool,
    /// Current timeline (the WAL file's on a primary, the received one on a standby).
    pub timeline: Option<i64>,
    /// Primary: `pg_current_wal_lsn()`; standby: `pg_last_wal_replay_lsn()`.
    pub lsn: Option<Lsn>,
    /// Standby: `pg_stat_wal_receiver.status` (`streaming`, …); `None` without a receiver.
    pub receiver: Option<String>,
}

/// The WAL receiver's status on a standby (`streaming`, …), `None` without
/// one. Roles without `pg_read_all_stats` see the row's details as NULL,
/// so for them a receiver that received WAL (`pg_last_wal_receive_lsn()`)
/// with a live receiver process counts as `streaming`.
///
/// # Errors
/// SQL errors.
pub async fn receiver_status(conn: &mut PgConnection) -> Result<Option<String>> {
    let row: Option<(Option<String>, Option<i32>, Option<String>)> = sqlx::query_as(
        "SELECT status::text, pid, pg_last_wal_receive_lsn()::text FROM pg_stat_wal_receiver",
    )
    .fetch_optional(&mut *conn)
    .await
    .map_err(sql_err("read pg_stat_wal_receiver"))?;
    Ok(match row {
        None => None,
        Some((Some(status), _, _)) => Some(status),
        Some((None, Some(_), Some(_))) => Some("streaming".to_owned()),
        Some((None, _, _)) => Some("starting".to_owned()),
    })
}

/// Reads [`ClusterFacts`] (any role that may log in).
///
/// # Errors
/// SQL errors.
pub async fn facts(conn: &mut PgConnection) -> Result<ClusterFacts> {
    let row = sqlx::query(
        "SELECT (SELECT system_identifier::text FROM pg_control_system()), \
                current_setting('server_version_num')::int, pg_is_in_recovery()",
    )
    .fetch_one(&mut *conn)
    .await
    .map_err(sql_err("read the cluster facts"))?;
    let system_identifier: String = row.try_get(0).map_err(sql_err("system_identifier"))?;
    let version_num: i32 = row.try_get(1).map_err(sql_err("server_version_num"))?;
    let in_recovery: bool = row.try_get(2).map_err(sql_err("pg_is_in_recovery"))?;
    let (timeline, lsn, receiver) = if in_recovery {
        let row = sqlx::query(
            "SELECT pg_last_wal_replay_lsn()::text, \
                    COALESCE((SELECT received_tli::bigint FROM pg_stat_wal_receiver), \
                             (SELECT timeline_id::bigint FROM pg_control_checkpoint()))",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(sql_err("read the standby position"))?;
        let receiver = receiver_status(conn).await?;
        (
            row.try_get::<Option<i64>, _>(1).map_err(sql_err("timeline"))?,
            opt_lsn(row.try_get(0).map_err(sql_err("replay_lsn"))?)?,
            receiver,
        )
    } else {
        let row = sqlx::query(
            "SELECT pg_current_wal_lsn()::text, \
                    ('x' || substr(pg_walfile_name(pg_current_wal_lsn()), 1, 8))::bit(32)::bigint",
        )
        .fetch_one(&mut *conn)
        .await
        .map_err(sql_err("read the primary position"))?;
        (
            row.try_get::<Option<i64>, _>(1).map_err(sql_err("timeline"))?,
            opt_lsn(row.try_get(0).map_err(sql_err("current_lsn"))?)?,
            None,
        )
    };
    Ok(ClusterFacts {
        system_identifier,
        version_num: u32::try_from(version_num).unwrap_or(0),
        in_recovery,
        timeline,
        lsn,
        receiver,
    })
}

// ---------------------------------------------------------------------------
// pg_controldata.
// ---------------------------------------------------------------------------

/// What `pg_controldata` says about a stopped (or running) cluster.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ControlData {
    /// `Database cluster state` (`shut down`, `shut down in recovery`, `in production`, …).
    pub state: String,
    /// `Latest checkpoint location`: on a clean shutdown, the shutdown checkpoint record.
    pub checkpoint: Lsn,
    /// `Latest checkpoint's TimeLineID`.
    pub timeline: u32,
    /// `Database system identifier`.
    pub system_identifier: String,
}

impl ControlData {
    /// A primary stopped by a fast or smart shutdown: every commit is in
    /// WAL before the shutdown checkpoint, and walsenders sent it all
    /// before they exited.
    pub fn primary_stopped_cleanly(&self) -> bool {
        self.state == "shut down"
    }

    /// Parses `pg_controldata` output.
    ///
    /// # Errors
    /// A missing line.
    pub fn parse(text: &str) -> Result<Self> {
        let field = |name: &str| {
            text.lines()
                .find_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(':')))
                .map(str::trim)
                .ok_or_else(|| EngineError::State(format!("pg_controldata printed no {name:?}")))
        };
        Ok(Self {
            state: field("Database cluster state")?.to_owned(),
            checkpoint: field("Latest checkpoint location")?.parse()?,
            timeline: field("Latest checkpoint's TimeLineID")?
                .parse()
                .map_err(|_| EngineError::State("pg_controldata printed a bad TimeLineID".into()))?,
            system_identifier: field("Database system identifier")?.to_owned(),
        })
    }
}

/// Runs `pg_controldata` on `pgdata`.
///
/// # Errors
/// The program fails or prints something unexpected.
pub async fn control_data(engine: &EngineDir, pgdata: &Path) -> Result<ControlData> {
    let bin = engine.bin("pg_controldata");
    let out = tokio::process::Command::new(&bin)
        .arg(pgdata)
        .env("LC_ALL", "C")
        .env_remove("PGDATA")
        .stdin(Stdio::null())
        .output()
        .await
        .map_err(|e| EngineError::io("run", &bin, e))?;
    if !out.status.success() {
        return Err(EngineError::Cluster {
            dir: pgdata.to_path_buf(),
            reason: format!("pg_controldata failed: {}", String::from_utf8_lossy(&out.stderr).trim()),
        });
    }
    ControlData::parse(&String::from_utf8_lossy(&out.stdout))
}

// ---------------------------------------------------------------------------
// pg_hba.conf entries (FB-153's generator renders them).
// ---------------------------------------------------------------------------

/// Connection type of a `pg_hba.conf` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HbaType {
    /// Unix socket.
    Local,
    /// TCP with TLS only.
    Hostssl,
}

/// One `pg_hba.conf` line replication needs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HbaEntry {
    /// `local` or `hostssl`.
    pub kind: HbaType,
    /// Database column (`replication` is the replication keyword).
    pub database: String,
    /// User column.
    pub user: String,
    /// CIDR address (`None` for `local`).
    pub address: Option<String>,
    /// Always `scram-sha-256`.
    pub method: String,
    /// Why the line exists (rendered as a trailing comment).
    pub reason: String,
}

impl HbaEntry {
    fn new(kind: HbaType, database: &str, user: &str, address: Option<IpAddr>, reason: &str) -> Self {
        Self {
            kind,
            database: database.to_owned(),
            user: user.to_owned(),
            address: address.map(|a| match a {
                IpAddr::V4(v4) => format!("{v4}/32"),
                IpAddr::V6(v6) => format!("{v6}/128"),
            }),
            method: "scram-sha-256".to_owned(),
            reason: reason.to_owned(),
        }
    }

    /// The `pg_hba.conf` line.
    pub fn line(&self) -> String {
        let kind = match self.kind {
            HbaType::Local => "local",
            HbaType::Hostssl => "hostssl",
        };
        match &self.address {
            Some(a) => format!(
                "{kind}\t{}\t{}\t{a}\t{}\t# {}",
                self.database, self.user, self.method, self.reason
            ),
            None => format!("{kind}\t{}\t{}\t{}\t# {}", self.database, self.user, self.method, self.reason),
        }
    }
}

/// Who replication must let in (ADR-0029 §2.3).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HbaInput {
    /// Addresses of `forgebase node` agents (remote replicas) of this cluster.
    pub agents: Vec<IpAddr>,
    /// Addresses the ForgeBase server reads a remote replica from (written
    /// into the primary's file too: `pg_basebackup` copies it to replicas).
    pub readers: Vec<IpAddr>,
}

/// The `pg_hba.conf` lines replication needs, in order:
///
/// - `local replication forgebase_replicator` (managed replicas on this
///   host clone and stream over the socket);
/// - per agent: `hostssl replication forgebase_replicator <ip>` (stream)
///   and `hostssl postgres forgebase_replicator <ip>` (`pg_rewind`);
/// - per reader: `hostssl forgebase forgebase_reader <ip>` (the server's
///   read pool on a remote replica).
pub fn hba_entries(input: &HbaInput) -> Vec<HbaEntry> {
    let repl = Role::Replicator.name();
    let mut out = vec![
        HbaEntry::new(
            HbaType::Local,
            "replication",
            repl,
            None,
            "FB-168: managed replicas on this host",
        ),
        HbaEntry::new(
            HbaType::Local,
            "postgres",
            repl,
            None,
            "FB-168: standby checks and pg_rewind on this host",
        ),
    ];
    for a in &input.agents {
        out.push(HbaEntry::new(
            HbaType::Hostssl,
            "replication",
            repl,
            Some(*a),
            "FB-168: forgebase node (streaming)",
        ));
        out.push(HbaEntry::new(
            HbaType::Hostssl,
            "postgres",
            repl,
            Some(*a),
            "FB-168: forgebase node (pg_rewind)",
        ));
    }
    for a in &input.readers {
        out.push(HbaEntry::new(
            HbaType::Hostssl,
            crate::DATABASE,
            Role::Reader.name(),
            Some(*a),
            "FB-168: ForgeBase server read pool",
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Exact engine identity.
// ---------------------------------------------------------------------------

/// The exact engine a node runs: a remote replica must run the same one as
/// its primary (major, minor and ForgeBase build), or the join is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EngineIdentity {
    /// Major (18).
    pub major: u32,
    /// Minor (6).
    pub minor: u32,
    /// The version token (`18.6`).
    pub version: String,
    /// ForgeBase engine build (`r1`) from the archive manifest; `None` for
    /// an installed directory.
    pub build: Option<String>,
}

impl EngineIdentity {
    /// The identity of `engine` (its `postgres --version` and, for an
    /// extracted archive, the manifest's build).
    pub fn of(engine: &EngineDir) -> Self {
        let build = std::fs::read(engine.root.join(crate::archive::MANIFEST_NAME))
            .ok()
            .and_then(|b| serde_json::from_slice::<serde_json::Value>(&b).ok())
            .and_then(|v| v.get("build").and_then(|b| b.as_str()).map(str::to_owned));
        Self {
            major: engine.version.major,
            minor: engine.version.minor,
            version: engine.version.text.clone(),
            build,
        }
    }

    /// `18.6-r1`, or `18.6` without a build.
    pub fn label(&self) -> String {
        match &self.build {
            Some(b) => format!("{}-{b}", self.version),
            None => self.version.clone(),
        }
    }

    /// Refuses anything but the exact same engine.
    ///
    /// # Errors
    /// The two differ; the message names both.
    pub fn check_same(&self, primary: &EngineIdentity) -> std::result::Result<(), String> {
        if self == primary {
            Ok(())
        } else {
            Err(format!(
                "this node runs PostgreSQL engine {} and the primary runs {}: a replica must run the \
                 exact same engine (major, minor and build); install the ForgeBase release the server runs",
                self.label(),
                primary.label()
            ))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slot_names() {
        assert_eq!(slot_name("replica-1"), "fbr_replica_1");
        assert_eq!(slot_name("eu_read_1"), "fbr_eu_read_1");
        assert_eq!(slot_name("primary"), "fbr_primary");
    }

    #[test]
    fn slot_limits_settings() {
        let d = SlotLimits::default();
        assert_eq!(
            d.settings(18),
            vec![
                ("max_slot_wal_keep_size", "8GB".to_owned()),
                ("idle_replication_slot_timeout", "1d".to_owned())
            ]
        );
        assert_eq!(d.settings(17), vec![("max_slot_wal_keep_size", "8GB".to_owned())]);
        let none = SlotLimits {
            wal_keep_size: "512MB".into(),
            idle_timeout: None,
        };
        assert_eq!(none.settings(18).len(), 1);
        assert_eq!(duration_setting(Duration::from_secs(5400)), "90min");
        assert_eq!(duration_setting(Duration::from_secs(7200)), "2h");
        assert_eq!(duration_setting(Duration::from_secs(61)), "61s");
        assert!(d.check().is_ok());
        for bad in ["8 GB", "GB", "8gb", "8PB", ""] {
            let l = SlotLimits {
                wal_keep_size: bad.to_owned().into(),
                idle_timeout: None,
            };
            assert!(l.check().is_err(), "{bad:?}");
        }
        for good in ["-1", "1024", "64MB", "1TB"] {
            let l = SlotLimits {
                wal_keep_size: good.to_owned().into(),
                idle_timeout: None,
            };
            assert!(l.check().is_ok(), "{good:?}");
        }
    }

    #[test]
    fn lsn_round_trip_and_order() {
        let a: Lsn = "16/B374D848".parse().expect("lsn");
        assert_eq!(a.0, (0x16 << 32) | 0xB374_D848);
        assert_eq!(a.to_string(), "16/B374D848");
        let b: Lsn = "0/3000028".parse().expect("lsn");
        assert!(b < a);
        assert_eq!(b.to_string(), "0/3000028");
        assert!("x".parse::<Lsn>().is_err());
        assert!("1/".parse::<Lsn>().is_err());
        assert!("100000000/0".parse::<Lsn>().is_err());
        let j = serde_json::to_string(&a).expect("json");
        assert_eq!(j, "\"16/B374D848\"");
        assert_eq!(serde_json::from_str::<Lsn>(&j).expect("back"), a);
    }

    #[test]
    fn controldata_is_parsed() {
        let text = "pg_control version number:            1800\n\
                    Database system identifier:           7437313468213545621\n\
                    Database cluster state:               shut down\n\
                    Latest checkpoint location:           0/3000028\n\
                    Latest checkpoint's REDO location:    0/3000028\n\
                    Latest checkpoint's TimeLineID:       1\n";
        let c = ControlData::parse(text).expect("parse");
        assert_eq!(c.state, "shut down");
        assert!(c.primary_stopped_cleanly());
        assert_eq!(c.checkpoint, "0/3000028".parse().expect("lsn"));
        assert_eq!(c.timeline, 1);
        assert_eq!(c.system_identifier, "7437313468213545621");
        let crashed = text.replace("shut down", "in production");
        assert!(!ControlData::parse(&crashed).expect("parse").primary_stopped_cleanly());
        assert!(ControlData::parse("nothing").is_err());
    }

    #[test]
    fn hba_lines() {
        let e = hba_entries(&HbaInput {
            agents: vec!["10.0.0.7".parse().expect("ip")],
            readers: vec!["fd00::1".parse().expect("ip")],
        });
        let lines: Vec<String> = e.iter().map(HbaEntry::line).collect();
        assert_eq!(
            lines[0],
            "local\treplication\tforgebase_replicator\tscram-sha-256\t# FB-168: managed replicas on this host"
        );
        assert!(lines[1].starts_with("local\tpostgres\tforgebase_replicator\tscram-sha-256"));
        assert!(lines[2].starts_with("hostssl\treplication\tforgebase_replicator\t10.0.0.7/32\tscram-sha-256"));
        assert!(lines[3].starts_with("hostssl\tpostgres\tforgebase_replicator\t10.0.0.7/32"));
        assert!(lines[4].starts_with("hostssl\tforgebase\tforgebase_reader\tfd00::1/128"));
        assert_eq!(e.len(), 5);
        assert_eq!(hba_entries(&HbaInput::default()).len(), 2);
    }

    #[test]
    fn engine_identity_must_match_exactly() {
        let p = EngineIdentity {
            major: 18,
            minor: 6,
            version: "18.6".into(),
            build: Some("r1".into()),
        };
        assert!(p.check_same(&p.clone()).is_ok());
        let mut other = p.clone();
        other.build = Some("r2".into());
        let e = other.check_same(&p).expect_err("build differs");
        assert!(e.contains("18.6-r2") && e.contains("18.6-r1"), "{e}");
        other.build = None;
        assert!(other.check_same(&p).is_err());
        let minor = EngineIdentity {
            minor: 5,
            version: "18.5".into(),
            ..p.clone()
        };
        assert!(minor.check_same(&p).is_err());
    }

    #[test]
    fn rewind_grants_name_the_replicator() {
        let s = rewind_grants_sql();
        assert_eq!(s.matches("TO forgebase_replicator;").count(), REWIND_FUNCTIONS.len());
    }
}

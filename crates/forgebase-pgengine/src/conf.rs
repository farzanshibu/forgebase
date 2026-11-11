//! `forgebase-postgres.conf`: the settings ForgeBase writes on every start
//! (ADR-0028 §4.3, ADR-0029 §1.3) and the deny list for user settings.

use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::Path;
use std::time::Duration;

use crate::error::{EngineError, Result};

/// Parameters users cannot set in `[database.postgres.settings]`: ForgeBase
/// owns them (network exposure, files, preloads, replication and pg_cron
/// settings ADR-0029 relies on, the log format it parses), or they run
/// programs or load libraries as the server's OS user, which would turn a
/// configuration edit (the FB-91 dashboard) into code execution.
pub const DENY: &[&str] = &[
    // ADR-0028 §4.3
    "listen_addresses",
    "port",
    "unix_socket_directories",
    "data_directory",
    "hba_file",
    "shared_preload_libraries",
    // ADR-0029 §1.3 (also every `cron.*`)
    "max_worker_processes",
    "wal_level",
    "max_wal_senders",
    "max_replication_slots",
    "max_prepared_transactions",
    // files and sockets
    "config_file",
    "ident_file",
    "external_pid_file",
    "unix_socket_group",
    "unix_socket_permissions",
    // code execution and library loading
    "session_preload_libraries",
    "local_preload_libraries",
    "dynamic_library_path",
    "jit_provider",
    "archive_command",
    "archive_library",
    "restore_command",
    "archive_cleanup_command",
    "recovery_end_command",
    "ssl_passphrase_command",
    // replication wiring (FB-168) carries credentials
    "primary_conninfo",
    "primary_slot_name",
    // slot WAL limits come from [database.replicas] slot_wal_keep_size (FB-168)
    "max_slot_wal_keep_size",
    "idle_replication_slot_timeout",
    // the log stream ForgeBase reads
    "log_destination",
    "logging_collector",
    "log_line_prefix",
    "lc_messages",
];

/// Denied prefixes (`cron.*`, `include*` directives).
pub const DENY_PREFIXES: &[&str] = &["cron.", "include"];

/// Everything the settings file depends on.
#[derive(Debug, Clone)]
pub struct ConfInput<'a> {
    /// Unix socket directory; `None` on Windows (TCP only).
    pub socket_dir: Option<&'a Path>,
    /// Port (names the socket; the TCP port when listening).
    pub port: u16,
    /// TCP address to listen on (loopback only).
    pub listen: Option<SocketAddr>,
    /// Libraries for `shared_preload_libraries`.
    pub preload: Vec<&'static str>,
    /// Whether pg_cron is preloaded (writes `cron.*`).
    pub cron: bool,
    /// Hot standby settings (FB-168 replicas).
    pub standby: bool,
    /// Engine major (settings that exist only in later majors).
    pub major: u32,
    /// Slot WAL limits (FB-168).
    pub slots: &'a crate::replication::SlotLimits,
    /// `log_min_duration_statement`.
    pub slow_query: Option<Duration>,
    /// Tests only: no durability.
    pub unsafe_fast: bool,
    /// User settings (already checked by [`check_user_settings`]).
    pub user: &'a BTreeMap<String, String>,
}

/// Fails on a malformed or deny-listed user setting.
///
/// # Errors
/// The first offending key or value.
pub fn check_user_settings(user: &BTreeMap<String, String>) -> Result<()> {
    for (k, v) in user {
        check_key(k)?;
        let lower = k.to_ascii_lowercase();
        if DENY.contains(&lower.as_str()) || DENY_PREFIXES.iter().any(|p| lower.starts_with(p)) {
            return Err(EngineError::Config(format!(
                "settings.{k} is managed by ForgeBase and cannot be set"
            )));
        }
        if v.chars().any(char::is_control) {
            return Err(EngineError::Config(format!(
                "settings.{k}: the value must be one line without control characters"
            )));
        }
    }
    Ok(())
}

fn check_key(k: &str) -> Result<()> {
    let part = |p: &str| {
        !p.is_empty()
            && p.as_bytes()[0].is_ascii_alphabetic()
            && p.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    };
    let ok = match k.split_once('.') {
        None => part(k),
        Some((a, b)) => part(a) && part(b),
    };
    if ok && k.len() <= 63 {
        Ok(())
    } else {
        Err(EngineError::Config(format!(
            "settings key {k:?} is not a PostgreSQL parameter name"
        )))
    }
}

/// The effective settings, in file order: ForgeBase's, then user overrides
/// (replacing a default of the same name) and additions.
pub fn settings(input: &ConfInput<'_>) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    let mut set = |k: &str, v: String| {
        if let Some(e) = out.iter_mut().find(|(ek, _)| ek == k) {
            e.1 = v;
        } else {
            out.push((k.to_owned(), v));
        }
    };
    match input.listen {
        Some(a) => set("listen_addresses", a.ip().to_string()),
        None if input.socket_dir.is_none() => set("listen_addresses", "127.0.0.1".into()),
        None => set("listen_addresses", String::new()),
    }
    set("port", input.port.to_string());
    match input.socket_dir {
        Some(d) => {
            set("unix_socket_directories", d.display().to_string());
            set("unix_socket_permissions", "0700".into());
        }
        None => set("unix_socket_directories", String::new()),
    }
    set("max_connections", "64".into());
    set("shared_buffers", "128MB".into());
    set("password_encryption", "scram-sha-256".into());
    // ADR-0029 §1.3
    set("wal_level", "logical".into());
    set("max_wal_senders", "16".into());
    set("max_replication_slots", "16".into());
    set("max_prepared_transactions", "16".into());
    set("max_worker_processes", "16".into());
    set("synchronous_commit", "on".into());
    // ADR-0029 §2.2: WAL kept for slots is bounded; a replica further behind
    // is invalidated (needs_rebuild) instead of filling the primary's disk.
    for (k, v) in input.slots.settings(input.major) {
        set(k, v);
    }
    set("shared_preload_libraries", input.preload.join(","));
    if input.cron {
        set("cron.database_name", crate::DATABASE.into());
        set("cron.use_background_workers", "on".into());
        set("cron.log_run", "off".into());
    }
    if input.standby {
        set("hot_standby", "on".into());
        set("hot_standby_feedback", "on".into());
        set("max_standby_streaming_delay", "30s".into());
    }
    // The log stream ForgeBase reads into `tracing`.
    set("log_destination", "stderr".into());
    set("logging_collector", "off".into());
    set("log_line_prefix", "[%p] ".into());
    set("lc_messages", "C".into());
    set("log_timezone", "UTC".into());
    set("timezone", "UTC".into());
    set(
        "log_min_duration_statement",
        input
            .slow_query
            .map_or_else(|| "-1".to_owned(), |d| d.as_millis().to_string()),
    );
    if input.unsafe_fast {
        set("fsync", "off".into());
        set("synchronous_commit", "off".into());
        set("full_page_writes", "off".into());
    }
    for (k, v) in input.user {
        set(&k.to_ascii_lowercase(), v.clone());
    }
    out
}

/// Renders `settings` as a `postgresql.conf` fragment.
pub fn render(settings: &[(String, String)]) -> String {
    let mut s = String::from(
        "# Written by ForgeBase on every start (ADR-0028 §4.3). Do not edit:\n\
         # set [database.postgres.settings] in forgebase.toml instead.\n",
    );
    for (k, v) in settings {
        s.push_str(k);
        s.push_str(" = '");
        for c in v.chars() {
            match c {
                '\'' => s.push_str("''"),
                '\\' => s.push_str("\\\\"),
                c => s.push(c),
            }
        }
        s.push_str("'\n");
    }
    s
}

/// The line that makes `postgresql.conf` read ForgeBase's file.
pub const INCLUDE_LINE: &str = "include_if_exists = '../forgebase-postgres.conf'\t# ForgeBase (ADR-0028 §4.3)";

/// Appends [`INCLUDE_LINE`] to `postgresql.conf` once.
///
/// # Errors
/// File-system errors.
pub fn ensure_include(pgdata: &Path) -> Result<()> {
    let path = pgdata.join("postgresql.conf");
    let text = std::fs::read_to_string(&path).map_err(|e| EngineError::io("read", &path, e))?;
    if text.lines().any(|l| {
        l.trim_start()
            .starts_with("include_if_exists = '../forgebase-postgres.conf'")
    }) {
        return Ok(());
    }
    let mut text = text;
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text.push_str(INCLUDE_LINE);
    text.push('\n');
    std::fs::write(&path, text).map_err(|e| EngineError::io("write", &path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    static SLOTS: crate::replication::SlotLimits = crate::replication::SlotLimits::DEFAULT;

    fn input<'a>(user: &'a BTreeMap<String, String>, dir: &'a Path) -> ConfInput<'a> {
        ConfInput {
            socket_dir: Some(dir),
            port: 5432,
            listen: None,
            preload: vec!["pg_stat_statements", "pg_cron"],
            cron: true,
            standby: false,
            major: 18,
            slots: &SLOTS,
            slow_query: Some(Duration::from_millis(250)),
            unsafe_fast: false,
            user,
        }
    }

    fn get<'a>(s: &'a [(String, String)], k: &str) -> Option<&'a str> {
        s.iter().find(|(ek, _)| ek == k).map(|(_, v)| v.as_str())
    }

    #[test]
    fn adr_settings_are_written() {
        let user = BTreeMap::new();
        let dir = Path::new("/d/postgres/run");
        let s = settings(&input(&user, dir));
        for (k, v) in [
            ("listen_addresses", ""),
            ("unix_socket_directories", "/d/postgres/run"),
            ("unix_socket_permissions", "0700"),
            ("shared_buffers", "128MB"),
            ("max_connections", "64"),
            ("synchronous_commit", "on"),
            ("shared_preload_libraries", "pg_stat_statements,pg_cron"),
            ("cron.database_name", "forgebase"),
            ("cron.use_background_workers", "on"),
            ("cron.log_run", "off"),
            ("max_worker_processes", "16"),
            ("wal_level", "logical"),
            ("max_wal_senders", "16"),
            ("max_replication_slots", "16"),
            ("max_prepared_transactions", "16"),
            ("max_slot_wal_keep_size", "8GB"),
            ("idle_replication_slot_timeout", "1d"),
            ("log_min_duration_statement", "250"),
            ("log_destination", "stderr"),
        ] {
            assert_eq!(get(&s, k), Some(v), "{k}");
        }
        assert_eq!(get(&s, "hot_standby"), None);
        assert_eq!(get(&s, "fsync"), None);
    }

    #[test]
    fn missing_cron_writes_no_cron_settings() {
        let user = BTreeMap::new();
        let dir = Path::new("/r");
        let mut i = input(&user, dir);
        i.preload = vec!["pg_stat_statements"];
        i.cron = false;
        i.slow_query = None;
        let s = settings(&i);
        assert_eq!(get(&s, "shared_preload_libraries"), Some("pg_stat_statements"));
        assert!(s.iter().all(|(k, _)| !k.starts_with("cron.")));
        assert_eq!(get(&s, "log_min_duration_statement"), Some("-1"));
    }

    #[test]
    fn standby_tcp_and_windows_shapes() {
        let user = BTreeMap::new();
        let dir = Path::new("/r");
        let mut i = input(&user, dir);
        i.standby = true;
        i.listen = Some("127.0.0.1:5433".parse().expect("addr"));
        i.port = 5433;
        let s = settings(&i);
        assert_eq!(get(&s, "hot_standby"), Some("on"));
        assert_eq!(get(&s, "hot_standby_feedback"), Some("on"));
        assert_eq!(get(&s, "listen_addresses"), Some("127.0.0.1"));
        assert_eq!(get(&s, "port"), Some("5433"));
        i.socket_dir = None;
        i.listen = None;
        let s = settings(&i);
        assert_eq!(get(&s, "listen_addresses"), Some("127.0.0.1"));
        assert_eq!(get(&s, "unix_socket_directories"), Some(""));
    }

    #[test]
    fn user_settings_override_and_add() {
        let mut user = BTreeMap::new();
        user.insert("shared_buffers".to_owned(), "1GB".to_owned());
        user.insert("Work_Mem".to_owned(), "16MB".to_owned());
        check_user_settings(&user).expect("allowed");
        let dir = Path::new("/r");
        let s = settings(&input(&user, dir));
        assert_eq!(get(&s, "shared_buffers"), Some("1GB"));
        assert_eq!(get(&s, "work_mem"), Some("16MB"));
        assert_eq!(s.iter().filter(|(k, _)| k == "shared_buffers").count(), 1);
    }

    #[test]
    fn deny_list_and_malformed_keys() {
        for k in [
            "listen_addresses",
            "PORT",
            "shared_preload_libraries",
            "wal_level",
            "max_prepared_transactions",
            "cron.database_name",
            "cron.anything",
            "archive_command",
            "restore_command",
            "ssl_passphrase_command",
            "dynamic_library_path",
            "session_preload_libraries",
            "include_dir",
            "log_line_prefix",
            "primary_conninfo",
        ] {
            let mut user = BTreeMap::new();
            user.insert(k.to_owned(), "x".to_owned());
            let err = check_user_settings(&user).expect_err(k);
            assert!(err.to_string().contains("managed by ForgeBase"), "{k}: {err}");
        }
        for k in ["", "1abc", "a b", "a.b.c", "a-b", "x;y"] {
            let mut user = BTreeMap::new();
            user.insert(k.to_owned(), "x".to_owned());
            assert!(check_user_settings(&user).is_err(), "{k:?}");
        }
        let mut user = BTreeMap::new();
        user.insert("work_mem".to_owned(), "1MB'\ninclude = '/etc/x".to_owned());
        assert!(check_user_settings(&user).is_err());
    }

    #[test]
    fn values_are_quoted() {
        let s = render(&[("search_path".into(), "a, 'b'".into()), ("x.y".into(), "c:\\d".into())]);
        assert!(s.contains("search_path = 'a, ''b'''\n"), "{s}");
        assert!(s.contains("x.y = 'c:\\\\d'\n"), "{s}");
        assert!(s.starts_with('#'));
    }

    #[test]
    fn include_line_is_added_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("postgresql.conf"), "max_connections = 100").expect("write");
        ensure_include(dir.path()).expect("once");
        ensure_include(dir.path()).expect("twice");
        let text = std::fs::read_to_string(dir.path().join("postgresql.conf")).expect("read");
        assert_eq!(text.matches("forgebase-postgres.conf").count(), 1);
        assert!(text.starts_with("max_connections = 100\n"));
    }
}

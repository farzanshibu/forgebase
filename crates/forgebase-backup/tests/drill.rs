//! Restore drills, library side (ADR-0041): the counts in the manifest, the
//! sealed `tls/` state in a backup, the drill history and its gauges, and the
//! drill API's refusals. The drill itself (restore, quiescent server, checks)
//! is the CLI's and is tested black-box in `forgebase-cli/tests/drill_e2e.rs`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use common::{admin, open, read};
use forgebase_backup::drill::{
    CheckStatus, DrillCheck, DrillCounts, DrillOutcome, DrillPhases, DrillReport, DrillStore, KEEP_DRILLS,
};
use forgebase_backup::manifest::{BackupStats, CountRange, FileKind};
use forgebase_backup::restore::{self, RestoreOptions};
use forgebase_backup::{BackupOptions, backup, stats};

fn report(id: &str, backup_id: &str, outcome: DrillOutcome, finished: &str) -> DrillReport {
    DrillReport {
        id: id.to_owned(),
        backup_id: backup_id.to_owned(),
        backup_created: "2026-11-14T00:00:00Z".to_owned(),
        engine: "sqlite".to_owned(),
        schema_version: 66,
        started_at: finished.to_owned(),
        finished_at: finished.to_owned(),
        outcome,
        rto_seconds: 31.5,
        rpo_seconds: 7200,
        phases: DrillPhases {
            restore_ms: 10_000,
            start_ms: 6_000,
            checks_ms: 15_500,
        },
        counts: DrillCounts::default(),
        checks: vec![DrillCheck {
            name: "restore".to_owned(),
            status: CheckStatus::Ok,
            detail: "restored".to_owned(),
        }],
        problems: if outcome == DrillOutcome::Passed {
            Vec::new()
        } else {
            vec!["counts: collection books has 1 record(s), the backup recorded 2".to_owned()]
        },
        trigger: "cli".to_owned(),
        kept: None,
    }
}

/// A backup records what it holds, exactly on SQLite, and `compare` holds a
/// restore to it.
#[tokio::test]
async fn a_backup_records_counts_and_compare_finds_a_difference() {
    let tmp = tempfile::tempdir().unwrap();
    let src = open(&tmp.path().join("src")).await;
    src.seed(7).await;
    let dest = tmp.path().join("bk");
    let m = backup::create(&src.db, &src.layout, &dest, BackupOptions::default())
        .await
        .unwrap();
    let st = m.stats.clone().expect("a SQLite backup records counts");
    assert_eq!(st.collections["books"], CountRange::exact(7));
    assert_eq!(st.collections["authors"], CountRange::exact(1));
    assert_eq!(st.storage_objects, CountRange::exact(0));
    // It is in MANIFEST.json, and an older reader's view (no stats) still parses.
    let text = std::fs::read_to_string(dest.join("MANIFEST.json")).unwrap();
    assert!(text.contains("\"stats\""), "{text}");
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value.as_object_mut().unwrap().remove("stats");
    let old: forgebase_backup::Manifest = serde_json::from_value(value).unwrap();
    assert!(old.stats.is_none());

    // The restored database counts the same; one record fewer does not.
    let got = stats::collect(&src.db).await.unwrap();
    assert!(stats::compare(&st, &got).is_empty());
    src.data
        .delete_record(&admin(), "books", "book0000000003", None)
        .await
        .unwrap();
    let after = stats::collect(&src.db).await.unwrap();
    let diffs = stats::compare(&st, &after);
    assert_eq!(
        diffs,
        ["collection books has 6 record(s), the backup recorded 7"],
        "{diffs:?}"
    );
}

/// A window (PostgreSQL's before and after) accepts anything inside it.
#[test]
fn a_count_window_accepts_values_inside_it() {
    let mut st = BackupStats::default();
    st.collections.insert("books".into(), CountRange::span(10, 12));
    let mut got = stats::Counts::default();
    got.collections.insert("books".into(), 11);
    assert!(stats::compare(&st, &got).is_empty());
    got.collections.insert("books".into(), 13);
    assert_eq!(stats::compare(&st, &got).len(), 1);
    got.collections.clear();
    assert!(stats::compare(&st, &got)[0].contains("missing after the restore"));
    assert_eq!(CountRange::span(12, 10).describe(), "10..12");
    assert_eq!(CountRange::exact(4).describe(), "4");
}

/// `include_tls` puts the sealed `tls/` files in the manifest and a restore
/// brings them back; without it nothing of `tls/` is in the backup.
#[tokio::test]
async fn the_sealed_tls_state_is_backed_up_and_restored() {
    let tmp = tempfile::tempdir().unwrap();
    let src = open(&tmp.path().join("src")).await;
    src.seed(1).await;
    let state = src.layout.root().join("tls/acme/0123456789abcdef");
    std::fs::create_dir_all(&state).unwrap();
    std::fs::write(state.join("cert.sealed"), b"sealed cert").unwrap();
    std::fs::write(state.join("account.sealed"), b"sealed account").unwrap();
    src.put_key(b"0123456789abcdef0123456789abcdef");

    let plain = backup::create(
        &src.db,
        &src.layout,
        &tmp.path().join("plain"),
        BackupOptions::default(),
    )
    .await
    .unwrap();
    assert!(!plain.includes_tls());
    assert!(!tmp.path().join("plain/tls").exists());

    let dest = tmp.path().join("with-tls");
    let m = backup::create(
        &src.db,
        &src.layout,
        &dest,
        BackupOptions {
            include_tls: true,
            ..BackupOptions::default()
        },
    )
    .await
    .unwrap();
    assert!(m.includes_tls());
    let tls: Vec<&str> = m
        .files
        .iter()
        .filter(|f| f.kind == FileKind::Tls)
        .map(|f| f.path.as_str())
        .collect();
    assert_eq!(
        tls,
        [
            "tls/acme/0123456789abcdef/account.sealed",
            "tls/acme/0123456789abcdef/cert.sealed"
        ]
    );
    assert!(forgebase_backup::verify(&dest).await.ok);

    // A restore into a data directory with other TLS state replaces it (kept aside) ...
    let target = tmp.path().join("restored");
    let layout = forgebase_core::DataLayout::new(&target);
    let other = target.join("tls/acme/old");
    std::fs::create_dir_all(&other).unwrap();
    std::fs::write(other.join("cert.sealed"), b"stale").unwrap();
    let key = tmp.path().join("m.key");
    std::fs::write(&key, b"0123456789abcdef0123456789abcdef").unwrap();
    let r = restore::restore(
        &dest,
        &layout,
        RestoreOptions {
            master_key: Some(key.clone()),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(
        read(layout.root().join("tls/acme/0123456789abcdef/cert.sealed")),
        b"sealed cert"
    );
    assert!(!layout.root().join("tls/acme/old").exists());
    let aside = r.previous_data.expect("the replaced tls state is kept");
    assert_eq!(read(aside.join("tls/acme/old/cert.sealed")), b"stale");

    // ... and a backup without TLS state leaves the target's own alone.
    let again = tmp.path().join("again");
    let lay2 = forgebase_core::DataLayout::new(&again);
    std::fs::create_dir_all(again.join("tls")).unwrap();
    std::fs::write(again.join("tls/keep"), b"mine").unwrap();
    restore::restore(
        &tmp.path().join("plain"),
        &lay2,
        RestoreOptions {
            master_key: Some(key),
            ..RestoreOptions::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(read(again.join("tls/keep")), b"mine");
}

/// The history keeps the newest hundred, newest first, per backup too; the
/// gauges say when the last drill *passed*.
#[test]
fn the_history_is_kept_and_the_gauges_follow_it() {
    // The recorder must exist before a gauge is set.
    let handle = forgebase_observability::metrics_handle();
    let tmp = tempfile::tempdir().unwrap();
    let layout = forgebase_core::DataLayout::new(tmp.path());
    layout.ensure().unwrap();
    let store = DrillStore::new(&layout);
    assert!(store.list().is_empty());
    forgebase_backup::drill::publish_metrics(&store); // nothing on record: no gauge, no panic

    store
        .record(&report(
            "20261101T000000-0001",
            "snap-a",
            DrillOutcome::Passed,
            "2026-11-01T00:10:00Z",
        ))
        .unwrap();
    store
        .record(&report(
            "20261102T000000-0001",
            "snap-b",
            DrillOutcome::Failed,
            "2026-11-02T00:10:00Z",
        ))
        .unwrap();
    let all = store.list();
    assert_eq!(all.len(), 2);
    assert_eq!(all[0].backup_id, "snap-b", "newest first");
    assert_eq!(store.of_backup("snap-a").len(), 1);
    assert_eq!(store.of_backup("nope").len(), 0);
    assert!(!all[0].passed() && all[1].passed());
    assert_eq!(all[1].finished_unix(), 1_793_491_800);
    // Files that are not drills are ignored.
    std::fs::write(store.dir().join("junk.json"), b"{not json").unwrap();
    assert_eq!(store.list().len(), 2);

    forgebase_backup::drill::publish_metrics(&store);
    let text = handle.render();
    assert!(
        text.contains("forgebase_backup_last_drill_success_timestamp_seconds 1793491800"),
        "the last PASSED drill: {text}"
    );
    assert!(
        text.contains("forgebase_backup_last_drill_ok 0"),
        "the latest failed: {text}"
    );
    assert!(
        text.contains("forgebase_backup_drill_duration_seconds{phase=\"total\"} 31.5"),
        "{text}"
    );
    assert!(
        text.contains("forgebase_backup_drill_duration_seconds{phase=\"restore\"} 10"),
        "{text}"
    );
    assert!(text.contains("forgebase_backup_drill_rpo_seconds 7200"), "{text}");

    for i in 0..KEEP_DRILLS + 5 {
        store
            .record(&report(
                &format!("20261110T{i:06}-0001"),
                "snap-c",
                DrillOutcome::Passed,
                "2026-11-10T00:00:00Z",
            ))
            .unwrap();
    }
    assert_eq!(store.list().len(), KEEP_DRILLS);
}

/// `[backup] drill_schedule` creates, changes and disables the schedule
/// `backup-drill`; a bad expression changes nothing.
#[tokio::test]
async fn the_drill_schedule_follows_the_setting() {
    use std::sync::Arc;

    use forgebase_backup::drill::{DRILL_JOB, DRILL_SCHEDULE, sync_schedule};
    use forgebase_data::AccessContext;

    let tmp = tempfile::tempdir().unwrap();
    let inst = open(&tmp.path().join("data")).await;
    let state = forgebase_api::AppState::with_authorizer(
        inst.db.clone(),
        inst.layout.clone(),
        Arc::new(forgebase_data::DefaultAuthorizer),
    )
    .await
    .unwrap();
    let jobs = forgebase_jobs::JobService::new(state.data().clone());
    let sys = AccessContext::system();
    let ours = |all: Vec<forgebase_jobs::Schedule>| all.into_iter().find(|s| s.name == DRILL_SCHEDULE);

    // Off by default: no schedule.
    sync_schedule(&jobs, None).await;
    assert!(ours(jobs.schedules(&sys).await.unwrap()).is_none());

    sync_schedule(&jobs, Some("30 4 * * 0")).await;
    let s = ours(jobs.schedules(&sys).await.unwrap()).expect("created");
    assert_eq!(
        (s.cron.as_str(), s.kind.as_str(), s.enabled),
        ("30 4 * * 0", DRILL_JOB, true)
    );
    assert_eq!(s.payload["backup_id"], "latest");
    assert_eq!(s.payload["trigger"], "schedule");

    // Changed, and idempotent.
    sync_schedule(&jobs, Some("0 3 * * *")).await;
    sync_schedule(&jobs, Some("0 3 * * *")).await;
    let all = jobs.schedules(&sys).await.unwrap();
    assert_eq!(all.iter().filter(|s| s.name == DRILL_SCHEDULE).count(), 1);
    assert_eq!(ours(all).unwrap().cron, "0 3 * * *");

    // An expression that does not parse leaves the old one in place.
    sync_schedule(&jobs, Some("not a cron")).await;
    assert_eq!(ours(jobs.schedules(&sys).await.unwrap()).unwrap().cron, "0 3 * * *");

    // Removed from the configuration: disabled, not deleted.
    sync_schedule(&jobs, None).await;
    let s = ours(jobs.schedules(&sys).await.unwrap()).unwrap();
    assert!(!s.enabled);
    sync_schedule(&jobs, Some("0 3 * * *")).await;
    assert!(ours(jobs.schedules(&sys).await.unwrap()).unwrap().enabled);
}

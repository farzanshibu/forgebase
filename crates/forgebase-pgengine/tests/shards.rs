//! Shard primaries on this host (ADR-0029 §3.9, FB-174): created next to
//! the primary with its roles, restarted at server start, retired with
//! their data kept, deleted on request. Skips without
//! `FORGEBASE_PG_ENGINE_DIR`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use forgebase_pgengine::shard::{LocalShards, SHARD_MARKER};
use forgebase_pgengine::{Engine, EngineConfig, EngineSource, Layout, Role};
use sqlx::{Connection, PgConnection};

fn engine_dir() -> Option<PathBuf> {
    match std::env::var_os("FORGEBASE_PG_ENGINE_DIR").filter(|v| !v.is_empty()) {
        Some(d) => Some(PathBuf::from(d)),
        None => {
            eprintln!("skipped: FORGEBASE_PG_ENGINE_DIR is not set");
            None
        }
    }
}

fn tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("FORGEBASE_PGENGINE_TEST_TMP")
        .or_else(|| std::env::var_os("FORGEBASE_TEST_PG_TMP"))
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    std::fs::create_dir_all(&base).expect("test tmp base");
    tempfile::Builder::new().prefix("sh").tempdir_in(base).expect("tempdir")
}

fn config(dir: &Path) -> EngineConfig {
    EngineConfig {
        source: EngineSource::Dir(dir.to_path_buf()),
        socket_port: 18_430,
        unsafe_fast: true,
        ready_timeout: Duration::from_secs(60),
        stop_timeout: Duration::from_secs(30),
        ..EngineConfig::default()
    }
}

async fn setting(url: &str, name: &str) -> String {
    let mut c = PgConnection::connect(url).await.unwrap();
    sqlx::query_scalar(sqlx::AssertSqlSafe(format!("SELECT current_setting('{name}')")))
        .fetch_one(&mut c)
        .await
        .unwrap()
}

#[tokio::test]
async fn a_local_shard_starts_with_the_primarys_roles_and_is_retired_with_its_data_kept() {
    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let cfg = config(&engine);
    let primary = Engine::prepare(&Layout::primary(tmp.path()), &cfg).await.unwrap();
    let shards = LocalShards::new(tmp.path(), &cfg);

    let s1 = shards.add("shard-1", "set-1").await.unwrap();
    assert!(s1.tcp_addr().is_none(), "shards never listen on TCP");
    // The server reaches it with the primary's server role and password.
    assert_eq!(
        s1.url(Role::Server).expose().contains("forgebase_server"),
        primary.url(Role::Server).expose().contains("forgebase_server")
    );
    let url = s1.url(Role::Server).expose().to_owned();
    assert_eq!(setting(&url, "max_prepared_transactions").await, "16");
    assert_eq!(setting(&url, "wal_level").await, "logical");
    {
        let mut c = PgConnection::connect(s1.url(Role::Super).expose()).await.unwrap();
        sqlx::raw_sql("CREATE TABLE forgebase.fb174_keep (n int); INSERT INTO forgebase.fb174_keep VALUES (7)")
            .execute(&mut c)
            .await
            .unwrap();
    }
    assert!(shards.add("shard-1", "set-1").await.is_err(), "already running");
    assert_eq!(shards.on_disk(), vec![("shard-1".to_owned(), "set-1".to_owned())]);
    // Another set may not take the directory.
    shards.stop("shard-1").await.unwrap();
    assert!(shards.add("shard-1", "set-2").await.is_err());

    // Server start: shard nodes on disk come back with their data.
    let started = shards.start_existing().await;
    assert_eq!(started.len(), 1);
    assert!(started[0].2.is_ok(), "{:?}", started[0].2);
    let s1 = shards.get("shard-1").await.unwrap();
    let mut c = PgConnection::connect(s1.url(Role::Super).expose()).await.unwrap();
    let n: i32 = sqlx::query_scalar("SELECT n FROM forgebase.fb174_keep")
        .fetch_one(&mut c)
        .await
        .unwrap();
    assert_eq!(n, 7);
    drop(c);
    // A replica's directory is not a shard.
    std::fs::create_dir_all(tmp.path().join("postgres/nodes/rep-1/data")).unwrap();
    assert!(shards.start("rep-1").await.is_err());
    assert!(!tmp.path().join("postgres/nodes/rep-1").join(SHARD_MARKER).exists());

    // Remove: stopped, data kept under retired/, then deleted on request.
    let retired = shards.retire("shard-1").await.unwrap();
    assert!(shards.get("shard-1").await.is_none());
    assert!(retired.path.starts_with(tmp.path().join("postgres/retired")));
    assert!(retired.path.join("data/PG_VERSION").is_file());
    assert!(retired.bytes > 1_000_000);
    assert_eq!(shards.retired().len(), 1);
    assert!(shards.on_disk().is_empty());
    shards.delete_retired(&retired.id).unwrap();
    assert!(!retired.path.exists());
    shards.stop_all().await;
    primary.stop().await.unwrap();
}

/// A shard primary on another host (FB-174): a primary join token for a
/// provisioning shard; the grant says `primary` and carries no slot; the
/// agent bootstraps its own cluster with the cluster's roles.
#[tokio::test]
async fn a_shard_primary_joins_with_a_primary_token_and_bootstraps_with_the_clusters_roles() {
    use forgebase_db::topology::{self, NewSet, NodeKind as TopoKind, NodeRole, NodeState};
    use forgebase_db::{Database, DbOptions, DbTarget};
    use forgebase_pgengine::replication::service::{self, JoinRequest, JoinTokenRequest, ReplicationSource};
    use forgebase_pgengine::replication::EngineIdentity;
    use forgebase_pgengine::standby::SslMode;
    use forgebase_pgengine::NodeKind;

    let Some(engine) = engine_dir() else { return };
    let tmp = tempdir();
    let mut cfg = config(&engine);
    cfg.socket_port = 18_436;
    cfg.listen = Some("127.0.0.1:18436".parse().unwrap());
    let primary = Engine::prepare(&Layout::primary(tmp.path()), &cfg).await.unwrap();
    let control = Database::open(DbOptions {
        target: DbTarget::postgres(forgebase_db::SecretString::new(primary.url(Role::Server).expose()), true),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 2,
        create_if_missing: false,
        read_only: false,
    })
    .await
    .unwrap();
    control.migrate(None).await.unwrap();
    let source = ReplicationSource::from_handle(&primary, None, SslMode::Require).unwrap();

    // Control and active sets take no primary token.
    let refused = service::issue_join_token(
        &control,
        &JoinTokenRequest {
            set_id: "control".into(),
            role: NodeRole::Primary,
        },
        None,
        "https://fb.example/",
    )
    .await
    .unwrap_err();
    assert_eq!(refused.code(), "node_unsupported");
    let set = topology::insert_set(
        &control,
        &NewSet {
            name: "far-shard".into(),
            weight: 1,
        },
    )
    .await
    .unwrap();
    let tok = service::issue_join_token(
        &control,
        &JoinTokenRequest {
            set_id: set.id.clone(),
            role: NodeRole::Primary,
        },
        Some("admin"),
        "https://fb.example/",
    )
    .await
    .unwrap();
    let mut grant = service::redeem_join(
        &control,
        &source,
        &JoinRequest {
            token: tok.token,
            name: "far-1".into(),
            engine: EngineIdentity::of(primary.engine()),
            listen: Some("10.0.0.7:5432".into()),
        },
        Some("10.0.0.7".parse().unwrap()),
    )
    .await
    .unwrap();
    assert_eq!(grant.role, NodeRole::Primary);
    assert!(grant.upstream.slot.is_empty(), "a shard primary streams from nobody");
    let node = topology::load(&control)
        .await
        .unwrap()
        .nodes
        .into_iter()
        .find(|n| n.id == grant.node_id)
        .unwrap();
    assert_eq!((node.kind, node.role, node.state), (TopoKind::Remote, NodeRole::Primary, NodeState::Provisioning));
    assert_eq!(node.set_id, set.id);
    // The server adds the cluster's role passwords; the agent bootstraps with them.
    grant.secrets = Some(std::fs::read_to_string(primary.layout().secrets_file()).unwrap());
    let wire: service::JoinGrant = serde_json::from_str(&serde_json::to_string(&grant).unwrap()).unwrap();
    assert_eq!(wire.role, NodeRole::Primary);
    let agent_dir = tmp.path().join("agent");
    let layout = Layout::node(&agent_dir, "far-1", NodeKind::Primary).unwrap();
    std::fs::create_dir_all(layout.postgres_dir()).unwrap();
    std::fs::write(layout.secrets_file(), wire.secrets.unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(layout.secrets_file(), std::fs::Permissions::from_mode(0o600)).unwrap();
        std::fs::set_permissions(layout.postgres_dir(), std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::set_permissions(&agent_dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mut agent_cfg = config(&engine);
    agent_cfg.socket_port = 18_437;
    let far = Engine::prepare(&layout, &agent_cfg).await.unwrap();
    assert_eq!(far.url(Role::Server).expose().split('@').next(), primary.url(Role::Server).expose().split('@').next(),
        "same server role and password");
    let url = far.url(Role::Server).expose().to_owned();
    assert_eq!(setting(&url, "max_prepared_transactions").await, "16");
    let shown = service::server_url("10.0.0.7:5432", &forgebase_pgengine::Secret::new("pw"), SslMode::Require);
    assert_eq!(shown.expose(), "postgres://forgebase_server:pw@10.0.0.7:5432/forgebase?sslmode=require");
    far.stop().await.unwrap();
    control.close().await;
    primary.stop().await.unwrap();
}

//! `forgebase node join` end to end (FB-168): the built binary joins a
//! primary through the agent endpoints, clones it over TCP as
//! `forgebase_replicator`, streams, heartbeats, and exits on a `stop`
//! command with `remove`. The endpoints are mounted here on a small axum
//! router over the same service functions the server mounts (FB-175).
//! Skips unless `FORGEBASE_PG_ENGINE_DIR` names a PostgreSQL 17+ engine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path as UrlPath, RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use forgebase_db::topology::{NodeRole, NodeState};
use forgebase_db::{Database, DbOptions, DbTarget};
use forgebase_pgengine::replication;
use forgebase_pgengine::replication::service::{
    self, AgentState, CommandKind, Heartbeat, JoinRequest, JoinTokenRequest, ReplicationSource, ServiceError,
};
use forgebase_pgengine::standby::SslMode;
use forgebase_pgengine::{Engine, EngineConfig, EngineSource, Layout, Role};

struct App {
    control: Database,
    source: ReplicationSource,
}

fn err(e: ServiceError) -> Response {
    let status = StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (
        status,
        Json(serde_json::json!({"error": {"code": e.code(), "message": e.to_string()}})),
    )
        .into_response()
}

fn bearer(h: &HeaderMap) -> String {
    h.get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned()
}

async fn join(State(app): State<Arc<App>>, Json(req): Json<JoinRequest>) -> Response {
    match service::redeem_join(&app.control, &app.source, &req, Some("127.0.0.1".parse().unwrap())).await {
        Ok(g) => Json(g).into_response(),
        Err(e) => err(e),
    }
}

async fn commands(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<String>,
    RawQuery(q): RawQuery,
    h: HeaderMap,
) -> Response {
    let q = q.unwrap_or_default();
    let param = |k: &str| {
        q.split('&')
            .find_map(|kv| kv.strip_prefix(k).and_then(|v| v.strip_prefix('=')))
            .map(str::to_owned)
    };
    let wait = Duration::from_millis(param("wait_ms").and_then(|v| v.parse().ok()).unwrap_or(0));
    let resend = param("resend").as_deref() == Some("1");
    match service::poll_commands(&app.control, &id, &bearer(&h), wait, resend).await {
        Ok(b) => Json(b).into_response(),
        Err(e) => err(e),
    }
}

async fn heartbeat(
    State(app): State<Arc<App>>,
    UrlPath(id): UrlPath<String>,
    h: HeaderMap,
    Json(hb): Json<Heartbeat>,
) -> Response {
    match service::heartbeat(&app.control, &id, &bearer(&h), &hb).await {
        Ok(r) => Json(r).into_response(),
        Err(e) => err(e),
    }
}

/// Kills the agent if the test fails half-way (its engine stops with it).
struct Child(Option<tokio::process::Child>);

impl Drop for Child {
    fn drop(&mut self) {
        if let Some(c) = self.0.as_mut() {
            let _ = c.start_kill();
        }
    }
}

#[tokio::test]
async fn node_join_clones_streams_heartbeats_and_stops_on_command() {
    let Some(engine) = std::env::var_os("FORGEBASE_PG_ENGINE_DIR")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
    else {
        eprintln!("skipped: FORGEBASE_PG_ENGINE_DIR is not set");
        return;
    };
    let tmp = tempfile::Builder::new().prefix("nj").tempdir().unwrap();
    let cfg = EngineConfig {
        source: EngineSource::Dir(engine.clone()),
        socket_port: 18_691,
        listen: Some("127.0.0.1:18691".parse().unwrap()),
        unsafe_fast: true,
        ready_timeout: Duration::from_secs(60),
        ..EngineConfig::default()
    };
    let primary = Engine::prepare(&Layout::primary(tmp.path().join("server")), &cfg)
        .await
        .expect("primary");
    let control = Database::open(DbOptions {
        target: DbTarget::postgres(
            forgebase_db::SecretString::new(primary.url(Role::Server).expose()),
            true,
        ),
        synchronous: forgebase_core::Synchronous::Full,
        busy_timeout: Duration::from_secs(5),
        read_pool_size: 2,
        create_if_missing: false,
        read_only: false,
    })
    .await
    .expect("control");
    control.migrate(None).await.expect("migrate");
    // FB-153's pg_hba.conf admits ForgeBase's own roles only over TCP: add
    // the replicator's lines the way the generator should, then reload.
    let hba = tmp.path().join("server/postgres/data/pg_hba.conf");
    let text = std::fs::read_to_string(&hba).unwrap();
    let entries = replication::hba_entries(&replication::HbaInput {
        agents: vec!["127.0.0.1".parse().unwrap()],
        readers: vec![],
    });
    std::fs::write(&hba, replication::splice_hba(&text, &entries)).unwrap();
    primary.pg_ctl(&["reload", "-s"]).await.unwrap();
    let source = ReplicationSource::from_handle(&primary, None, SslMode::Require).expect("source");
    let app = Arc::new(App {
        control: control.clone(),
        source,
    });
    let router = axum::Router::new()
        .route(service::paths::JOIN, post(join))
        .route(service::paths::COMMANDS, get(commands))
        .route(service::paths::HEARTBEAT, post(heartbeat))
        .with_state(app);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let tok = service::issue_join_token(
        &control,
        &JoinTokenRequest {
            set_id: "control".into(),
            role: NodeRole::Replica,
        },
        None,
        &url,
    )
    .await
    .expect("token");
    let agent_dir = tmp.path().join("agent");
    let mut child = Child(Some(
        tokio::process::Command::new(env!("CARGO_BIN_EXE_forgebase"))
            .args([
                "node",
                "join",
                &url,
                "--token",
                &tok.token,
                "--name",
                "far-1",
                "--data-dir",
            ])
            .arg(&agent_dir)
            .arg("--engine-dir")
            .arg(&engine)
            .kill_on_drop(true)
            .spawn()
            .expect("spawn the agent"),
    ));

    // The node streams and reports its replay position (clone with fsync
    // plus a first start: generous under a loaded machine).
    let deadline = Instant::now() + Duration::from_secs(300);
    let node = loop {
        let nodes = service::nodes(&control, Some("control")).await.expect("nodes");
        if let Some(n) = nodes.into_iter().find(|n| n.name == "far-1")
            && n.state == NodeState::CatchingUp
            && n.agent.as_ref().is_some_and(|a| a.state == AgentState::Streaming)
        {
            break n;
        }
        assert!(Instant::now() < deadline, "the agent did not start streaming");
        if let Some(status) = child.0.as_mut().unwrap().try_wait().unwrap() {
            panic!("the agent exited early: {status}");
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let agent = node.agent.clone().unwrap();
    assert_eq!(agent.address.as_deref(), Some("127.0.0.1"));
    assert!(agent.disk_free.is_some());
    assert!(agent_dir.join("node.json").is_file());
    let secrets = std::fs::read_to_string(agent_dir.join("postgres/roles.secret")).unwrap();
    assert!(secrets.contains("forgebase_replicator="));
    let super_pw = forgebase_pgengine::RoleSecrets::load(&tmp.path().join("server/postgres/roles.secret"))
        .unwrap()
        .unwrap();
    assert!(
        !secrets.contains(super_pw.password(Role::Super).unwrap()),
        "the agent never holds the superuser's password"
    );

    // A write on the primary reaches the replica.
    let lsn = forgebase_pgengine::standby::facts(&primary).await.unwrap().lsn.unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let a = service::node_view(&control, &node.id).await.unwrap().agent.unwrap();
        if a.replay_lsn.is_some_and(|r| r >= lsn) {
            break;
        }
        assert!(Instant::now() < deadline, "replay did not reach {lsn}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    // `stop` with `remove`: the agent answers, deletes its data and exits 0.
    let cmd = service::queue_command(
        &control,
        &node.id,
        CommandKind::Stop,
        serde_json::json!({"remove": true}),
        None,
    )
    .await
    .unwrap();
    let mut c = child.0.take().unwrap();
    let status = tokio::time::timeout(Duration::from_secs(60), c.wait())
        .await
        .expect("the agent exits")
        .unwrap();
    assert!(status.success(), "{status}");
    let out = service::wait_command(&control, &cmd.id, Duration::from_secs(5))
        .await
        .unwrap();
    assert_eq!(out.state, "done");
    assert!(!agent_dir.join("node.json").exists());
    assert!(!agent_dir.join("postgres/nodes/far-1").exists());

    control.close().await;
    primary.stop().await.unwrap();
}

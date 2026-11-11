//! FB-178: the topology test harness.
//!
//! One managed PostgreSQL primary with local replicas added through
//! `POST /api/system/database/nodes`, and shards added through
//! `POST /api/system/database/shards` (each shard is a separate PostgreSQL
//! cluster from [`forgebase_db::shard_testing`]). Every check goes through
//! the public HTTP API of a real [`forgebase_server`]; direct database
//! access is only used to *verify* where bytes physically are.
//!
//! The suites are gated by `FORGEBASE_TOPOLOGY=1` on top of the usual
//! `FORGEBASE_TEST_BACKEND=postgres` and an engine in
//! `FORGEBASE_TEST_PG_ENGINE` / `FORGEBASE_PG_ENGINE_DIR`, so a plain
//! `cargo test --workspace` skips them in a second.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo};
use forgebase_core::Config;
use forgebase_core::config::{DatabaseBackend, PostgresMode};
use forgebase_data::DefaultAuthorizer;
use forgebase_db::shard_testing::TestCluster;
use forgebase_db::{Backend, Statement};
use forgebase_server::RunningServer;
use serde_json::{Value as Json, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub use forgebase_db::shard_testing::TestCluster as ShardCluster;

/// Base path of the topology API.
pub const BASE: &str = "/api/system/database";
/// Password of the test administrator.
pub const ADMIN_PASSWORD: &str = "a-long-admin-password-1";
/// Password of the test users.
pub const USER_PASSWORD: &str = "correct-horse-battery-9";

/// Whether the topology suites run: `FORGEBASE_TOPOLOGY=1`, the PostgreSQL
/// backend and an engine. Prints why when not.
pub fn enabled() -> bool {
    let on = std::env::var("FORGEBASE_TOPOLOGY").is_ok_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
    let pg = matches!(
        std::env::var("FORGEBASE_TEST_BACKEND").ok().as_deref().map(str::trim),
        Some("postgres" | "postgresql" | "pg")
    );
    if !on {
        eprintln!("skipped: set FORGEBASE_TOPOLOGY=1 (and FORGEBASE_TEST_BACKEND=postgres) to run the topology suites");
        return false;
    }
    if !pg || forgebase_db::shard_testing::engine_dir().is_none() {
        eprintln!("skipped: the topology suites need FORGEBASE_TEST_BACKEND=postgres and an engine directory");
        return false;
    }
    true
}

/// One HTTP response.
#[derive(Debug, Clone)]
pub struct Resp {
    /// Status code.
    pub status: u16,
    /// Lower-cased header names.
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
}

impl Resp {
    /// The body as JSON (`null` when it is not).
    pub fn json(&self) -> Json {
        serde_json::from_slice(&self.body).unwrap_or(Json::Null)
    }
    /// The body as text.
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }
    /// A header's value.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    /// The error code of an error body.
    pub fn code(&self) -> String {
        self.json()["error"]["code"].as_str().unwrap_or_default().to_owned()
    }
}

/// One request over a fresh connection.
pub async fn send(addr: SocketAddr, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Resp {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: forgebase\r\nConnection: close\r\n");
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    if !body.is_empty() {
        req.push_str("Content-Type: application/json\r\n");
    }
    req.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
    s.write_all(req.as_bytes()).await.unwrap();
    let _ = s.write_all(body).await;
    let mut out = Vec::new();
    let _ = s.read_to_end(&mut out).await;
    parse(&out)
}

fn parse(raw: &[u8]) -> Resp {
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").unwrap_or(raw.len());
    let head = String::from_utf8_lossy(&raw[..split]).into_owned();
    let mut rest = raw.get(split + 4..).unwrap_or_default().to_vec();
    let mut lines = head.split("\r\n");
    let status = lines
        .next()
        .and_then(|l| l.split(' ').nth(1))
        .and_then(|c| c.parse().ok())
        .unwrap_or(0);
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
        .collect();
    if headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.eq_ignore_ascii_case("chunked"))
    {
        rest = dechunk(&rest);
    }
    Resp {
        status,
        headers,
        body: rest,
    }
}

fn dechunk(mut b: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    while let Some(nl) = b.windows(2).position(|w| w == b"\r\n") {
        let size = usize::from_str_radix(String::from_utf8_lossy(&b[..nl]).trim(), 16).unwrap_or(0);
        if size == 0 {
            break;
        }
        let start = nl + 2;
        out.extend_from_slice(&b[start..start + size]);
        b = &b[start + size + 2..];
    }
    out
}

/// A data directory with a short path (the engine's socket path is limited).
pub fn data_tempdir() -> tempfile::TempDir {
    let base = std::env::var_os("FORGEBASE_TEST_PG_TMP").unwrap_or_else(|| std::env::temp_dir().into_os_string());
    tempfile::Builder::new().prefix("fbt-").tempdir_in(base).unwrap()
}

/// The server's configuration: managed PostgreSQL, durability off.
pub fn config(dir: &std::path::Path) -> Config {
    let mut cfg = Config::defaults(dir);
    cfg.server.host = "127.0.0.1".into();
    cfg.server.port = 0;
    cfg.server.shutdown_timeout_secs = 5;
    cfg.queue.poll_interval_ms = 50;
    cfg.database.backend = DatabaseBackend::Postgres;
    cfg.database.postgres.mode = PostgresMode::Managed;
    for (k, v) in [
        ("fsync", "off"),
        ("synchronous_commit", "off"),
        ("full_page_writes", "off"),
    ] {
        cfg.database.postgres.settings.insert(k.into(), v.into());
    }
    cfg
}

/// The running topology under test.
pub struct Topo {
    server: Option<RunningServer>,
    auth: Option<AuthService>,
    /// Address of the HTTP server.
    pub addr: SocketAddr,
    /// The data directory.
    pub dir: tempfile::TempDir,
    /// The configuration the server runs on.
    pub cfg: Config,
    /// An administrator's access token.
    pub admin: String,
    /// The shard clusters, in the order they were added.
    pub shards: Vec<(String, TestCluster)>,
    /// Counter for unique emails and slugs.
    seq: usize,
}

impl Topo {
    /// A server on a fresh data directory, with an administrator. No
    /// replicas and no shards yet.
    pub async fn bare() -> Self {
        Self::bare_with(|_| {}).await
    }

    /// [`Topo::bare`] with a hook on the configuration.
    pub async fn bare_with(f: impl FnOnce(&mut Config)) -> Self {
        let dir = data_tempdir();
        let mut cfg = config(dir.path());
        f(&mut cfg);
        let server = forgebase_server::start(&cfg).await.unwrap();
        let auth = open_auth(&server, &cfg).await;
        let addr = server.local_addr();
        let mut t = Self {
            server: Some(server),
            auth: Some(auth),
            addr,
            dir,
            cfg,
            admin: String::new(),
            shards: Vec::new(),
            seq: 0,
        };
        t.admin = t.make_admin("topology-admin@example.com").await;
        t
    }

    /// The full topology: `replicas` local replicas on control and `shards`
    /// shard clusters, all added through the API.
    pub async fn start(replicas: usize, shards: usize) -> Self {
        let mut t = Self::bare().await;
        for i in 1..=replicas {
            t.add_replica(&format!("r{i}")).await;
        }
        for i in 1..=shards {
            t.add_shard(&format!("s{i}")).await;
        }
        t
    }

    /// The running server.
    pub fn server(&self) -> &RunningServer {
        self.server.as_ref().expect("server is running")
    }

    /// A request without credentials.
    pub async fn req(&self, method: &str, path: &str, headers: &[(&str, &str)], body: Option<Json>) -> Resp {
        let bytes = body.map(|b| serde_json::to_vec(&b).unwrap()).unwrap_or_default();
        send(self.addr, method, path, headers, &bytes).await
    }

    /// A request with a bearer token.
    pub async fn as_(&self, token: &str, method: &str, path: &str, extra: &[(&str, &str)], body: Option<Json>) -> Resp {
        let auth = format!("Bearer {token}");
        let mut h = vec![("Authorization", auth.as_str())];
        h.extend_from_slice(extra);
        self.req(method, path, &h, body).await
    }

    /// A request as the administrator.
    pub async fn adm(&self, method: &str, path: &str, body: Option<Json>) -> Resp {
        self.as_(&self.admin, method, path, &[], body).await
    }

    async fn make_admin(&self, email: &str) -> String {
        self.auth
            .as_ref()
            .unwrap()
            .create_admin(email, ADMIN_PASSWORD, "test", &ClientInfo::default())
            .await
            .unwrap();
        let r = self
            .req(
                "POST",
                "/api/auth/admins/login",
                &[],
                Some(json!({"email": email, "password": ADMIN_PASSWORD})),
            )
            .await;
        assert_eq!(r.status, 200, "admin login: {}", r.text());
        r.json()["access_token"].as_str().unwrap().to_owned()
    }

    /// Signs the administrator in again (tokens survive restarts, but a
    /// topology change that restarts nothing never needs this).
    pub async fn relogin(&mut self) {
        let r = self
            .req(
                "POST",
                "/api/auth/admins/login",
                &[],
                Some(json!({"email": "topology-admin@example.com", "password": ADMIN_PASSWORD})),
            )
            .await;
        assert_eq!(r.status, 200, "admin login: {}", r.text());
        self.admin = r.json()["access_token"].as_str().unwrap().to_owned();
    }

    /// A user in the default realm: (access token, principal key).
    pub async fn user(&mut self) -> String {
        self.seq += 1;
        let email = format!("topo-user-{}@example.com", self.seq);
        self.auth
            .as_ref()
            .unwrap()
            .signup(&ClientInfo::default(), None, &email, USER_PASSWORD, None)
            .await
            .unwrap();
        // FIX-15 (found by FB-178): identity reads run on the primary, so an
        // anonymous login right after a signup finds the user on a lagging replica.
        let r = self
            .req(
                "POST",
                "/api/auth/login",
                &[],
                Some(json!({"email": email, "password": USER_PASSWORD})),
            )
            .await;
        assert_eq!(r.status, 200, "login: {}", r.text());
        r.json()["access_token"].as_str().unwrap().to_owned()
    }

    /// Polls an operation until it ends.
    pub async fn wait_op(&self, id: &str, timeout: Duration) -> Json {
        let start = Instant::now();
        loop {
            let r = self.adm("GET", &format!("{BASE}/operations/{id}"), None).await;
            assert_eq!(r.status, 200, "operation: {}", r.text());
            let j = r.json();
            if matches!(j["status"].as_str(), Some("succeeded" | "dead" | "cancelled")) {
                return j;
            }
            assert!(start.elapsed() < timeout, "operation {id} still {}: {j}", j["status"]);
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// `GET /topology`.
    pub async fn topology(&self) -> Json {
        let r = self.adm("GET", &format!("{BASE}/topology"), None).await;
        assert_eq!(r.status, 200, "topology: {}", r.text());
        r.json()
    }

    /// A node of the topology by name.
    pub async fn node(&self, name: &str) -> Option<Json> {
        let t = self.topology().await;
        t["nodes"].as_array()?.iter().find(|n| n["name"] == name).cloned()
    }

    /// A set (`control` or a shard) of the topology by id or name.
    pub async fn set(&self, id_or_name: &str) -> Option<Json> {
        let t = self.topology().await;
        t["sets"]
            .as_array()?
            .iter()
            .find(|s| s["id"] == id_or_name || s["name"] == id_or_name)
            .cloned()
    }

    /// Adds a local replica through the API and waits for the clone.
    pub async fn add_replica(&mut self, name: &str) -> Json {
        let r = self
            .adm(
                "POST",
                &format!("{BASE}/nodes"),
                Some(json!({"name": name, "kind": "local"})),
            )
            .await;
        assert_eq!(r.status, 202, "add {name}: {}", r.text());
        let id = r.json()["operation"]["id"].as_str().unwrap().to_owned();
        let done = self.wait_op(&id, Duration::from_secs(240)).await;
        assert_eq!(done["status"], "succeeded", "{done}");
        self.wait_in_rotation(name, Duration::from_secs(120)).await;
        self.node(name).await.expect("the replica is listed")
    }

    /// Waits until the health monitor has `name` in read rotation.
    pub async fn wait_in_rotation(&self, name: &str, timeout: Duration) {
        let start = Instant::now();
        loop {
            if let Some(n) = self.node(name).await
                && n["in_rotation"] == true
            {
                return;
            }
            assert!(start.elapsed() < timeout, "{name} never entered rotation");
            tokio::time::sleep(Duration::from_millis(300)).await;
        }
    }

    /// Starts a shard cluster, adds it with `POST /shards` and waits for the
    /// `database.shard.add` job. Returns the set id.
    pub async fn add_shard(&mut self, name: &str) -> String {
        let cluster = TestCluster::start(name, &[]).await.expect("an engine");
        cluster.exec("postgres", "CREATE DATABASE forgebase").await;
        let r = self
            .adm(
                "POST",
                &format!("{BASE}/shards"),
                Some(json!({
                    "name": name,
                    "node": {"name": format!("{name}-primary"), "kind": "external", "url": cluster.url_of("forgebase")}
                })),
            )
            .await;
        assert_eq!(r.status, 202, "add shard {name}: {}", r.text());
        let id = r.json()["operation"]["id"].as_str().unwrap().to_owned();
        let done = self.wait_op(&id, Duration::from_secs(300)).await;
        assert_eq!(done["status"], "succeeded", "{done}");
        if self.shards.is_empty() {
            // Tenants live on the shards (control keeps its own data).
            self.takes_tenants("control", false).await;
        }
        self.shards.push((name.to_owned(), cluster));
        self.set(name).await.expect("the shard is listed")["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// The cluster behind shard `name`.
    pub fn shard(&self, name: &str) -> &TestCluster {
        &self.shards.iter().find(|(n, _)| n == name).expect("shard cluster").1
    }

    /// A new tenant (the production environment of a new project) and the
    /// access token of its owner.
    ///
    /// Creating the project places its environments through the placement
    /// policy (FIX-15): [`Topo::add_shard`] keeps new tenants off control, so
    /// they go to the least-loaded shard (a tie to the lower number), the
    /// three environments of a project together.
    pub async fn new_tenant(&mut self) -> String {
        let user = self.user().await;
        self.seq += 1;
        let slug = format!("org{}", self.seq);
        let r = self
            .as_(
                &user,
                "POST",
                "/api/v1/orgs",
                &[],
                Some(json!({"slug": slug, "name": slug})),
            )
            .await;
        assert_eq!(r.status, 201, "org: {}", r.text());
        let org = r.json()["id"].as_str().unwrap().to_owned();
        let p = self
            .as_(
                &user,
                "POST",
                &format!("/api/v1/orgs/{org}/projects"),
                &[("X-ForgeBase-Org", &org)],
                Some(json!({"slug": "app", "name": "app"})),
            )
            .await;
        assert_eq!(p.status, 201, "project: {}", p.text());
        p.json()["environments"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["name"] == "production")
            .unwrap()["id"]
            .as_str()
            .unwrap()
            .to_owned()
    }

    /// Whether `set` takes new tenants (`PATCH /shards/{id}`).
    pub async fn takes_tenants(&self, set: &str, on: bool) {
        let r = self
            .adm(
                "PATCH",
                &format!("{BASE}/shards/{set}"),
                Some(json!({"hosts_tenants": on})),
            )
            .await;
        assert_eq!(r.status, 200, "hosts_tenants of {set}: {}", r.text());
    }

    /// Creates a tenant-scoped collection `name` with a text `body` field and
    /// an integer `n`.
    pub async fn tenant_collection(&self, name: &str) {
        let r = self
            .adm(
                "POST",
                "/api/v1/collections",
                Some(json!({"name": name, "options": {"tenant_scoped": true}, "fields": [
                    {"name": "body", "type": "text"}, {"name": "n", "type": "number"}]})),
            )
            .await;
        assert_eq!(r.status, 201, "collection: {}", r.text());
    }

    /// `POST /api/v1/collections/{c}/records` in `tenant`.
    pub async fn write(&self, tenant: &str, collection: &str, body: Json) -> Resp {
        self.as_(
            &self.admin,
            "POST",
            &format!("/api/v1/collections/{collection}/records"),
            &[("X-ForgeBase-Tenant", tenant)],
            Some(body),
        )
        .await
    }

    /// `GET /api/v1/collections/{c}/records` in `tenant` with extra headers.
    pub async fn list(&self, tenant: &str, collection: &str, extra: &[(&str, &str)]) -> Resp {
        let mut h = vec![("X-ForgeBase-Tenant", tenant)];
        h.extend_from_slice(extra);
        self.as_(
            &self.admin,
            "GET",
            &format!("/api/v1/collections/{collection}/records?perPage=500"),
            &h,
            None,
        )
        .await
    }

    /// Record count of a listing.
    pub fn count(resp: &Resp) -> usize {
        let j = resp.json();
        j["items"].as_array().map_or(0, Vec::len)
    }

    /// Rows of `sql` on the primary (the control database).
    pub async fn control_scalar(&self, sql: &str) -> i64 {
        let rows = self.server().database().fetch_all(&Statement::raw(sql)).await.unwrap();
        rows.rows[0][0].as_i64().unwrap_or(-1)
    }

    /// Where the server's primary keeps its data directory.
    pub fn primary_pgdata(&self) -> PathBuf {
        self.server().engine().unwrap().layout().pgdata().to_owned()
    }

    /// The data directory of local node `name` (`postgres/nodes/<name>/data`).
    pub fn node_data(&self, name: &str) -> PathBuf {
        self.cfg.data_dir.join("postgres").join("nodes").join(name).join("data")
    }

    /// Stops the server, keeping the data directory. [`Topo::start_again`]
    /// brings it back.
    pub async fn stop(&mut self) {
        self.auth = None;
        if let Some(s) = self.server.take() {
            s.shutdown().await.unwrap();
        }
    }

    /// Starts the server again on the same directory (after [`Topo::stop`]
    /// or a crash), and signs the administrator in again.
    pub async fn start_again(&mut self) {
        let server = forgebase_server::start(&self.cfg).await.unwrap();
        self.auth = Some(open_auth(&server, &self.cfg).await);
        self.addr = server.local_addr();
        self.server = Some(server);
        self.relogin().await;
    }

    /// A crash of the server: the shutdown is attempted for at most 30 s and
    /// its result ignored (the caller kills what it wants dead first).
    pub async fn crash(&mut self) {
        self.auth = None;
        if let Some(s) = self.server.take() {
            let _ = tokio::time::timeout(Duration::from_secs(30), s.shutdown()).await;
        }
    }

    /// Stops the server gracefully and starts it again.
    pub async fn restart(&mut self) {
        self.stop().await;
        self.start_again().await;
    }

    /// Stops the server.
    pub async fn shutdown(mut self) {
        self.stop().await;
    }
}

async fn open_auth(server: &RunningServer, cfg: &Config) -> AuthService {
    let mut acfg = AuthConfig::for_data_dir(&cfg.data_dir);
    acfg.argon2 = Argon2Config::insecure_fast_for_tests();
    AuthService::open_with_data(
        Arc::new(server.database().clone()),
        Arc::new(DefaultAuthorizer),
        &cfg.data_dir,
        acfg,
        None,
    )
    .await
    .unwrap()
}

/// The process id in a data directory's `postmaster.pid`.
pub fn postmaster_pid(data: &std::path::Path) -> Option<u32> {
    std::fs::read_to_string(data.join("postmaster.pid"))
        .ok()?
        .lines()
        .next()?
        .trim()
        .parse()
        .ok()
}

/// `kill -9` of a process (a crash: no shutdown checkpoint, no goodbye).
pub fn kill9(pid: u32) {
    let st = std::process::Command::new("kill")
        .args(["-9", &pid.to_string()])
        .status()
        .unwrap();
    assert!(st.success(), "kill -9 {pid}");
}

/// Waits until `f` is true.
pub async fn until(what: &str, timeout: Duration, mut f: impl AsyncFnMut() -> bool) {
    let start = Instant::now();
    loop {
        if f().await {
            return;
        }
        assert!(start.elapsed() < timeout, "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// What `postmaster.pid` of a data directory says: process id, port and
/// socket directory.
pub fn postmaster_info(data: &std::path::Path) -> Option<(u32, u16, PathBuf)> {
    let text = std::fs::read_to_string(data.join("postmaster.pid")).ok()?;
    let mut l = text.lines();
    let pid = l.next()?.trim().parse().ok()?;
    let port = l.nth(2)?.trim().parse().ok()?;
    let sock = PathBuf::from(l.next()?.trim());
    Some((pid, port, sock))
}

/// Runs `sql` with the engine's `psql` on the node whose data directory is
/// `data`, as the engine's superuser (its password from `roles.secret` under
/// `data_dir`) over the node's socket; returns stdout (unaligned, tuples
/// only).
pub fn psql(data_dir: &std::path::Path, node: &std::path::Path, db: &str, sql: &str) -> Result<String, String> {
    let (_, port, sock) = postmaster_info(node).ok_or("node is not running")?;
    let engine = forgebase_db::shard_testing::engine_dir().ok_or("no engine")?;
    let secrets = forgebase_pgengine::RoleSecrets::load(&data_dir.join("postgres").join("roles.secret"))
        .map_err(|e| e.to_string())?
        .ok_or("no roles.secret")?;
    let password = secrets
        .password(forgebase_pgengine::Role::Super)
        .ok_or("no superuser password")?;
    let out = std::process::Command::new(engine.join("bin").join("psql"))
        .env("PGPASSWORD", password)
        .args([
            "-X",
            "-A",
            "-t",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-w",
            "-U",
            "forgebase_super",
            "-d",
            db,
            "-h",
        ])
        .arg(&sock)
        .args(["-p", &port.to_string(), "-c", sql])
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_owned())
    } else {
        Err(String::from_utf8_lossy(&out.stderr).trim().to_owned())
    }
}

/// Records a measurement: one `FB178_MEASURE <name> <json>` line on stderr,
/// and appended to the file named by `FORGEBASE_TOPOLOGY_OUT` when set (CI
/// uploads it).
pub fn record(name: &str, value: &Json) {
    let line = format!("FB178_MEASURE {name} {value}");
    eprintln!("{line}");
    if let Some(path) = std::env::var_os("FORGEBASE_TOPOLOGY_OUT") {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
            let _ = writeln!(f, "{line}");
        }
    }
}

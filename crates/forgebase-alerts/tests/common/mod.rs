//! Test harness: a migrated database on the backend `FORGEBASE_TEST_BACKEND`
//! selects (SQLite by default), the queue service, customization with an
//! SMTP provider pointed at a scripted in-process server, a local HTTP
//! receiver reached through the SSRF-safe client (a fake DNS maps
//! `hooks.test` to a public address routed to the receiver), synthetic
//! Prometheus text and a manual clock.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Bytes;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use forgebase_alerts::{AlertDeps, AlertService, AlertSettings, MetricsSource};
use forgebase_auth::crypto::Keyring;
use forgebase_core::DataLayout;
use forgebase_data::{AccessContext, DataService, DefaultAuthorizer};
use forgebase_db::testing::{TestDb, test_backend};
use forgebase_db::{Backend, BoxFuture, Database};
use forgebase_functions::fetch::{Connector, Resolver};
use forgebase_functions::{FetchConfig, Fetcher};
use forgebase_jobs::ManualClock;
use forgebase_queue::{QueueService, QueueSettings};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn system() -> AccessContext {
    AccessContext::system()
}

// ------------------------------------------------------------ metrics

/// Prometheus text the tests set by hand.
#[derive(Debug, Default)]
pub struct FakeMetrics(pub Mutex<String>);

impl MetricsSource for FakeMetrics {
    fn render(&self) -> String {
        self.0.lock().unwrap().clone()
    }
}

impl FakeMetrics {
    pub fn set(&self, text: &str) {
        *self.0.lock().unwrap() = text.to_owned();
    }
}

// ------------------------------------------------------------ receiver

#[derive(Debug)]
struct MapResolver(HashMap<String, Vec<IpAddr>>);

impl Resolver for MapResolver {
    fn resolve<'a>(&'a self, host: &'a str, _port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        let r = self
            .0
            .get(host)
            .cloned()
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "no such host"));
        Box::pin(async move { r })
    }
}

fn public() -> IpAddr {
    "93.184.216.34".parse().unwrap()
}

#[derive(Debug)]
struct Route(SocketAddr);

impl Connector for Route {
    fn connect(&self, addr: SocketAddr) -> BoxFuture<'_, io::Result<TcpStream>> {
        let target = if addr.ip() == public() { self.0 } else { addr };
        Box::pin(TcpStream::connect(target))
    }
}

/// The SSRF-safe client, with `hooks.test` routed to `receiver` and
/// `internal.test` resolving to a private address.
pub fn fetcher(receiver: SocketAddr) -> Fetcher {
    let dns = MapResolver(
        [
            ("hooks.test".to_owned(), vec![public()]),
            ("internal.test".to_owned(), vec!["10.1.2.3".parse().unwrap()]),
        ]
        .into_iter()
        .collect(),
    );
    Fetcher::with(
        &forgebase_auth::net::TlsSettings::new(),
        FetchConfig::default(),
        Arc::new(dns),
        Arc::new(Route(receiver)),
    )
    .unwrap()
}

/// One request the receiver got.
#[derive(Debug, Clone)]
pub struct Call {
    pub path: String,
    pub headers: HashMap<String, String>,
    pub body: Vec<u8>,
}

impl Call {
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }

    pub fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }
}

/// A local HTTP server recording every request; answers `status`.
#[derive(Debug)]
pub struct Receiver {
    pub calls: Mutex<Vec<Call>>,
    pub status: Mutex<u16>,
}

impl Receiver {
    pub fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    pub fn on(&self, path: &str) -> Vec<Call> {
        self.calls().into_iter().filter(|c| c.path == path).collect()
    }

    pub fn set_status(&self, s: u16) {
        *self.status.lock().unwrap() = s;
    }
}

pub async fn receiver() -> (SocketAddr, Arc<Receiver>) {
    let r = Arc::new(Receiver {
        calls: Mutex::new(Vec::new()),
        status: Mutex::new(204),
    });
    async fn handle(State(r): State<Arc<Receiver>>, req: Request) -> impl IntoResponse {
        let path = req.uri().path().to_owned();
        let headers = req
            .headers()
            .iter()
            .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or("").to_owned()))
            .collect();
        let body: Bytes = axum::body::to_bytes(req.into_body(), 1 << 22).await.unwrap_or_default();
        r.calls.lock().unwrap().push(Call {
            path,
            headers,
            body: body.to_vec(),
        });
        let s = *r.status.lock().unwrap();
        StatusCode::from_u16(s).unwrap_or(StatusCode::OK)
    }
    let app = Router::new().fallback(handle).with_state(r.clone());
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(l, app).await;
    });
    (addr, r)
}

// ---------------------------------------------------------------- SMTP

/// A scripted SMTP server: accepts every message; records the data.
pub async fn smtp() -> (u16, Arc<Mutex<Vec<String>>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    tokio::spawn(async move {
        loop {
            let Ok((sock, _)) = l.accept().await else { return };
            let seen = s2.clone();
            tokio::spawn(async move {
                let (r, mut w) = sock.into_split();
                let mut r = BufReader::new(r);
                w.write_all(b"220 mock ESMTP\r\n").await.unwrap();
                let mut line = String::new();
                loop {
                    line.clear();
                    if r.read_line(&mut line).await.unwrap_or(0) == 0 {
                        return;
                    }
                    let up = line.trim_end().to_ascii_uppercase();
                    let reply: &[u8] = if up.starts_with("EHLO") {
                        b"250-mock\r\n250 OK\r\n"
                    } else if up == "DATA" {
                        w.write_all(b"354 go\r\n").await.unwrap();
                        let mut buf = Vec::new();
                        let mut b = [0u8; 1];
                        while !buf.ends_with(b"\r\n.\r\n") {
                            if r.read(&mut b).await.unwrap_or(0) == 0 {
                                return;
                            }
                            buf.push(b[0]);
                        }
                        seen.lock().unwrap().push(String::from_utf8_lossy(&buf).into_owned());
                        b"250 queued\r\n"
                    } else if up == "QUIT" {
                        w.write_all(b"221 bye\r\n").await.unwrap();
                        return;
                    } else {
                        b"250 ok\r\n"
                    };
                    w.write_all(reply).await.unwrap();
                }
            });
        }
    });
    (port, seen)
}

// ------------------------------------------------------------- harness

pub struct H {
    pub db: Database,
    pub data: DataService,
    pub svc: AlertService,
    pub queue: QueueService,
    pub customize: forgebase_customize::CustomizeService,
    pub jobs: forgebase_jobs::JobService,
    pub clock: ManualClock,
    pub metrics: Arc<FakeMetrics>,
    pub receiver: Arc<Receiver>,
    pub addr: SocketAddr,
    pub smtp: Arc<Mutex<Vec<String>>>,
    pub layout: DataLayout,
    pub test: TestDb,
    pub dir: tempfile::TempDir,
}

pub async fn harness() -> H {
    harness_with(|_| {}).await
}

pub async fn harness_with(tune: impl FnOnce(&mut AlertSettings)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let test = test_backend().await;
    let db = test.database().clone();
    let backend: Arc<dyn Backend> = Arc::new(db.clone());
    let layout = DataLayout::new(dir.path().join("data"));
    layout.ensure().unwrap();
    let data = DataService::new(backend.clone(), Arc::new(DefaultAuthorizer))
        .await
        .unwrap();
    let keyring = Keyring::load_or_create(&dir.path().join("master.key")).unwrap();
    let clock = ManualClock::from_system();
    let (addr, receiver) = receiver().await;
    let queue = QueueService::with_clock(
        data.clone(),
        keyring.clone(),
        QueueSettings {
            poll_interval: Duration::from_millis(20),
            allow_http_push: true,
            ..QueueSettings::default()
        },
        Arc::new(clock.clone()),
    );
    queue.set_fetcher(fetcher(addr));
    let policy = forgebase_policy::PolicyService::open(backend.clone()).await.unwrap();
    let customize = forgebase_customize::CustomizeService::open(
        backend.clone(),
        policy,
        Arc::new(forgebase_auth::SystemClock),
        forgebase_customize::CustomizeConfig::default(),
    )
    .await
    .unwrap();
    let jobs = forgebase_jobs::JobService::new(data.clone());
    customize.attach_jobs(jobs.clone());
    let (smtp_port, smtp) = smtp().await;
    let mut sc = forgebase_customize::SmtpConfig::new("127.0.0.1", "alerts@forgebase.test");
    sc.port = smtp_port;
    sc.tls = forgebase_customize::SmtpTls::None;
    sc.timeout = Duration::from_secs(5);
    customize.set_mail_provider(Arc::new(forgebase_customize::SmtpProvider::new(sc)));
    let metrics = Arc::new(FakeMetrics::default());
    // The built-in rules stay off unless a test turns them on (the disk
    // rules would fire on a full test volume).
    let mut settings = AlertSettings {
        allow_http: true,
        retry_base_ms: 1_000,
        disabled_rules: forgebase_alerts::rules::builtins("x")
            .into_iter()
            .map(|r| r.id)
            .collect(),
        ..AlertSettings::default()
    };
    tune(&mut settings);
    let svc = AlertService::open(AlertDeps {
        db: backend,
        keyring,
        clock: Arc::new(clock.clone()),
        fetcher: Some(fetcher(addr)),
        customize: Some(customize.clone()),
        queue: Some(queue.clone()),
        layout: Some(layout.clone()),
        metrics: metrics.clone(),
        settings,
        public_url: Some("https://forgebase.example.com".into()),
    })
    .await
    .unwrap();
    H {
        db,
        data,
        svc,
        queue,
        customize,
        jobs,
        clock,
        metrics,
        receiver,
        addr,
        smtp,
        layout,
        test,
        dir,
    }
}

impl H {
    pub fn advance(&self, d: Duration) {
        self.clock.advance(d);
    }

    /// One round: evaluate, then deliver what is due.
    pub async fn round(&self) -> forgebase_alerts::RoundReport {
        let r = self.svc.evaluate().await.unwrap();
        self.svc.deliver_due().await.unwrap();
        r
    }
}

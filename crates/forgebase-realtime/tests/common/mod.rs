//! Test harness: a real SQLite database, the real auth service and auth
//! layer, the realtime router served on a loopback port, and an authorizer
//! whose rules a test can change while connections are open.
#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::http::{Request, StatusCode};
use forgebase_auth::{Argon2Config, AuthConfig, AuthService, ClientInfo, LoginOptions, LoginOutcome};
use forgebase_data::db::{Database, DbOptions, Statement, Value};
use forgebase_data::schema::Collection;
use forgebase_data::{AccessContext, Action, Authorizer, DataService, Decision, Predicate, Principal};
use forgebase_realtime::{FanOut, RealtimeConfig, RealtimeService};
use futures_util::{SinkExt, StreamExt};
use http_body_util::BodyExt;
use serde_json::{Value as Json, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use tower::ServiceExt;

/// What users may read in a collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rule {
    /// Every record.
    Allow,
    /// Records whose `owner` is the caller's id.
    Owner,
    /// Nothing.
    Deny,
}

/// Administrators may do everything; users may list/view collections per
/// [`Rule`] (changeable at runtime) and use channels named `public:*`.
#[derive(Debug, Default)]
pub struct Rules {
    rules: RwLock<HashMap<String, Rule>>,
}

impl Rules {
    pub fn set(&self, collection: &str, rule: Rule) {
        self.rules.write().unwrap().insert(collection.to_owned(), rule);
    }
}

impl Authorizer for Rules {
    fn authorize(&self, ctx: &AccessContext, action: Action, coll: Option<&Collection>) -> Decision {
        if ctx.is_privileged() {
            return Decision::Allow;
        }
        let Principal::User { id, .. } = &ctx.principal else {
            return Decision::Deny;
        };
        if !matches!(action, Action::List | Action::View) {
            return Decision::Deny;
        }
        let rule = coll.and_then(|c| self.rules.read().unwrap().get(&c.name).copied());
        match rule {
            Some(Rule::Allow) => Decision::Allow,
            Some(Rule::Owner) => Decision::Filter(
                Predicate::parse("owner = {:me}", HashMap::from([("me".to_owned(), json!(id))])).unwrap(),
            ),
            _ => Decision::Deny,
        }
    }

    fn authorize_channel(&self, ctx: &AccessContext, _action: Action, channel: &str) -> Decision {
        if ctx.is_privileged() || (matches!(ctx.principal, Principal::User { .. }) && channel.starts_with("public:")) {
            Decision::Allow
        } else {
            Decision::Deny
        }
    }
}

pub fn admin() -> AccessContext {
    AccessContext::admin("root")
}

pub fn ci() -> ClientInfo {
    ClientInfo {
        ip: Some("127.0.0.1".into()),
        user_agent: None,
        request_id: None,
        impersonator: None,
    }
}

/// A test configuration with short timings.
pub fn fast_config() -> RealtimeConfig {
    RealtimeConfig {
        heartbeat_interval: Duration::from_secs(30),
        revalidate_interval: Duration::from_millis(200),
        poll_interval: Duration::from_millis(200),
        send_timeout: Duration::from_secs(2),
        ..RealtimeConfig::default()
    }
}

pub struct Stack {
    pub addr: SocketAddr,
    pub auth: AuthService,
    pub data: DataService,
    pub rt: RealtimeService,
    pub rules: Arc<Rules>,
    pub db: Database,
    pub app: Router,
    server: tokio::task::JoinHandle<()>,
    _tmp: tempfile::TempDir,
}

impl Drop for Stack {
    fn drop(&mut self) {
        self.rt.shutdown();
        self.server.abort();
    }
}

impl Stack {
    pub async fn start(cfg: RealtimeConfig) -> Self {
        Self::start_with(cfg, None).await
    }

    pub async fn start_with(cfg: RealtimeConfig, fanout: Option<Arc<dyn FanOut>>) -> Self {
        let tmp = tempfile::tempdir().unwrap();
        let core = forgebase_core::Config::defaults(tmp.path());
        let db = Database::open(DbOptions::from_config(&core.database, tmp.path().join("forgebase.db")))
            .await
            .unwrap();
        db.migrate(None).await.unwrap();
        let mut acfg = AuthConfig::for_data_dir(tmp.path());
        acfg.argon2 = Argon2Config::insecure_fast_for_tests();
        acfg.cookie_secure = false;
        let rules = Arc::new(Rules::default());
        let auth = AuthService::open_with_data(Arc::new(db.clone()), rules.clone(), tmp.path(), acfg, None)
            .await
            .unwrap();
        let data = auth.data().clone();
        let rt = RealtimeService::start(data.clone(), Some(auth.clone()), cfg, fanout)
            .await
            .unwrap();
        let app = Router::new()
            .nest_service("/api/v1/realtime", forgebase_realtime::router(rt.clone()))
            .layer(axum::middleware::from_fn_with_state(
                auth.clone(),
                forgebase_auth::http::authenticate,
            ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let svc = app.clone().into_make_service_with_connect_info::<SocketAddr>();
        let server = tokio::spawn(async move {
            axum::serve(listener, svc).await.unwrap();
        });
        Self {
            addr,
            auth,
            data,
            rt,
            rules,
            db,
            app,
            server,
            _tmp: tmp,
        }
    }

    /// Creates a collection as administrator.
    pub async fn collection(&self, def: Json) -> Collection {
        let c: Collection = serde_json::from_value(def).unwrap();
        self.data.create_collection(&admin(), c).await.unwrap()
    }

    /// Creates a record as administrator.
    pub async fn create(&self, collection: &str, rec: Json) -> Json {
        Json::Object(self.data.create_record(&admin(), collection, rec).await.unwrap())
    }

    /// Signs up and signs in a user; returns (user id, access token, session id).
    pub async fn user(&self, email: &str) -> (String, String, String) {
        let view = self
            .auth
            .signup(&ci(), None, email, "correct horse battery", None)
            .await
            .unwrap();
        let id = view["id"].as_str().unwrap().to_owned();
        let out = self
            .auth
            .login_password(&ci(), None, email, "correct horse battery", &LoginOptions::default())
            .await
            .unwrap();
        let LoginOutcome::Session(s) = out else {
            panic!("unexpected MFA")
        };
        (id, s.access_token.clone().unwrap(), s.session_id.clone())
    }

    /// Signs in a new administrator; returns its access token.
    pub async fn admin_token(&self) -> String {
        self.auth
            .create_admin("root@example.com", "a long admin password", "system", &ci())
            .await
            .unwrap();
        let out = self
            .auth
            .login_admin(
                &ci(),
                "root@example.com",
                "a long admin password",
                &LoginOptions::default(),
            )
            .await
            .unwrap();
        let LoginOutcome::Session(s) = out else {
            panic!("unexpected MFA")
        };
        s.access_token.clone().unwrap()
    }

    /// Outbox rows.
    pub async fn outbox_len(&self) -> i64 {
        let rows = self
            .data
            .backend()
            .fetch_all(&Statement::new(
                "SELECT count(*) FROM _forgebase_realtime_outbox",
                vec![],
            ))
            .await
            .unwrap();
        rows.rows[0][0].as_i64().unwrap()
    }

    /// Opens a WebSocket with an optional bearer token; returns the client
    /// after reading the `welcome` message.
    pub async fn ws(&self, token: Option<&str>) -> Ws {
        let mut ws = self.ws_raw(token, "").await.expect("websocket upgrade");
        let w = ws.next_json().await;
        assert_eq!(w["type"], "welcome", "{w}");
        ws.welcome = w;
        ws
    }

    /// Opens a WebSocket whose socket has a tiny receive buffer, so a client
    /// that stops reading backs up the server quickly.
    pub async fn ws_small_buffer(&self, token: &str) -> Ws {
        let url = format!("ws://{}/api/v1/realtime/ws", self.addr);
        let mut req = url.into_client_request().unwrap();
        req.headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        let sock = tokio::net::TcpSocket::new_v4().unwrap();
        sock.set_recv_buffer_size(4096).unwrap();
        let tcp = sock.connect(self.addr).await.unwrap();
        let (stream, _) = tokio_tungstenite::client_async(req, MaybeTlsStream::Plain(tcp))
            .await
            .unwrap();
        let mut ws = Ws {
            stream,
            welcome: Json::Null,
        };
        ws.welcome = ws.next_json().await;
        ws
    }

    /// Opens a WebSocket; `Err(status, body)` when the upgrade is refused.
    pub async fn ws_raw(&self, token: Option<&str>, query: &str) -> Result<Ws, (u16, String)> {
        let url = format!("ws://{}/api/v1/realtime/ws{query}", self.addr);
        let mut req = url.into_client_request().unwrap();
        if let Some(t) = token {
            req.headers_mut()
                .insert("authorization", format!("Bearer {t}").parse().unwrap());
        }
        match tokio_tungstenite::connect_async(req).await {
            Ok((stream, _)) => Ok(Ws {
                stream,
                welcome: Json::Null,
            }),
            Err(tokio_tungstenite::tungstenite::Error::Http(res)) => {
                let body = res
                    .body()
                    .as_ref()
                    .map(|b| String::from_utf8_lossy(b).into_owned())
                    .unwrap_or_default();
                Err((res.status().as_u16(), body))
            }
            Err(e) => panic!("websocket connect failed: {e}"),
        }
    }

    /// Opens an SSE stream in-process (through the auth layer).
    pub async fn sse(&self, token: Option<&str>, query: &str, last_event_id: Option<&str>) -> Result<Sse, (u16, Json)> {
        let mut b = Request::builder().uri(format!("/api/v1/realtime/sse{query}"));
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        if let Some(id) = last_event_id {
            b = b.header("last-event-id", id);
        }
        let res = self.app.clone().oneshot(b.body(Body::empty()).unwrap()).await.unwrap();
        if res.status() != StatusCode::OK {
            let status = res.status().as_u16();
            let bytes = res.into_body().collect().await.unwrap().to_bytes();
            return Err((status, serde_json::from_slice(&bytes).unwrap_or(Json::Null)));
        }
        Ok(Sse {
            body: res.into_body(),
            buf: String::new(),
        })
    }

    /// Waits until `f` holds (5 s).
    pub async fn until(&self, what: &str, f: impl Fn(&forgebase_realtime::RealtimeStats) -> bool) {
        for _ in 0..500 {
            if f(&self.rt.stats()) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}: {:?}", self.rt.stats());
    }
}

/// URL-encodes a query value.
pub fn enc(v: &str) -> String {
    url::form_urlencoded::byte_serialize(v.as_bytes()).collect()
}

pub struct Ws {
    pub stream: WebSocketStream<MaybeTlsStream<TcpStream>>,
    pub welcome: Json,
}

/// What a WebSocket read produced.
#[derive(Debug)]
pub enum Got {
    Json(Json),
    Close(Option<u16>, String),
}

impl Ws {
    pub async fn send(&mut self, v: Json) {
        self.stream.send(Message::Text(v.to_string().into())).await.unwrap();
    }

    /// Next text message or close (pings and pongs skipped), 5 s timeout.
    pub async fn next(&mut self) -> Got {
        self.next_within(Duration::from_secs(5))
            .await
            .unwrap_or_else(|| panic!("no message within 5 s"))
    }

    pub async fn next_within(&mut self, d: Duration) -> Option<Got> {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            let m = tokio::time::timeout_at(deadline, self.stream.next()).await.ok()?;
            match m {
                Some(Ok(Message::Text(t))) => return Some(Got::Json(serde_json::from_str(t.as_str()).unwrap())),
                Some(Ok(Message::Close(f))) => {
                    return Some(Got::Close(
                        f.as_ref().map(|f| u16::from(f.code)),
                        f.map(|f| f.reason.to_string()).unwrap_or_default(),
                    ));
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Some(Got::Close(None, e.to_string())),
                None => return Some(Got::Close(None, "eof".into())),
            }
        }
    }

    pub async fn next_json(&mut self) -> Json {
        match self.next().await {
            Got::Json(j) => j,
            Got::Close(c, r) => panic!("connection closed ({c:?} {r}) while expecting a message"),
        }
    }

    /// Asserts nothing arrives for `d`.
    pub async fn quiet(&mut self, d: Duration) {
        if let Some(g) = self.next_within(d).await {
            panic!("expected silence, got {g:?}");
        }
    }

    /// Subscribes; returns the `subscribed` or `error` reply (replayed events
    /// before it are returned too, in order).
    pub async fn subscribe(&mut self, spec: Json) -> (Vec<Json>, Json) {
        let mut msg = spec;
        msg["type"] = json!("subscribe");
        self.send(msg).await;
        let mut events = Vec::new();
        loop {
            let m = self.next_json().await;
            match m["type"].as_str() {
                Some("subscribed" | "error") => return (events, m),
                _ => events.push(m),
            }
        }
    }

    /// Reads until a close frame; returns its code.
    pub async fn close_code(&mut self) -> Option<u16> {
        loop {
            match self.next_within(Duration::from_secs(10)).await {
                Some(Got::Close(c, r)) => {
                    if c.is_none() {
                        eprintln!("closed without a close frame: {r}");
                    }
                    return c;
                }
                Some(Got::Json(_)) => continue,
                None => panic!("connection not closed within 10 s"),
            }
        }
    }
}

pub struct Sse {
    body: Body,
    buf: String,
}

/// One SSE event.
#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: String,
    pub id: Option<String>,
    pub data: Json,
}

impl Sse {
    /// Next event (comments skipped); `None` when the stream ended.
    pub async fn next(&mut self) -> Option<SseEvent> {
        loop {
            if let Some(pos) = self.buf.find("\n\n") {
                let block: String = self.buf.drain(..pos + 2).collect();
                let mut ev = SseEvent {
                    event: "message".into(),
                    id: None,
                    data: Json::Null,
                };
                let mut data = String::new();
                for line in block.lines() {
                    if let Some(v) = line.strip_prefix("event:") {
                        ev.event = v.trim().to_owned();
                    } else if let Some(v) = line.strip_prefix("id:") {
                        ev.id = Some(v.trim().to_owned());
                    } else if let Some(v) = line.strip_prefix("data:") {
                        data.push_str(v.trim_start());
                    }
                }
                if data.is_empty() {
                    continue; // comment (heartbeat)
                }
                ev.data = serde_json::from_str(&data).unwrap();
                return Some(ev);
            }
            let frame = tokio::time::timeout(Duration::from_secs(5), self.body.frame())
                .await
                .expect("no SSE data within 5 s")?;
            let frame = frame.unwrap();
            if let Ok(bytes) = frame.into_data() {
                self.buf.push_str(&String::from_utf8_lossy(&bytes));
            }
        }
    }

    pub async fn expect(&mut self, event: &str) -> SseEvent {
        let e = self.next().await.expect("stream ended");
        assert_eq!(e.event, event, "{e:?}");
        e
    }
}

/// A Value as i64 (helper for raw SQL counts).
pub fn int(v: &Value) -> i64 {
    v.as_i64().unwrap()
}

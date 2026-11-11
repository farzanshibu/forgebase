//! Transport behaviour against a scripted HTTP server: retries, backoff, refresh, error mapping.
//! Hermetic: no ForgeBase binary needed.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use forgebase_sdk::generated::NewJob;
use forgebase_sdk::{Consistency, ErrorKind, ForgeBase, ListOptions, RetryPolicy, SignInOptions};
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

#[derive(Clone)]
enum Reply {
    Json(u16, Value, Vec<(&'static str, &'static str)>),
    /// Close the connection without answering.
    Drop,
}

fn ok(body: Value) -> Reply {
    Reply::Json(200, body, vec![])
}

fn err(status: u16, code: &str) -> Reply {
    Reply::Json(
        status,
        json!({"error": {"code": code, "message": code, "request_id": "req-1"}}),
        vec![],
    )
}

#[derive(Debug, Clone)]
struct Seen {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

impl Seen {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

struct Mock {
    base: String,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Mock {
    /// Replies are consumed in order; the last one repeats.
    async fn start(script: Vec<Reply>) -> Self {
        let l = TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let base = format!("http://{}", l.local_addr().expect("addr"));
        let seen: Arc<Mutex<Vec<Seen>>> = Arc::default();
        let (s2, script) = (seen.clone(), Arc::new(script));
        tokio::spawn(async move {
            loop {
                let Ok((mut sock, _)) = l.accept().await else { return };
                let (seen, script) = (s2.clone(), script.clone());
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let head_end = loop {
                        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break i + 4;
                        }
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                    let mut lines = head.lines();
                    let mut first = lines.next().unwrap_or_default().split(' ');
                    let (method, path) = (
                        first.next().unwrap_or_default().to_owned(),
                        first.next().unwrap_or_default().to_owned(),
                    );
                    let headers: Vec<(String, String)> = lines
                        .filter_map(|l| {
                            l.split_once(':')
                                .map(|(k, v)| (k.trim().to_owned(), v.trim().to_owned()))
                        })
                        .collect();
                    let len = headers
                        .iter()
                        .find(|(k, _)| k.eq_ignore_ascii_case("content-length"))
                        .and_then(|(_, v)| v.parse::<usize>().ok())
                        .unwrap_or(0);
                    while buf.len() < head_end + len {
                        match sock.read(&mut chunk).await {
                            Ok(0) | Err(_) => break,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    }
                    let body = String::from_utf8_lossy(&buf[head_end.min(buf.len())..]).into_owned();
                    let n = {
                        let mut g = seen.lock().expect("lock");
                        g.push(Seen {
                            method,
                            path,
                            headers,
                            body,
                        });
                        g.len()
                    };
                    match script[(n - 1).min(script.len() - 1)].clone() {
                        Reply::Drop => {}
                        Reply::Json(status, body, extra) => {
                            let body = body.to_string();
                            let mut out = format!(
                                "HTTP/1.1 {status} X\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n",
                                body.len()
                            );
                            for (k, v) in extra {
                                out.push_str(&format!("{k}: {v}\r\n"));
                            }
                            out.push_str("\r\n");
                            out.push_str(&body);
                            let _ = sock.write_all(out.as_bytes()).await;
                            let _ = sock.shutdown().await;
                        }
                    }
                });
            }
        });
        Self { base, seen }
    }

    fn calls(&self) -> Vec<Seen> {
        self.seen.lock().expect("lock").clone()
    }

    fn client(&self) -> ForgeBase {
        self.client_with(fast_retry())
    }

    fn client_with(&self, retry: RetryPolicy) -> ForgeBase {
        ForgeBase::builder(&self.base).retry(retry).build().expect("client")
    }
}

fn fast_retry() -> RetryPolicy {
    RetryPolicy {
        base_delay: Duration::from_millis(5),
        max_delay: Duration::from_millis(20),
        ..RetryPolicy::default()
    }
}

fn session_body(access: &str, refresh: &str) -> Value {
    json!({"token_type": "Bearer", "access_token": access, "expires_in": 3600, "refresh_token": refresh,
        "session_id": "s1", "session_expires_at": "2099-01-01T00:00:00Z", "subject": {"kind": "user"}, "user": {"id": "u1"}})
}

#[tokio::test]
async fn retries_503_for_any_method_then_succeeds() {
    let m = Mock::start(vec![
        err(503, "unavailable"),
        err(503, "unavailable"),
        ok(json!({"id": "r1"})),
    ])
    .await;
    let created = m
        .client()
        .collection("posts")
        .create(&json!({"title": "x"}))
        .await
        .expect("created after retries");
    assert_eq!(created["id"], "r1");
    let calls = m.calls();
    assert_eq!(calls.len(), 3);
    assert!(
        calls.iter().all(|c| c.method == "POST" && c.body.contains("title")),
        "the body is resent on every attempt"
    );
}

#[tokio::test]
async fn gives_up_after_max_retries_with_the_last_error() {
    let m = Mock::start(vec![err(503, "unavailable")]).await;
    let e = m.client().collection("posts").get("r1").await.expect_err("always 503");
    assert_eq!(e.kind(), Some(ErrorKind::Server));
    assert_eq!(e.status(), Some(503));
    assert_eq!(m.calls().len(), 4, "1 attempt + 3 retries");

    let m = Mock::start(vec![err(503, "unavailable")]).await;
    let e = m
        .client_with(RetryPolicy::none())
        .collection("posts")
        .get("r1")
        .await
        .expect_err("no retries");
    assert_eq!(e.status(), Some(503));
    assert_eq!(m.calls().len(), 1);
}

#[tokio::test]
async fn bad_gateway_is_retried_for_idempotent_methods_only() {
    let m = Mock::start(vec![err(502, "bad_gateway"), ok(json!({"id": "r1", "title": "t"}))]).await;
    assert!(
        m.client().collection("posts").get("r1").await.is_ok(),
        "GET is retried on 502"
    );
    assert_eq!(m.calls().len(), 2);

    let m = Mock::start(vec![err(502, "bad_gateway"), ok(json!({"id": "r1"}))]).await;
    let e = m
        .client()
        .collection("posts")
        .create(&json!({}))
        .await
        .expect_err("POST is not retried on 502");
    assert_eq!(e.status(), Some(502));
    assert_eq!(m.calls().len(), 1);
}

#[tokio::test]
async fn quota_429_is_never_retried_but_rate_limit_429_is() {
    let m = Mock::start(vec![err(429, "quota_exceeded")]).await;
    let e = m
        .client()
        .collection("posts")
        .create(&json!({}))
        .await
        .expect_err("quota");
    assert!(e.is_quota_exceeded(), "{e}");
    assert_eq!(m.calls().len(), 1, "waiting does not help against a quota");

    let m = Mock::start(vec![err(429, "rate_limited"), ok(json!({"id": "r1"}))]).await;
    assert!(m.client().collection("posts").create(&json!({})).await.is_ok());
    assert_eq!(m.calls().len(), 2);
}

#[tokio::test]
async fn honours_retry_after_and_refuses_absurd_waits() {
    let m = Mock::start(vec![
        Reply::Json(
            429,
            json!({"error": {"code": "rate_limited", "message": "slow down"}}),
            vec![("retry-after", "0.3")],
        ),
        ok(json!({"id": "r1"})),
    ])
    .await;
    let t = Instant::now();
    assert!(m.client().collection("posts").create(&json!({})).await.is_ok());
    assert!(t.elapsed() >= Duration::from_millis(280), "waited {:?}", t.elapsed());

    let m = Mock::start(vec![Reply::Json(
        429,
        json!({"error": {"code": "rate_limited", "message": "later"}}),
        vec![("retry-after", "3600")],
    )])
    .await;
    let e = m
        .client()
        .collection("posts")
        .create(&json!({}))
        .await
        .expect_err("too long to wait");
    assert!(e.is_rate_limited());
    assert_eq!(e.api().and_then(|a| a.retry_after), Some(Duration::from_secs(3600)));
    assert_eq!(m.calls().len(), 1, "fails at once instead of sleeping an hour");
}

#[tokio::test]
async fn network_errors_are_retried_only_when_safe() {
    // A dropped connection on a GET is retried.
    let m = Mock::start(vec![Reply::Drop, ok(json!({"id": "r1"}))]).await;
    assert!(m.client().collection("posts").get("r1").await.is_ok());
    assert_eq!(m.calls().len(), 2);

    // ... on a plain POST it is not (the server may have executed it).
    let m = Mock::start(vec![Reply::Drop, ok(json!({"id": "r1"}))]).await;
    let e = m
        .client()
        .collection("posts")
        .create(&json!({}))
        .await
        .expect_err("POST not repeated");
    assert_eq!(e.code(), "network_error");
    assert_eq!(m.calls().len(), 1);

    // ... but a POST that carries an idempotency key is.
    let m = Mock::start(vec![Reply::Drop, ok(json!({"id": "j1", "kind": "k"}))]).await;
    let job = NewJob {
        kind: "k".into(),
        idempotency_key: Some("once".into()),
        ..NewJob::default()
    };
    let _ = m.client().jobs().enqueue(&job).await; // the mock's reply is not a full Job: only the retry matters
    assert_eq!(m.calls().len(), 2, "idempotency key makes the POST safe to repeat");
}

#[tokio::test]
async fn refused_connection_is_a_network_error() {
    let fb = ForgeBase::builder("http://127.0.0.1:1")
        .retry(RetryPolicy::none())
        .build()
        .expect("client");
    let e = fb.collection("posts").get("x").await.expect_err("nothing listens");
    assert_eq!(e.code(), "network_error");
    assert!(e.api().is_none());
}

#[tokio::test]
async fn refreshes_after_a_401_and_retries_with_the_new_token() {
    let m = Mock::start(vec![
        ok(session_body("access-1", "refresh-1")), // POST /api/auth/login
        err(401, "unauthenticated"),               // GET /api/auth/me with access-1
        ok(session_body("access-2", "refresh-2")), // POST /api/auth/refresh
        ok(json!({"subject": {"kind": "user"}})),  // GET /api/auth/me with access-2
    ])
    .await;
    let fb = m.client();
    fb.auth()
        .sign_in("a@b.co", "pw", &SignInOptions::default())
        .await
        .expect("sign in");
    let me = fb.auth().me().await.expect("me after refresh");
    assert!(me.get("subject").is_some());

    let calls = m.calls();
    assert_eq!(
        calls.iter().map(|c| c.path.as_str()).collect::<Vec<_>>(),
        ["/api/auth/login", "/api/auth/me", "/api/auth/refresh", "/api/auth/me"]
    );
    assert_eq!(calls[1].header("authorization"), Some("Bearer access-1"));
    assert!(
        calls[2].body.contains("refresh-1") && calls[2].header("authorization").is_none(),
        "refresh is anonymous and sends the refresh token"
    );
    assert_eq!(calls[3].header("authorization"), Some("Bearer access-2"));
    assert_eq!(
        fb.auth().session().expect("session").refresh_token.as_deref(),
        Some("refresh-2")
    );
}

#[tokio::test]
async fn a_rejected_refresh_token_ends_the_session() {
    let m = Mock::start(vec![
        ok(session_body("access-1", "refresh-1")),
        err(401, "unauthenticated"),
    ])
    .await;
    let fb = m.client();
    fb.auth()
        .sign_in("a@b.co", "pw", &SignInOptions::default())
        .await
        .expect("sign in");
    let e = fb.auth().me().await.expect_err("everything is 401");
    assert!(e.is_unauthenticated());
    assert!(
        fb.auth().session().is_none(),
        "a revoked or reused refresh token signs the client out"
    );
    // Only one refresh attempt: no loops.
    assert_eq!(m.calls().iter().filter(|c| c.path == "/api/auth/refresh").count(), 1);
}

#[tokio::test]
async fn proactively_refreshes_a_token_about_to_expire() {
    let near = json!({"token_type": "Bearer", "access_token": "access-1", "expires_in": 5, "refresh_token": "refresh-1",
        "session_id": "s1", "session_expires_at": "2099-01-01T00:00:00Z", "subject": {}});
    let m = Mock::start(vec![
        ok(near),
        ok(session_body("access-2", "refresh-2")),
        ok(json!({"subject": {}})),
    ])
    .await;
    let fb = m.client(); // default skew 30 s: a 5 s token counts as expiring
    fb.auth()
        .sign_in("a@b.co", "pw", &SignInOptions::default())
        .await
        .expect("sign in");
    fb.auth().me().await.expect("me");
    let calls = m.calls();
    assert_eq!(
        calls[1].path, "/api/auth/refresh",
        "refreshed before the call, not after a 401"
    );
    assert_eq!(calls[2].header("authorization"), Some("Bearer access-2"));
}

#[tokio::test]
async fn api_keys_are_sent_as_x_api_key() {
    let m = Mock::start(vec![ok(json!({"items": []}))]).await;
    let fb = ForgeBase::builder(&m.base)
        .api_key("fb_secret")
        .user_agent("my-app/1")
        .header("X-Trace", "t1")
        .build()
        .expect("client");
    fb.collections().list().await.expect("list");
    let c = &m.calls()[0];
    assert_eq!(c.header("x-api-key"), Some("fb_secret"));
    assert!(c.header("authorization").is_none());
    assert_eq!(c.header("user-agent"), Some("my-app/1"));
    assert_eq!(c.header("x-trace"), Some("t1"));
}

#[tokio::test]
async fn maps_error_envelopes_to_typed_errors() {
    let body = json!({"error": {"code": "validation_failed", "message": "bad input", "request_id": "r-9",
        "details": {"fields": {"title": {"code": "required", "message": "is required"}}}}});
    let m = Mock::start(vec![Reply::Json(400, body, vec![])]).await;
    let e = m
        .client()
        .collection("posts")
        .create(&json!({}))
        .await
        .expect_err("400");
    assert!(e.is_validation());
    assert_eq!((e.code(), e.request_id()), ("validation_failed", Some("r-9")));
    assert_eq!(
        e.api().expect("api").field_errors(),
        [("title".to_owned(), "required".to_owned(), "is required".to_owned())]
    );

    for (status, kind) in [
        (401, ErrorKind::Authentication),
        (403, ErrorKind::Permission),
        (404, ErrorKind::NotFound),
        (409, ErrorKind::Conflict),
        (412, ErrorKind::Conflict),
        (500, ErrorKind::Server),
    ] {
        let m = Mock::start(vec![err(status, "x")]).await;
        let e = m
            .client_with(RetryPolicy::none())
            .collection("posts")
            .get("r")
            .await
            .expect_err("error status");
        assert_eq!(e.kind(), Some(kind), "status {status}");
    }
}

#[tokio::test]
async fn builds_the_right_requests() {
    let m = Mock::start(vec![ok(
        json!({"items": [], "per_page": 5, "page": 1, "total_pages": 1, "total_items": 0}),
    )])
    .await;
    let fb = m.client();
    fb.collection("my posts")
        .list(
            &ListOptions::new()
                .filter(r#"a = "b c""#)
                .sort("-x")
                .sort("y")
                .per_page(5)
                .page(1)
                .skip_total(true),
        )
        .await
        .expect("list");
    let c = &m.calls()[0];
    assert_eq!(c.method, "GET");
    assert!(
        c.path.starts_with("/api/v1/collections/my%20posts/records?"),
        "{}",
        c.path
    );
    for want in [
        "filter=a+%3D+%22b+c%22",
        "sort=-x%2Cy",
        "per_page=5",
        "page=1",
        "skip_total=true",
    ] {
        assert!(c.path.contains(want), "{want} not in {}", c.path);
    }

    // Typed params switch to the JSON query endpoint.
    fb.collection("posts")
        .list(&ListOptions::new().filter("n > {:min}").params(json!({"min": 3})))
        .await
        .expect("query");
    let c = &m.calls()[1];
    assert_eq!(
        (c.method.as_str(), c.path.as_str()),
        ("POST", "/api/v1/collections/posts/records/query")
    );
    let body: Value = serde_json::from_str(&c.body).expect("json body");
    assert_eq!(body["params"], json!({"min": 3}));
    assert!(body.get("sort").is_none(), "unset options are left out: {body}");

    // Optimistic concurrency header.
    fb.collection("posts")
        .update("r1", &json!({"n": 1}), forgebase_sdk::WriteOptions { version: Some(7) })
        .await
        .ok();
    let c = &m.calls()[2];
    assert_eq!((c.method.as_str(), c.header("if-match")), ("PATCH", Some("\"7\"")));
}

#[tokio::test]
async fn rejects_bad_base_urls() {
    assert!(ForgeBase::new("not a url").is_err());
    assert!(ForgeBase::new("ftp://example.com").is_err());
    assert!(ForgeBase::new("https://example.com/").is_ok());
}

fn store_purchase(provider: &str) -> Value {
    json!({"id": "sp1", "provider": provider, "purchase_id": "orig1", "subject": "usr_1", "status": "active",
        "kind": "subscription", "price": "pro_month", "product_id": "com.x.pro", "environment": "test",
        "auto_renew": true, "created_at": "t", "updated_at": "t"})
}

#[tokio::test]
async fn billing_store_verify_checkout_and_purchases() {
    use forgebase_sdk::{StoreCheckout, StoreProvider, StoreReceipt};
    let m = Mock::start(vec![
        ok(store_purchase("app_store")),
        ok(json!({"session_id": "co_1", "url": "https://pay.creem.io/x"})),
        ok(json!({"items": [store_purchase("creem")]})),
        ok(json!({"items": []})),
    ])
    .await;
    let store = m.client().billing().store();

    let p = store
        .verify(
            StoreProvider::AppStore,
            &StoreReceipt {
                signed_transaction: Some("jws".into()),
                subject: Some("org:1".into()),
                ..StoreReceipt::default()
            },
        )
        .await
        .expect("verify");
    assert_eq!(
        (p.provider.as_str(), p.status.as_str(), p.auto_renew),
        ("app_store", "active", true)
    );
    let c = &m.calls()[0];
    assert_eq!(
        (c.method.as_str(), c.path.as_str()),
        ("POST", "/api/v1/billing/store/app_store/verify")
    );
    let body: Value = serde_json::from_str(&c.body).expect("json body");
    assert_eq!(body, json!({"signed_transaction": "jws", "subject": "org:1"}));

    let page = store
        .checkout(
            StoreProvider::Creem,
            &StoreCheckout {
                price: "pro_month".into(),
                success_url: "https://app/ok".into(),
                idempotency_key: "k1".into(),
                subject: None,
            },
        )
        .await
        .expect("checkout");
    assert_eq!(page.url, "https://pay.creem.io/x");
    let c = &m.calls()[1];
    assert_eq!(c.path, "/api/v1/billing/store/creem/checkout");
    let body: Value = serde_json::from_str(&c.body).expect("json body");
    assert_eq!(
        body,
        json!({"price": "pro_month", "success_url": "https://app/ok", "idempotency_key": "k1"})
    );

    assert_eq!(store.purchases(None).await.expect("purchases").len(), 1);
    assert!(store.purchases(Some("org:9")).await.expect("purchases").is_empty());
    let calls = m.calls();
    assert_eq!(calls[2].path, "/api/v1/billing/store-purchases");
    assert_eq!(calls[3].path, "/api/v1/billing/store-purchases?subject=org%3A9");
}

#[tokio::test]
async fn billing_store_verify_surfaces_a_conflict() {
    use forgebase_sdk::{StoreProvider, StoreReceipt};
    let m = Mock::start(vec![err(409, "purchase_owned")]).await;
    let e = m
        .client()
        .billing()
        .store()
        .verify(
            StoreProvider::GooglePlay,
            &StoreReceipt {
                purchase_token: Some("t".into()),
                product_id: Some("p".into()),
                ..StoreReceipt::default()
            },
        )
        .await
        .expect_err("owned");
    assert_eq!(e.kind(), Some(ErrorKind::Conflict));
}

#[tokio::test]
async fn commit_tokens_are_tracked_and_sent_back() {
    let m = Mock::start(vec![
        ok(json!({"items": [], "per_page": 30})),
        Reply::Json(200, json!({"id": "r1"}), vec![("x-forgebase-commit", "control/0/1A")]),
        Reply::Json(
            409,
            json!({"error": {"code": "conflict", "message": "x", "request_id": "r"}}),
            vec![("X-ForgeBase-Commit", "s1/0/5")],
        ),
        ok(json!({"items": [], "per_page": 30})),
    ])
    .await;
    let fb = m.client();
    let posts = fb.collection("posts");
    posts.list(&ListOptions::default()).await.expect("list");
    posts.create(&json!({"title": "t"})).await.expect("create");
    let _ = posts.create(&json!({"title": "t"})).await;
    fb.with_consistency(Consistency::Strong)
        .collection("posts")
        .list(&ListOptions::default())
        .await
        .expect("strong list");
    let calls = m.calls();
    assert_eq!(
        calls[0].header("x-forgebase-read-after"),
        None,
        "nothing before the first commit"
    );
    assert_eq!(calls[0].header("x-forgebase-consistency"), None);
    assert_eq!(calls[2].header("x-forgebase-read-after"), Some("control/0/1A"));
    assert_eq!(calls[3].header("x-forgebase-read-after"), Some("control/0/1A, s1/0/5"));
    assert_eq!(calls[3].header("x-forgebase-consistency"), Some("strong"));
    assert_eq!(fb.commit_tokens().get("s1").as_deref(), Some("0/5"));
}

#[tokio::test]
async fn strong_consistency_from_the_builder() {
    let m = Mock::start(vec![ok(json!({"ok": true}))]).await;
    let fb = ForgeBase::builder(&m.base)
        .consistency(Consistency::Strong)
        .build()
        .expect("client");
    fb.request("GET", "/api/v1/x", None).await.expect("request");
    assert_eq!(m.calls()[0].header("x-forgebase-consistency"), Some("strong"));
}

//! Webhook delivery tooling (ADR-0031) against real persistence: the push
//! attempt log, test sends, dead-letter redrive (once, even concurrently),
//! zero-downtime secret rotation, endpoint health with the circuit breaker
//! and resume, subscription expiry, "retry now" and the event catalog, plus
//! their HTTP routes. Webhooks go to a local receiver on `127.0.0.1:0`
//! through the SSRF-safe client (a fake DNS maps `hooks.test` to a public
//! address that is routed to the receiver).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::io;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::body::Body;
use axum::extract::Request;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use common::{H, admin, harness, harness_with, settings, user};
use forgebase_api::{AppState, RouterOptions};
use forgebase_core::{DataLayout, ErrorCode};
use forgebase_db::{Backend, BoxFuture, Statement};
use forgebase_functions::fetch::{Connector, Resolver};
use forgebase_functions::{FetchConfig, Fetcher};
use forgebase_queue::{
    AttemptQuery, ExampleSource, NewMessage, NewSubscription, NewTopic, PauseRequest, PullRequest, PushConfig,
    PushErrorClass, PushKind, PushOutcome, QueueSettings, RecordSource, RedriveRequest, RetryRequest,
    RotateSecretRequest, SubscriptionPatch, TestSendRequest, push,
};
use serde_json::{Value as Json, json};
use tokio::net::TcpStream;
use tower::ServiceExt;

// ---------------------------------------------------------------- receiver

/// Maps host names to addresses (a fake DNS).
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

/// Routes the public test address to the local receiver.
#[derive(Debug)]
struct Route(SocketAddr);

impl Connector for Route {
    fn connect(&self, addr: SocketAddr) -> BoxFuture<'_, io::Result<TcpStream>> {
        let target = if addr.ip() == public() { self.0 } else { addr };
        Box::pin(TcpStream::connect(target))
    }
}

fn fetcher(receiver: SocketAddr) -> Fetcher {
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
struct Call {
    headers: HashMap<String, String>,
    body: Json,
    raw: Vec<u8>,
    status: u16,
}

impl Call {
    fn header(&self, name: &str) -> &str {
        self.headers.get(name).map_or("", String::as_str)
    }
}

/// A webhook receiver. Answers from `script` while it lasts, then with
/// `status`; with `accept` set it answers `204` when the signature verifies
/// with that secret and `401` otherwise.
#[derive(Default)]
struct Receiver {
    calls: Mutex<Vec<Call>>,
    script: Mutex<VecDeque<(u16, String)>>,
    status: Mutex<u16>,
    accept: Mutex<Option<String>>,
}

impl Receiver {
    fn calls(&self) -> Vec<Call> {
        self.calls.lock().unwrap().clone()
    }

    fn set_status(&self, s: u16) {
        *self.status.lock().unwrap() = s;
    }

    fn accept(&self, secret: Option<&str>) {
        *self.accept.lock().unwrap() = secret.map(str::to_owned);
    }

    fn script(&self, replies: &[(u16, &str)]) {
        let mut s = self.script.lock().unwrap();
        s.extend(replies.iter().map(|(c, b)| (*c, (*b).to_owned())));
    }
}

async fn receiver() -> (SocketAddr, Arc<Receiver>) {
    let r = Arc::new(Receiver {
        status: Mutex::new(204),
        ..Receiver::default()
    });
    let state = r.clone();
    let app = Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: axum::body::Bytes| {
            let state = state.clone();
            async move {
                let hs: HashMap<String, String> = headers
                    .iter()
                    .map(|(k, v)| (k.as_str().to_owned(), v.to_str().unwrap_or_default().to_owned()))
                    .collect();
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_secs();
                let (status, reply) = if let Some(secret) = state.accept.lock().unwrap().clone() {
                    let sig = hs.get(push::SIGNATURE_HEADER).cloned().unwrap_or_default();
                    // The harness clock starts at the real time: a wide tolerance covers clock moves.
                    let ok = push::verify(&secret, &sig, &body, i64::try_from(now).unwrap(), 86_400 * 30);
                    (if ok { 204 } else { 401 }, String::new())
                } else if let Some(next) = state.script.lock().unwrap().pop_front() {
                    next
                } else {
                    (*state.status.lock().unwrap(), String::new())
                };
                state.calls.lock().unwrap().push(Call {
                    headers: hs,
                    body: serde_json::from_slice(&body).unwrap_or(Json::Null),
                    raw: body.to_vec(),
                    status,
                });
                let mut res = (StatusCode::from_u16(status).unwrap(), reply).into_response();
                res.headers_mut().insert("x-request-id", "req-42".parse().unwrap());
                res.headers_mut()
                    .insert("set-cookie", "session=secret".parse().unwrap());
                res
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (addr, r)
}

fn hook(name: &str, topic: &str) -> NewSubscription {
    NewSubscription {
        name: name.into(),
        topic: topic.into(),
        backoff_base_ms: Some(0),
        push: Some(PushConfig {
            kind: PushKind::Webhook,
            endpoint: "http://hooks.test/hook".into(),
        }),
        ..NewSubscription::default()
    }
}

async fn setup(settings: QueueSettings) -> (H, Arc<Receiver>) {
    let h = harness_with(settings).await;
    let (addr, r) = receiver().await;
    h.svc.set_fetcher(fetcher(addr));
    h.topic("t").await;
    (h, r)
}

async fn attempts(h: &H, sub: &str, q: AttemptQuery) -> Vec<forgebase_queue::PushAttempt> {
    h.svc
        .push_attempts(&admin(), sub, AttemptQuery { limit: 100, ..q })
        .await
        .unwrap()
        .items
}

async fn audit_events(h: &H, event: &str) -> i64 {
    let rows =
        h.db.fetch_all(&Statement::new(
            "SELECT COUNT(*) FROM _forgebase_auth_audit WHERE event = ?1",
            vec![event.into()],
        ))
        .await
        .unwrap();
    rows.scalar().and_then(forgebase_db::Value::as_i64).unwrap_or(0)
}

// --------------------------------------------------------------- attempt log

#[tokio::test]
async fn attempts_record_status_latency_headers_and_a_truncated_body() {
    let (h, r) = setup(QueueSettings {
        push_response_body_bytes: 16,
        ..settings()
    })
    .await;
    let sub = h
        .sub_with(NewSubscription {
            max_delivery_attempts: Some(5),
            ..hook("hook", "t")
        })
        .await;
    let secret = sub.push_secret.unwrap();
    let long = "x".repeat(100);
    r.script(&[(500, long.as_str()), (200, "ok")]);
    let id = h.publish("t", json!({"n": 1})).await;
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0);
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);
    let calls = r.calls();
    assert_eq!(calls.len(), 2);
    assert!(push::verify(
        &secret,
        calls[0].header(push::SIGNATURE_HEADER),
        &calls[0].raw,
        h.svc.now() / 1000,
        300
    ));

    let all = attempts(&h, "hook", AttemptQuery::default()).await;
    assert_eq!(all.len(), 2, "{all:?}");
    let (ok, failed) = (&all[0], &all[1]);
    assert!(ok.id > failed.id, "newest first");
    assert_eq!((failed.attempt, ok.attempt), (1, 2));
    assert_eq!(failed.outcome, PushOutcome::Failure);
    assert_eq!(failed.status, Some(500));
    assert_eq!(failed.error_class, Some(PushErrorClass::HttpStatus));
    assert_eq!(failed.response_body.as_deref(), Some(&long[..16]));
    assert!(failed.response_truncated);
    assert_eq!(failed.response_bytes, Some(100));
    assert_eq!(
        failed.response_headers.get("x-request-id").map(String::as_str),
        Some("req-42")
    );
    assert!(!failed.response_headers.contains_key("set-cookie"), "not allowlisted");
    let sig = &failed.request_headers[push::SIGNATURE_HEADER];
    assert!(sig.starts_with("t=") && sig.ends_with("v1=<redacted>"), "{sig}");
    assert_eq!(failed.request_headers["x-forgebase-message-id"], id);
    assert_eq!(failed.request_headers["x-forgebase-delivery-attempt"], "1");
    assert_eq!(failed.message_id, id);
    assert!(!failed.test);
    assert_eq!(ok.outcome, PushOutcome::Success);
    assert_eq!(ok.status, Some(200));
    assert_eq!(ok.response_body.as_deref(), Some("ok"));
    assert!(!ok.response_truncated);
    assert!(ok.latency_ms >= 0 && ok.latency_ms < 10_000);

    // Filters and one record.
    let only = attempts(
        &h,
        "hook",
        AttemptQuery {
            outcome: Some(PushOutcome::Failure),
            ..AttemptQuery::default()
        },
    )
    .await;
    assert_eq!(only.iter().map(|a| a.id).collect::<Vec<_>>(), vec![failed.id]);
    let by_msg = attempts(
        &h,
        "hook",
        AttemptQuery {
            message_id: Some("msg-none".into()),
            ..AttemptQuery::default()
        },
    )
    .await;
    assert!(by_msg.is_empty());
    let later = attempts(
        &h,
        "hook",
        AttemptQuery {
            since_ms: Some(h.svc.now() + 1),
            ..AttemptQuery::default()
        },
    )
    .await;
    assert!(later.is_empty());
    let page = h
        .svc
        .push_attempts(
            &admin(),
            "hook",
            AttemptQuery {
                limit: 1,
                ..AttemptQuery::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(page.items.len(), 1);
    assert_eq!(page.next_before, Some(ok.id));
    assert_eq!(h.svc.push_attempt(&admin(), "hook", failed.id).await.unwrap(), *failed);
    let err = h.svc.push_attempt(&admin(), "hook", 999_999).await.unwrap_err();
    assert_eq!(err.code(), "attempt_not_found");
    // Reading needs QueueRead on the subscription.
    let err = h
        .svc
        .push_attempts(&user("u9"), "hook", AttemptQuery::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "forbidden");

    // Endpoint health in the metrics.
    let m = h.svc.subscription_metrics(&admin(), "hook").await.unwrap();
    let health = m.push.expect("push health");
    assert_eq!((health.attempts, health.successes, health.failures), (2, 1, 1));
    assert_eq!(health.success_rate, Some(0.5));
    assert!(health.latency_p50_ms.is_some() && health.latency_p99_ms >= health.latency_p50_ms);
    assert_eq!(health.consecutive_failures, 0);
    assert!(health.last_success_at.is_some() && health.last_failure_at.is_some());
    assert!(health.last_error.unwrap().contains("HTTP 500"));
    assert!(!health.paused);
    // Pull subscriptions have none.
    h.sub("pull", "t").await;
    assert!(
        h.svc
            .subscription_metrics(&admin(), "pull")
            .await
            .unwrap()
            .push
            .is_none()
    );
}

#[tokio::test]
async fn failures_are_classified_ssrf_and_dns() {
    let (h, _r) = setup(settings()).await;
    for (name, url) in [
        ("private", "http://internal.test/hook"),
        ("nowhere", "http://nowhere.test/hook"),
    ] {
        h.sub_with(NewSubscription {
            push: Some(PushConfig {
                kind: PushKind::Webhook,
                endpoint: url.into(),
            }),
            ..hook(name, "t")
        })
        .await;
    }
    h.publish("t", json!(1)).await;
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0);
    let a = attempts(&h, "private", AttemptQuery::default()).await;
    assert_eq!(a[0].error_class, Some(PushErrorClass::SsrfRefused), "{a:?}");
    assert_eq!(a[0].status, None);
    let a = attempts(&h, "nowhere", AttemptQuery::default()).await;
    assert_eq!(a[0].error_class, Some(PushErrorClass::Dns), "{a:?}");
    let m = h.svc.subscription_metrics(&admin(), "nowhere").await.unwrap();
    assert_eq!(m.push.unwrap().consecutive_failures, 1);
}

#[tokio::test]
async fn the_attempt_log_is_pruned_by_count_and_age() {
    let (h, r) = setup(QueueSettings {
        push_attempts_per_subscription: 3,
        push_attempts_retention_ms: 3_600_000,
        ..settings()
    })
    .await;
    h.sub_with(hook("hook", "t")).await;
    r.set_status(500);
    h.publish("t", json!(1)).await;
    for _ in 0..5 {
        h.svc.deliver_push().await.unwrap();
    }
    assert_eq!(attempts(&h, "hook", AttemptQuery::default()).await.len(), 5);
    h.svc.run_maintenance().await.unwrap();
    let kept = attempts(&h, "hook", AttemptQuery::default()).await;
    assert_eq!(
        kept.iter().map(|a| a.attempt).collect::<Vec<_>>(),
        vec![5, 4, 3],
        "the newest 3"
    );
    h.clock.advance(Duration::from_secs(3601));
    h.svc.run_maintenance().await.unwrap();
    assert!(
        attempts(&h, "hook", AttemptQuery::default()).await.is_empty(),
        "older than the retention"
    );
}

// ----------------------------------------------------------------- test send

#[tokio::test]
async fn a_test_send_is_signed_and_leaves_no_trace() {
    let (h, r) = setup(QueueSettings {
        push_test_sends_per_minute: 2,
        ..settings()
    })
    .await;
    let secret = h
        .sub_with(NewSubscription {
            max_delivery_attempts: Some(1),
            dead_letter_topic: None,
            ..hook("hook", "t")
        })
        .await
        .push_secret
        .unwrap();
    let a = h
        .svc
        .test_push(&admin(), "hook", TestSendRequest::default())
        .await
        .unwrap();
    assert!(a.test && a.message_id.starts_with("test-"), "{a:?}");
    assert_eq!(a.outcome, PushOutcome::Success);
    assert_eq!(a.status, Some(204));
    let call = &r.calls()[0];
    assert_eq!(call.body["type"], push::TEST_EVENT_TYPE);
    assert_eq!(call.body["subscription"], "hook");
    assert_eq!(call.body["delivery_attempt"], 1);
    assert_eq!(call.body["message"]["id"], a.message_id);
    assert_eq!(call.body["message"]["topic"], "t");
    assert_eq!(call.body["message"]["attributes"]["forgebase.test"], "true");
    assert_eq!(call.header(push::TEST_HEADER), "1");
    assert_eq!(call.header("x-forgebase-subscription"), "hook");
    assert!(push::verify(
        &secret,
        call.header(push::SIGNATURE_HEADER),
        &call.raw,
        h.svc.now() / 1000,
        300
    ));

    // A failing test send: no nack, no dead letter, no health change.
    r.set_status(500);
    let a = h
        .svc
        .test_push(
            &admin(),
            "hook",
            TestSendRequest {
                data: Some(json!({"x": 1})),
            },
        )
        .await
        .unwrap();
    assert_eq!(a.outcome, PushOutcome::Failure);
    assert_eq!(a.error_class, Some(PushErrorClass::HttpStatus));
    assert_eq!(r.calls()[1].body["message"]["data"], json!({"x": 1}));
    let m = h.svc.subscription_metrics(&admin(), "hook").await.unwrap();
    assert_eq!(
        (
            m.delivered_total,
            m.acked_total,
            m.nacked_total,
            m.dead_lettered_total,
            m.dead_letters
        ),
        (0, 0, 0, 0, 0)
    );
    assert_eq!((m.backlog_messages, m.in_flight), (0, 0));
    let health = m.push.unwrap();
    assert_eq!(
        (health.attempts, health.consecutive_failures),
        (0, 0),
        "test sends are not deliveries"
    );
    assert!(health.last_failure_at.is_none());
    let tests = attempts(
        &h,
        "hook",
        AttemptQuery {
            test: Some(true),
            ..AttemptQuery::default()
        },
    )
    .await;
    assert_eq!(tests.len(), 2);
    let deliveries = attempts(
        &h,
        "hook",
        AttemptQuery {
            test: Some(false),
            ..AttemptQuery::default()
        },
    )
    .await;
    assert!(deliveries.is_empty());
    // Nothing was published either.
    let page = h.svc.messages(&admin(), "t", 0, 10).await.unwrap();
    assert!(page.items.is_empty());

    // Rate limit, then a new minute.
    let err = h
        .svc
        .test_push(&admin(), "hook", TestSendRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "rate_limited");
    h.clock.advance(Duration::from_secs(61));
    h.svc
        .test_push(&admin(), "hook", TestSendRequest::default())
        .await
        .unwrap();

    // Pull subscriptions have no endpoint; callers need QueueAdmin.
    h.sub("pull", "t").await;
    let err = h
        .svc
        .test_push(&admin(), "pull", TestSendRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_push_subscription");
    let err = h
        .svc
        .test_push(&user("u9"), "hook", TestSendRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "forbidden");
}

// ------------------------------------------------------------------- redrive

async fn dead_letter_ids(h: &H, sub: &str) -> Vec<i64> {
    let mut ids: Vec<i64> = h
        .svc
        .dead_letters(&admin(), sub, None, 100)
        .await
        .unwrap()
        .items
        .iter()
        .map(|d| d.id)
        .collect();
    ids.sort_unstable();
    ids
}

#[tokio::test]
async fn redrive_one_then_all_delivers_again_once_even_concurrently() {
    let (h, r) = setup(settings()).await;
    h.sub_with(NewSubscription {
        max_delivery_attempts: Some(1),
        ..hook("hook", "t")
    })
    .await;
    r.set_status(503);
    let mut published = Vec::new();
    for i in 0..3 {
        published.push(h.publish("t", json!({"i": i})).await);
    }
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0);
    let dead = dead_letter_ids(&h, "hook").await;
    assert_eq!(dead.len(), 3);
    r.set_status(204);

    // One id.
    let out = h
        .svc
        .redrive(
            &admin(),
            "hook",
            RedriveRequest {
                ids: vec![dead[0], 424_242],
                ..RedriveRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(out.redriven, vec![dead[0]]);
    assert_eq!(out.not_found, vec![424_242]);
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);
    let call = r.calls().last().unwrap().clone();
    assert_eq!(call.body["delivery_attempt"], 1, "the attempt counter starts again");
    let again = h
        .svc
        .redrive(
            &admin(),
            "hook",
            RedriveRequest {
                ids: vec![dead[0]],
                ..RedriveRequest::default()
            },
        )
        .await
        .unwrap();
    assert!(again.redriven.is_empty());
    assert_eq!(again.already_redriven, vec![dead[0]]);

    // All the rest, by two concurrent callers: each record exactly once.
    let root = admin();
    let (a, b) = tokio::join!(
        h.svc.redrive(
            &root,
            "hook",
            RedriveRequest {
                all: true,
                ..RedriveRequest::default()
            }
        ),
        h.svc.redrive(
            &root,
            "hook",
            RedriveRequest {
                all: true,
                ..RedriveRequest::default()
            }
        ),
    );
    let (a, b) = (a.unwrap(), b.unwrap());
    let union: BTreeSet<i64> = a.redriven.iter().chain(&b.redriven).copied().collect();
    assert_eq!(
        union.len(),
        a.redriven.len() + b.redriven.len(),
        "no record twice: {a:?} {b:?}"
    );
    assert_eq!(union, [dead[1], dead[2]].into_iter().collect());
    while h.svc.deliver_push().await.unwrap() > 0 {}
    let delivered: Vec<String> = r
        .calls()
        .iter()
        .filter(|c| c.status == 204)
        .map(|c| c.body["message"]["id"].as_str().unwrap().to_owned())
        .collect();
    let mut sorted = delivered.clone();
    sorted.sort();
    let mut expect = published.clone();
    expect.sort();
    assert_eq!(sorted, expect, "each dead letter delivered once: {delivered:?}");
    let dl = h.svc.dead_letters(&admin(), "hook", None, 10).await.unwrap().items;
    assert!(
        dl.iter()
            .all(|d| d.redriven_at.is_some() && d.redriven_by.as_deref() == Some("admin:root"))
    );
    assert!(audit_events(&h, "queue.dead_letters.redrive").await >= 2);

    // Bad requests.
    for req in [
        RedriveRequest::default(),
        RedriveRequest {
            ids: vec![1],
            all: true,
            ..RedriveRequest::default()
        },
        RedriveRequest {
            ids: vec![1],
            since: Some("2026-01-01T00:00:00Z".into()),
            ..RedriveRequest::default()
        },
    ] {
        let err = h.svc.redrive(&admin(), "hook", req).await.unwrap_err();
        assert_eq!(err.code(), "invalid_redrive");
    }
    let err = h
        .svc
        .redrive(
            &user("u9"),
            "hook",
            RedriveRequest {
                all: true,
                ..RedriveRequest::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "forbidden");
}

#[tokio::test]
async fn redrive_all_filters_by_time_and_attributes() {
    let (h, r) = setup(settings()).await;
    h.sub_with(NewSubscription {
        max_delivery_attempts: Some(1),
        ..hook("hook", "t")
    })
    .await;
    r.set_status(503);
    h.svc
        .publish(
            &admin(),
            "t",
            vec![
                NewMessage::new(json!(1)).attr("kind", "a"),
                NewMessage::new(json!(2)).attr("kind", "b"),
            ],
        )
        .await
        .unwrap();
    h.svc.deliver_push().await.unwrap();
    h.clock.advance(Duration::from_secs(10));
    let cut = forgebase_queue::iso(h.svc.now());
    h.publish("t", json!(3)).await;
    h.svc.deliver_push().await.unwrap();
    assert_eq!(dead_letter_ids(&h, "hook").await.len(), 3);
    let out = h
        .svc
        .redrive(
            &admin(),
            "hook",
            RedriveRequest {
                all: true,
                filter: Some(json!({"kind": "b"})),
                ..RedriveRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(out.redriven.len(), 1);
    let out = h
        .svc
        .redrive(
            &admin(),
            "hook",
            RedriveRequest {
                all: true,
                since: Some(cut),
                ..RedriveRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(out.redriven.len(), 1);
    let out = h
        .svc
        .redrive(
            &admin(),
            "hook",
            RedriveRequest {
                all: true,
                limit: Some(1),
                ..RedriveRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!((out.redriven.len(), out.remaining), (1, 0));
    r.set_status(204);
    while h.svc.deliver_push().await.unwrap() > 0 {}
    let data: BTreeSet<i64> = r
        .calls()
        .iter()
        .filter(|c| c.status == 204)
        .map(|c| c.body["message"]["data"].as_i64().unwrap())
        .collect();
    assert_eq!(data, [1, 2, 3].into_iter().collect());
}

#[tokio::test]
async fn a_dead_letter_whose_message_expired_is_redriven_to_its_subscription_only() {
    let (h, r) = setup(settings()).await;
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "short".into(),
                retention_ms: Some(60_000),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.sub_with(NewSubscription {
        max_delivery_attempts: Some(1),
        ..hook("hook", "short")
    })
    .await;
    h.sub("other", "short").await;
    r.set_status(503);
    let id = h.publish("short", json!({"keep": true})).await;
    h.svc.deliver_push().await.unwrap();
    let got = h.pull("other", 10).await;
    h.ack("other", &got).await;
    h.clock.advance(Duration::from_secs(120));
    assert_eq!(
        h.svc.run_maintenance().await.unwrap().pruned,
        1,
        "retention dropped the message"
    );
    let dead = dead_letter_ids(&h, "hook").await;
    let out = h
        .svc
        .redrive(
            &admin(),
            "hook",
            RedriveRequest {
                ids: dead.clone(),
                ..RedriveRequest::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(out.redriven, dead);
    r.set_status(204);
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);
    let call = r.calls().last().unwrap().clone();
    assert_eq!(call.body["message"]["id"], id, "same message id");
    assert_eq!(call.body["message"]["data"], json!({"keep": true}));
    assert!(
        h.pull("other", 10).await.is_empty(),
        "the copy is for the redriven subscription only"
    );
    assert!(
        h.svc.messages(&admin(), "short", 0, 10).await.unwrap().items.is_empty(),
        "not listed as a topic message"
    );
}

// --------------------------------------------------------------- retry now

#[tokio::test]
async fn retry_now_ends_a_backoff() {
    let h = harness().await;
    h.topic("t").await;
    h.sub_with(NewSubscription {
        name: "s".into(),
        topic: "t".into(),
        backoff_base_ms: Some(60_000),
        ..NewSubscription::default()
    })
    .await;
    h.publish("t", json!(1)).await;
    let got = h.pull("s", 1).await;
    let ids: Vec<String> = got.iter().map(|m| m.ack_id.clone()).collect();
    h.svc.nack(&admin(), "s", &ids).await.unwrap();
    assert!(h.pull("s", 1).await.is_empty(), "waiting for the backoff");
    assert_eq!(h.svc.subscription_metrics(&admin(), "s").await.unwrap().delayed, 1);
    let n = h
        .svc
        .retry_now(
            &admin(),
            "s",
            RetryRequest {
                message_ids: vec!["msg-other".into()],
            },
        )
        .await
        .unwrap();
    assert_eq!(n.count, 0);
    let n = h.svc.retry_now(&admin(), "s", RetryRequest::default()).await.unwrap();
    assert_eq!(n.count, 1);
    assert_eq!(h.pull("s", 1).await.len(), 1);
}

// ----------------------------------------------------------- secret rotation

#[tokio::test]
async fn rotation_signs_with_both_secrets_during_the_grace_window_only() {
    let (h, r) = setup(settings()).await;
    let old = h
        .sub_with(NewSubscription {
            max_delivery_attempts: Some(10),
            ..hook("hook", "t")
        })
        .await
        .push_secret
        .unwrap();
    r.accept(Some(&old));
    h.publish("t", json!("before")).await;
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);

    let rotated = h
        .svc
        .rotate_secret(&admin(), "hook", RotateSecretRequest { grace_ms: Some(60_000) })
        .await
        .unwrap();
    let new = rotated.push_secret.clone();
    assert!(new.starts_with("whsec_") && new != old);
    assert!(rotated.previous_expires_at.is_some());
    let shown = h.svc.subscription(&admin(), "hook").await.unwrap();
    assert!(shown.push_secret.is_none(), "shown once");
    assert_eq!(shown.push_secret_previous_expires_at, rotated.previous_expires_at);
    // During the grace window a receiver on either secret accepts.
    for (secret, label) in [(&old, "old"), (&new, "new")] {
        r.accept(Some(secret));
        h.publish("t", json!(label)).await;
        assert_eq!(h.svc.deliver_push().await.unwrap(), 1, "{label} secret during grace");
    }
    let sig = r.calls().last().unwrap().header(push::SIGNATURE_HEADER).to_owned();
    assert_eq!(sig.matches("v1=").count(), 2, "{sig}");
    // After it only the new one.
    h.clock.advance(Duration::from_secs(61));
    r.accept(Some(&old));
    h.publish("t", json!("after-old")).await;
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0, "old secret after grace");
    r.accept(Some(&new));
    assert_eq!(
        h.svc.deliver_push().await.unwrap(),
        1,
        "new secret after grace (the retry)"
    );
    let sig = r.calls().last().unwrap().header(push::SIGNATURE_HEADER).to_owned();
    assert_eq!(sig.matches("v1=").count(), 1, "{sig}");
    h.svc.run_maintenance().await.unwrap();
    assert!(
        h.svc
            .subscription(&admin(), "hook")
            .await
            .unwrap()
            .push_secret_previous_expires_at
            .is_none()
    );

    // Rotate again and drop the old secret at once.
    let newer = h
        .svc
        .rotate_secret(&admin(), "hook", RotateSecretRequest::default())
        .await
        .unwrap()
        .push_secret;
    let s = h.svc.expire_previous_secret(&admin(), "hook").await.unwrap();
    assert!(s.push_secret_previous_expires_at.is_none());
    r.accept(Some(&new));
    h.publish("t", json!("expired")).await;
    assert_eq!(
        h.svc.deliver_push().await.unwrap(),
        0,
        "the previous secret no longer signs"
    );
    r.accept(Some(&newer));
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);
    // No grace: one signature at once.
    let none = h
        .svc
        .rotate_secret(&admin(), "hook", RotateSecretRequest { grace_ms: Some(0) })
        .await
        .unwrap();
    assert!(none.previous_expires_at.is_none());
    assert_eq!(audit_events(&h, "queue.secret.rotate").await, 3);
    assert_eq!(audit_events(&h, "queue.secret.expire_previous").await, 1);
    // The secret survives a restart (sealed with the keyring).
    let h = h.restart().await;
    let (addr2, r2) = receiver().await;
    h.svc.set_fetcher(fetcher(addr2));
    r2.accept(Some(&none.push_secret));
    h.publish("t", json!("restart")).await;
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);

    // Only webhooks have secrets.
    h.sub("pull", "t").await;
    let err = h
        .svc
        .rotate_secret(&admin(), "pull", RotateSecretRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "not_push_subscription");
    let err = h
        .svc
        .rotate_secret(
            &admin(),
            "hook",
            RotateSecretRequest {
                grace_ms: Some(8 * 86_400_000),
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_parameter");
}

// ------------------------------------------------- health, circuit breaker

#[tokio::test]
async fn consecutive_failures_pause_and_resume_delivers_the_backlog_in_order() {
    let (h, r) = setup(settings()).await;
    h.sub_with(NewSubscription {
        max_delivery_attempts: Some(1),
        pause_after_failures: Some(3),
        ..hook("hook", "t")
    })
    .await;
    r.set_status(503);
    let mut ids = Vec::new();
    for i in 1..=5 {
        ids.push(h.publish("t", json!(i)).await);
    }
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0);
    assert_eq!(r.calls().len(), 3, "the breaker opened after the third failure");
    let s = h.svc.subscription(&admin(), "hook").await.unwrap();
    assert!(s.paused_at.is_some());
    assert!(
        s.paused_reason.as_deref().unwrap().starts_with("auto: 3 consecutive"),
        "{s:?}"
    );
    let m = h.svc.subscription_metrics(&admin(), "hook").await.unwrap();
    let health = m.push.clone().unwrap();
    assert!(health.paused);
    assert_eq!(health.consecutive_failures, 3);
    // Messages 1 and 2 used their one attempt before the pause; the third,
    // whose failure opened the breaker, stays, like everything after it.
    assert_eq!(m.dead_letters, 2);
    assert_eq!(m.backlog_messages, 3);
    assert_eq!(m.in_flight, 0, "the rest of the batch was released");
    assert_eq!(audit_events(&h, "queue.subscription.auto_pause").await, 1);

    // Paused: nothing is sent, messages accumulate, none is dead-lettered.
    ids.push(h.publish("t", json!(6)).await);
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0);
    h.clock.advance(Duration::from_secs(60));
    h.svc.run_maintenance().await.unwrap();
    assert_eq!(r.calls().len(), 3);
    let m = h.svc.subscription_metrics(&admin(), "hook").await.unwrap();
    assert_eq!((m.dead_letters, m.backlog_messages), (2, 4));

    // A test send still works while paused (to check the endpoint).
    r.set_status(204);
    let t = h
        .svc
        .test_push(&admin(), "hook", TestSendRequest::default())
        .await
        .unwrap();
    assert_eq!(t.outcome, PushOutcome::Success);

    let s = h.svc.resume(&admin(), "hook").await.unwrap();
    assert!(s.paused_at.is_none() && s.paused_reason.is_none());
    while h.svc.deliver_push().await.unwrap() > 0 {}
    let delivered: Vec<String> = r
        .calls()
        .iter()
        .filter(|c| c.status == 204 && c.body.get("type").is_none())
        .map(|c| c.body["message"]["id"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(delivered, ids[2..].to_vec(), "the backlog in publish order");
    let m = h.svc.subscription_metrics(&admin(), "hook").await.unwrap();
    assert_eq!((m.backlog_messages, m.dead_letters), (0, 2));
    assert_eq!(m.push.unwrap().consecutive_failures, 0);

    // Manual pause and resume.
    let s = h
        .svc
        .pause(
            &admin(),
            "hook",
            PauseRequest {
                reason: Some("maintenance window".into()),
            },
        )
        .await
        .unwrap();
    assert_eq!(s.paused_reason.as_deref(), Some("maintenance window"));
    h.publish("t", json!(7)).await;
    assert_eq!(h.svc.deliver_push().await.unwrap(), 0);
    h.svc.resume(&admin(), "hook").await.unwrap();
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);
    assert_eq!(audit_events(&h, "queue.subscription.pause").await, 1);
    assert_eq!(audit_events(&h, "queue.subscription.resume").await, 2);

    // Turning auto-pause off.
    let s = h
        .svc
        .update_subscription(
            &admin(),
            "hook",
            SubscriptionPatch {
                pause_after_failures: Some(0),
                ..SubscriptionPatch::default()
            },
        )
        .await
        .unwrap();
    assert!(s.pause_after_failures.is_none());
    let err = h
        .svc
        .pause(&admin(), "nope", PauseRequest::default())
        .await
        .unwrap_err();
    assert_eq!(err.code(), "subscription_not_found");
}

// ------------------------------------------------------------------- expiry

#[tokio::test]
async fn subscriptions_with_a_ttl_are_deleted_when_it_runs_out() {
    let h = harness().await;
    h.topic("t").await;
    let s = h
        .sub_with(NewSubscription {
            name: "tmp".into(),
            topic: "t".into(),
            ttl_ms: Some(60_000),
            ..NewSubscription::default()
        })
        .await;
    assert!(s.expires_at.is_some());
    h.sub("keep", "t").await;
    h.clock.advance(Duration::from_secs(50));
    // Extending it moves the expiry.
    h.svc
        .update_subscription(
            &admin(),
            "tmp",
            SubscriptionPatch {
                ttl_ms: Some(60_000),
                ..SubscriptionPatch::default()
            },
        )
        .await
        .unwrap();
    h.clock.advance(Duration::from_secs(20));
    assert_eq!(h.svc.run_maintenance().await.unwrap().expired_subscriptions, 0);
    h.clock.advance(Duration::from_secs(41));
    assert_eq!(h.svc.run_maintenance().await.unwrap().expired_subscriptions, 1);
    let err = h.svc.subscription(&admin(), "tmp").await.unwrap_err();
    assert_eq!(err.code(), "subscription_not_found");
    h.svc.subscription(&admin(), "keep").await.unwrap();
    let err = h
        .svc
        .create_subscription(
            &admin(),
            NewSubscription {
                name: "short".into(),
                topic: "t".into(),
                ttl_ms: Some(1_000),
                ..NewSubscription::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_parameter");
}

// ------------------------------------------------------------------ catalog

#[tokio::test]
async fn the_catalog_lists_topics_and_record_source_event_types_with_examples() {
    let h = harness().await;
    h.data
        .create_collection(
            &admin(),
            serde_json::from_value(json!({
                "name": "posts",
                "fields": [
                    {"name": "title", "type": "text", "min_length": 3},
                    {"name": "views", "type": "number", "integer": true, "min": 0},
                    {"name": "draft", "type": "bool"},
                    {"name": "secret", "type": "text", "hidden": true}
                ]
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    let schema = json!({
        "type": "object",
        "properties": {"order": {"type": "integer", "minimum": 1}, "state": {"enum": ["new", "paid"]}},
        "required": ["order"],
    });
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "orders".into(),
                schema: Some(schema.clone()),
                description: Some("Order lifecycle events".into()),
                example: Some(json!({"order": 42, "state": "paid"})),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "derived".into(),
                schema: Some(schema.clone()),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    h.topic("raw").await;
    h.svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "post-changes".into(),
                source: Some(RecordSource {
                    collection: "posts".into(),
                    ops: vec!["create".into(), "delete".into()],
                }),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap();
    // An example must satisfy the schema.
    let err = h
        .svc
        .create_topic(
            &admin(),
            NewTopic {
                name: "bad".into(),
                schema: Some(schema.clone()),
                example: Some(json!({"order": "x"})),
                ..NewTopic::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(err.code(), "invalid_example");

    let cat = h.svc.catalog(&admin()).await.unwrap();
    let names: Vec<&str> = cat.topics.iter().map(|t| t.topic.as_str()).collect();
    assert_eq!(names, vec!["derived", "orders", "post-changes", "raw"]);
    let by = |n: &str| cat.topics.iter().find(|t| t.topic == n).unwrap();
    let orders = by("orders");
    assert_eq!(orders.description.as_deref(), Some("Order lifecycle events"));
    assert_eq!(orders.example, json!({"order": 42, "state": "paid"}));
    assert_eq!(orders.example_source, ExampleSource::Topic);
    assert_eq!(orders.data_schema.as_ref(), Some(&schema));
    let derived = by("derived");
    assert_eq!(derived.example_source, ExampleSource::Schema);
    assert!(
        forgebase_queue::schema::validate(&schema, &derived.example).is_ok(),
        "{}",
        derived.example
    );
    assert_eq!(derived.example["order"], 1);
    assert_eq!(by("raw").example_source, ExampleSource::None);
    assert!(by("raw").event_types.is_empty());

    let posts = by("post-changes");
    assert_eq!(posts.source.as_ref().unwrap().collection, "posts");
    let types: Vec<&str> = posts.event_types.iter().map(|e| e.event_type.as_str()).collect();
    assert_eq!(types, vec!["record.created", "record.deleted"]);
    let created = &posts.event_types[0];
    assert_eq!(created.op, "create");
    assert_eq!(created.filter, json!({"forgebase.op": "create"}));
    let record = &created.data_schema["properties"]["record"]["anyOf"][0]["properties"];
    assert_eq!(record["title"]["type"], "string");
    assert_eq!(record["views"]["type"], "integer");
    assert_eq!(record["draft"]["type"], "boolean");
    assert!(record.get("secret").is_none(), "hidden fields are left out");
    assert_eq!(created.example["op"], "create");
    assert_eq!(created.example["collection"], "posts");
    assert_eq!(created.example["record"]["title"], "string");
    assert!(
        forgebase_queue::schema::validate(&created.data_schema, &created.example).is_ok(),
        "{}",
        created.example
    );
    assert_eq!(posts.example_source, ExampleSource::Schema);
    assert!(posts.description.as_deref().unwrap().contains("posts"));

    // The catalog's filter selects the event type.
    h.sub_with(NewSubscription {
        name: "created-only".into(),
        topic: "post-changes".into(),
        filter: Some(created.filter.clone()),
        ..NewSubscription::default()
    })
    .await;
    let rec = h
        .data
        .create_record(&admin(), "posts", json!({"title": "hello", "views": 1}))
        .await
        .unwrap();
    let id = rec["id"].as_str().unwrap().to_owned();
    h.data.delete_record(&admin(), "posts", &id, None).await.unwrap();
    h.svc.run_maintenance().await.unwrap();
    let got = h.pull("created-only", 10).await;
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].message.data["op"], "create");

    // Description and example can be changed.
    let t = h
        .svc
        .update_topic(
            &admin(),
            "raw",
            forgebase_queue::TopicPatch {
                description: Some("Free-form".into()),
                example: Some(json!([1, 2])),
                ..forgebase_queue::TopicPatch::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(t.description.as_deref(), Some("Free-form"));
    assert_eq!(t.example, Some(json!([1, 2])));
    let err = h
        .svc
        .update_topic(
            &admin(),
            "orders",
            forgebase_queue::TopicPatch {
                schema: Some(json!({"type": "string"})),
                ..forgebase_queue::TopicPatch::default()
            },
        )
        .await
        .unwrap_err();
    assert_eq!(
        err.code(),
        "invalid_example",
        "the stored example must fit a new schema"
    );

    // Only readable topics: none for a user without permission.
    let err = h.svc.catalog(&user("u1")).await.unwrap_err();
    assert_eq!(err.code(), "forbidden");
    h.grant("user:users/u1", "engine:queue_read").await;
    assert_eq!(h.svc.catalog(&user("u1")).await.unwrap().topics.len(), 4);
}

// --------------------------------------------------------------------- HTTP

fn app(h: &H) -> Router {
    let layout = DataLayout::new(h.dir.path().join("data"));
    let state = AppState::from_data(h.db.clone(), layout, h.data.clone());
    forgebase_api::router(
        state,
        RouterOptions {
            extensions: vec![forgebase_queue::extension(h.svc.clone())],
            ..RouterOptions::default()
        },
    )
    .layer(axum::middleware::from_fn(as_caller))
}

async fn as_caller(mut req: Request, next: Next) -> Response {
    if let Some(v) = req.headers().get("x-test-as").and_then(|v| v.to_str().ok()) {
        let ctx = if v == "admin" { admin() } else { user(v) };
        req.extensions_mut().insert(ctx);
    }
    next.run(req).await
}

async fn call(app: &Router, who: Option<&str>, method: Method, uri: &str, body: Option<&str>) -> (StatusCode, Json) {
    let mut b = axum::http::Request::builder().method(method).uri(uri);
    if let Some(w) = who {
        b = b.header("x-test-as", w);
    }
    let req = match body {
        Some(j) => b
            .header("content-type", "application/json")
            .body(Body::from(j.to_owned()))
            .unwrap(),
        None => b.body(Body::empty()).unwrap(),
    };
    let res = app.clone().oneshot(req).await.unwrap();
    let status = res.status();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 24).await.unwrap();
    (status, serde_json::from_slice(&bytes).unwrap_or(Json::Null))
}

#[tokio::test]
async fn http_routes_authorise_first_and_accept_empty_bodies() {
    let (h, r) = setup(settings()).await;
    h.sub_with(NewSubscription {
        max_delivery_attempts: Some(1),
        ..hook("hook", "t")
    })
    .await;
    let app = app(&h);
    let a = Some("admin");
    let base = "/api/v1/queues/subscriptions/hook";

    // 401 / 403 before any body is looked at.
    let (s, _) = call(&app, None, Method::POST, &format!("{base}/redrive"), Some("not json")).await;
    assert_eq!(s, StatusCode::UNAUTHORIZED);
    let (s, _) = call(
        &app,
        Some("u9"),
        Method::POST,
        &format!("{base}/test"),
        Some("not json"),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, _) = call(&app, Some("u9"), Method::GET, "/api/v1/queues/catalog", None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Empty bodies.
    let (s, t) = call(&app, a, Method::POST, &format!("{base}/test"), None).await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["test"], true);
    assert_eq!(t["outcome"], "success");
    let (s, rot) = call(&app, a, Method::POST, &format!("{base}/secret/rotate"), None).await;
    assert_eq!(s, StatusCode::OK, "{rot}");
    assert!(rot["push_secret"].as_str().unwrap().starts_with("whsec_"));
    let (s, _) = call(
        &app,
        a,
        Method::POST,
        &format!("{base}/secret/rotate"),
        Some(r#"{"grace_ms": "soon"}"#),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, sub) = call(&app, a, Method::POST, &format!("{base}/secret/expire-previous"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(sub["push_secret_previous_expires_at"].is_null());
    let (s, p) = call(&app, a, Method::POST, &format!("{base}/pause"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(p["paused_at"].is_string());
    let (s, p) = call(&app, a, Method::POST, &format!("{base}/resume"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert!(p["paused_at"].is_null());
    let (s, rt) = call(&app, a, Method::POST, &format!("{base}/retry"), None).await;
    assert_eq!((s, rt["count"].as_i64()), (StatusCode::OK, Some(0)));

    // A dead letter to redrive and inspect.
    r.set_status(500);
    h.publish("t", json!(1)).await;
    h.svc.deliver_push().await.unwrap();
    let (s, page) = call(
        &app,
        a,
        Method::GET,
        &format!("{base}/attempts?outcome=failure&test=false"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{page}");
    let items = page["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["error_class"], "http_status");
    let aid = items[0]["id"].as_i64().unwrap();
    let (s, one) = call(&app, a, Method::GET, &format!("{base}/attempts/{aid}"), None).await;
    assert_eq!((s, one["status"].as_i64()), (StatusCode::OK, Some(500)));
    let (s, _) = call(&app, a, Method::GET, &format!("{base}/attempts?outcome=maybe"), None).await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = call(&app, a, Method::GET, &format!("{base}/attempts/987654"), None).await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, m) = call(&app, a, Method::GET, &format!("{base}/metrics"), None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(m["push"]["attempts"], 1);
    assert_eq!(m["push"]["consecutive_failures"], 1);
    r.set_status(204);
    let (s, out) = call(
        &app,
        a,
        Method::POST,
        &format!("{base}/redrive"),
        Some(r#"{"all": true}"#),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{out}");
    assert_eq!(out["redriven"].as_array().unwrap().len(), 1);
    let (s, _) = call(&app, a, Method::POST, &format!("{base}/redrive"), None).await;
    assert_eq!(s, StatusCode::UNSUPPORTED_MEDIA_TYPE, "redrive needs a body");
    assert_eq!(h.svc.deliver_push().await.unwrap(), 1);
    let (s, cat) = call(&app, a, Method::GET, "/api/v1/queues/catalog", None).await;
    assert_eq!(s, StatusCode::OK);
    assert_eq!(cat["topics"][0]["topic"], "t");

    // Rate limit answers 429.
    for _ in 0..12 {
        let (s, _) = call(&app, a, Method::POST, &format!("{base}/test"), Some("{}")).await;
        if s == StatusCode::TOO_MANY_REQUESTS {
            return;
        }
        assert_eq!(s, StatusCode::OK);
    }
    panic!("test sends were never rate-limited");
}

#[test]
fn the_openapi_document_names_every_tooling_operation() {
    let doc = serde_json::to_value(forgebase_queue::openapi()).unwrap();
    let mut ops = BTreeSet::new();
    for (_, item) in doc["paths"].as_object().unwrap() {
        for (_, op) in item.as_object().unwrap() {
            if let Some(id) = op.get("operationId").and_then(Json::as_str) {
                assert!(id.starts_with("queues_"), "{id}");
                ops.insert(id.to_owned());
            }
        }
    }
    for id in [
        "queues_list_push_attempts",
        "queues_get_push_attempt",
        "queues_test_push",
        "queues_redrive",
        "queues_retry_now",
        "queues_rotate_secret",
        "queues_expire_previous_secret",
        "queues_pause",
        "queues_resume",
        "queues_event_catalog",
    ] {
        assert!(ops.contains(id), "{id} in {ops:?}");
    }
}

#[tokio::test]
async fn webhook_subscriptions_keep_working_through_pull_refusals() {
    // Pull of a push subscription stays refused (ADR-0022 §18).
    let (h, _r) = setup(settings()).await;
    h.sub_with(hook("hook", "t")).await;
    let err = h.svc.pull(&admin(), "hook", PullRequest::default()).await.unwrap_err();
    assert_eq!(err.code(), "push_subscription");
}

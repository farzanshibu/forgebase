//! Live tests for storage, KV, functions, jobs and entitlements.

mod common;

use std::time::Duration;

use bytes::Bytes;
use common::Server;
use forgebase_sdk::generated::{BucketInput, DeployRequest, NewJob, SignRequest, SignedMethod, Visibility};
use forgebase_sdk::{
    ForgeBase, HasQuery, KvValue, ListJobs, ListObjects, MeteredRequest, PutOptions, QuotaRequest, SetOptions,
    SignInOptions,
};
use serde_json::json;

// ---- storage ----

async fn bucket(s: &Server, name: &str, versioning: bool) -> ForgeBase {
    let admin = s.admin().await;
    admin
        .storage()
        .create_bucket(&BucketInput {
            name: name.into(),
            versioning: Some(versioning),
            visibility: Some(Visibility::Private),
            ..BucketInput::default()
        })
        .await
        .expect("create bucket");
    admin
}

#[tokio::test(flavor = "multi_thread")]
async fn storage_objects_ranges_listing_copy_and_delete() {
    let Some(s) = Server::start() else { return };
    let admin = bucket(&s, "files", false).await;
    let st = admin.storage();

    assert!(
        st.list_buckets()
            .await
            .expect("buckets")
            .iter()
            .any(|b| b.name == "files")
    );
    assert_eq!(st.get_bucket("files").await.expect("bucket").name, "files");

    let info = st
        .put_object(
            "files",
            "docs/a b.txt",
            "hello world",
            &PutOptions {
                content_type: Some("text/plain".into()),
                metadata: vec![("owner".into(), "sdk".into())],
                ..PutOptions::default()
            },
        )
        .await
        .expect("put");
    assert_eq!(
        (info.size, info.content_type.as_str(), info.key.as_str()),
        (11, "text/plain", "docs/a b.txt")
    );
    assert!(info.checksum_sha256.is_some());
    assert_eq!(info.metadata.get("owner").and_then(|v| v.as_str()), Some("sdk"));

    assert_eq!(
        st.get_object("files", "docs/a b.txt", None).await.expect("get"),
        Bytes::from_static(b"hello world")
    );
    assert_eq!(
        st.get_object_range("files", "docs/a b.txt", 6, Some(10))
            .await
            .expect("range"),
        Bytes::from_static(b"world")
    );
    assert_eq!(
        st.get_object_range("files", "docs/a b.txt", 6, None)
            .await
            .expect("open range"),
        Bytes::from_static(b"world")
    );

    let mut stream = st.get_object_stream("files", "docs/a b.txt").await.expect("stream");
    assert_eq!(stream.content_length(), Some(11));
    let mut all = Vec::new();
    while let Some(c) = stream.chunk().await.expect("chunk") {
        all.extend_from_slice(&c);
    }
    assert_eq!(all, b"hello world");

    // A checksum mismatch is refused.
    let bad = st
        .put_object(
            "files",
            "bad.txt",
            "x",
            &PutOptions {
                checksum_sha256: Some("0".repeat(64)),
                ..PutOptions::default()
            },
        )
        .await
        .expect_err("checksum");
    assert_eq!(bad.status(), Some(422), "{bad}");

    st.put_object("files", "docs/b.txt", "b", &PutOptions::default())
        .await
        .expect("put b");
    st.put_object("files", "top.txt", "t", &PutOptions::default())
        .await
        .expect("put top");
    let page = st
        .list_objects(
            "files",
            &ListObjects {
                prefix: Some("docs/".into()),
                ..ListObjects::default()
            },
        )
        .await
        .expect("list");
    assert_eq!(page.objects.len(), 2);
    let grouped = st
        .list_objects(
            "files",
            &ListObjects {
                delimiter: Some("/".into()),
                ..ListObjects::default()
            },
        )
        .await
        .expect("grouped");
    assert_eq!(grouped.prefixes, ["docs/"]);
    assert_eq!(
        grouped.objects.iter().map(|o| o.key.as_str()).collect::<Vec<_>>(),
        ["top.txt"]
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn storage_copy_move_delete_and_not_found() {
    let Some(s) = Server::start() else { return };
    let admin = bucket(&s, "files", false).await;
    let st = admin.storage();
    st.put_object("files", "a.txt", "abc", &PutOptions::default())
        .await
        .expect("put");
    st.copy_object("files", "a.txt", "copy.txt", None).await.expect("copy");
    st.move_object("files", "copy.txt", "moved.txt", None)
        .await
        .expect("move");
    assert_eq!(
        st.get_object("files", "moved.txt", None).await.expect("moved"),
        Bytes::from_static(b"abc")
    );
    assert!(
        st.get_object("files", "copy.txt", None)
            .await
            .expect_err("moved away")
            .is_not_found()
    );
    assert!(!st.delete_object("files", "a.txt").await.expect("delete").delete_marker);
    assert!(
        st.get_object("files", "a.txt", None)
            .await
            .expect_err("deleted")
            .is_not_found()
    );
    st.delete_object("files", "moved.txt").await.expect("delete");
    st.delete_bucket("files").await.expect("delete bucket");
}

#[tokio::test(flavor = "multi_thread")]
async fn storage_signed_urls_work_without_credentials() {
    let Some(s) = Server::start() else { return };
    let admin = bucket(&s, "files", false).await;
    let st = admin.storage();
    st.put_object("files", "secret.txt", "top secret", &PutOptions::default())
        .await
        .expect("put");

    let get = st
        .sign(
            "files",
            &SignRequest {
                key: "secret.txt".into(),
                method: Some(SignedMethod::GET),
                expires_in: Some(60),
                ..SignRequest::default()
            },
        )
        .await
        .expect("sign get");
    let anon = ForgeBase::new(&s.base).expect("client");
    assert_eq!(
        anon.storage().download_signed(&get).await.expect("signed download"),
        Bytes::from_static(b"top secret")
    );
    assert!(
        anon.storage()
            .get_object("files", "secret.txt", None)
            .await
            .expect_err("no credentials")
            .is_unauthenticated()
    );

    let put = st
        .sign(
            "files",
            &SignRequest {
                key: "up.bin".into(),
                method: Some(SignedMethod::PUT),
                expires_in: Some(60),
                ..SignRequest::default()
            },
        )
        .await
        .expect("sign put");
    let info = anon
        .storage()
        .upload_signed(&put, vec![1u8, 2, 3], "application/octet-stream")
        .await
        .expect("signed upload");
    assert_eq!(info.size, 3);
    assert_eq!(
        st.get_object("files", "up.bin", None)
            .await
            .expect("read back")
            .as_ref(),
        [1, 2, 3]
    );

    let mut tampered = get.clone();
    tampered.url.push('x');
    assert!(
        anon.storage().download_signed(&tampered).await.is_err(),
        "a tampered signature is refused"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn storage_multipart_and_resumable_uploads() {
    let Some(s) = Server::start() else { return };
    let admin = bucket(&s, "big", true).await;
    let st = admin.storage();

    let data: Vec<u8> = (0..300_000u32).map(|i| (i % 251) as u8).collect();
    let bytes = Bytes::from(data.clone());
    let info = st
        .upload_multipart("big", "multi.bin", &bytes, 100_000, Some("application/octet-stream"))
        .await
        .expect("multipart");
    assert_eq!(info.size, 300_000);
    assert_eq!(
        st.get_object("big", "multi.bin", None).await.expect("read").as_ref(),
        data.as_slice()
    );

    // Resumable: append chunks at their offsets; ask the server where to continue.
    let up = st
        .create_upload(
            "big",
            &forgebase_sdk::generated::UploadInput {
                key: "resumable.bin".into(),
                size: Some(300_000),
                kind: Some("resumable".into()),
                ..Default::default()
            },
        )
        .await
        .expect("create upload");
    let r1 = st
        .append_chunk(&up.id, 0, bytes.slice(0..100_000))
        .await
        .expect("chunk 1");
    assert_eq!(r1["received_bytes"], 100_000, "{r1}");
    let resumed = st.get_upload(&up.id).await.expect("state");
    assert_eq!(resumed.received_bytes, 100_000);
    let wrong = st
        .append_chunk(&up.id, 5, bytes.slice(0..10))
        .await
        .expect_err("wrong offset");
    assert!(wrong.is_conflict(), "{wrong}");
    st.append_chunk(&up.id, 100_000, bytes.slice(100_000..200_000))
        .await
        .expect("chunk 2");
    let done = st
        .append_chunk(&up.id, 200_000, bytes.slice(200_000..300_000))
        .await
        .expect("chunk 3");
    assert_eq!(done["size"], 300_000, "the last chunk finishes the object: {done}");
    assert_eq!(
        st.get_object("big", "resumable.bin", None)
            .await
            .expect("read")
            .as_ref(),
        data.as_slice()
    );

    // Versioned bucket: overwriting keeps history.
    st.put_object("big", "v.txt", "one", &PutOptions::default())
        .await
        .expect("v1");
    st.put_object("big", "v.txt", "two", &PutOptions::default())
        .await
        .expect("v2");
    let versions = st.list_versions("big", "v.txt").await.expect("versions");
    assert_eq!(versions.len(), 2);
    let old = &versions[1];
    assert_eq!(
        st.get_object("big", "v.txt", Some(&old.version_id))
            .await
            .expect("old version")
            .as_ref(),
        b"one"
    );
    let marker = st.delete_object("big", "v.txt").await.expect("delete adds a marker");
    assert!(marker.delete_marker);
    assert!(
        st.get_object("big", "v.txt", None)
            .await
            .expect_err("hidden by the marker")
            .is_not_found()
    );
    assert_eq!(
        st.get_object("big", "v.txt", Some(&old.version_id))
            .await
            .expect("history survives")
            .as_ref(),
        b"one"
    );
    st.delete_object_version("big", "v.txt", &marker.version_id)
        .await
        .expect("remove the marker");
    assert_eq!(
        st.get_object("big", "v.txt", None)
            .await
            .expect("visible again")
            .as_ref(),
        b"two"
    );
}

// ---- KV ----

#[tokio::test(flavor = "multi_thread")]
async fn kv_strings_ttl_counters_and_scan() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let kv = admin.kv("cache");

    kv.set("greeting", "hello", &SetOptions::default()).await.expect("set");
    assert_eq!(kv.get_string("greeting").await.expect("get"), Some("hello".into()));
    assert_eq!(kv.get_string("missing").await.expect("get missing"), None);
    assert!(kv.get("missing").await.expect_err("404").is_not_found());

    // nx refuses to overwrite.
    kv.set(
        "greeting",
        "other",
        &SetOptions {
            nx: true,
            ..SetOptions::default()
        },
    )
    .await
    .expect("nx");
    assert_eq!(kv.get_string("greeting").await.expect("get"), Some("hello".into()));

    kv.set("bin", vec![0u8, 1, 255], &SetOptions::default())
        .await
        .expect("set bytes");
    assert_eq!(
        kv.get_value("bin").await.expect("get"),
        Some(KvValue::Bytes(vec![0, 1, 255]))
    );

    kv.set("n", "10", &SetOptions::default()).await.expect("set n");
    let after = kv.incr("n", Some(5)).await.expect("incr");
    assert!(after.to_string().contains("15"), "{after}");
    assert_eq!(kv.get_string("n").await.expect("get"), Some("15".into()));

    kv.set(
        "short",
        "x",
        &SetOptions {
            ttl_ms: Some(60_000),
            ..SetOptions::default()
        },
    )
    .await
    .expect("ttl");
    let k = kv.get("short").await.expect("get short");
    assert!(k.ttl_ms.is_some_and(|t| t > 0 && t <= 60_000), "{k:?}");
    kv.persist("short").await.expect("persist");
    assert!(kv.get("short").await.expect("get").ttl_ms.is_none());
    kv.expire("short", 30_000).await.expect("expire");
    assert!(kv.get("short").await.expect("get").ttl_ms.is_some());

    let (keys, _) = kv.scan("0", Some("gr*"), Some(100)).await.expect("scan");
    assert_eq!(keys, ["greeting"]);
    kv.mset(&[("m1", "a".into()), ("m2", "b".into())]).await.expect("mset");
    let m = kv.mget(&["m1", "m2", "nope"]).await.expect("mget");
    assert!(m.to_string().contains('a') && m.to_string().contains('b'), "{m}");

    kv.delete("greeting").await.expect("delete");
    assert_eq!(kv.get_string("greeting").await.expect("get"), None);
    assert!(kv.info().await.expect("info").keys >= 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn kv_collections_and_namespace_isolation() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let kv = admin.kv("data");

    kv.hash_set("user:1", json!({"name": "ada", "age": "36"}))
        .await
        .expect("hset");
    kv.hash_incr("user:1", "visits", Some(2)).await.expect("hincr");
    let h = kv.get("user:1").await.expect("hash");
    assert_eq!(h.r#type, "hash");
    assert_eq!(h.value["name"], "ada");
    kv.hash_delete("user:1", &["age"]).await.expect("hdel");

    kv.list_push("queue", false, &["a".into(), "b".into(), "c".into()])
        .await
        .expect("rpush");
    let popped = kv.list_pop("queue", true, Some(2)).await.expect("lpop");
    assert!(
        popped.to_string().contains('a') && popped.to_string().contains('b'),
        "{popped}"
    );

    kv.set_add("tags", &["x".into(), "y".into()]).await.expect("sadd");
    assert_eq!(kv.get("tags").await.expect("set").r#type, "set");
    kv.set_remove("tags", &["x".into()]).await.expect("srem");
    kv.zset_add("board", &[("ann".into(), 10.0), ("bob".into(), 20.0)])
        .await
        .expect("zadd");
    assert_eq!(kv.get("board").await.expect("zset").r#type, "zset");
    kv.zset_remove("board", &["ann".into()]).await.expect("zrem");
    kv.stream_add("events", json!({"kind": "login"}), None)
        .await
        .expect("xadd");
    let range = kv.stream_range("events", None, None, None).await.expect("xrange");
    assert!(range.to_string().contains("login"), "{range}");

    let ping = kv.exec(&["INCRBY", "hits", "3"]).await.expect("exec");
    assert!(ping.to_string().contains('3'), "{ping}");
    assert!(kv.publish("chan", "hi").await.is_ok());

    // Namespaces do not see each other's keys.
    let other = admin.kv("other");
    assert!(other.get("hits").await.expect_err("isolated").is_not_found());
    kv.flush().await.expect("flush");
    assert!(kv.get("hits").await.expect_err("flushed").is_not_found());
}

// ---- functions ----

/// `(module (memory (export "memory") 1) (func (export "handle") (result i32) i32.const 0))`
const HANDLE_WASM: &[u8] = &[
    0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x05, 0x01, 0x60, 0x00, 0x01, 0x7f, 0x03, 0x02, 0x01, 0x00,
    0x05, 0x03, 0x01, 0x00, 0x01, 0x07, 0x13, 0x02, 0x06, b'm', b'e', b'm', b'o', b'r', b'y', 0x02, 0x00, 0x06, b'h',
    b'a', b'n', b'd', b'l', b'e', 0x00, 0x00, 0x0a, 0x06, 0x01, 0x04, 0x00, 0x41, 0x00, 0x0b,
];

#[tokio::test(flavor = "multi_thread")]
async fn functions_deploy_invoke_logs_versions_and_secrets() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let f = admin.functions();

    let req = DeployRequest {
        name: "hello".into(),
        description: Some("says hi".into()),
        ..DeployRequest::default()
    };
    let deployed = f.deploy(req.clone(), HANDLE_WASM).await.expect("deploy v1");
    assert_eq!(
        (deployed.function.name.as_str(), deployed.function.active_version),
        ("hello", 1)
    );
    let v2 = f
        .deploy(
            DeployRequest {
                activate: Some(false),
                ..req
            },
            HANDLE_WASM,
        )
        .await
        .expect("deploy v2");
    assert_eq!((v2.function.active_version, v2.function.latest_version), (1, 2));

    assert_eq!(f.list().await.expect("list").len(), 1);
    assert_eq!(f.get("hello").await.expect("get").description, "says hi");

    let out = f.invoke("hello", &json!({"x": 1})).await.expect("invoke");
    assert!(out.is_null() || out.is_object() || out.is_string(), "{out}");
    let logs = f.logs("hello", Some(10)).await.expect("logs");
    assert_eq!(logs.len(), 1);
    assert_eq!((logs[0].function.as_str(), logs[0].version), ("hello", 1));

    let rolled = f.rollback("hello", Some(2)).await.expect("rollback to 2");
    assert_eq!(rolled.active_version, 2);

    f.put_secret("hello", "API_KEY", "s3cr3t").await.expect("secret");
    assert_eq!(
        f.secrets("hello")
            .await
            .expect("secrets")
            .iter()
            .map(|x| x.name.as_str())
            .collect::<Vec<_>>(),
        ["API_KEY"]
    );
    f.delete_secret("hello", "API_KEY").await.expect("delete secret");

    // A module the server rejects reports the server's code.
    let bad = f
        .deploy(
            DeployRequest {
                name: "broken".into(),
                ..DeployRequest::default()
            },
            b"\0asm\x01\0\0\0",
        )
        .await
        .expect_err("invalid module");
    assert_eq!(bad.code(), "invalid_module", "{bad}");
    assert!(f.get("nothing").await.expect_err("unknown").is_not_found());
    f.delete("hello").await.expect("delete");
}

// ---- jobs ----

#[tokio::test(flavor = "multi_thread")]
async fn jobs_enqueue_status_worker_protocol_and_wait() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    let jobs = admin.jobs();

    let job = jobs
        .enqueue(&NewJob {
            kind: "email".into(),
            payload: Some(json!({"to": "a@b.co"})),
            idempotency_key: Some("once".into()),
            queue: Some("default".into()),
            ..NewJob::default()
        })
        .await
        .expect("enqueue");
    assert_eq!((job.kind.as_str(), job.attempts), ("email", 0));
    let again = jobs
        .enqueue(&NewJob {
            kind: "email".into(),
            idempotency_key: Some("once".into()),
            ..NewJob::default()
        })
        .await
        .expect("idempotent enqueue");
    assert_eq!(again.id, job.id, "the same idempotency key returns the same job");

    assert_eq!(jobs.get(&job.id).await.expect("get").id, job.id);
    let (listed, total) = jobs
        .list(&ListJobs {
            kind: Some("email".into()),
            ..ListJobs::default()
        })
        .await
        .expect("list");
    assert_eq!((listed.len(), total), (1, 1));
    assert!(
        jobs.stats()
            .await
            .expect("stats")
            .queues
            .iter()
            .any(|q| q.queue == "default")
    );

    // An external worker claims, heartbeats and completes it.
    let claim = jobs.claim("worker-1", &["default"], &[], Some(1)).await.expect("claim");
    assert_eq!(claim.jobs.len(), 1);
    let lease = &claim.jobs[0].lease_id;
    let hb = jobs.heartbeat(&job.id, lease).await.expect("heartbeat");
    assert!(!hb.cancel_requested);
    let waiter = {
        let jobs = jobs.clone();
        let id = job.id.clone();
        tokio::spawn(async move { jobs.wait(&id, Duration::from_millis(50), Duration::from_secs(10)).await })
    };
    tokio::time::sleep(Duration::from_millis(150)).await;
    jobs.complete(&job.id, lease, Some(json!({"sent": true})))
        .await
        .expect("complete");
    let done = waiter.await.expect("join").expect("wait");
    assert_eq!(done.result, Some(json!({"sent": true})));
    assert!(!jobs.events(&job.id).await.expect("events").is_empty());

    // A failing, non-retryable job dies; a retry re-queues it; cancel stops a queued one.
    let j2 = jobs
        .enqueue(&NewJob {
            kind: "flaky".into(),
            max_attempts: Some(1),
            ..NewJob::default()
        })
        .await
        .expect("enqueue");
    let c2 = jobs.claim("worker-1", &[], &["flaky"], None).await.expect("claim");
    jobs.fail(&j2.id, &c2.jobs[0].lease_id, "boom", false)
        .await
        .expect("fail");
    assert_eq!(jobs.get(&j2.id).await.expect("get").last_error.as_deref(), Some("boom"));
    let revived = jobs.retry(&j2.id).await.expect("retry");
    assert_eq!(format!("{:?}", revived.status), "Queued");
    let cancelled = jobs.cancel(&j2.id).await.expect("cancel");
    assert_eq!(format!("{:?}", cancelled.status), "Cancelled");
    let timeout = jobs
        .wait(&job.id, Duration::from_millis(10), Duration::from_millis(100))
        .await
        .expect("already finished");
    assert_eq!(timeout.id, job.id);
}

// ---- entitlements ----

#[tokio::test(flavor = "multi_thread")]
async fn entitlements_has_check_consume_reserve_and_lease() {
    let Some(s) = Server::start() else { return };
    let admin = s.admin().await;
    admin
        .request(
            "PUT",
            "/api/v1/features/ai_calls",
            Some(json!({"kind": "metered", "unit": "call"})),
        )
        .await
        .expect("feature");
    admin
        .request("PUT", "/api/v1/features/beta_ui", Some(json!({"kind": "boolean"})))
        .await
        .expect("feature");
    admin
        .request(
            "PUT",
            "/api/v1/plans/free",
            Some(json!({"is_default": true, "features": [{"feature": "ai_calls", "limit": 3, "period": "month"}]})),
        )
        .await
        .expect("plan");

    let user = ForgeBase::new(&s.base).expect("client");
    user.auth()
        .sign_up("reader@example.com", "another long passphrase 7", None, None)
        .await
        .expect("sign up");
    let session = user
        .auth()
        .sign_in(
            "reader@example.com",
            "another long passphrase 7",
            &SignInOptions::default(),
        )
        .await
        .expect("sign in")
        .into_session()
        .expect("session");
    let subject = format!(
        "user:users/{}",
        session.user.as_ref().and_then(|u| u["id"].as_str()).expect("user id")
    );
    let e = user.entitlements();

    assert!(e.has(HasQuery::Feature("ai_calls".into()), None).await.expect("has"));
    assert!(!e.has(HasQuery::Feature("beta_ui".into()), None).await.expect("has"));
    assert!(e.has(HasQuery::Plan("free".into()), None).await.expect("has plan"));
    let ent = e.get(None).await.expect("entitlements");
    assert!(ent.features["ai_calls"].enabled);

    let check = e.check_quota(&QuotaRequest::new("ai_calls")).await.expect("check");
    assert!(check.allowed);
    assert_eq!(check.limit, Some(3));

    // Recording usage needs `usage:write`: the admin meters the user's subject.
    let ad = admin.entitlements();
    let meter = |key: &str| MeteredRequest {
        quota: QuotaRequest {
            subject: Some(subject.clone()),
            ..QuotaRequest::new("ai_calls")
        },
        idempotency_key: key.into(),
        ttl_secs: None,
    };
    let first = ad.consume(&meter("k1")).await.expect("consume");
    let replay = ad.consume(&meter("k1")).await.expect("replay");
    assert!(!first.replayed && replay.replayed);
    assert_eq!(first.event_id, replay.event_id);
    ad.consume(&meter("k2")).await.expect("consume 2");
    ad.consume(&meter("k3")).await.expect("consume 3");
    assert!(
        !e.check_quota(&QuotaRequest::new("ai_calls"))
            .await
            .expect("check")
            .allowed
    );
    let over = ad.consume(&meter("k4")).await.expect_err("hard limit");
    assert!(over.is_quota_exceeded(), "{over}");
    let used = ad
        .usage(Some(&subject), None)
        .await
        .expect("usage")
        .into_iter()
        .find(|u| u.feature == "ai_calls")
        .expect("ai_calls usage");
    assert_eq!(used.used, 3);

    // Raise the limit, then reserve and release / commit.
    admin
        .request(
            "PUT",
            "/api/v1/plans/free",
            Some(json!({"is_default": true, "features": [{"feature": "ai_calls", "limit": 10, "period": "month"}]})),
        )
        .await
        .expect("plan");
    let held = ad
        .reserve(&MeteredRequest {
            quota: QuotaRequest {
                units: Some(2),
                ..meter("r1").quota
            },
            ..meter("r1")
        })
        .await
        .expect("reserve");
    assert_eq!(format!("{:?}", held.status), "Held");
    assert_eq!(
        format!("{:?}", ad.release_reservation(&held.id).await.expect("release").status),
        "Released"
    );
    let out = ad
        .with_reservation(&meter("r2"), |r| async move { Ok(r.id) })
        .await
        .expect("with_reservation");
    assert!(!out.is_empty());
    let failed: forgebase_sdk::Result<()> = ad
        .with_reservation(&meter("r3"), |_| async {
            Err(forgebase_sdk::Error::Config("boom".into()))
        })
        .await;
    assert!(failed.is_err());
}

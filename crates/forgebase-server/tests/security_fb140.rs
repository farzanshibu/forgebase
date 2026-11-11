//! FB-140 (rls-coverage G1, G11, G14, G15): object storage RLS over HTTP
//! against the real server. Each test failed before FB-140: an ACL entry for
//! a key prefix granted the whole bucket, bucket names were global across
//! tenants, bucket resource rules were never consulted (so object fields
//! like `created_by` were never checked), the URL signing key sat in plain
//! text in the database, and signed URLs could not be revoked.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use security_common::{Env, Resp, bearer, send};
use serde_json::{Value as Json, json};

/// A user's organization with one project; returns the production tenant.
async fn tenant_of(env: &Env, token: &str, slug: &str) -> String {
    let r = env
        .as_(
            token,
            "POST",
            "/api/v1/orgs",
            &[],
            Some(json!({"slug": slug, "name": slug})),
        )
        .await;
    assert_eq!(r.status, 201, "org: {}", r.text());
    let id = r.json()["id"].as_str().unwrap().to_owned();
    let p = env
        .as_(
            token,
            "POST",
            &format!("/api/v1/orgs/{id}/projects"),
            &[("X-ForgeBase-Org", &id)],
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

fn headers<'a>(auth: &'a str, tenant: Option<&'a str>) -> Vec<(&'a str, &'a str)> {
    let mut h = vec![("Authorization", auth)];
    if let Some(t) = tenant {
        h.push(("X-ForgeBase-Tenant", t));
    }
    h
}

/// JSON request as `token`, optionally in `tenant`.
async fn call(env: &Env, token: &str, tenant: Option<&str>, method: &str, path: &str, body: Option<Json>) -> Resp {
    let t: Vec<(&str, &str)> = tenant.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    env.as_(token, method, path, &t, body).await
}

async fn put_obj(env: &Env, token: &str, tenant: Option<&str>, bucket: &str, key: &str, body: &[u8]) -> Resp {
    let auth = bearer(token);
    let mut h = headers(&auth, tenant);
    h.push(("Content-Type", "text/plain"));
    send(
        env.addr,
        "PUT",
        &format!("/api/v1/storage/buckets/{bucket}/objects/{key}"),
        &h,
        body,
    )
    .await
}

async fn get_obj(env: &Env, token: &str, tenant: Option<&str>, bucket: &str, key: &str) -> Resp {
    let auth = bearer(token);
    send(
        env.addr,
        "GET",
        &format!("/api/v1/storage/buckets/{bucket}/objects/{key}"),
        &headers(&auth, tenant),
        b"",
    )
    .await
}

async fn bucket(env: &Env, token: &str, tenant: Option<&str>, name: &str) -> Resp {
    call(
        env,
        token,
        tenant,
        "POST",
        "/api/v1/storage/buckets",
        Some(json!({"name": name})),
    )
    .await
}

fn keys(r: &Resp) -> Vec<String> {
    r.json()["objects"]
        .as_array()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|o| o["key"].as_str().unwrap().to_owned())
        .collect()
}

fn id_of(pk: &str) -> String {
    pk.rsplit('/').next().unwrap().to_owned()
}

// ---------------------------------------------------------------------------
// G1: an ACL entry for a prefix is not a grant on the bucket
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g1_a_prefix_grant_is_not_a_bucket_grant() {
    let env = Env::new().await;
    let admin = env.admin("fb140-admin@example.com").await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    assert_eq!(bucket(&env, &admin, None, "shared").await.status, 201);
    let r = call(
        &env,
        &admin,
        None,
        "PUT",
        "/api/v1/storage/buckets/shared/acl",
        Some(json!({"entries": [{"prefix": "users/alice/", "grantee": alice_pk, "permission": "admin"}]})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // The grant covers its keys.
    let r = put_obj(&env, &alice, None, "shared", "users/alice/a.txt", b"mine").await;
    assert!(r.status < 300, "{}", r.text());
    assert_eq!(
        get_obj(&env, &alice, None, "shared", "users/alice/a.txt").await.text(),
        "mine"
    );
    assert_eq!(
        put_obj(&env, &alice, None, "shared", "users/bob/x", b"x").await.status,
        403
    );

    // ... and never the bucket itself: every bucket-level operation.
    let path = "/api/v1/storage/buckets/shared";
    let attempts: Vec<(&str, String, Option<Json>)> = vec![
        ("PATCH", path.to_owned(), Some(json!({"visibility": "public"}))),
        ("GET", format!("{path}/acl"), None),
        (
            "PUT",
            format!("{path}/acl"),
            Some(json!({"entries": [{"prefix": "", "grantee": alice_pk, "permission": "admin"}]})),
        ),
        ("GET", format!("{path}/usage"), None),
        ("GET", path.to_owned(), None),
        ("DELETE", format!("{path}?force=true"), None),
    ];
    for (method, p, body) in attempts {
        let r = call(&env, &alice, None, method, &p, body).await;
        assert_eq!(r.status, 403, "{method} {p}: {}", r.text());
    }
    let r = call(&env, &alice, None, "GET", "/api/v1/storage/buckets", None).await;
    assert_eq!(r.json()["items"], json!([]), "{}", r.text());

    // Nothing changed.
    let r = call(&env, &admin, None, "GET", path, None).await;
    assert_eq!(r.json()["visibility"], "private", "{}", r.text());
    let r = call(&env, &admin, None, "GET", &format!("{path}/acl"), None).await;
    assert_eq!(r.json()["entries"].as_array().unwrap().len(), 1, "{}", r.text());
    assert_eq!(
        get_obj(&env, &admin, None, "shared", "users/alice/a.txt").await.status,
        200
    );
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G14: tenant-scoped buckets
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g14_bucket_names_are_unique_per_tenant_and_tenants_are_isolated() {
    let env = Env::new().await;
    let admin = env.admin("fb140-admin@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    let (bob, _) = env.user("bob@example.com").await;
    let ta = tenant_of(&env, &alice, "alpha").await;
    let tb = tenant_of(&env, &bob, "bravo").await;

    // The same name in two tenants.
    let r = bucket(&env, &alice, Some(&ta), "files").await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["tenant"], json!(ta));
    let r = bucket(&env, &bob, Some(&tb), "files").await;
    assert_eq!(r.status, 201, "bob's own files bucket: {}", r.text());
    assert_eq!(r.json()["tenant"], json!(tb));
    // An instance-wide name is not taken over by a tenant.
    assert_eq!(bucket(&env, &admin, None, "assets").await.status, 201);
    let r = bucket(&env, &alice, Some(&ta), "assets").await;
    assert_eq!(r.status, 409, "{}", r.text());

    let r = put_obj(&env, &alice, Some(&ta), "files", "doc.txt", b"alpha-secret").await;
    assert!(r.status < 300, "{}", r.text());
    assert_eq!(r.json()["bucket"], "files", "{}", r.text());
    assert_eq!(
        get_obj(&env, &alice, Some(&ta), "files", "doc.txt").await.text(),
        "alpha-secret"
    );

    // Bob's "files" is another bucket.
    assert_eq!(get_obj(&env, &bob, Some(&tb), "files", "doc.txt").await.status, 404);
    let r = call(
        &env,
        &bob,
        Some(&tb),
        "GET",
        "/api/v1/storage/buckets/files/objects",
        None,
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(keys(&r).is_empty(), "{}", r.text());
    // The stored name of alice's bucket is not a name anyone can use.
    let stored = format!("{ta}%2Ffiles");
    for tenant in [Some(tb.as_str()), None] {
        let r = get_obj(&env, &bob, tenant, &stored, "doc.txt").await;
        assert_eq!(r.status, 404, "{tenant:?}: {}", r.text());
    }
    assert_eq!(get_obj(&env, &admin, None, &stored, "doc.txt").await.status, 404);
    // Without a tenant there is no "files" bucket at all.
    let r = call(&env, &bob, None, "GET", "/api/v1/storage/buckets/files", None).await;
    assert_eq!(r.status, 404, "{}", r.text());

    // Listings: the tenant's buckets and the instance-wide ones.
    let r = call(&env, &alice, Some(&ta), "GET", "/api/v1/storage/buckets", None).await;
    let items = r.json()["items"].as_array().unwrap().clone();
    assert!(
        items.iter().any(|b| b["name"] == "files" && b["tenant"] == json!(ta)),
        "{}",
        r.text()
    );
    assert!(!items.iter().any(|b| b["tenant"] == json!(tb)), "{}", r.text());

    // Statistics are the tenant's.
    let r = call(&env, &alice, Some(&ta), "GET", "/api/v1/storage/stats", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["scope"], "tenant", "{}", r.text());
    assert_eq!(r.json()["buckets"], 1, "{}", r.text());
    assert_eq!(r.json()["objects"], 1, "{}", r.text());

    // A signed URL names the tenant; changing it breaks the signature.
    let r = call(
        &env,
        &alice,
        Some(&ta),
        "POST",
        "/api/v1/storage/buckets/files/sign",
        Some(json!({"key": "doc.txt", "expires_in": 60})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let url = r.json()["url"].as_str().unwrap().to_owned();
    assert!(url.contains(&format!("t={ta}")), "{url}");
    let r = env.req("GET", &url, &[], None).await;
    assert_eq!(r.text(), "alpha-secret");
    let forged = url.replace(&format!("t={ta}"), &format!("t={tb}"));
    assert_eq!(env.req("GET", &forged, &[], None).await.status, 403);

    // Upload sessions stay in their tenant.
    let r = call(
        &env,
        &alice,
        Some(&ta),
        "POST",
        "/api/v1/storage/buckets/files/uploads",
        Some(json!({"key": "big.bin", "kind": "resumable"})),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    assert_eq!(r.json()["bucket"], "files", "{}", r.text());
    let up = r.json()["id"].as_str().unwrap().to_owned();
    let r = call(
        &env,
        &alice,
        Some(&tb),
        "GET",
        &format!("/api/v1/storage/uploads/{up}"),
        None,
    )
    .await;
    assert!(r.status == 403 || r.status == 404, "{}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G14: object rules over object fields
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g14_bucket_rules_filter_objects_by_owner_folder_and_size() {
    let env = Env::new().await;
    let admin = env.admin("fb140-admin@example.com").await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    let (bob, bob_pk) = env.user("bob@example.com").await;
    let (a, b) = (id_of(&alice_pk), id_of(&bob_pk));
    assert_eq!(bucket(&env, &admin, None, "home").await.status, 201);
    let mine = "owner = @request.auth.id && owner_collection = @request.auth.collection";
    let r = call(
        &env,
        &admin,
        None,
        "PUT",
        "/api/v1/policies/resources/bucket/home",
        Some(json!({"rules": {
            "read": mine,
            "create": "folders.0 = @request.auth.id && size <= 64",
            "update": format!("{mine} && size <= 64"),
            "delete": mine,
        }})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());

    // create: own folder, small files.
    let r = put_obj(&env, &alice, None, "home", &format!("{a}/a.txt"), b"hello").await;
    assert!(r.status < 300, "alice in her folder: {}", r.text());
    assert_eq!(r.json()["created_by"], json!(alice_pk));
    let r = put_obj(&env, &bob, None, "home", &format!("{a}/b.txt"), b"x").await;
    assert_eq!(r.status, 403, "bob in alice's folder: {}", r.text());
    assert!(
        put_obj(&env, &bob, None, "home", &format!("{b}/b.txt"), b"bob")
            .await
            .status
            < 300
    );
    let r = put_obj(&env, &alice, None, "home", &format!("{a}/big.txt"), &[b'x'; 100]).await;
    assert_eq!(r.status, 403, "size rule: {}", r.text());

    // view / list: only one's own objects; folder names do not leak.
    assert_eq!(
        get_obj(&env, &alice, None, "home", &format!("{a}/a.txt")).await.text(),
        "hello"
    );
    assert_eq!(
        get_obj(&env, &bob, None, "home", &format!("{a}/a.txt")).await.status,
        404
    );
    let r = call(&env, &bob, None, "GET", "/api/v1/storage/buckets/home/objects", None).await;
    assert_eq!(keys(&r), vec![format!("{b}/b.txt")], "{}", r.text());
    let r = call(
        &env,
        &bob,
        None,
        "GET",
        "/api/v1/storage/buckets/home/objects?delimiter=/",
        None,
    )
    .await;
    assert_eq!(r.json()["prefixes"], json!([format!("{b}/")]), "{}", r.text());
    let r = call(
        &env,
        &bob,
        None,
        "GET",
        &format!("/api/v1/storage/buckets/home/versions/{a}/a.txt"),
        None,
    )
    .await;
    assert_eq!(r.json()["items"], json!([]), "{}", r.text());

    // update / delete / copy of someone else's object.
    let r = put_obj(&env, &bob, None, "home", &format!("{a}/a.txt"), b"evil").await;
    assert_eq!(r.status, 403, "overwrite: {}", r.text());
    let r = call(
        &env,
        &bob,
        None,
        "DELETE",
        &format!("/api/v1/storage/buckets/home/objects/{a}/a.txt"),
        None,
    )
    .await;
    assert_eq!(r.status, 404, "delete: {}", r.text());
    let r = call(
        &env,
        &bob,
        None,
        "POST",
        "/api/v1/storage/buckets/home/copy",
        Some(json!({"key": format!("{a}/a.txt"), "dest_key": format!("{b}/stolen.txt")})),
    )
    .await;
    assert_eq!(r.status, 404, "copy: {}", r.text());
    assert_eq!(
        get_obj(&env, &alice, None, "home", &format!("{a}/a.txt")).await.text(),
        "hello"
    );

    // Signing follows the same rules.
    let sign = |token: String, body: Json| {
        let env = &env;
        async move {
            call(
                env,
                &token,
                None,
                "POST",
                "/api/v1/storage/buckets/home/sign",
                Some(body),
            )
            .await
        }
    };
    let r = sign(bob.clone(), json!({"key": format!("{a}/a.txt")})).await;
    assert_eq!(r.status, 404, "bob signs alice's object: {}", r.text());
    let r = sign(alice.clone(), json!({"key": format!("{a}/a.txt")})).await;
    assert_eq!(r.status, 200, "{}", r.text());
    // An upload URL cannot promise the size the create rule needs.
    let r = sign(bob.clone(), json!({"key": format!("{b}/up.txt"), "method": "PUT"})).await;
    assert_eq!(r.status, 403, "{}", r.text());

    // Alice deletes her own object.
    let r = call(
        &env,
        &alice,
        None,
        "DELETE",
        &format!("/api/v1/storage/buckets/home/objects/{a}/a.txt"),
        None,
    )
    .await;
    assert!(r.status < 300, "{}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G11: the URL signing key is sealed; G15: rotating it revokes signed URLs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g11_the_signing_key_is_not_readable_through_sql() {
    let env = Env::new().await;
    let admin = env.admin("fb140-admin@example.com").await;
    let r = call(
        &env,
        &admin,
        None,
        "POST",
        "/api/v1/sql",
        Some(json!({"sql": "SELECT name, value FROM _forgebase_storage_settings"})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    let rows = r.json()["rows"].as_array().unwrap().clone();
    assert!(
        !rows.iter().any(|row| row[0] == "signing_key"),
        "a plain-text key is readable: {}",
        r.text()
    );
    let sealed = rows
        .iter()
        .find(|row| row[0] == "signing_key_sealed")
        .expect("sealed key row");
    // 24-byte nonce + 32-byte key + 16-byte tag, hex.
    assert_eq!(sealed[1].as_str().unwrap().len(), 144, "{}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn g15_rotating_the_signing_key_revokes_every_signed_url() {
    let env = Env::new().await;
    let admin = env.admin("fb140-admin@example.com").await;
    let (alice, _) = env.user("alice@example.com").await;
    assert_eq!(bucket(&env, &admin, None, "docs").await.status, 201);
    assert!(put_obj(&env, &admin, None, "docs", "r.txt", b"report").await.status < 300);
    let sign = || async {
        let r = call(
            &env,
            &admin,
            None,
            "POST",
            "/api/v1/storage/buckets/docs/sign",
            Some(json!({"key": "r.txt", "expires_in": 600})),
        )
        .await;
        assert_eq!(r.status, 200, "{}", r.text());
        r.json()["url"].as_str().unwrap().to_owned()
    };
    let url = sign().await;
    assert_eq!(env.req("GET", &url, &[], None).await.text(), "report");

    let rotate = "/api/v1/storage/maintenance/rotate-signing-key";
    assert_eq!(call(&env, &alice, None, "POST", rotate, None).await.status, 403);
    assert_eq!(env.req("POST", rotate, &[], None).await.status, 401);
    let r = call(&env, &admin, None, "POST", rotate, None).await;
    assert_eq!(r.status, 204, "{}", r.text());

    let r = env.req("GET", &url, &[], None).await;
    assert_eq!(r.status, 403, "{}", r.text());
    assert_eq!(r.code(), "signature_invalid", "{}", r.text());
    assert_eq!(env.req("GET", &sign().await, &[], None).await.text(), "report");
    env.shutdown().await;
}

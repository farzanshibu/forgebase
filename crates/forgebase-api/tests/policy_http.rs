//! FB-21 over HTTP: SPEC §27 #1 (create a project), #9 (tenant isolation by
//! header and by known id), #25 (field leak through export, existence
//! oracle, cross-realm) with the real router, tenancy layer and policy engine.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use axum::body::Body;
use axum::http::{Method, StatusCode};
use common::{Api, Resp};
use serde_json::{Value as Json, json};

async fn req(
    api: &Api,
    method: Method,
    uri: &str,
    who: Option<&str>,
    extra: &[(&str, &str)],
    body: Option<Json>,
) -> Resp {
    let mut h: Vec<(&str, &str)> = vec![("content-type", "application/json")];
    if let Some(w) = who {
        h.push(("x-test-as", w));
    }
    h.extend_from_slice(extra);
    let body = body.map_or_else(Body::empty, |b| Body::from(b.to_string()));
    api.send(method, uri, &h, body).await
}

#[tokio::test]
async fn projects_tenants_rules_and_invitations_over_http() {
    let api = Api::with_policy_engine(None).await;
    let admin = Some("admin");
    // Schema and rules (super-admin).
    req(
        &api,
        Method::POST,
        "/api/v1/collections",
        admin,
        &[],
        Some(json!({"name": "users", "fields": [
        {"name": "email", "type": "text"}]})),
    )
    .await
    .expect(StatusCode::CREATED);
    for (id, email) in [("alice", "alice@example.com"), ("bob", "bob@example.com")] {
        req(
            &api,
            Method::POST,
            "/api/v1/collections/users/records",
            admin,
            &[],
            Some(json!({"id": id, "email": email})),
        )
        .await
        .expect(StatusCode::CREATED);
    }
    req(
        &api,
        Method::POST,
        "/api/v1/collections",
        admin,
        &[],
        Some(json!({"name": "tasks",
        "options": {"tenant_scoped": true},
        "fields": [{"name": "title", "type": "text"}, {"name": "secret", "type": "text"}]})),
    )
    .await
    .expect(StatusCode::CREATED);
    let any_user = "@request.auth.id != ''";
    req(
        &api,
        Method::PUT,
        "/api/v1/policies/tasks",
        admin,
        &[],
        Some(json!({
            "list_rule": any_user, "view_rule": any_user, "create_rule": any_user,
            "fields": {"secret": {"read": "@request.auth.roles ?= 'owner'"}}
        })),
    )
    .await
    .expect(StatusCode::OK);
    // Policies are super-admin only.
    let r = req(&api, Method::GET, "/api/v1/policies/tasks", Some("alice"), &[], None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);

    // #1: alice creates an organization and a project with environments.
    let org = req(
        &api,
        Method::POST,
        "/api/v1/orgs",
        Some("alice"),
        &[],
        Some(json!({"slug": "acme", "name": "Acme"})),
    )
    .await
    .expect(StatusCode::CREATED)
    .json();
    let org_id = org["id"].as_str().unwrap().to_owned();
    let project = req(
        &api,
        Method::POST,
        &format!("/api/v1/orgs/{org_id}/projects"),
        Some("alice"),
        &[],
        Some(json!({"slug": "app", "name": "App"})),
    )
    .await
    .expect(StatusCode::CREATED)
    .json();
    let envs: Vec<String> = project["environments"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(envs, ["development", "staging", "production"]);
    let tenant = project["environments"][2]["id"].as_str().unwrap().to_owned();
    let read_back = req(
        &api,
        Method::GET,
        &format!("/api/v1/projects/{}", project["id"].as_str().unwrap()),
        Some("alice"),
        &[],
        None,
    )
    .await
    .expect(StatusCode::OK)
    .json();
    assert_eq!(read_back["environments"].as_array().unwrap().len(), 3);

    // Data in alice's tenant.
    let th = [("x-forgebase-tenant", tenant.as_str())];
    let rec = req(
        &api,
        Method::POST,
        "/api/v1/collections/tasks/records",
        Some("alice"),
        &th,
        Some(json!({"title": "ship", "secret": "s3cr3t"})),
    )
    .await
    .expect(StatusCode::CREATED)
    .json();
    assert_eq!(rec["secret"], "s3cr3t", "the owner reads the field");
    let rid = rec["id"].as_str().unwrap().to_owned();

    // #9: bob is not a member: the tenant header is refused (step 2) ...
    let r = req(
        &api,
        Method::GET,
        &format!("/api/v1/collections/tasks/records/{rid}"),
        Some("bob"),
        &th,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN, "{}", r.text());
    assert_eq!(r.json()["error"]["details"]["step"], "tenant");
    // ... and from his own tenant the known id does not exist.
    let borg = req(
        &api,
        Method::POST,
        "/api/v1/orgs",
        Some("bob"),
        &[],
        Some(json!({"slug": "bobco", "name": "Bob Co"})),
    )
    .await
    .expect(StatusCode::CREATED)
    .json();
    let bproj = req(
        &api,
        Method::POST,
        &format!("/api/v1/orgs/{}/projects", borg["id"].as_str().unwrap()),
        Some("bob"),
        &[],
        Some(json!({"slug": "x", "name": "X"})),
    )
    .await
    .expect(StatusCode::CREATED)
    .json();
    let btenant = bproj["environments"][0]["id"].as_str().unwrap().to_owned();
    let bh = [("x-forgebase-tenant", btenant.as_str())];
    let r = req(
        &api,
        Method::GET,
        &format!("/api/v1/collections/tasks/records/{rid}"),
        Some("bob"),
        &bh,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    let missing = req(
        &api,
        Method::GET,
        "/api/v1/collections/tasks/records/nope",
        Some("bob"),
        &bh,
        None,
    )
    .await;
    assert_eq!(r.json()["error"]["code"], missing.json()["error"]["code"]);
    let list = req(
        &api,
        Method::GET,
        "/api/v1/collections/tasks/records",
        Some("bob"),
        &bh,
        None,
    )
    .await
    .expect(StatusCode::OK)
    .json();
    assert_eq!(list["items"].as_array().unwrap().len(), 0);

    // Invitation: alice invites bob as viewer; bob accepts.
    let inv = req(
        &api,
        Method::POST,
        &format!("/api/v1/orgs/{org_id}/invitations"),
        Some("alice"),
        &[],
        Some(json!({"email": "bob@example.com", "roles": ["viewer"]})),
    )
    .await
    .expect(StatusCode::CREATED)
    .json();
    let token = inv["token"].as_str().unwrap().to_owned();
    // Someone else cannot use bob's invitation.
    let r = req(
        &api,
        Method::POST,
        "/api/v1/invitations/accept",
        Some("alice"),
        &[],
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);
    req(
        &api,
        Method::POST,
        "/api/v1/invitations/accept",
        Some("bob"),
        &[],
        Some(json!({"token": token})),
    )
    .await
    .expect(StatusCode::OK);
    let r = req(
        &api,
        Method::POST,
        "/api/v1/invitations/accept",
        Some("bob"),
        &[],
        Some(json!({"token": token})),
    )
    .await;
    assert_eq!(r.status, StatusCode::NOT_FOUND, "single use");
    let me = req(
        &api,
        Method::GET,
        &format!("/api/v1/authz/me?org={org_id}"),
        Some("bob"),
        &[],
        None,
    )
    .await
    .expect(StatusCode::OK)
    .json();
    assert_eq!(me["roles"], json!(["viewer"]));

    // Now bob reads alice's tenant, without the owner-only field — not even
    // through an export.
    let r = req(
        &api,
        Method::GET,
        &format!("/api/v1/collections/tasks/records/{rid}"),
        Some("bob"),
        &th,
        None,
    )
    .await
    .expect(StatusCode::OK)
    .json();
    assert!(r.get("secret").is_none() && r["title"] == "ship");
    let export = req(
        &api,
        Method::GET,
        "/api/v1/collections/tasks/export?format=ndjson",
        Some("bob"),
        &th,
        None,
    )
    .await
    .expect(StatusCode::OK)
    .text();
    assert!(export.contains("ship") && !export.contains("s3cr3t"), "{export}");
    let r = req(
        &api,
        Method::GET,
        "/api/v1/collections/tasks/records?filter=secret%3D's3cr3t'",
        Some("bob"),
        &th,
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::BAD_REQUEST);

    // Viewers cannot manage the org; owners can.
    let r = req(
        &api,
        Method::POST,
        &format!("/api/v1/orgs/{org_id}/projects"),
        Some("bob"),
        &[],
        Some(json!({"slug": "evil", "name": "Evil"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    assert_eq!(r.json()["error"]["details"]["step"], "permission");

    // Existence oracle over HTTP: missing and locked collections look the same.
    req(
        &api,
        Method::POST,
        "/api/v1/collections",
        admin,
        &[],
        Some(json!({"name": "vault", "fields": []})),
    )
    .await
    .expect(StatusCode::CREATED);
    let a = req(
        &api,
        Method::GET,
        "/api/v1/collections/vault/records",
        Some("bob"),
        &[],
        None,
    )
    .await;
    let b = req(
        &api,
        Method::GET,
        "/api/v1/collections/vaulx/records",
        Some("bob"),
        &[],
        None,
    )
    .await;
    assert_eq!((a.status, a.code()), (b.status, b.code()));
    let a = req(&api, Method::GET, "/api/v1/collections/vault/records", None, &[], None).await;
    let b = req(&api, Method::GET, "/api/v1/collections/vaulx/records", None, &[], None).await;
    assert_eq!((a.status, a.code()), (StatusCode::UNAUTHORIZED, b.code()));
    assert_eq!(b.status, StatusCode::UNAUTHORIZED);

    // Cross-realm and anonymous writes.
    let r = req(
        &api,
        Method::GET,
        "/api/v1/orgs",
        Some("alice"),
        &[("x-forgebase-realm", "other")],
        None,
    )
    .await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let r = req(
        &api,
        Method::POST,
        "/api/v1/orgs",
        None,
        &[],
        Some(json!({"slug": "anon", "name": "Anon"})),
    )
    .await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    // The audit log records denials with their step (super-admins only).
    let r = req(&api, Method::GET, "/api/v1/authz/audit", Some("alice"), &[], None).await;
    assert_eq!(r.status, StatusCode::FORBIDDEN);
    let mut steps = Vec::new();
    for _ in 0..50 {
        let log = req(&api, Method::GET, "/api/v1/authz/audit?limit=100", admin, &[], None)
            .await
            .expect(StatusCode::OK)
            .json();
        steps = log
            .as_array()
            .unwrap()
            .iter()
            .map(|e| e["step"].as_str().unwrap().to_owned())
            .collect();
        if steps.contains(&"tenant".to_owned()) && steps.contains(&"permission".to_owned()) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(40)).await;
    }
    assert!(
        steps.contains(&"tenant".to_owned()) && steps.contains(&"permission".to_owned()),
        "{steps:?}"
    );
}

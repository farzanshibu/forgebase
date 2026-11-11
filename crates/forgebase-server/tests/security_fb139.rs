//! FB-139 (rls-coverage G9, G10): vector collection rules and graph label
//! rules, against the real server. Each test failed before FB-139: vector
//! and graph calls asked only the plain `engine:vector_*` / `engine:graph_*`
//! keys (no resource rules, a Filter was a denial), item metadata copied
//! from records was returned without the records' rules, and callers without
//! a tenant shared one `''` scope. Edge functions reach vector as the caller
//! (edge-functions.md §2), so `ctx.vector` is checked the same way.
//! Binds `127.0.0.1:0`.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod security_common;

use forgebase_data::AccessContext;
use forgebase_functions::{DeployRequest, InvokeAccess};
use security_common::{Env, Resp};
use serde_json::{Value as Json, json};

/// A user's organization with one project; returns its production tenant.
async fn org(env: &Env, token: &str, slug: &str) -> String {
    org_ids(env, token, slug).await.1
}

/// An organization `owner` owns, with `member` added under an org role that
/// grants nothing (an owner holds every permission, so rules would never be
/// asked); returns the production tenant.
async fn member_org(env: &Env, owner: &str, slug: &str, member: &str) -> String {
    let (id, tenant) = org_ids(env, owner, slug).await;
    let oh = [("X-ForgeBase-Org", id.as_str())];
    let r = env
        .as_(
            owner,
            "PUT",
            &format!("/api/v1/orgs/{id}/roles"),
            &oh,
            Some(json!({"key": "plain", "allow": ["app:none"]})),
        )
        .await;
    assert_eq!(r.status, 200, "org role: {}", r.text());
    let r = env
        .as_(
            owner,
            "PUT",
            &format!("/api/v1/orgs/{id}/members"),
            &oh,
            Some(json!({"principal": member, "roles": ["plain"]})),
        )
        .await;
    assert_eq!(r.status, 200, "member: {}", r.text());
    tenant
}

/// The organization's id and its production tenant.
async fn org_ids(env: &Env, token: &str, slug: &str) -> (String, String) {
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
    let tenant = p.json()["environments"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "production")
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    (id, tenant)
}

/// A realm role bound to `principal` (as an administrator).
async fn realm_role(env: &Env, admin: &str, principal: &str, key: &str, allow: &[&str]) {
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/roles",
            &[],
            Some(json!({"key": key, "allow": allow, "deny": []})),
        )
        .await;
    assert_eq!(r.status, 200, "role: {}", r.text());
    let r = env
        .as_(
            admin,
            "PUT",
            "/api/v1/realm-bindings",
            &[],
            Some(json!({"principal": principal, "roles": [key]})),
        )
        .await;
    assert_eq!(r.status, 200, "binding: {}", r.text());
}

async fn rules(env: &Env, admin: &str, kind: &str, name: &str, rules: Json) {
    let r = env
        .as_(
            admin,
            "PUT",
            &format!("/api/v1/policies/resources/{kind}/{name}"),
            &[],
            Some(json!({ "rules": rules })),
        )
        .await;
    assert_eq!(r.status, 200, "{kind}/{name} rules: {}", r.text());
}

/// `method /api/v1<path>` as `token` in `tenant`.
async fn call(env: &Env, token: &str, tenant: Option<&str>, method: &str, path: &str, body: Option<Json>) -> Resp {
    let h: Vec<(&str, &str)> = tenant.map(|t| ("X-ForgeBase-Tenant", t)).into_iter().collect();
    env.as_(token, method, &format!("/api/v1{path}"), &h, body).await
}

fn hit_ids(r: &Resp) -> Vec<String> {
    let mut ids: Vec<String> = r.json()["hits"]
        .as_array()
        .unwrap_or_else(|| panic!("hits: {}", r.text()))
        .iter()
        .map(|h| h["id"].as_str().unwrap().to_owned())
        .collect();
    ids.sort();
    ids
}

fn ids_of(v: &Json) -> Vec<String> {
    let mut ids: Vec<String> = v
        .as_array()
        .unwrap_or_else(|| panic!("not a list: {v}"))
        .iter()
        .map(|n| {
            n["id"]
                .as_str()
                .or_else(|| n["node"]["id"].as_str())
                .unwrap()
                .to_owned()
        })
        .collect();
    ids.sort();
    ids
}

fn user_id(principal: &str) -> String {
    principal.rsplit('/').next().unwrap().to_owned()
}

fn collection(name: &str, extra: Json) -> forgebase_vector::model::NewCollection {
    let mut v = json!({"name": name, "fields": [{"name": "v", "dimension": 2, "metric": "cosine"}]});
    v.as_object_mut().unwrap().extend(extra.as_object().unwrap().clone());
    serde_json::from_value(v).unwrap()
}

fn items(v: Json) -> Vec<forgebase_vector::model::UpsertItem> {
    serde_json::from_value(v).unwrap()
}

const VECTOR_FN: &str = r#"import { serve } from "forgebase";

serve(async (_req: Request, ctx) => {
  try {
    const r = await ctx.vector.from("docs").search({ vector: [1, 0], k: 10, exact: true });
    return Response.json({ ids: r.hits.map((h: any) => h.id).sort() });
  } catch (e: any) {
    return Response.json({ error: e.code });
  }
});
"#;

// ---------------------------------------------------------------------------
// G9: vector collection rules, source_collection, tenant_required
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g9_collection_rules_prefilter_searches_and_guard_writes_also_in_functions() {
    let env = Env::new().await;
    let admin = env.admin("fb139-g9@example.com").await;
    let (owner, _) = env.user("owner@example.com").await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    let ta = member_org(&env, &owner, "alpha", &alice_pk).await;
    let t = Some(ta.as_str());
    let v = env.server.vector().expect("vector enabled");
    let root = AccessContext::admin("root").with_tenant(ta.clone());
    v.create_collection(&root, collection("docs", json!({}))).await.unwrap();
    v.upsert(
        &root,
        "docs",
        items(json!([
            {"id": "a1", "vectors": {"v": [1.0, 0.0]}, "metadata": {"team": "a"}, "text": "hello alpha"},
            {"id": "b1", "vectors": {"v": [1.0, 0.1]}, "metadata": {"team": "b"}, "text": "hello bravo"}
        ])),
    )
    .await
    .unwrap();
    let fns = env.server.functions().expect("functions enabled");
    fns.deploy(
        &AccessContext::admin("root"),
        DeployRequest {
            name: "vsearch".into(),
            source: Some(VECTOR_FN.into()),
            capabilities: vec!["vector.read".into()],
            access: Some(InvokeAccess::Authenticated),
            ..DeployRequest::default()
        },
    )
    .await
    .unwrap();
    let search = json!({"vector": [1.0, 0.0], "k": 10, "exact": true});
    let search_path = "/vector/collections/docs/search";

    // No permission and no rule: denied, in a function too.
    let r = call(&env, &alice, t, "POST", search_path, Some(search.clone())).await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = call(&env, &alice, t, "POST", "/functions/vsearch/invoke", Some(json!({}))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(r.json()["error"], "forbidden", "{}", r.text());

    // A collection rule over item metadata is a search pre-filter.
    rules(
        &env,
        &admin,
        "vector_collection",
        "docs",
        json!({"read": "metadata.team = 'a'", "write": "metadata.team = 'a'"}),
    )
    .await;
    let r = call(&env, &alice, t, "POST", search_path, Some(search.clone())).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(hit_ids(&r), ["a1"]);
    // The caller's own filter cannot widen it, and hybrid search obeys it.
    let mut probe = search.clone();
    probe["filter"] = json!("team = 'b'");
    let r = call(&env, &alice, t, "POST", search_path, Some(probe)).await;
    assert_eq!(hit_ids(&r), Vec::<String>::new(), "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/vector/collections/docs/hybrid",
        Some(json!({"text": "hello", "k": 10})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(hit_ids(&r), ["a1"]);
    // ctx.vector in a function runs as the caller: the same rule applies.
    let r = call(&env, &alice, t, "POST", "/functions/vsearch/invoke", Some(json!({}))).await;
    assert_eq!(r.json()["ids"], json!(["a1"]), "{}", r.text());

    // Single reads: a hidden item looks missing.
    let r = call(&env, &alice, t, "GET", "/vector/collections/docs/items/b1", None).await;
    assert_eq!(r.status, 404, "{}", r.text());
    let r = call(&env, &alice, t, "GET", "/vector/collections/docs/items/a1", None).await;
    assert_eq!(r.status, 200, "{}", r.text());

    // Writes: the written item must satisfy the rule, an existing item too.
    let upsert =
        |id: &str, team: &str| json!({"items": [{"id": id, "vectors": {"v": [0.5, 0.5]}, "metadata": {"team": team}}]});
    let up = "/vector/collections/docs/upsert";
    let r = call(&env, &alice, t, "POST", up, Some(upsert("c1", "a"))).await;
    assert_eq!(r.status, 200, "{}", r.text());
    let r = call(&env, &alice, t, "POST", up, Some(upsert("c2", "b"))).await;
    assert_eq!(r.status, 403, "creating outside the rule: {}", r.text());
    let r = call(&env, &alice, t, "POST", up, Some(upsert("b1", "a"))).await;
    assert_eq!(r.status, 403, "taking over a hidden item: {}", r.text());
    let r = call(&env, &alice, t, "DELETE", "/vector/collections/docs/items/b1", None).await;
    assert_eq!(r.status, 404, "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/vector/collections/docs/delete",
        Some(json!({"ids": ["b1", "c1"]})),
    )
    .await;
    assert_eq!(r.json()["deleted"], 1, "only the allowed item: {}", r.text());
    let b1 = v.get_item(&root, "docs", "b1").await.unwrap();
    assert_eq!(b1.metadata["team"], "b", "the hidden item is untouched");
    assert!(v.get_item(&root, "docs", "c2").await.is_err());

    // Definition reads count what the rule shows; index statistics count
    // everything, so a restricted caller is refused; admin needs permission.
    let r = call(&env, &alice, t, "GET", "/vector/collections/docs", None).await;
    assert_eq!((r.status, r.json()["items"].clone()), (200, json!(1)), "{}", r.text());
    let r = call(&env, &alice, t, "GET", "/vector/collections/docs/stats", None).await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/vector/collections/docs/rebuild",
        Some(json!({})),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn g9_source_collection_rechecks_the_records_view_rule_and_field_mask() {
    let env = Env::new().await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    let ta = org(&env, &alice, "alpha").await;
    let t = Some(ta.as_str());
    let admin = env.admin("fb139-src@example.com").await;
    realm_role(&env, &admin, &alice_pk, "vector-reader", &["engine:vector_read"]).await;
    let fns = env.server.functions().expect("functions enabled");
    let data = fns.data();
    let root = AccessContext::admin("root");
    data.create_collection(
        &root,
        serde_json::from_value(json!({"name": "notes", "fields": [
            {"name": "owner", "type": "text"},
            {"name": "salary", "type": "number", "hidden": true}
        ]}))
        .unwrap(),
    )
    .await
    .unwrap();
    let mine = data
        .create_record(&root, "notes", json!({"owner": user_id(&alice_pk), "salary": 10}))
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let theirs = data
        .create_record(&root, "notes", json!({"owner": "someone-else", "salary": 99}))
        .await
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    let rule = Some("owner = @request.auth.id".to_owned());
    fns.attached_services()
        .policy
        .clone()
        .unwrap()
        .put_policy(
            &root,
            "notes",
            &forgebase_policy::store::PolicyInput {
                list_rule: rule.clone(),
                view_rule: rule,
                ..Default::default()
            },
        )
        .await
        .unwrap();

    let v = env.server.vector().unwrap();
    let rt = root.clone().with_tenant(ta.clone());
    let r = v
        .create_collection(&rt, collection("bad", json!({"source_collection": "nope"})))
        .await;
    assert!(r.is_err(), "an unknown source collection is refused");
    v.create_collection(&rt, collection("notevecs", json!({"source_collection": "notes"})))
        .await
        .unwrap();
    v.upsert(
        &rt,
        "notevecs",
        items(json!([
            {"id": mine, "vectors": {"v": [1.0, 0.0]}, "metadata": {"owner": user_id(&alice_pk), "salary": 10}},
            {"id": theirs, "vectors": {"v": [1.0, 0.1]}, "metadata": {"owner": "someone-else", "salary": 99}}
        ])),
    )
    .await
    .unwrap();

    let search = json!({"vector": [1.0, 0.0], "k": 2, "exact": true});
    let path = "/vector/collections/notevecs/search";
    let r = call(&env, &alice, t, "POST", path, Some(search.clone())).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(hit_ids(&r), std::slice::from_ref(&mine), "only records alice may view");
    let meta = &r.json()["hits"][0]["metadata"];
    assert_eq!(meta["owner"], json!(user_id(&alice_pk)), "{meta}");
    assert!(meta.get("salary").is_none(), "the hidden field is masked: {meta}");
    // A filter on a field alice may not read is refused (no oracle).
    let mut probe = search.clone();
    probe["filter"] = json!("salary > 50");
    let r = call(&env, &alice, t, "POST", path, Some(probe)).await;
    assert_eq!(r.status, 400, "{}", r.text());
    // Single reads re-check too.
    let item = |id: &str| format!("/vector/collections/notevecs/items/{id}");
    let r = call(&env, &alice, t, "GET", &item(&theirs), None).await;
    assert_eq!(r.status, 404, "{}", r.text());
    let r = call(&env, &alice, t, "GET", &item(&mine), None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert!(r.json()["metadata"].get("salary").is_none(), "{}", r.text());
    // Administrators see everything.
    let r = call(&env, &admin, t, "POST", path, Some(search)).await;
    assert_eq!(hit_ids(&r).len(), 2, "{}", r.text());
    env.shutdown().await;
}

#[tokio::test]
async fn g9_g10_callers_without_a_tenant_are_privileged_or_get_tenant_required() {
    let env = Env::new().await;
    let admin = env.admin("fb139-tenant@example.com").await;
    let (bob, bob_pk) = env.user("bob@example.com").await;
    let (carol, _) = env.user("carol@example.com").await;
    realm_role(
        &env,
        &admin,
        &bob_pk,
        "engines",
        &["engine:vector_read", "engine:graph_read"],
    )
    .await;
    let v = env.server.vector().unwrap();
    let root = AccessContext::admin("root");
    v.create_collection(&root, collection("shared", json!({})))
        .await
        .unwrap();
    v.upsert(
        &root,
        "shared",
        items(json!([{"id": "x", "vectors": {"v": [1.0, 0.0]}}])),
    )
    .await
    .unwrap();
    let r = call(&env, &admin, None, "POST", "/graph/nodes", Some(json!({"id": "n1"}))).await;
    assert_eq!(r.status, 201, "{}", r.text());
    let search = json!({"vector": [1.0, 0.0], "k": 1, "exact": true});
    let path = "/vector/collections/shared/search";

    // Before FB-139 bob read the shared '' scope.
    let r = call(&env, &bob, None, "POST", path, Some(search.clone())).await;
    assert_eq!(
        (r.status, r.code()),
        (400, "tenant_required".to_owned()),
        "{}",
        r.text()
    );
    let r = call(&env, &bob, None, "GET", "/graph/nodes/n1", None).await;
    assert_eq!(
        (r.status, r.code()),
        (400, "tenant_required".to_owned()),
        "{}",
        r.text()
    );
    // Callers that are not allowed at all learn nothing more.
    let r = call(&env, &carol, None, "POST", path, Some(search.clone())).await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = call(&env, &carol, None, "GET", "/graph/nodes/n1", None).await;
    assert_eq!(r.status, 403, "{}", r.text());
    // Administrators keep the shared scope.
    let r = call(&env, &admin, None, "POST", path, Some(search)).await;
    assert_eq!(hit_ids(&r), ["x"], "{}", r.text());
    let r = call(&env, &admin, None, "GET", "/graph/nodes/n1", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    env.shutdown().await;
}

// ---------------------------------------------------------------------------
// G10: graph label and edge-type rules in the traversal SQL
// ---------------------------------------------------------------------------

#[tokio::test]
async fn g10_label_rules_hide_nodes_and_edges_from_reads_and_walks() {
    let env = Env::new().await;
    let admin = env.admin("fb139-g10@example.com").await;
    let (owner, _) = env.user("owner@example.com").await;
    let (alice, alice_pk) = env.user("alice@example.com").await;
    let ta = member_org(&env, &owner, "alpha", &alice_pk).await;
    let t = Some(ta.as_str());
    // a -knows-> b(secret) -knows-> c ; a -hidden_link-> c ; a -knows-> d(blue) ; a -knows-> f
    for (id, label, team) in [
        ("a", "person", "red"),
        ("b", "secret", "red"),
        ("c", "person", "red"),
        ("d", "person", "blue"),
        ("f", "person", "red"),
    ] {
        let r = call(
            &env,
            &admin,
            t,
            "POST",
            "/graph/nodes",
            Some(json!({"id": id, "label": label, "properties": {"team": team}})),
        )
        .await;
        assert_eq!(r.status, 201, "{}", r.text());
    }
    for (id, ty, s, d) in [
        ("e1", "knows", "a", "b"),
        ("e2", "knows", "b", "c"),
        ("e3", "hidden_link", "a", "c"),
        ("e4", "knows", "a", "d"),
        ("e5", "knows", "a", "f"),
    ] {
        let r = call(
            &env,
            &admin,
            t,
            "POST",
            "/graph/edges",
            Some(json!({"id": id, "type": ty, "source": s, "target": d})),
        )
        .await;
        assert_eq!(r.status, 201, "{}", r.text());
    }
    let r = call(&env, &alice, t, "GET", "/graph/nodes", None).await;
    assert_eq!(r.status, 403, "no permission and no rule: {}", r.text());

    rules(
        &env,
        &admin,
        "graph_label",
        "person",
        json!({"read": "properties.team = 'red'", "create": "properties.team = 'red'"}),
    )
    .await;
    rules(&env, &admin, "graph_edge_type", "knows", json!({"read": ""})).await;

    let r = call(&env, &alice, t, "GET", "/graph/nodes", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(ids_of(&r.json()["items"]), ["a", "c", "f"]);
    for hidden in ["b", "d"] {
        let r = call(&env, &alice, t, "GET", &format!("/graph/nodes/{hidden}"), None).await;
        assert_eq!(r.status, 404, "{hidden}: {}", r.text());
    }
    // The walk never crosses the secret node or the hidden edge type.
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/graph/traverse",
        Some(json!({"start": "a", "max_depth": 5})),
    )
    .await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(ids_of(&r.json()["nodes"]), ["f"], "{}", r.text());
    assert_eq!(ids_of(&r.json()["edges"]), ["e5"], "{}", r.text());
    let r = call(&env, &alice, t, "GET", "/graph/nodes/a/neighbors", None).await;
    assert_eq!(r.status, 200, "{}", r.text());
    assert_eq!(ids_of(&r.json()["items"]), ["f"], "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/graph/shortest-path",
        Some(json!({"from": "a", "to": "c"})),
    )
    .await;
    assert_eq!(r.json()["found"], false, "{}", r.text());
    // Edges: the type must be readable and both nodes visible.
    let r = call(&env, &alice, t, "GET", "/graph/edges", None).await;
    assert_eq!(ids_of(&r.json()["items"]), ["e5"], "{}", r.text());
    for hidden in ["e1", "e3", "e4"] {
        let r = call(&env, &alice, t, "GET", &format!("/graph/edges/{hidden}"), None).await;
        assert_eq!(r.status, 404, "{hidden}: {}", r.text());
    }
    // Administrators still see the whole graph.
    let r = call(
        &env,
        &admin,
        t,
        "POST",
        "/graph/traverse",
        Some(json!({"start": "a", "max_depth": 5})),
    )
    .await;
    assert_eq!(ids_of(&r.json()["nodes"]), ["b", "c", "d", "f"], "{}", r.text());

    // Writes: the label's create rule must hold for the new node.
    let node = |id: &str, label: &str, team: &str| json!({"id": id, "label": label, "properties": {"team": team}});
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/graph/nodes",
        Some(node("g", "person", "red")),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/graph/nodes",
        Some(node("h", "person", "blue")),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/graph/nodes",
        Some(node("i", "secret", "red")),
    )
    .await;
    assert_eq!(r.status, 403, "{}", r.text());
    // No update or delete rule: a visible node is forbidden, a hidden one missing.
    let r = call(&env, &alice, t, "DELETE", "/graph/nodes/a", None).await;
    assert_eq!(r.status, 403, "{}", r.text());
    let r = call(&env, &alice, t, "DELETE", "/graph/nodes/b", None).await;
    assert_eq!(r.status, 404, "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "PATCH",
        "/graph/nodes/d",
        Some(json!({"properties": {"team": "red"}})),
    )
    .await;
    assert_eq!(r.status, 404, "{}", r.text());
    // Edges to hidden nodes cannot be created.
    rules(
        &env,
        &admin,
        "graph_edge_type",
        "knows",
        json!({"read": "", "create": ""}),
    )
    .await;
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/graph/edges",
        Some(json!({"type": "knows", "source": "g", "target": "b"})),
    )
    .await;
    assert_eq!(r.status, 400, "{}", r.text());
    let r = call(
        &env,
        &alice,
        t,
        "POST",
        "/graph/edges",
        Some(json!({"type": "knows", "source": "g", "target": "a"})),
    )
    .await;
    assert_eq!(r.status, 201, "{}", r.text());
    env.shutdown().await;
}

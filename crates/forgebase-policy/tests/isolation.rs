//! SPEC §27 #1, #9, #25 at the service layer: projects as tenants, tenant
//! isolation by known id, field leaks, filter injection into rules, the
//! existence oracle, fail-closed rules, and a property test that a random
//! rule never widens access beyond its own semantics.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::collections::{BTreeSet, HashMap};

use common::{Env, admin, pk, user};
use forgebase_data::{
    AccessContext, AggregateFunction, AggregateQuery, AggregateSpec, DataError, GetOptions, ListQuery, UpdateOptions,
};
use forgebase_policy::snapshot::FieldRuleText;
use forgebase_policy::store::{PolicyInput, RoleInput};
use serde_json::{Value as Json, json};

fn open_rules() -> PolicyInput {
    let rule = Some("@request.auth.id != ''".to_owned());
    PolicyInput {
        list_rule: rule.clone(),
        view_rule: rule.clone(),
        create_rule: rule.clone(),
        update_rule: rule.clone(),
        delete_rule: rule,
        ..PolicyInput::default()
    }
}

async fn ids(env: &Env, ctx: &AccessContext, coll: &str) -> BTreeSet<String> {
    env.data
        .list_records(
            ctx,
            coll,
            &ListQuery {
                per_page: Some(500),
                ..ListQuery::default()
            },
        )
        .await
        .unwrap()
        .items
        .iter()
        .map(|r| r["id"].as_str().unwrap().to_owned())
        .collect()
}

/// FB-80: a privileged caller's scoped key (an admin service account's API
/// key) reaches only the collections its scopes name, also through `expand`
/// and relation filters, which the HTTP path check cannot see.
#[tokio::test]
async fn scopes_bind_privileged_callers_across_relations() {
    let env = Env::new().await;
    let secrets = env
        .collection(json!({"name": "vault", "fields": [{"name": "title", "type": "text"}]}))
        .await;
    env.collection(json!({"name": "memos", "fields": [
        {"name": "title", "type": "text"},
        {"name": "ref", "type": "relation", "collection_id": secrets.id}
    ]}))
    .await;
    let s = env
        .data
        .create_record(&admin(), "vault", json!({"title": "top secret"}))
        .await
        .unwrap();
    env.data
        .create_record(&admin(), "memos", json!({"title": "memo", "ref": s["id"]}))
        .await
        .unwrap();
    let scoped = admin().with_scopes(vec!["collections:read:memos".into()]);
    assert_eq!(ids(&env, &scoped, "memos").await.len(), 1, "its own collection");
    assert!(env.data.list_records(&scoped, "vault", &ListQuery::default()).await.is_err());
    let expanded = env
        .data
        .list_records(
            &scoped,
            "memos",
            &ListQuery {
                expand: vec!["ref".into()],
                ..ListQuery::default()
            },
        )
        .await;
    let text = format!("{expanded:?}");
    assert!(!text.contains("top secret"), "expand crossed the scope: {text}");
    let filtered = env
        .data
        .list_records(
            &scoped,
            "memos",
            &ListQuery {
                filter: Some("ref.title = 'top secret'".into()),
                ..ListQuery::default()
            },
        )
        .await;
    assert!(
        filtered.as_ref().map_or(true, |p| p.items.is_empty()),
        "relation filter crossed the scope: {filtered:?}"
    );
    let write = admin().with_scopes(vec!["collections:read".into()]);
    assert!(
        env.data
            .create_record(&write, "memos", json!({"title": "x"}))
            .await
            .is_err(),
        "a read scope does not write, even for an admin"
    );
    let unscoped = admin();
    assert_eq!(ids(&env, &unscoped, "vault").await.len(), 1, "unscoped admins are unaffected");
}

/// #1 and #9: two organizations, a project each; records stay in their
/// tenant even when the other tenant knows the id.
#[tokio::test]
async fn tenants_are_isolated_even_with_known_ids() {
    let env = Env::new().await;
    let a = env.org("alice", "a").await;
    let b = env.org("bob", "b").await;
    let (_, ta) = env.project("alice", &a, "app").await;
    let (_, tb) = env.project("bob", &b, "app").await;
    env.collection(
        json!({"id": "notes_self", "name": "notes", "options": {"tenant_scoped": true}, "fields": [
            {"name": "title", "type": "text"}, {"name": "code", "type": "text", "unique": true},
            {"name": "parent", "type": "relation", "collection_id": "notes_self"}
        ]}),
    )
    .await;
    env.policy.put_policy(&admin(), "notes", &open_rules()).await.unwrap();
    let alice = user("alice").with_tenant(&ta);
    let bob = user("bob").with_tenant(&tb);

    let n = env
        .data
        .create_record(&alice, "notes", json!({"title": "secret", "code": "X1"}))
        .await
        .unwrap();
    let nid = n["id"].as_str().unwrap();
    assert!(n.get("_tenant").is_none(), "the tenant column is not shown");
    env.data
        .create_record(&bob, "notes", json!({"title": "bob's", "code": "X1"}))
        .await
        .expect("unique per tenant: same code in another tenant is fine");
    let dup = env
        .data
        .create_record(&alice, "notes", json!({"code": "X1"}))
        .await
        .unwrap_err();
    assert!(matches!(dup, DataError::Validation(_)), "{dup:?}");

    // Known id from another tenant: indistinguishable from a missing record.
    let get = env
        .data
        .get_record(&bob, "notes", nid, &GetOptions::default())
        .await
        .unwrap_err();
    let missing = env
        .data
        .get_record(&bob, "notes", "nope", &GetOptions::default())
        .await
        .unwrap_err();
    assert_eq!(std::mem::discriminant(&get), std::mem::discriminant(&missing));
    assert!(matches!(get, DataError::RecordNotFound { .. }));
    assert!(
        env.data
            .update_record(&bob, "notes", nid, json!({"title": "pwned"}), UpdateOptions::default())
            .await
            .is_err()
    );
    assert!(env.data.delete_record(&bob, "notes", nid, None).await.is_err());
    assert!(env.data.record_versions(&bob, "notes", nid).await.is_err());
    assert_eq!(ids(&env, &bob, "notes").await.len(), 1);
    assert_eq!(
        env.data
            .count_records(&bob, "notes", None, HashMap::new())
            .await
            .unwrap(),
        1
    );
    let agg = env
        .data
        .aggregate(&bob, "notes", &AggregateQuery::default())
        .await
        .unwrap();
    assert_eq!(agg[0]["count"], 1);
    // A filter cannot reach the other tenant either.
    let q = ListQuery {
        filter: Some(format!("id = '{nid}' || 1 = 1")),
        ..ListQuery::default()
    };
    assert_eq!(env.data.list_records(&bob, "notes", &q).await.unwrap().items.len(), 1);
    // Relations cannot cross tenants.
    let cross = env
        .data
        .create_record(&bob, "notes", json!({"title": "x", "parent": nid}))
        .await
        .unwrap_err();
    assert!(matches!(cross, DataError::Validation(_)), "{cross:?}");
    // Selecting a tenant one does not belong to is refused (step 2).
    let sneaky = user("bob").with_tenant(&ta);
    assert!(matches!(
        env.data.list_records(&sneaky, "notes", &ListQuery::default()).await,
        Err(DataError::Forbidden(_))
    ));
    // No tenant at all: refused.
    assert!(
        env.data
            .list_records(&user("bob"), "notes", &ListQuery::default())
            .await
            .is_err()
    );
    // Super-admins see every tenant without one, and one tenant with one.
    assert_eq!(ids(&env, &admin(), "notes").await.len(), 2);
    assert_eq!(ids(&env, &admin().with_tenant(&ta), "notes").await.len(), 1);
    // The flag is immutable, and shared collections cannot point into tenants.
    let mut def = env.data.get_collection(&admin(), "notes").await.unwrap();
    def.options.remove("tenant_scoped");
    assert!(env.data.update_collection(&admin(), "notes", def).await.is_err());
    let notes_id = env.data.get_collection(&admin(), "notes").await.unwrap().id;
    let shared = env
        .data
        .create_collection(
            &AccessContext::system(),
            serde_json::from_value(json!({"name": "links", "fields": [
                {"name": "note", "type": "relation", "collection_id": notes_id}
            ]}))
            .unwrap(),
        )
        .await;
    assert!(shared.is_err(), "shared -> tenant-scoped relation refused");
}

/// #9: a personal (B2C) project is a tenant of one user only.
#[tokio::test]
async fn personal_projects_belong_to_their_owner() {
    let env = Env::new().await;
    let p = env
        .policy
        .create_project(&user("alice"), None, "mine", "Mine")
        .await
        .unwrap();
    let t = p["environments"][0]["id"].as_str().unwrap().to_owned();
    env.collection(
        json!({"name": "diary", "options": {"tenant_scoped": true}, "fields": [{"name": "t", "type": "text"}]}),
    )
    .await;
    // No rule at all: the owner still has every permission on their tenant.
    let mine = user("alice").with_tenant(&t);
    env.data
        .create_record(&mine, "diary", json!({"t": "dear diary"}))
        .await
        .unwrap();
    assert_eq!(ids(&env, &mine, "diary").await.len(), 1);
    let other = user("bob").with_tenant(&t);
    assert!(
        env.data
            .list_records(&other, "diary", &ListQuery::default())
            .await
            .is_err()
    );
    assert!(env.policy.get_project(&user("bob"), p["id"].as_str().unwrap()).is_err());
}

/// #25: fields hidden by a field rule do not leak through output, filter,
/// sort, group-by, aggregates, projection, expand, relation paths or search;
/// read-only fields cannot be written.
#[tokio::test]
async fn hidden_fields_do_not_leak() {
    let env = Env::new().await;
    let staff = env
        .collection(json!({"name": "staff", "fields": [
            {"name": "name", "type": "text"}, {"name": "salary", "type": "number"}, {"name": "bio", "type": "text"}
        ]}))
        .await;
    env.collection(json!({"name": "tasks", "fields": [
        {"name": "title", "type": "text"},
        {"name": "owner", "type": "relation", "collection_id": staff.id}
    ]}))
    .await;
    env.data
        .create_index(
            &admin(),
            forgebase_data::IndexDefinition {
                name: "staff_fts".into(),
                collection: "staff".into(),
                keys: vec![forgebase_data::IndexKey::asc("bio")],
                unique: false,
                partial: None,
                kind: forgebase_data::IndexKind::Fulltext,
            },
        )
        .await
        .unwrap();
    let hr_only = Some("@request.auth.roles ?= 'hr'".to_owned());
    let mut fields = HashMap::new();
    fields.insert(
        "salary".to_owned(),
        FieldRuleText {
            read: hr_only.clone(),
            write: hr_only.clone(),
        },
    );
    fields.insert(
        "bio".to_owned(),
        FieldRuleText {
            read: hr_only.clone(),
            write: None,
        },
    );
    env.policy
        .put_policy(&admin(), "staff", &PolicyInput { fields, ..open_rules() })
        .await
        .unwrap();
    env.policy.put_policy(&admin(), "tasks", &open_rules()).await.unwrap();
    let s = env
        .data
        .create_record(
            &AccessContext::system(),
            "staff",
            json!({"name": "Ann", "salary": 9000, "bio": "likes chess"}),
        )
        .await
        .unwrap();
    let sid = s["id"].as_str().unwrap().to_owned();
    env.data
        .create_record(&AccessContext::system(), "tasks", json!({"title": "t", "owner": sid}))
        .await
        .unwrap();

    let u = user("eve");
    let rec = env
        .data
        .get_record(&u, "staff", &sid, &GetOptions::default())
        .await
        .unwrap();
    assert!(rec.get("salary").is_none() && rec.get("bio").is_none() && rec["name"] == "Ann");
    let probes = [
        ListQuery {
            filter: Some("salary > 1000".into()),
            ..ListQuery::default()
        },
        ListQuery {
            sort: Some("-salary".into()),
            ..ListQuery::default()
        },
        ListQuery {
            search: Some("chess".into()),
            ..ListQuery::default()
        },
    ];
    for q in &probes {
        assert!(env.data.list_records(&u, "staff", q).await.is_err(), "{q:?} must fail");
    }
    let proj = ListQuery {
        fields: Some(vec!["salary".into(), "id".into()]),
        ..ListQuery::default()
    };
    let r = env.data.list_records(&u, "staff", &proj).await.unwrap();
    assert!(r.items[0].get("salary").is_none());
    for agg in [
        AggregateQuery {
            group_by: vec!["salary".into()],
            ..AggregateQuery::default()
        },
        AggregateQuery {
            aggregates: vec![AggregateSpec {
                function: AggregateFunction::Max,
                field: Some("salary".into()),
                alias: None,
            }],
            ..AggregateQuery::default()
        },
    ] {
        assert!(env.data.aggregate(&u, "staff", &agg).await.is_err());
    }
    // Through another collection: relation paths and expand.
    let q = ListQuery {
        filter: Some("owner.salary > 1".into()),
        ..ListQuery::default()
    };
    assert!(env.data.list_records(&u, "tasks", &q).await.is_err());
    let q = ListQuery {
        expand: vec!["owner".into()],
        ..ListQuery::default()
    };
    let t = env.data.list_records(&u, "tasks", &q).await.unwrap();
    let expanded = &t.items[0]["expand"]["owner"];
    assert_eq!(expanded["name"], "Ann");
    assert!(expanded.get("salary").is_none());
    // Writes: read-only for this caller.
    assert!(matches!(
        env.data
            .create_record(&u, "staff", json!({"name": "Bo", "salary": 1}))
            .await,
        Err(DataError::Forbidden(_))
    ));
    assert!(matches!(
        env.data
            .update_record(&u, "staff", &sid, json!({"salary": 1}), UpdateOptions::default())
            .await,
        Err(DataError::Forbidden(_))
    ));
    let renamed = env
        .data
        .update_record(
            &u,
            "staff",
            &sid,
            json!({"name": "Anna"}),
            UpdateOptions {
                replace: true,
                ..UpdateOptions::default()
            },
        )
        .await
        .unwrap();
    assert!(renamed.get("salary").is_none());
    let stored = env
        .data
        .get_record(&admin(), "staff", &sid, &GetOptions::default())
        .await
        .unwrap();
    assert_eq!(
        stored["salary"].as_f64(),
        Some(9000.0),
        "a replacing update keeps the read-only field"
    );
    // HR sees and writes it.
    env.policy
        .put_role(
            &admin(),
            None,
            &RoleInput {
                key: "hr".into(),
                allow: vec!["hr:*".into()],
                ..RoleInput::default()
            },
        )
        .await
        .unwrap();
    env.policy
        .put_realm_binding(&admin(), &pk("hank"), &["hr".into()], &[])
        .await
        .unwrap();
    let hr = user("hank");
    let rec = env
        .data
        .get_record(&hr, "staff", &sid, &GetOptions::default())
        .await
        .unwrap();
    assert_eq!(rec["salary"].as_f64(), Some(9000.0));
    let q = ListQuery {
        filter: Some("salary > 1000".into()),
        ..ListQuery::default()
    };
    assert_eq!(env.data.list_records(&hr, "staff", &q).await.unwrap().items.len(), 1);
}

/// #25: injection attempts into rules, through request values and through
/// the caller's filter; missing collections answer like forbidden ones.
#[tokio::test]
async fn injection_and_existence_oracles_fail() {
    let env = Env::new().await;
    env.collection(
        json!({"name": "docs", "fields": [{"name": "owner", "type": "text"}, {"name": "t", "type": "text"}]}),
    )
    .await;
    for bad in [
        "owner = @request.auth.id; DROP TABLE docs",
        "owner = {:x}",
        "owner = @request.auth.id) OR (1 = 1",
        "nosuchfield = 1",
        "owner = @request.body.id",
    ] {
        let r = env
            .policy
            .put_policy(
                &admin(),
                "docs",
                &PolicyInput {
                    list_rule: Some(bad.into()),
                    ..PolicyInput::default()
                },
            )
            .await;
        assert!(r.is_err(), "{bad} must be refused");
    }
    let owner_rule = Some("owner = @request.auth.id".to_owned());
    env.policy
        .put_policy(
            &admin(),
            "docs",
            &PolicyInput {
                list_rule: owner_rule.clone(),
                view_rule: owner_rule,
                ..PolicyInput::default()
            },
        )
        .await
        .unwrap();
    for (o, t) in [("u1", "a"), ("u2", "b"), ("", "c")] {
        let o = if o.is_empty() { Json::Null } else { Json::from(o) };
        env.data
            .create_record(&AccessContext::system(), "docs", json!({"owner": o, "t": t}))
            .await
            .unwrap();
    }
    // A hostile id is a bound parameter.
    let evil = AccessContext::user("people", "x' OR '1'='1");
    assert!(ids(&env, &evil, "docs").await.is_empty());
    // Anonymous never matches the empty owner.
    assert!(matches!(
        env.data
            .list_records(&AccessContext::anonymous(), "docs", &ListQuery::default())
            .await,
        Err(DataError::Unauthenticated)
    ));
    // The caller's filter is ANDed with the rule: it can only narrow.
    let q = ListQuery {
        filter: Some("owner != 'u1' || 1 = 1".into()),
        ..ListQuery::default()
    };
    let r = env.data.list_records(&user("u1"), "docs", &q).await.unwrap();
    assert_eq!(r.items.len(), 1);
    let q = ListQuery {
        filter: Some("1=1) OR (1=1".into()),
        ..ListQuery::default()
    };
    assert!(env.data.list_records(&user("u1"), "docs", &q).await.is_err());
    // Existence oracle: a locked collection and a missing one look the same.
    env.collection(json!({"name": "vault", "fields": [{"name": "t", "type": "text"}]}))
        .await;
    for ctx in [user("u1"), AccessContext::anonymous()] {
        let real = env
            .data
            .list_records(&ctx, "vault", &ListQuery::default())
            .await
            .unwrap_err();
        let fake = env
            .data
            .list_records(&ctx, "vaulx", &ListQuery::default())
            .await
            .unwrap_err();
        assert_eq!(
            real.to_string().replace("vault", "X"),
            fake.to_string().replace("vaulx", "X")
        );
        let real = env.data.get_collection(&ctx, "VAULT").await.unwrap_err();
        let fake = env.data.get_collection(&ctx, "VAULX").await.unwrap_err();
        assert_eq!(
            real.to_string().replace("VAULT", "X"),
            fake.to_string().replace("VAULX", "X")
        );
    }
    // A rule that stops compiling (its field was dropped) fails closed.
    let mut def = env.data.get_collection(&admin(), "docs").await.unwrap();
    def.fields.retain(|f| f.name != "owner");
    env.data.update_collection(&admin(), "docs", def).await.unwrap();
    assert!(matches!(
        env.data.list_records(&user("u1"), "docs", &ListQuery::default()).await,
        Err(DataError::Forbidden(_))
    ));
}

// ---------------------------------------------------------------------------
// Property test: a random rule never widens access beyond its semantics.
// ---------------------------------------------------------------------------

/// SplitMix64: small, deterministic, no dependency.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn pick(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

/// Three-valued truth.
type T = Option<bool>;

fn and(a: T, b: T) -> T {
    match (a, b) {
        (Some(false), _) | (_, Some(false)) => Some(false),
        (Some(true), Some(true)) => Some(true),
        _ => None,
    }
}

fn or(a: T, b: T) -> T {
    match (a, b) {
        (Some(true), _) | (_, Some(true)) => Some(true),
        (Some(false), Some(false)) => Some(false),
        _ => None,
    }
}

#[derive(Debug, Clone)]
enum Gen {
    And(Box<Gen>, Box<Gen>),
    Or(Box<Gen>, Box<Gen>),
    Not(Box<Gen>),
    /// `n op k`
    Num(&'static str, i64),
    /// `a = 'v'` / `a != 'v'`
    Text(bool, &'static str),
    /// `owner = @request.auth.id` / `!=`
    Owner(bool),
    /// `@request.auth.level >= k` (absent for some callers)
    Level(i64),
    /// `b = true|false`
    Bool(bool),
}

const WORDS: [&str; 3] = ["x", "y", "z"];

fn generate(r: &mut Rng, depth: u32) -> Gen {
    let leaf = depth == 0 || r.pick(3) == 0;
    if leaf {
        return match r.pick(5) {
            0 => Gen::Num(["=", "!=", ">", "<", ">=", "<="][r.pick(6)], r.pick(5) as i64),
            1 => Gen::Text(r.pick(2) == 0, WORDS[r.pick(3)]),
            2 => Gen::Owner(r.pick(2) == 0),
            3 => Gen::Level(r.pick(4) as i64),
            _ => Gen::Bool(r.pick(2) == 0),
        };
    }
    match r.pick(3) {
        0 => Gen::And(Box::new(generate(r, depth - 1)), Box::new(generate(r, depth - 1))),
        1 => Gen::Or(Box::new(generate(r, depth - 1)), Box::new(generate(r, depth - 1))),
        _ => Gen::Not(Box::new(generate(r, depth - 1))),
    }
}

fn render(g: &Gen) -> String {
    match g {
        Gen::And(a, b) => format!("({} && {})", render(a), render(b)),
        Gen::Or(a, b) => format!("({} || {})", render(a), render(b)),
        Gen::Not(a) => format!("!({})", render(a)),
        Gen::Num(op, k) => format!("n {op} {k}"),
        Gen::Text(eq, v) => format!("a {} '{v}'", if *eq { "=" } else { "!=" }),
        Gen::Owner(eq) => format!("owner {} @request.auth.id", if *eq { "=" } else { "!=" }),
        Gen::Level(k) => format!("@request.auth.level >= {k}"),
        Gen::Bool(v) => format!("b = {v}"),
    }
}

struct Row {
    id: String,
    n: Option<i64>,
    a: Option<&'static str>,
    b: Option<bool>,
    owner: Option<&'static str>,
}

struct Caller {
    id: Option<&'static str>,
    level: Option<i64>,
}

/// Reference semantics, written independently of the engine: SQL
/// three-valued logic; `!=` is NULL-safe; a comparison with an absent
/// request value is unknown.
fn eval(g: &Gen, row: &Row, c: &Caller) -> T {
    match g {
        Gen::And(a, b) => and(eval(a, row, c), eval(b, row, c)),
        Gen::Or(a, b) => or(eval(a, row, c), eval(b, row, c)),
        Gen::Not(a) => eval(a, row, c).map(|v| !v),
        Gen::Num(op, k) => match (*op, row.n) {
            ("!=", n) => Some(n != Some(*k)),
            (_, None) => None,
            ("=", Some(n)) => Some(n == *k),
            (">", Some(n)) => Some(n > *k),
            ("<", Some(n)) => Some(n < *k),
            (">=", Some(n)) => Some(n >= *k),
            ("<=", Some(n)) => Some(n <= *k),
            _ => unreachable!(),
        },
        Gen::Text(eq, v) => match (eq, row.a) {
            (false, a) => Some(a != Some(*v)),
            (true, None) => None,
            (true, Some(a)) => Some(a == *v),
        },
        Gen::Owner(eq) => match c.id {
            None => None,
            Some(id) if *eq => row.owner.map(|o| o == id),
            Some(id) => Some(row.owner != Some(id)),
        },
        Gen::Level(k) => c.level.map(|l| l >= *k),
        Gen::Bool(v) => row.b.map(|b| b == *v),
    }
}

#[tokio::test]
async fn random_rules_never_widen_access() {
    let env = Env::new().await;
    env.collection(json!({"name": "items", "fields": [
        {"name": "n", "type": "number", "integer": true},
        {"name": "a", "type": "text"}, {"name": "b", "type": "bool"}, {"name": "owner", "type": "text"}
    ]}))
    .await;
    let mut r = Rng(0xF0_2B_21);
    let mut rows = Vec::new();
    for i in 0..40 {
        let row = Row {
            id: format!("r{i}"),
            n: (r.pick(4) > 0).then(|| r.pick(5) as i64),
            a: (r.pick(4) > 0).then(|| WORDS[r.pick(3)]),
            b: (r.pick(4) > 0).then(|| r.pick(2) == 0),
            owner: (r.pick(4) > 0).then(|| ["u1", "u2", "u3"][r.pick(3)]),
        };
        env.data
            .create_record(
                &AccessContext::system(),
                "items",
                json!({"id": row.id, "n": row.n, "a": row.a, "b": row.b, "owner": row.owner}),
            )
            .await
            .unwrap();
        rows.push(row);
    }
    // Callers: anonymous, users with and without an ABAC attribute.
    let callers: Vec<(AccessContext, Caller)> = vec![
        (AccessContext::anonymous(), Caller { id: None, level: None }),
        (
            user("u1"),
            Caller {
                id: Some("u1"),
                level: None,
            },
        ),
        (
            user("u2").with_attributes(json!({"level": 2}).as_object().unwrap().clone()),
            Caller {
                id: Some("u2"),
                level: Some(2),
            },
        ),
    ];
    let mut checked = 0;
    for case in 0..80 {
        let g = generate(&mut r, 3);
        let rule = render(&g);
        env.policy
            .put_policy(
                &admin(),
                "items",
                &PolicyInput {
                    list_rule: Some(rule.clone()),
                    view_rule: Some(rule.clone()),
                    ..PolicyInput::default()
                },
            )
            .await
            .unwrap_or_else(|e| panic!("case {case}: {rule}: {e}"));
        for (ctx, c) in &callers {
            let expected: BTreeSet<String> = rows
                .iter()
                .filter(|row| eval(&g, row, c) == Some(true))
                .map(|row| row.id.clone())
                .collect();
            let got = match env
                .data
                .list_records(
                    ctx,
                    "items",
                    &ListQuery {
                        per_page: Some(500),
                        ..ListQuery::default()
                    },
                )
                .await
            {
                Ok(l) => l.items.iter().map(|x| x["id"].as_str().unwrap().to_owned()).collect(),
                Err(DataError::Forbidden(_) | DataError::Unauthenticated) => BTreeSet::new(),
                Err(e) => panic!("case {case}: {rule}: {e}"),
            };
            assert!(
                got.is_subset(&expected),
                "case {case}: {rule} widened access for {:?}: {:?}",
                c.id,
                got.difference(&expected)
            );
            assert_eq!(got, expected, "case {case}: {rule} for {:?}", c.id);
            // Records outside the rule are not found one by one either.
            if let Some(outside) = rows.iter().find(|row| !expected.contains(&row.id)) {
                let res = env
                    .data
                    .get_record(ctx, "items", &outside.id, &GetOptions::default())
                    .await;
                assert!(res.is_err(), "case {case}: {rule}: {} visible by id", outside.id);
            }
            checked += 1;
        }
    }
    assert_eq!(checked, 240);
    let _ = Json::Null;
}

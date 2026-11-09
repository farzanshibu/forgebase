//! Every `/api/auth/*` route registered in the HTTP modules is documented in the
//! auth OpenAPI fragment, and every documented operation is a registered route.
#![allow(clippy::unwrap_used, missing_docs)]

use std::collections::BTreeSet;

const SOURCES: &[&str] = &[
    include_str!("../src/http.rs"),
    include_str!("../src/social/http.rs"),
    include_str!("../src/webauthn/http.rs"),
    include_str!("../src/identity_http.rs"),
];

const METHODS: &[&str] = &["get", "post", "put", "patch", "delete"];

/// `(method, "/api/auth" + path)` of every `.route("path", method(handler)...)` call.
fn routed() -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for src in SOURCES {
        let mut rest = *src;
        while let Some(at) = rest.find(".route(") {
            let args_start = at + ".route(".len();
            let tail = &rest[args_start..];
            // The balanced argument list of this `.route(`.
            let mut depth = 1usize;
            let mut end = tail.len();
            for (i, c) in tail.char_indices() {
                match c {
                    '(' => depth += 1,
                    ')' => {
                        depth -= 1;
                        if depth == 0 {
                            end = i;
                            break;
                        }
                    }
                    _ => {}
                }
            }
            let args = &tail[..end];
            let q1 = args.find('"').unwrap();
            let q2 = q1 + 1 + args[q1 + 1..].find('"').unwrap();
            let path = &args[q1 + 1..q2];
            let after = &args[q2 + 1..];
            for m in METHODS {
                let needle = format!("{m}(");
                let mut from = 0;
                while let Some(p) = after[from..].find(&needle) {
                    let idx = from + p;
                    let boundary = after[..idx]
                        .chars()
                        .next_back()
                        .is_none_or(|c| !(c.is_alphanumeric() || c == '_'));
                    if boundary {
                        out.insert(((*m).to_owned(), format!("/api/auth{path}")));
                    }
                    from = idx + needle.len();
                }
            }
            rest = &tail[end..];
        }
    }
    out
}

fn documented() -> BTreeSet<(String, String)> {
    let doc = serde_json::to_value(forgebase_auth::openapi()).unwrap();
    let mut out = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for method in item.as_object().unwrap().keys() {
            out.insert((method.clone(), path.clone()));
        }
    }
    out
}

#[test]
fn every_routed_auth_operation_is_documented_and_vice_versa() {
    let routed = routed();
    assert!(routed.len() > 70, "route walk found only {}", routed.len());
    let documented = documented();
    let missing: Vec<_> = routed.difference(&documented).collect();
    let extra: Vec<_> = documented.difference(&routed).collect();
    assert!(missing.is_empty(), "routed but not in the OpenAPI fragment: {missing:?}");
    assert!(extra.is_empty(), "in the OpenAPI fragment but not routed: {extra:?}");
}

#[test]
fn auth_operation_ids_are_unique() {
    let doc = serde_json::to_value(forgebase_auth::openapi()).unwrap();
    let mut seen = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (method, op) in item.as_object().unwrap() {
            let id = op["operationId"].as_str().unwrap_or_else(|| panic!("{method} {path} has no operationId"));
            assert!(seen.insert(id.to_owned()), "duplicate operationId {id} ({method} {path})");
        }
    }
}

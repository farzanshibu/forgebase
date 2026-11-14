//! Minimal valid example values built from the checked-in OpenAPI document: every required
//! property filled, so the generated models decode them. Also keeps the models honest against the
//! document (a schema the generator and the SDK disagree on fails to decode here).
#![allow(dead_code, missing_docs)]

use std::sync::OnceLock;

use serde_json::{Map, Value, json};

fn doc() -> &'static Value {
    static DOC: OnceLock<Value> = OnceLock::new();
    DOC.get_or_init(|| serde_json::from_str(include_str!("../../openapi/openapi.json")).expect("openapi.json"))
}

/// An example of the schema `name` from `components.schemas`.
pub fn schema(name: &str) -> Value {
    let s = doc()
        .pointer(&format!("/components/schemas/{name}"))
        .unwrap_or_else(|| panic!("no schema {name}"));
    build(s, 0)
}

/// An example with some properties replaced: `sample("Balance", json!({"balance": 5}))`.
pub fn with(name: &str, patch: Value) -> Value {
    let mut v = schema(name);
    if let (Value::Object(m), Value::Object(p)) = (&mut v, patch) {
        m.extend(p);
    }
    v
}

fn build(s: &Value, depth: usize) -> Value {
    if depth > 12 {
        return Value::Null;
    }
    if let Some(r) = s.get("$ref").and_then(Value::as_str) {
        let target = doc().pointer(r.trim_start_matches('#')).expect("ref target");
        return build(target, depth + 1);
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(Value::Array(opts)) = s.get(key) {
            let pick = opts
                .iter()
                .find(|o| o.get("type") != Some(&json!("null")))
                .or(opts.first());
            return pick.map_or(Value::Null, |o| build(o, depth + 1));
        }
    }
    if let Some(Value::Array(parts)) = s.get("allOf") {
        let mut out = Map::new();
        for p in parts {
            if let Value::Object(m) = build(p, depth + 1) {
                out.extend(m);
            }
        }
        return Value::Object(out);
    }
    if let Some(Value::Array(e)) = s.get("enum") {
        return e.first().cloned().unwrap_or(Value::Null);
    }
    let ty = match s.get("type") {
        Some(Value::String(t)) => t.as_str(),
        Some(Value::Array(ts)) => ts
            .iter()
            .filter_map(Value::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null"),
        _ => {
            if s.get("properties").is_some() {
                "object"
            } else {
                return json!({});
            }
        }
    };
    match ty {
        "object" => {
            let mut out = Map::new();
            let required: Vec<&str> = s
                .get("required")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            if let Some(Value::Object(props)) = s.get("properties") {
                for (k, v) in props {
                    if required.contains(&k.as_str()) {
                        out.insert(k.clone(), build(v, depth + 1));
                    }
                }
            }
            Value::Object(out)
        }
        "array" => Value::Array(Vec::new()),
        "string" => json!("x"),
        "integer" => json!(0),
        "number" => json!(0.0),
        "boolean" => json!(false),
        _ => Value::Null,
    }
}

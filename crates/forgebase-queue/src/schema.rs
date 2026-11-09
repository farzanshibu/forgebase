//! A JSON Schema validator for message data (a subset of draft 2020-12).
//!
//! A topic may carry a schema that every published message's `data` must
//! satisfy. The workspace has no JSON Schema dependency, so this module
//! implements the keywords that describe message shapes:
//!
//! * `type` (one name or a list; `integer` accepts whole numbers),
//! * `enum`, `const`,
//! * objects: `properties`, `required`, `additionalProperties` (boolean or
//!   schema), `minProperties`, `maxProperties`,
//! * arrays: `items` (one schema), `minItems`, `maxItems`, `uniqueItems`,
//! * strings: `minLength`, `maxLength` (in characters),
//! * numbers: `minimum`, `maximum`, `exclusiveMinimum`, `exclusiveMaximum`,
//!   `multipleOf`,
//! * combinators: `allOf`, `anyOf`, `oneOf`, `not`,
//! * annotations that are accepted and ignored: `$schema`, `$id`,
//!   `$comment`, `title`, `description`, `default`, `examples`, `format`,
//!   `deprecated`, `readOnly`, `writeOnly`.
//!
//! Any other keyword (`$ref`, `pattern`, `if`, ...) makes the schema invalid
//! when the topic is created, so a rule is never silently unenforced.

use serde_json::Value as Json;

const ANNOTATIONS: &[&str] = &[
    "$schema",
    "$id",
    "$comment",
    "title",
    "description",
    "default",
    "examples",
    "format",
    "deprecated",
    "readOnly",
    "writeOnly",
];

const TYPES: &[&str] = &["null", "boolean", "object", "array", "number", "string", "integer"];

/// Largest schema nesting accepted.
const MAX_DEPTH: usize = 32;

/// Checks that `schema` only uses supported keywords with well-formed values.
pub fn check_schema(schema: &Json) -> Result<(), String> {
    check(schema, "#", 0)
}

fn check(schema: &Json, at: &str, depth: usize) -> Result<(), String> {
    if depth > MAX_DEPTH {
        return Err(format!("{at}: schema is nested too deeply"));
    }
    let obj = match schema {
        Json::Bool(_) => return Ok(()),
        Json::Object(o) => o,
        _ => return Err(format!("{at}: a schema must be an object or a boolean")),
    };
    for (k, v) in obj {
        let here = format!("{at}/{k}");
        match k.as_str() {
            k if ANNOTATIONS.contains(&k) => {}
            "type" => {
                let ok = |t: &Json| t.as_str().is_some_and(|s| TYPES.contains(&s));
                let valid = match v {
                    Json::Array(list) => !list.is_empty() && list.iter().all(ok),
                    other => ok(other),
                };
                if !valid {
                    return Err(format!("{here}: unknown type"));
                }
            }
            "enum" => {
                if !v.is_array() {
                    return Err(format!("{here}: must be an array"));
                }
            }
            "const" => {}
            "properties" => {
                let Some(props) = v.as_object() else {
                    return Err(format!("{here}: must be an object"));
                };
                for (name, s) in props {
                    check(s, &format!("{here}/{name}"), depth + 1)?;
                }
            }
            "required" => {
                if !v.as_array().is_some_and(|a| a.iter().all(Json::is_string)) {
                    return Err(format!("{here}: must be an array of strings"));
                }
            }
            "additionalProperties" | "items" | "not" => check(v, &here, depth + 1)?,
            "allOf" | "anyOf" | "oneOf" => {
                let Some(list) = v.as_array().filter(|a| !a.is_empty()) else {
                    return Err(format!("{here}: must be a non-empty array"));
                };
                for (i, s) in list.iter().enumerate() {
                    check(s, &format!("{here}/{i}"), depth + 1)?;
                }
            }
            "minProperties" | "maxProperties" | "minItems" | "maxItems" | "minLength" | "maxLength" => {
                if v.as_u64().is_none() {
                    return Err(format!("{here}: must be a non-negative integer"));
                }
            }
            "uniqueItems" => {
                if !v.is_boolean() {
                    return Err(format!("{here}: must be a boolean"));
                }
            }
            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum" => {
                if !v.is_number() {
                    return Err(format!("{here}: must be a number"));
                }
            }
            "multipleOf" => {
                if !v.as_f64().is_some_and(|f| f > 0.0) {
                    return Err(format!("{here}: must be a positive number"));
                }
            }
            other => return Err(format!("{at}: keyword {other:?} is not supported")),
        }
    }
    Ok(())
}

/// Validates `value` against a schema that passed [`check_schema`]. Returns
/// the first violation as `"<json pointer>: <reason>"`.
pub fn validate(schema: &Json, value: &Json) -> Result<(), String> {
    validate_at(schema, value, "")
}

fn type_name(v: &Json) -> &'static str {
    match v {
        Json::Null => "null",
        Json::Bool(_) => "boolean",
        Json::Object(_) => "object",
        Json::Array(_) => "array",
        Json::Number(_) => "number",
        Json::String(_) => "string",
    }
}

fn is_type(v: &Json, t: &str) -> bool {
    match t {
        "integer" => v.as_i64().is_some() || v.as_u64().is_some() || v.as_f64().is_some_and(|f| f.fract() == 0.0),
        "number" => v.is_number(),
        other => type_name(v) == other,
    }
}

fn path(at: &str) -> &str {
    if at.is_empty() { "/" } else { at }
}

#[allow(clippy::too_many_lines)]
fn validate_at(schema: &Json, value: &Json, at: &str) -> Result<(), String> {
    let obj = match schema {
        Json::Bool(true) => return Ok(()),
        Json::Bool(false) => return Err(format!("{}: no value is allowed here", path(at))),
        Json::Object(o) => o,
        _ => return Ok(()),
    };
    let fail = |why: String| Err(format!("{}: {why}", path(at)));
    if let Some(t) = obj.get("type") {
        let ok = match t {
            Json::Array(list) => list.iter().filter_map(Json::as_str).any(|t| is_type(value, t)),
            other => other.as_str().is_some_and(|t| is_type(value, t)),
        };
        if !ok {
            return fail(format!("expected type {t}, got {}", type_name(value)));
        }
    }
    if let Some(list) = obj.get("enum").and_then(Json::as_array)
        && !list.iter().any(|e| json_eq(e, value))
    {
        return fail("value is not one of the allowed values".into());
    }
    if let Some(c) = obj.get("const")
        && !json_eq(c, value)
    {
        return fail(format!("value must be {c}"));
    }
    match value {
        Json::Object(map) => {
            if let Some(req) = obj.get("required").and_then(Json::as_array) {
                for name in req.iter().filter_map(Json::as_str) {
                    if !map.contains_key(name) {
                        return fail(format!("missing required property {name:?}"));
                    }
                }
            }
            let props = obj.get("properties").and_then(Json::as_object);
            for (k, v) in map {
                let child = format!("{at}/{}", k.replace('~', "~0").replace('/', "~1"));
                match props.and_then(|p| p.get(k)) {
                    Some(s) => validate_at(s, v, &child)?,
                    None => {
                        if let Some(extra) = obj.get("additionalProperties") {
                            if extra == &Json::Bool(false) {
                                return fail(format!("property {k:?} is not allowed"));
                            }
                            validate_at(extra, v, &child)?;
                        }
                    }
                }
            }
            let n = map.len() as u64;
            if obj.get("minProperties").and_then(Json::as_u64).is_some_and(|m| n < m) {
                return fail("too few properties".into());
            }
            if obj.get("maxProperties").and_then(Json::as_u64).is_some_and(|m| n > m) {
                return fail("too many properties".into());
            }
        }
        Json::Array(items) => {
            if let Some(s) = obj.get("items") {
                for (i, v) in items.iter().enumerate() {
                    validate_at(s, v, &format!("{at}/{i}"))?;
                }
            }
            let n = items.len() as u64;
            if obj.get("minItems").and_then(Json::as_u64).is_some_and(|m| n < m) {
                return fail(format!("at least {} items are required", obj["minItems"]));
            }
            if obj.get("maxItems").and_then(Json::as_u64).is_some_and(|m| n > m) {
                return fail(format!("at most {} items are allowed", obj["maxItems"]));
            }
            if obj.get("uniqueItems") == Some(&Json::Bool(true)) {
                for (i, a) in items.iter().enumerate() {
                    if items[i + 1..].iter().any(|b| json_eq(a, b)) {
                        return fail("items must be unique".into());
                    }
                }
            }
        }
        Json::String(s) => {
            let n = s.chars().count() as u64;
            if obj.get("minLength").and_then(Json::as_u64).is_some_and(|m| n < m) {
                return fail(format!("must be at least {} characters", obj["minLength"]));
            }
            if obj.get("maxLength").and_then(Json::as_u64).is_some_and(|m| n > m) {
                return fail(format!("must be at most {} characters", obj["maxLength"]));
            }
        }
        Json::Number(num) => {
            let x = num.as_f64().unwrap_or(0.0);
            let bound = |k: &str| obj.get(k).and_then(Json::as_f64);
            if bound("minimum").is_some_and(|m| x < m) {
                return fail(format!("must be at least {}", obj["minimum"]));
            }
            if bound("maximum").is_some_and(|m| x > m) {
                return fail(format!("must be at most {}", obj["maximum"]));
            }
            if bound("exclusiveMinimum").is_some_and(|m| x <= m) {
                return fail(format!("must be greater than {}", obj["exclusiveMinimum"]));
            }
            if bound("exclusiveMaximum").is_some_and(|m| x >= m) {
                return fail(format!("must be less than {}", obj["exclusiveMaximum"]));
            }
            if let Some(m) = bound("multipleOf") {
                let q = x / m;
                if (q - q.round()).abs() > 1e-9 {
                    return fail(format!("must be a multiple of {}", obj["multipleOf"]));
                }
            }
        }
        _ => {}
    }
    if let Some(list) = obj.get("allOf").and_then(Json::as_array) {
        for s in list {
            validate_at(s, value, at)?;
        }
    }
    if let Some(list) = obj.get("anyOf").and_then(Json::as_array)
        && !list.iter().any(|s| validate_at(s, value, at).is_ok())
    {
        return fail("value matches none of anyOf".into());
    }
    if let Some(list) = obj.get("oneOf").and_then(Json::as_array) {
        let n = list.iter().filter(|s| validate_at(s, value, at).is_ok()).count();
        if n != 1 {
            return fail(format!("value must match exactly one of oneOf (matched {n})"));
        }
    }
    if let Some(s) = obj.get("not")
        && validate_at(s, value, at).is_ok()
    {
        return fail("value matches a schema it must not match".into());
    }
    Ok(())
}

/// JSON equality with numbers compared by value (`1` equals `1.0`).
fn json_eq(a: &Json, b: &Json) -> bool {
    match (a, b) {
        (Json::Number(x), Json::Number(y)) => x.as_f64() == y.as_f64(),
        (Json::Array(x), Json::Array(y)) => x.len() == y.len() && x.iter().zip(y).all(|(p, q)| json_eq(p, q)),
        (Json::Object(x), Json::Object(y)) => {
            x.len() == y.len() && x.iter().all(|(k, v)| y.get(k).is_some_and(|w| json_eq(v, w)))
        }
        _ => a == b,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unsupported_keywords_are_rejected() {
        assert!(check_schema(&json!({"type": "object", "pattern": "x"})).is_err());
        assert!(check_schema(&json!({"$ref": "#/x"})).is_err());
        assert!(check_schema(&json!({"type": "thing"})).is_err());
        assert!(check_schema(&json!({"title": "ok", "type": ["string", "null"]})).is_ok());
    }

    #[test]
    fn validates_objects_arrays_and_numbers() {
        let s = json!({
            "type": "object",
            "required": ["id", "qty"],
            "properties": {
                "id": {"type": "string", "minLength": 1},
                "qty": {"type": "integer", "minimum": 1},
                "tags": {"type": "array", "items": {"enum": ["a", "b"]}, "uniqueItems": true}
            },
            "additionalProperties": false
        });
        check_schema(&s).unwrap();
        assert!(validate(&s, &json!({"id": "x", "qty": 2, "tags": ["a"]})).is_ok());
        assert!(validate(&s, &json!({"id": "x"})).unwrap_err().contains("qty"));
        assert!(
            validate(&s, &json!({"id": "x", "qty": 0}))
                .unwrap_err()
                .contains("/qty")
        );
        assert!(validate(&s, &json!({"id": "x", "qty": 1.5})).is_err());
        assert!(validate(&s, &json!({"id": "x", "qty": 1, "extra": 1})).is_err());
        assert!(validate(&s, &json!({"id": "x", "qty": 1, "tags": ["a", "a"]})).is_err());
        assert!(validate(&s, &json!({"id": "x", "qty": 1, "tags": ["c"]})).is_err());
    }

    #[test]
    fn combinators() {
        let s = json!({"oneOf": [{"type": "string"}, {"type": "integer"}], "not": {"const": "no"}});
        check_schema(&s).unwrap();
        assert!(validate(&s, &json!("yes")).is_ok());
        assert!(validate(&s, &json!(3)).is_ok());
        assert!(validate(&s, &json!("no")).is_err());
        assert!(validate(&s, &json!(true)).is_err());
    }
}

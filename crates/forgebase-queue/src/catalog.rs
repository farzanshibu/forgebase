//! The event catalog (ADR-0031 §6): every topic the caller may read, with
//! what its messages look like, so webhook consumers know what they can
//! subscribe to.
//!
//! * A plain topic: its `description`, its JSON Schema (`data_schema`) and
//!   an example of `data`: the topic's `example`, else one derived from the
//!   schema ([`example_of`]).
//! * A record-source topic: one event type per relayed operation
//!   (`record.created`, `record.updated`, `record.deleted`), each with the
//!   subscription filter that selects it, the schema of the change message
//!   (the record described by the collection's fields, as far as the caller
//!   may read the collection's schema) and a derived example.

use forgebase_data::AccessContext;
use forgebase_data::schema::{ArrayItems, Collection, FieldKind, SYSTEM_FIELDS};
use serde_json::{Map, Value as Json, json};

use crate::model::{CatalogEventType, CatalogTopic, EventCatalog, ExampleSource, Topic};
use crate::service::{QueueService, Result};

/// Deepest schema nesting an example is derived for.
const MAX_DEPTH: usize = 16;

/// An example value for `schema` (a subset of JSON Schema, as topic schemas
/// use): `const`, the first `examples` entry, `default`, the first `enum`
/// value, else one built from `type` (objects with every property, arrays
/// with one item, bounds respected).
pub fn example_of(schema: &Json) -> Json {
    example_at(schema, 0)
}

fn example_at(schema: &Json, depth: usize) -> Json {
    let Some(o) = schema.as_object() else {
        return Json::Null;
    };
    if depth > MAX_DEPTH {
        return Json::Null;
    }
    if let Some(v) = o.get("const") {
        return v.clone();
    }
    if let Some(v) = o.get("examples").and_then(Json::as_array).and_then(|a| a.first()) {
        return v.clone();
    }
    if let Some(v) = o.get("default") {
        return v.clone();
    }
    if let Some(v) = o.get("enum").and_then(Json::as_array).and_then(|a| a.first()) {
        return v.clone();
    }
    for key in ["oneOf", "anyOf"] {
        if let Some(first) = o.get(key).and_then(Json::as_array).and_then(|a| a.first()) {
            return example_at(first, depth + 1);
        }
    }
    if let Some(all) = o.get("allOf").and_then(Json::as_array) {
        let mut merged = Map::new();
        for part in all {
            match example_at(part, depth + 1) {
                Json::Object(m) => merged.extend(m),
                other if merged.is_empty() => return other,
                _ => {}
            }
        }
        return Json::Object(merged);
    }
    let ty = match o.get("type") {
        Some(Json::String(t)) => t.as_str(),
        // The first non-null type of a list.
        Some(Json::Array(list)) => list
            .iter()
            .filter_map(Json::as_str)
            .find(|t| *t != "null")
            .unwrap_or("null"),
        _ if o.contains_key("properties") => "object",
        _ if o.contains_key("items") => "array",
        _ => return Json::Null,
    };
    match ty {
        "object" => {
            let mut out = Map::new();
            if let Some(props) = o.get("properties").and_then(Json::as_object) {
                for (k, v) in props {
                    out.insert(k.clone(), example_at(v, depth + 1));
                }
            }
            Json::Object(out)
        }
        "array" => {
            let item = o.get("items").map_or(Json::Null, |i| example_at(i, depth + 1));
            let n = o.get("minItems").and_then(Json::as_u64).unwrap_or(1).clamp(1, 3);
            Json::Array((0..n).map(|_| item.clone()).collect())
        }
        "string" => {
            let s = match o.get("format").and_then(Json::as_str) {
                Some("date-time") => "2026-01-01T00:00:00.000Z".to_owned(),
                Some("date") => "2026-01-01".to_owned(),
                Some("email") => "user@example.com".to_owned(),
                Some("uri" | "url") => "https://example.com/".to_owned(),
                Some("uuid") => "00000000-0000-4000-8000-000000000000".to_owned(),
                _ => "string".to_owned(),
            };
            let min = o.get("minLength").and_then(Json::as_u64).unwrap_or(0);
            let max = o.get("maxLength").and_then(Json::as_u64);
            let mut s = s;
            while (s.chars().count() as u64) < min {
                s.push('x');
            }
            if let Some(max) = max {
                s = s.chars().take(usize::try_from(max).unwrap_or(usize::MAX)).collect();
            }
            Json::String(s)
        }
        "integer" | "number" => {
            let lo = o
                .get("minimum")
                .and_then(Json::as_f64)
                .or_else(|| o.get("exclusiveMinimum").and_then(Json::as_f64).map(|m| m + 1.0));
            let hi = o
                .get("maximum")
                .and_then(Json::as_f64)
                .or_else(|| o.get("exclusiveMaximum").and_then(Json::as_f64).map(|m| m - 1.0));
            let mut v = lo.unwrap_or(0.0);
            if let Some(h) = hi
                && v > h
            {
                v = h;
            }
            if ty == "integer" {
                #[allow(clippy::cast_possible_truncation)]
                let i = v.ceil() as i64;
                json!(i)
            } else {
                json!(v)
            }
        }
        "boolean" => Json::Bool(true),
        _ => Json::Null,
    }
}

/// The JSON Schema of one field's value.
fn field_schema(kind: &FieldKind, max_select_array: bool) -> Json {
    let many = |one: Json, max: u32| {
        if max > 1 && max_select_array {
            json!({"type": "array", "items": one, "maxItems": max})
        } else {
            one
        }
    };
    match kind {
        FieldKind::Text { min_length, max_length } => {
            let mut s = json!({"type": "string"});
            if let Some(m) = min_length {
                s["minLength"] = json!(m);
            }
            if let Some(m) = max_length {
                s["maxLength"] = json!(m);
            }
            s
        }
        FieldKind::Number { min, max, integer } => {
            let mut s = json!({"type": if *integer { "integer" } else { "number" }});
            if let Some(m) = min {
                s["minimum"] = json!(m);
            }
            if let Some(m) = max {
                s["maximum"] = json!(m);
            }
            s
        }
        FieldKind::Bool => json!({"type": "boolean"}),
        FieldKind::Date => json!({"type": "string", "format": "date-time"}),
        FieldKind::Email => json!({"type": "string", "format": "email"}),
        FieldKind::Url => json!({"type": "string", "format": "uri"}),
        FieldKind::Select { values, max_select } => many(json!({"type": "string", "enum": values}), *max_select),
        FieldKind::Json { .. } => json!({}),
        FieldKind::Array { items, .. } => {
            let item = match items {
                ArrayItems::Text => json!({"type": "string"}),
                ArrayItems::Number => json!({"type": "number"}),
                ArrayItems::Bool => json!({"type": "boolean"}),
                ArrayItems::Json => json!({}),
            };
            json!({"type": "array", "items": item})
        }
        FieldKind::Relation { max_select, .. } => many(
            json!({"type": "string", "description": "id of the related record"}),
            *max_select,
        ),
        FieldKind::File { max_select } => many(json!({"type": "string", "description": "file name"}), *max_select),
        FieldKind::Geo => json!({
            "type": "object",
            "properties": {"lon": {"type": "number"}, "lat": {"type": "number"}},
            "required": ["lon", "lat"],
        }),
        FieldKind::Vector { dimensions } => json!({
            "type": "array", "items": {"type": "number"}, "minItems": 1, "maxItems": dimensions,
        }),
        // Never returned.
        FieldKind::Password { .. } => Json::Null,
    }
}

/// The JSON Schema of a record of `c` (system fields, then the declared
/// ones; hidden and password fields left out: they are not delivered to
/// most readers).
fn record_schema(c: &Collection) -> Json {
    let mut props = Map::new();
    props.insert("id".into(), json!({"type": "string"}));
    props.insert("created".into(), json!({"type": "string", "format": "date-time"}));
    props.insert("updated".into(), json!({"type": "string", "format": "date-time"}));
    for f in &c.fields {
        if f.hidden || SYSTEM_FIELDS.contains(&f.name.to_ascii_lowercase().as_str()) {
            continue;
        }
        let s = field_schema(&f.kind, true);
        if s.is_null() {
            continue;
        }
        props.insert(f.name.clone(), s);
    }
    json!({"type": "object", "properties": props, "additionalProperties": true})
}

/// The schema of a record-change message's `data` for `op`.
fn change_schema(op: &str, collection: &str, record: &Json) -> Json {
    json!({
        "type": "object",
        "properties": {
            "op": {"const": op},
            "collection": {"type": "string", "examples": [collection]},
            "record_id": {"type": "string"},
            "record": {
                "description": "the record as the subscription's owner may see it (null when withheld)",
                "anyOf": [record, {"type": "null"}],
            },
            "tenant": {"type": ["string", "null"]},
            "actor": {"type": "string", "examples": ["user:users/u1"]},
            "changed_at": {"type": "string", "format": "date-time"},
            "outbox_seq": {"type": "integer", "minimum": 1},
        },
        "required": ["op", "collection", "record_id", "record", "tenant", "actor", "changed_at", "outbox_seq"],
    })
}

impl QueueService {
    /// The event catalog: every topic the caller may read (as
    /// [`QueueService::topics`] lists them), with description, data schema,
    /// example, and for record-source topics their event types.
    pub async fn catalog(&self, ctx: &AccessContext) -> Result<EventCatalog> {
        let topics = self.topics(ctx).await?;
        let mut out = Vec::with_capacity(topics.len());
        for t in topics {
            out.push(self.catalog_topic(ctx, t).await);
        }
        Ok(EventCatalog { topics: out })
    }

    async fn catalog_topic(&self, ctx: &AccessContext, t: Topic) -> CatalogTopic {
        let Some(src) = t.source.clone() else {
            let (example, example_source) = match (&t.example, &t.schema) {
                (Some(e), _) => (e.clone(), ExampleSource::Topic),
                (None, Some(s)) => (example_of(s), ExampleSource::Schema),
                (None, None) => (Json::Null, ExampleSource::None),
            };
            return CatalogTopic {
                topic: t.name,
                tenant: t.tenant,
                description: t.description,
                data_schema: t.schema,
                example,
                example_source,
                source: None,
                event_types: Vec::new(),
            };
        };
        // The record's shape, if the caller may read the collection's schema.
        let collection = self.inner.data.get_collection(ctx, &src.collection).await.ok();
        let record = collection
            .as_ref()
            .map_or_else(|| json!({"type": "object"}), record_schema);
        let name = collection.as_ref().map_or(src.collection.as_str(), |c| c.name.as_str());
        let ops: Vec<&str> = if src.ops.is_empty() {
            vec!["create", "update", "delete"]
        } else {
            ["create", "update", "delete"]
                .into_iter()
                .filter(|o| src.ops.iter().any(|s| s == o))
                .collect()
        };
        let event_types: Vec<CatalogEventType> = ops
            .iter()
            .map(|op| {
                let (ty, what) = match *op {
                    "create" => ("record.created", "a record was created"),
                    "update" => ("record.updated", "a record was changed (the record after the change)"),
                    _ => ("record.deleted", "a record was deleted (the record before it)"),
                };
                let data_schema = change_schema(op, name, &record);
                let mut example = example_of(&data_schema);
                if let Some(o) = example.as_object_mut() {
                    o.insert("collection".into(), json!(name));
                    o.insert("record".into(), example_of(&record));
                    o.insert("record_id".into(), json!("rec_0123456789"));
                    o.insert("tenant".into(), Json::Null);
                }
                CatalogEventType {
                    event_type: ty.to_owned(),
                    op: (*op).to_owned(),
                    description: format!("{what} in collection {name:?}"),
                    filter: json!({"forgebase.op": op}),
                    data_schema,
                    example,
                }
            })
            .collect();
        let (example, example_source) = match (&t.example, event_types.first()) {
            (Some(e), _) => (e.clone(), ExampleSource::Topic),
            (None, Some(first)) => (first.example.clone(), ExampleSource::Schema),
            (None, None) => (Json::Null, ExampleSource::None),
        };
        CatalogTopic {
            topic: t.name,
            tenant: t.tenant,
            description: t
                .description
                .or_else(|| Some(format!("record changes of collection {name:?} ({})", ops.join(", ")))),
            data_schema: (!event_types.is_empty())
                .then(|| json!({"oneOf": event_types.iter().map(|e| e.data_schema.clone()).collect::<Vec<_>>()})),
            example,
            example_source,
            source: Some(src),
            event_types,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn examples_follow_the_schema() {
        let s = json!({
            "type": "object",
            "properties": {
                "id": {"type": "integer", "minimum": 5},
                "name": {"type": "string", "minLength": 8},
                "kind": {"enum": ["new", "old"]},
                "tags": {"type": "array", "items": {"type": "string"}},
                "at": {"type": "string", "format": "date-time"},
                "paid": {"type": "boolean"},
                "note": {"type": ["null", "string"]},
                "fixed": {"const": 3},
                "price": {"type": "number", "maximum": -1},
            },
        });
        let e = example_of(&s);
        assert_eq!(e["id"], 5);
        assert_eq!(e["name"].as_str().unwrap().len(), 8);
        assert_eq!(e["kind"], "new");
        assert_eq!(e["tags"], json!(["string"]));
        assert_eq!(e["at"], "2026-01-01T00:00:00.000Z");
        assert_eq!(e["paid"], true);
        assert_eq!(e["note"], "string");
        assert_eq!(e["fixed"], 3);
        assert_eq!(e["price"], json!(-1.0));
        assert!(crate::schema::validate(&s, &e).is_ok(), "{e}");
    }

    #[test]
    fn change_examples_validate_against_their_schema() {
        let record = json!({"type": "object", "properties": {"title": {"type": "string"}}});
        let s = change_schema("create", "posts", &record);
        crate::schema::check_schema(&s).unwrap();
        let e = example_of(&s);
        assert_eq!(e["op"], "create");
        assert!(crate::schema::validate(&s, &e).is_ok(), "{e}");
    }
}

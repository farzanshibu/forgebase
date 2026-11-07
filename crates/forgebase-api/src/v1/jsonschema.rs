//! JSON Schema (draft 2020-12) of a collection's records.

use forgebase_data::schema::{ArrayItems, Collection, Field, FieldKind, SchemaMode};
use serde_json::{Map, Value as Json, json};

fn insert_opt<T: Into<Json>>(m: &mut Map<String, Json>, key: &str, v: Option<T>) {
    if let Some(v) = v {
        m.insert(key.to_owned(), v.into());
    }
}

fn single_or_many(base: Json, max_select: u32) -> Json {
    if max_select > 1 {
        json!({ "type": "array", "items": base, "maxItems": max_select, "uniqueItems": true })
    } else {
        base
    }
}

fn field_schema(f: &Field) -> Json {
    let mut s = match &f.kind {
        FieldKind::Text { min_length, max_length } => {
            let mut m = Map::from_iter([("type".to_owned(), json!("string"))]);
            insert_opt(&mut m, "minLength", *min_length);
            insert_opt(&mut m, "maxLength", *max_length);
            Json::Object(m)
        }
        FieldKind::Number { min, max, integer } => {
            let mut m = Map::from_iter([("type".to_owned(), json!(if *integer { "integer" } else { "number" }))]);
            insert_opt(&mut m, "minimum", *min);
            insert_opt(&mut m, "maximum", *max);
            Json::Object(m)
        }
        FieldKind::Bool => json!({ "type": "boolean" }),
        FieldKind::Date => json!({ "type": "string", "format": "date-time" }),
        FieldKind::Email => json!({ "type": "string", "format": "email" }),
        FieldKind::Url => json!({ "type": "string", "format": "uri" }),
        FieldKind::Select { values, max_select } => {
            single_or_many(json!({ "type": "string", "enum": values }), *max_select)
        }
        FieldKind::Json { .. } => json!({}),
        FieldKind::Array {
            items,
            min_items,
            max_items,
        } => {
            let item = match items {
                ArrayItems::Text => json!({ "type": "string" }),
                ArrayItems::Number => json!({ "type": "number" }),
                ArrayItems::Bool => json!({ "type": "boolean" }),
                ArrayItems::Json => json!({}),
            };
            let mut m = Map::from_iter([("type".to_owned(), json!("array")), ("items".to_owned(), item)]);
            insert_opt(&mut m, "minItems", *min_items);
            insert_opt(&mut m, "maxItems", *max_items);
            Json::Object(m)
        }
        FieldKind::Relation { max_select, .. } => {
            single_or_many(json!({ "type": "string", "description": "record id" }), *max_select)
        }
        FieldKind::File { max_select } => single_or_many(json!({ "type": "string" }), *max_select),
        FieldKind::Password { min_length } => {
            json!({ "type": "string", "minLength": min_length, "writeOnly": true })
        }
        FieldKind::Geo => json!({
            "type": "object",
            "properties": {
                "lat": { "type": "number", "minimum": -90, "maximum": 90 },
                "lon": { "type": "number", "minimum": -180, "maximum": 180 },
            },
            "required": ["lat", "lon"],
        }),
        FieldKind::Vector { dimensions } => json!({
            "type": "array", "items": { "type": "number" },
            "minItems": dimensions, "maxItems": dimensions,
        }),
    };
    if let (Some(d), Json::Object(m)) = (&f.default, &mut s) {
        m.insert("default".to_owned(), d.clone());
    }
    s
}

/// The JSON Schema of one record of `coll`. Hidden fields are described only
/// when `privileged` (they are never returned to anyone else); password
/// fields are write-only.
pub fn record_schema(coll: &Collection, privileged: bool) -> Json {
    let mut props = Map::new();
    props.insert("id".into(), json!({ "type": "string", "readOnly": true }));
    props.insert(
        "created".into(),
        json!({ "type": "string", "format": "date-time", "readOnly": true }),
    );
    props.insert(
        "updated".into(),
        json!({ "type": "string", "format": "date-time", "readOnly": true }),
    );
    props.insert("_version".into(), json!({ "type": "integer", "readOnly": true }));
    let mut required = Vec::new();
    for f in coll.fields.iter().filter(|f| privileged || !f.hidden) {
        props.insert(f.name.clone(), field_schema(f));
        if f.required {
            required.push(Json::String(f.name.clone()));
        }
    }
    json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "title": coll.name,
        "type": "object",
        "properties": props,
        "required": required,
        "additionalProperties": coll.schema_mode == SchemaMode::Flexible,
    })
}

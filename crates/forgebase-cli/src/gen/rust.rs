//! Rust emitter: `serde` structs for `forgebase_sdk::Records<T>`
//! (`fb.collection("todos").typed::<Todos>()`), an input struct per collection and an
//! enum per `select` field.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use super::model::{Collection, Field, Item, Kind, Model, Ty, pascal, snake, type_names};

/// Version of the generated layout; bumped when the shape of the output changes.
pub const FORMAT: u32 = 1;

const KEYWORDS: &[&str] = &[
    "as", "break", "const", "continue", "else", "enum", "extern", "false", "fn", "for", "if", "impl", "in", "let",
    "loop", "match", "mod", "move", "mut", "pub", "ref", "return", "static", "struct", "trait", "true", "type",
    "unsafe", "use", "where", "while", "async", "await", "dyn", "abstract", "become", "box", "do", "final", "macro",
    "override", "priv", "typeof", "unsized", "virtual", "yield", "try", "gen",
];
/// Keywords that cannot be raw identifiers.
const NO_RAW: &[&str] = &["self", "Self", "super", "crate"];

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

/// A field identifier and whether it differs from the wire name.
fn ident(name: &str, seen: &mut BTreeSet<String>) -> (String, bool) {
    let mut base = snake(name);
    if NO_RAW.contains(&base.as_str()) {
        base.push('_');
    }
    let mut id = base.clone();
    let mut i = 2;
    while !seen.insert(id.clone()) {
        id = format!("{base}_{i}");
        i += 1;
    }
    let renamed = id != name;
    let written = if KEYWORDS.contains(&id.as_str()) {
        format!("r#{id}")
    } else {
        id
    };
    (written, renamed)
}

fn enum_name(type_name: &str, f: &Field) -> String {
    format!("{type_name}{}", pascal(&f.name))
}

fn base_ty(f: &Field, type_name: &str) -> String {
    match &f.ty {
        Ty::Text | Ty::Date | Ty::Password => "String".into(),
        Ty::Number { integer: true } => "i64".into(),
        Ty::Number { integer: false } => "f64".into(),
        Ty::Bool => "bool".into(),
        Ty::Select { values, multi } => {
            let base = if values.is_empty() {
                "String".to_owned()
            } else {
                enum_name(type_name, f)
            };
            if *multi { format!("Vec<{base}>") } else { base }
        }
        Ty::Json => "serde_json::Value".into(),
        Ty::Array(Item::Text) => "Vec<String>".into(),
        Ty::Array(Item::Number) => "Vec<f64>".into(),
        Ty::Array(Item::Bool) => "Vec<bool>".into(),
        Ty::Array(Item::Json) => "Vec<serde_json::Value>".into(),
        Ty::Relation { multi, .. } | Ty::File { multi } => if *multi { "Vec<String>" } else { "String" }.into(),
        Ty::Geo => "GeoPoint".into(),
        Ty::Vector { .. } => "Vec<f32>".into(),
    }
}

fn variants(values: &[String]) -> Vec<(String, &String)> {
    let mut seen = BTreeSet::new();
    values
        .iter()
        .map(|v| {
            let mut base = if v.is_empty() { "Empty".to_owned() } else { pascal(v) };
            if base == "Self" {
                base.push('_');
            }
            let mut name = base.clone();
            let mut i = 2;
            while !seen.insert(name.clone()) {
                name = format!("{base}{i}");
                i += 1;
            }
            (name, v)
        })
        .collect()
}

fn doc(f: &Field) -> Option<String> {
    match &f.ty {
        Ty::Relation { target, multi } => Some(format!(
            "Relation to `{target}`: {}.",
            if *multi { "record ids" } else { "a record id" }
        )),
        Ty::File { multi } => Some(format!(
            "{} in the record's storage.",
            if *multi { "File names" } else { "A file name" }
        )),
        Ty::Date => Some("RFC 3339 timestamp (UTC).".into()),
        Ty::Vector { dimensions } => Some(format!("Vector of {dimensions} numbers.")),
        _ => None,
    }
}

fn collection(out: &mut String, c: &Collection, name: &str) {
    if c.kind == Kind::View {
        let _ = writeln!(
            out,
            "/// `{}`: a read-only view; its columns come from its query.\npub type {name} = serde_json::Map<String, serde_json::Value>;\n",
            c.name
        );
        return;
    }
    for f in &c.fields {
        if let Ty::Select { values, .. } = &f.ty
            && !values.is_empty()
        {
            let _ = writeln!(
                out,
                "/// Values of `{}.{}`.\n#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]\npub enum {} {{",
                c.name,
                f.name,
                enum_name(name, f)
            );
            for (v, wire) in variants(values) {
                let _ = writeln!(out, "    #[serde(rename = {})]\n    {v},", quote(wire));
            }
            out.push_str("}\n\n");
        }
    }
    let extra = if c.flexible {
        "    /// Fields the collection does not declare (flexible schema).\n    #[serde(flatten)]\n    pub extra: serde_json::Map<String, serde_json::Value>,\n"
    } else {
        ""
    };
    // The record.
    let _ = writeln!(
        out,
        "/// A record of `{}` as the API returns it.\n#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\npub struct {name} {{",
        c.name
    );
    out.push_str("    pub id: String,\n    /// RFC 3339 timestamp (UTC).\n    pub created: String,\n    /// RFC 3339 timestamp (UTC).\n    pub updated: String,\n    /// Record version: pass it for optimistic concurrency.\n    #[serde(rename = \"_version\")]\n    pub version: i64,\n");
    let mut seen: BTreeSet<String> = ["id", "created", "updated", "version", "extra"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    for f in c.fields.iter().filter(|f| f.ty != Ty::Password) {
        let (id, renamed) = ident(&f.name, &mut seen);
        if let Some(d) = doc(f) {
            let _ = writeln!(out, "    /// {d}");
        }
        let optional = !f.required || f.hidden;
        if optional {
            // Absent and null both read as `None`.
            if renamed {
                let _ = writeln!(out, "    #[serde(rename = {}, default)]", quote(&f.name));
            } else {
                out.push_str("    #[serde(default)]\n");
            }
            let _ = writeln!(out, "    pub {id}: Option<{}>,", base_ty(f, name));
        } else {
            if renamed {
                let _ = writeln!(out, "    #[serde(rename = {})]", quote(&f.name));
            }
            let _ = writeln!(out, "    pub {id}: {},", base_ty(f, name));
        }
    }
    out.push_str(extra);
    out.push_str("}\n\n");
    let _ = writeln!(
        out,
        "impl {name} {{\n    /// The collection's name.\n    pub const COLLECTION: &'static str = {};\n}}\n",
        quote(&c.name)
    );
    // The input.
    let _ = writeln!(
        out,
        "/// The fields to create a record of `{}` with.\n#[derive(Debug, Clone, PartialEq, Serialize)]\npub struct {name}Create {{",
        c.name
    );
    out.push_str("    /// Pick the id yourself (1-64 characters of `A-Za-z0-9_-`); otherwise the server generates one.\n    #[serde(skip_serializing_if = \"Option::is_none\")]\n    pub id: Option<String>,\n");
    let mut seen: BTreeSet<String> = ["id", "extra"].iter().map(|s| (*s).to_owned()).collect();
    let mut required: Vec<(String, String)> = Vec::new();
    let mut optional_fields: Vec<String> = Vec::new();
    for f in &c.fields {
        let (id, renamed) = ident(&f.name, &mut seen);
        if let Some(d) = doc(f) {
            let _ = writeln!(out, "    /// {d}");
        }
        let optional = !f.required || f.has_default;
        if optional {
            let attr = if renamed {
                format!(
                    "#[serde(rename = {}, skip_serializing_if = \"Option::is_none\")]",
                    quote(&f.name)
                )
            } else {
                "#[serde(skip_serializing_if = \"Option::is_none\")]".to_owned()
            };
            let _ = writeln!(out, "    {attr}\n    pub {id}: Option<{}>,", base_ty(f, name));
            optional_fields.push(id);
        } else {
            if renamed {
                let _ = writeln!(out, "    #[serde(rename = {})]", quote(&f.name));
            }
            let _ = writeln!(out, "    pub {id}: {},", base_ty(f, name));
            required.push((id, base_ty(f, name)));
        }
    }
    out.push_str(extra);
    out.push_str("}\n\n");
    // A constructor with the required fields.
    let params: Vec<String> = required.iter().map(|(id, t)| format!("{id}: {t}")).collect();
    let _ = writeln!(
        out,
        "impl {name}Create {{\n    /// A create with the required fields; every other field is left out.\n    pub fn new({}) -> Self {{\n        Self {{",
        params.join(", ")
    );
    out.push_str("            id: None,\n");
    for (id, _) in &required {
        let _ = writeln!(out, "            {id},");
    }
    for id in &optional_fields {
        let _ = writeln!(out, "            {id}: None,");
    }
    if c.flexible {
        out.push_str("            extra: serde_json::Map::new(),\n");
    }
    out.push_str("        }\n    }\n}\n\n");
    // The patch.
    let _ = writeln!(
        out,
        "/// The fields to change in a record of `{}`; fields left `None` are not sent.\n#[derive(Debug, Clone, PartialEq, Default, Serialize)]\npub struct {name}Update {{",
        c.name
    );
    let mut seen: BTreeSet<String> = ["extra"].iter().map(|s| (*s).to_owned()).collect();
    for f in &c.fields {
        let (id, renamed) = ident(&f.name, &mut seen);
        let attr = if renamed {
            format!(
                "#[serde(rename = {}, skip_serializing_if = \"Option::is_none\")]",
                quote(&f.name)
            )
        } else {
            "#[serde(skip_serializing_if = \"Option::is_none\")]".to_owned()
        };
        let _ = writeln!(out, "    {attr}\n    pub {id}: Option<{}>,", base_ty(f, name));
    }
    out.push_str(extra);
    out.push_str("}\n\n");
}

/// The `.rs` file for a model (a module file: `mod types;`).
pub fn emit(model: &Model) -> String {
    let names = type_names(model);
    let mut out = String::new();
    let _ = write!(
        out,
        "// Generated by `forgebase gen types --lang rust` (format {FORMAT}). Do not edit: regenerate it.\n\
// A module file (`mod types;`, not `include!`). Use with forgebase-sdk:\n\
//   let todos = fb.collection(Todos::COLLECTION).typed::<Todos>();\n\n\
#![allow(missing_docs, clippy::all)]\n\n\
use serde::{{Deserialize, Serialize}};\n\n\
/// A geo point.\n\
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]\n\
pub struct GeoPoint {{\n    /// Latitude, -90 to 90.\n    pub lat: f64,\n    /// Longitude, -180 to 180.\n    pub lon: f64,\n}}\n\n"
    );
    for (c, n) in model.collections.iter().zip(&names) {
        collection(&mut out, c, n);
    }
    out.push_str("/// Names of the collections.\npub const COLLECTION_NAMES: &[&str] = &[");
    let list: Vec<String> = model.collections.iter().map(|c| quote(&c.name)).collect();
    out.push_str(&list.join(", "));
    out.push_str("];\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typegen::model::build;
    use serde_json::json;

    #[test]
    fn keywords_and_camel_case_fields_are_renamed() {
        let mut seen = BTreeSet::new();
        assert_eq!(ident("type", &mut seen), ("r#type".to_owned(), false));
        assert_eq!(ident("createdBy", &mut seen), ("created_by".to_owned(), true));
        assert_eq!(ident("created_by", &mut seen), ("created_by_2".to_owned(), true));
        assert_eq!(ident("self", &mut seen), ("self_".to_owned(), true));
    }

    #[test]
    fn emits_structs_enums_and_inputs() {
        let m = build(&[(
            "orders".to_owned(),
            json!({"type": "base", "fields": [
                {"name": "total", "type": "number", "required": true},
                {"name": "status", "type": "select", "values": ["pending", "in-progress"], "default": "pending"},
                {"name": "items", "type": "json"}
            ]}),
        )]);
        let rs = emit(&m);
        assert!(rs.contains("pub struct Orders {"), "{rs}");
        assert!(rs.contains("    pub total: f64,"));
        assert!(rs.contains("    pub status: Option<OrdersStatus>,"));
        assert!(rs.contains("#[serde(rename = \"in-progress\")]\n    InProgress,"));
        assert!(rs.contains("pub struct OrdersCreate {"));
        assert!(rs.contains("pub struct OrdersUpdate {"));
        assert!(rs.contains("pub const COLLECTION: &'static str = \"orders\";"));
        assert_eq!(rs, emit(&m), "deterministic");
    }
}

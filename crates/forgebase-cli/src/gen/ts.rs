//! TypeScript emitter: plain `type` aliases (an interface would not be
//! assignable to the SDK's `Row = Record<string, unknown>` bound), so
//! `fb.collection<Todos>("todos")` type-checks against `@forgebase/sdk`.

use std::fmt::Write as _;

use super::model::{Collection, Field, Item, Kind, Model, Ty, pascal, type_names};

/// Version of the generated layout; bumped when the shape of the output changes.
pub const FORMAT: u32 = 1;

fn quote(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".into())
}

fn key(name: &str) -> String {
    let ident = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '$');
    if ident { name.to_owned() } else { quote(name) }
}

/// Name of the alias of a select field.
fn enum_name(type_name: &str, field: &Field) -> String {
    format!("{type_name}{}", pascal(&field.name))
}

fn ty(f: &Field, type_name: &str) -> String {
    match &f.ty {
        Ty::Text | Ty::Date | Ty::Password => "string".into(),
        Ty::Number { .. } => "number".into(),
        Ty::Bool => "boolean".into(),
        Ty::Select { values, multi } => {
            let base = if values.is_empty() {
                "string".to_owned()
            } else {
                enum_name(type_name, f)
            };
            if *multi { format!("{base}[]") } else { base }
        }
        Ty::Json => "JsonValue".into(),
        Ty::Array(Item::Text) => "string[]".into(),
        Ty::Array(Item::Number) => "number[]".into(),
        Ty::Array(Item::Bool) => "boolean[]".into(),
        Ty::Array(Item::Json) => "JsonValue[]".into(),
        Ty::Relation { multi, .. } | Ty::File { multi } => if *multi { "string[]" } else { "string" }.into(),
        Ty::Geo => "GeoPoint".into(),
        Ty::Vector { .. } => "number[]".into(),
    }
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
        Ty::Password => Some("Write-only: never returned.".into()),
        _ => None,
    }
}

fn collection(out: &mut String, c: &Collection, name: &str) {
    if c.kind == Kind::View {
        let _ = writeln!(
            out,
            "/** `{}`: a read-only view; its columns come from its query. */\nexport type {name} = Record<string, unknown>;\n",
            c.name
        );
        return;
    }
    for f in &c.fields {
        if let Ty::Select { values, .. } = &f.ty
            && !values.is_empty()
        {
            let union: Vec<String> = values.iter().map(|v| quote(v)).collect();
            let _ = writeln!(out, "export type {} = {};\n", enum_name(name, f), union.join(" | "));
        }
    }
    let tail = if c.flexible { " & Record<string, unknown>" } else { "" };
    let _ = writeln!(
        out,
        "/** A record of `{}` as the API returns it. */\nexport type {name} = {{",
        c.name
    );
    out.push_str("  id: string;\n  /** RFC 3339 timestamp (UTC). */\n  created: string;\n  /** RFC 3339 timestamp (UTC). */\n  updated: string;\n  /** Record version: send it as `version` for optimistic concurrency. */\n  _version: number;\n");
    for f in c.fields.iter().filter(|f| f.ty != Ty::Password) {
        if let Some(d) = doc(f) {
            let _ = writeln!(out, "  /** {d} */");
        }
        let t = ty(f, name);
        let (opt, t) = match (f.hidden, f.required) {
            (true, true) => ("?", t),
            (true, false) => ("?", format!("{t} | null")),
            (false, true) => ("", t),
            (false, false) => ("", format!("{t} | null")),
        };
        let _ = writeln!(out, "  {}{opt}: {t};", key(&f.name));
    }
    let _ = writeln!(out, "}}{tail};\n");

    let _ = writeln!(
        out,
        "/** The fields to create a record of `{}` with. */\nexport type {name}Create = {{",
        c.name
    );
    out.push_str("  /** Pick the id yourself (1-64 characters of `A-Za-z0-9_-`); otherwise the server generates one. */\n  id?: string;\n");
    for f in &c.fields {
        if let Some(d) = doc(f) {
            let _ = writeln!(out, "  /** {d} */");
        }
        let t = ty(f, name);
        let optional = !f.required || f.has_default;
        let t = if f.required { t } else { format!("{t} | null") };
        let _ = writeln!(out, "  {}{}: {t};", key(&f.name), if optional { "?" } else { "" });
    }
    let _ = writeln!(out, "}}{tail};\n");
    let _ = writeln!(
        out,
        "/** The fields to change in a record of `{}`. */\nexport type {name}Update = Partial<{name}Create>;\n",
        c.name
    );
}

/// The `.ts` file for a model.
pub fn emit(model: &Model) -> String {
    let names = type_names(model);
    let mut out = String::new();
    let _ = write!(
        out,
        "// Generated by `forgebase gen types --lang ts` (format {FORMAT}). Do not edit: regenerate it.\n\
// Use with @forgebase/sdk:\n\
//   const todos = fb.collection<Todos>(\"todos\");\n\
//   await todos.create({{ title: \"Ship it\" }} satisfies TodosCreate);\n\n\
/** Any JSON value. */\n\
export type JsonValue = string | number | boolean | null | JsonValue[] | {{ [key: string]: JsonValue }};\n\n\
/** A geo point. */\n\
export type GeoPoint = {{ lat: number; lon: number }};\n\n"
    );
    for (c, n) in model.collections.iter().zip(&names) {
        collection(&mut out, c, n);
    }
    out.push_str("/** The record type of every collection, by collection name. */\nexport interface Collections {\n");
    for (c, n) in model.collections.iter().zip(&names) {
        let _ = writeln!(out, "  {}: {n};", key(&c.name));
    }
    out.push_str("}\n\n/** The create type of every writable collection, by collection name. */\nexport interface CollectionInputs {\n");
    for (c, n) in model.collections.iter().zip(&names) {
        if c.kind != Kind::View {
            let _ = writeln!(out, "  {}: {n}Create;", key(&c.name));
        }
    }
    out.push_str("}\n\n/** Names of the collections. */\nexport type CollectionName = keyof Collections;\n\n/** Names of the collections, at run time. */\nexport const collectionNames = [");
    let list: Vec<String> = model.collections.iter().map(|c| quote(&c.name)).collect();
    out.push_str(&list.join(", "));
    out.push_str("] as const;\n");
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::typegen::model::build;
    use serde_json::json;

    #[test]
    fn emits_nullable_optional_enum_and_system_fields() {
        let m = build(&[(
            "posts".to_owned(),
            json!({"type": "base", "fields": [
                {"name": "title", "type": "text", "required": true},
                {"name": "status", "type": "select", "values": ["draft", "live"], "required": true, "default": "draft"},
                {"name": "tags", "type": "select", "values": ["a"], "max_select": 3},
                {"name": "author", "type": "relation", "collection": "users"},
                {"name": "meta", "type": "json"}
            ]}),
        )]);
        let ts = emit(&m);
        assert!(ts.contains("export type PostsStatus = \"draft\" | \"live\";"), "{ts}");
        assert!(ts.contains("  _version: number;"));
        assert!(ts.contains("  title: string;"));
        assert!(ts.contains("  status: PostsStatus;"));
        assert!(ts.contains("  tags: PostsTags[] | null;"));
        assert!(ts.contains("  author: string | null;"));
        assert!(ts.contains("  meta: JsonValue | null;"));
        // Create: a default makes a required field optional; nullable ones accept null.
        assert!(ts.contains("export type PostsCreate = {"));
        assert!(ts.contains("  title: string;\n  status?: PostsStatus;"));
        assert!(ts.contains("  author?: string | null;"));
        assert!(ts.contains("export const collectionNames = [\"posts\"] as const;"));
        assert_eq!(ts, emit(&m), "deterministic");
    }
}

//! The language-neutral model of a project's collections that every emitter
//! reads. It is built from canonical collection specs (ADR-0037 §4), so a
//! project directory and a live server produce the same model.

use std::collections::BTreeSet;

use serde_json::Value;

/// What a field holds.
#[derive(Debug, Clone, PartialEq)]
pub enum Ty {
    /// `text`, `email`, `url`.
    Text,
    /// `date`: an RFC 3339 string.
    Date,
    /// `number`.
    Number {
        /// Integers only.
        integer: bool,
    },
    /// `bool`.
    Bool,
    /// `select`.
    Select {
        /// The allowed values.
        values: Vec<String>,
        /// More than one value may be chosen (an array).
        multi: bool,
    },
    /// `json`: any JSON value.
    Json,
    /// `array` of scalars or JSON.
    Array(Item),
    /// `relation`: record id(s) of another collection.
    Relation {
        /// Target collection.
        target: String,
        /// An array of ids.
        multi: bool,
    },
    /// `file`: file name(s) in the record's storage.
    File {
        /// An array of names.
        multi: bool,
    },
    /// `password`: write-only.
    Password,
    /// `geo`: `{lat, lon}`.
    Geo,
    /// `vector`.
    Vector {
        /// Number of dimensions.
        dimensions: u32,
    },
}

/// Element type of an `array` field.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Item {
    /// Strings.
    Text,
    /// Numbers.
    Number,
    /// Booleans.
    Bool,
    /// Any JSON value.
    Json,
}

/// One declared (or auth system) field.
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    /// Name on the wire.
    pub name: String,
    /// Type.
    pub ty: Ty,
    /// Must have a value.
    pub required: bool,
    /// Has a server default (so a create may leave it out).
    pub has_default: bool,
    /// Omitted from what non-administrators read.
    pub hidden: bool,
}

/// Kind of collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Ordinary records.
    Base,
    /// User accounts.
    Auth,
    /// Read-only records defined by a query.
    View,
}

/// One collection.
#[derive(Debug, Clone, PartialEq)]
pub struct Collection {
    /// Collection name.
    pub name: String,
    /// Kind.
    pub kind: Kind,
    /// Accepts fields it does not declare.
    pub flexible: bool,
    /// Fields in declared order (auth system fields first).
    pub fields: Vec<Field>,
}

/// Collections sorted by name.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Model {
    /// The collections.
    pub collections: Vec<Collection>,
}

fn field_from_spec(f: &Value) -> Option<Field> {
    let name = f["name"].as_str()?.to_owned();
    let multi = f["max_select"].as_i64().unwrap_or(1) > 1;
    let ty = match f["type"].as_str()? {
        "text" | "email" | "url" => Ty::Text,
        "date" => Ty::Date,
        "number" => Ty::Number {
            integer: f["integer"].as_bool().unwrap_or(false),
        },
        "bool" => Ty::Bool,
        "select" => Ty::Select {
            values: f["values"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect(),
            multi,
        },
        "json" => Ty::Json,
        "array" => Ty::Array(match f["items"].as_str() {
            Some("text") => Item::Text,
            Some("number") => Item::Number,
            Some("bool") => Item::Bool,
            _ => Item::Json,
        }),
        "relation" => Ty::Relation {
            target: f["collection"].as_str().unwrap_or_default().to_owned(),
            multi,
        },
        "file" => Ty::File { multi },
        "password" => Ty::Password,
        "geo" => Ty::Geo,
        "vector" => Ty::Vector {
            dimensions: f["dimensions"].as_u64().unwrap_or(0) as u32,
        },
        _ => return None,
    };
    Some(Field {
        name,
        ty,
        required: f["required"].as_bool().unwrap_or(false),
        has_default: f.get("default").is_some_and(|d| !d.is_null()),
        hidden: f["hidden"].as_bool().unwrap_or(false),
    })
}

/// The system fields of an auth collection that records carry or accept (the server keeps them; files do not list them).
fn auth_fields() -> Vec<Field> {
    let f = |name: &str, ty, required, has_default| Field {
        name: name.to_owned(),
        ty,
        required,
        has_default,
        hidden: false,
    };
    vec![
        f("email", Ty::Text, true, false),
        f("email_visibility", Ty::Bool, true, true),
        f("verified", Ty::Bool, true, true),
        f("password", Ty::Password, true, false),
    ]
}

/// Builds the model from `(name, canonical spec)` pairs.
pub fn build(specs: &[(String, Value)]) -> Model {
    let mut collections: Vec<Collection> = specs
        .iter()
        .map(|(name, spec)| {
            let kind = match spec["type"].as_str() {
                Some("auth") => Kind::Auth,
                Some("view") => Kind::View,
                _ => Kind::Base,
            };
            let mut fields = if kind == Kind::Auth { auth_fields() } else { Vec::new() };
            if kind != Kind::View {
                let declared: Vec<Field> = spec["fields"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter_map(field_from_spec)
                    .collect();
                for d in declared {
                    // A declared field wins over the system field of the same name.
                    fields.retain(|f| !f.name.eq_ignore_ascii_case(&d.name));
                    fields.push(d);
                }
            }
            Collection {
                name: name.clone(),
                kind,
                flexible: spec["schema_mode"].as_str() == Some("flexible"),
                fields,
            }
        })
        .collect();
    collections.sort_by(|a, b| a.name.cmp(&b.name));
    Model { collections }
}

// ---------------------------------------------------------------------------
// Names

/// Splits a name into lower-case words at `_`, `-`, `.`, spaces and camelCase humps.
fn words(s: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut cur = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_ascii_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let hump = c.is_ascii_uppercase()
            && !cur.is_empty()
            && (chars[i - 1].is_ascii_lowercase()
                || chars[i - 1].is_ascii_digit()
                || chars.get(i + 1).is_some_and(char::is_ascii_lowercase));
        if hump {
            out.push(std::mem::take(&mut cur));
        }
        cur.push(c.to_ascii_lowercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// `blog_posts` -> `BlogPosts`; a leading digit gets an underscore.
pub fn pascal(s: &str) -> String {
    let mut out: String = words(s)
        .iter()
        .map(|w| {
            let mut c = w.chars();
            c.next()
                .map(|f| f.to_ascii_uppercase().to_string() + c.as_str())
                .unwrap_or_default()
        })
        .collect();
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// `createdBy` -> `created_by`.
pub fn snake(s: &str) -> String {
    let mut out = words(s).join("_");
    if out.is_empty() || out.starts_with(|c: char| c.is_ascii_digit()) {
        out.insert(0, '_');
    }
    out
}

/// Type names per collection, unique and stable: a clash gets a numeric suffix in name order.
pub fn type_names(model: &Model) -> Vec<String> {
    let mut seen = BTreeSet::new();
    model
        .collections
        .iter()
        .map(|c| {
            let base = pascal(&c.name);
            let mut n = base.clone();
            let mut i = 2;
            while !seen.insert(n.clone()) {
                n = format!("{base}{i}");
                i += 1;
            }
            n
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_are_converted_predictably() {
        assert_eq!(pascal("blog_posts"), "BlogPosts");
        assert_eq!(pascal("createdBy"), "CreatedBy");
        assert_eq!(pascal("HTTPLog"), "HttpLog");
        assert_eq!(pascal("2fa"), "_2fa");
        assert_eq!(snake("createdBy"), "created_by");
        assert_eq!(snake("HTTPLog"), "http_log");
        assert_eq!(snake("title"), "title");
        assert_eq!(snake("v2Items"), "v2_items");
    }

    #[test]
    fn clashing_type_names_get_a_suffix_in_name_order() {
        let specs = vec![
            ("a_b".to_owned(), json!({"type": "base", "fields": []})),
            ("aB".to_owned(), json!({"type": "base", "fields": []})),
        ];
        let m = build(&specs);
        let names = type_names(&m);
        assert_eq!(m.collections[0].name, "aB");
        assert_eq!(names, vec!["AB", "AB2"]);
    }

    #[test]
    fn auth_collections_get_their_system_fields_and_declared_fields_win() {
        let specs = vec![(
            "users".to_owned(),
            json!({"type": "auth", "fields": [{"name": "name", "type": "text"}, {"name": "verified", "type": "bool", "required": false}]}),
        )];
        let m = build(&specs);
        let names: Vec<&str> = m.collections[0].fields.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, vec!["email", "email_visibility", "password", "name", "verified"]);
    }
}

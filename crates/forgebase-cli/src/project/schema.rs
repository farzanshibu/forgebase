//! JSON Schemas of the project files (ADR-0037 §2), the validator that checks
//! every file against them, and the locators that turn a JSON path into a
//! `file:line` position.
//!
//! The same schemas are written to `<dir>/.schemas/*.schema.json` for editor
//! validation (`$schema` in JSON files, `#:schema` in TOML files), so what an
//! editor flags and what `forgebase project plan` refuses cannot drift apart.
//! The validator implements the subset the schemas use: `type`, `enum`,
//! `const`, `properties`, `required`, `additionalProperties`, `items`,
//! `minimum`, `minLength`, `$ref` to `#/$defs/…` and `oneOf` (discriminated by
//! a `type` constant when every branch has one).

use serde_json::{Map, Value, json};

/// Draft of the generated schemas.
const DRAFT: &str = "https://json-schema.org/draft/2020-12/schema";

/// One schema file of `.schemas/`.
pub struct SchemaFile {
    /// File name inside `.schemas/`.
    pub file: &'static str,
    /// The schema.
    pub schema: Value,
}

/// Every schema, in a stable order.
pub fn all() -> Vec<SchemaFile> {
    vec![
        SchemaFile {
            file: "project.schema.json",
            schema: project(),
        },
        SchemaFile {
            file: "collection.schema.json",
            schema: collection(),
        },
        SchemaFile {
            file: "function.schema.json",
            schema: function(),
        },
        SchemaFile {
            file: "topic.schema.json",
            schema: topic(),
        },
        SchemaFile {
            file: "subscription.schema.json",
            schema: subscription(),
        },
        SchemaFile {
            file: "bucket.schema.json",
            schema: bucket(),
        },
        SchemaFile {
            file: "policy.schema.json",
            schema: policy(),
        },
        SchemaFile {
            file: "oauth-client.schema.json",
            schema: oauth_client(),
        },
    ]
}

/// The schema of one file name of [`all`].
pub fn by_file(file: &str) -> Value {
    all()
        .into_iter()
        .find(|s| s.file == file)
        .map(|s| s.schema)
        .unwrap_or(Value::Null)
}

fn doc(id: &str, title: &str, mut body: Value) -> Value {
    if let Some(o) = body.as_object_mut() {
        o.insert("$schema".into(), json!(DRAFT));
        o.insert(
            "$id".into(),
            json!(format!("https://forgebase.dev/schemas/project/{id}")),
        );
        o.insert("title".into(), json!(title));
    }
    body
}

fn int(min: i64, description: &str) -> Value {
    json!({"type": "integer", "minimum": min, "description": description})
}

fn opt_int(min: i64, description: &str) -> Value {
    json!({"type": ["integer", "null"], "minimum": min, "description": description})
}

fn strings(description: &str) -> Value {
    json!({"type": "array", "items": {"type": "string"}, "description": description})
}

fn string_map(description: &str) -> Value {
    json!({"type": "object", "additionalProperties": {"type": "string"}, "description": description})
}

fn rule(description: &str) -> Value {
    json!({
        "type": ["string", "null"],
        "description": format!("{description}: null locks it (privileged callers only), \"\" opens it to everyone, anything else is a filter expression.")
    })
}

/// `forgebase.project.toml`.
pub fn project() -> Value {
    let target = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "url": {"type": "string", "description": "Server base URL (http:// or https://). FORGEBASE_URL and --server override it."},
            "realm": {"type": "string", "description": "Realm for OAuth clients and the X-Forgebase-Realm header."},
            "project": {"type": "string", "description": "ForgeBase project id or slug that owns the environment."},
            "environment": {"type": "string", "description": "ForgeBase environment name of the project (development, staging, production, ...)."}
        }
    });
    doc(
        "project.schema.json",
        "ForgeBase project",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["format", "name"],
            "properties": {
                "format": {"const": 1, "description": "Layout format version."},
                "name": {"type": "string", "minLength": 1, "description": "Project name (informational)."},
                "manage": {
                    "type": "array",
                    "description": "Resource kinds this project owns. Kinds left out are neither changed nor reported as drift. Default: all.",
                    "items": {"enum": ["collections", "functions", "queues", "storage", "policies", "oauth-clients"]}
                },
                "defaults": target.clone(),
                "environments": {
                    "type": "object",
                    "description": "Named targets for --env NAME, merged over [defaults]. `environment` defaults to the table name.",
                    "additionalProperties": target
                }
            }
        }),
    )
}

fn field_branch(ty: &str, options: Value, required: &[&str]) -> Value {
    let mut props = json!({
        "type": {"const": ty},
        "name": {"type": "string", "minLength": 1, "description": "Field (column) name."},
        "renamed_from": {"type": "string", "description": "Rename: the field's previous name on the server (its data is kept)."},
        "required": {"type": "boolean", "description": "Must be present and non-empty."},
        "unique": {"type": "boolean", "description": "Unique across the collection (a unique index)."},
        "hidden": {"type": "boolean", "description": "Omitted from records returned to non-administrators."},
        "default": {"description": "Value used on create when the field is missing."}
    });
    if let (Some(p), Some(o)) = (props.as_object_mut(), options.as_object()) {
        for (k, v) in o {
            p.insert(k.clone(), v.clone());
        }
    }
    let mut req = vec!["name", "type"];
    req.extend_from_slice(required);
    json!({"type": "object", "additionalProperties": false, "required": req, "properties": props})
}

/// The field types and their option schemas.
fn field_types() -> Vec<Value> {
    let none = json!({});
    vec![
        field_branch(
            "text",
            json!({"min_length": int(0, "Minimum length in characters."), "max_length": int(0, "Maximum length in characters (server default 1 000 000).")}),
            &[],
        ),
        field_branch(
            "number",
            json!({"min": {"type": "number"}, "max": {"type": "number"}, "integer": {"type": "boolean", "description": "Integers only."}}),
            &[],
        ),
        field_branch("bool", none.clone(), &[]),
        field_branch("date", none.clone(), &[]),
        field_branch("email", none.clone(), &[]),
        field_branch("url", none.clone(), &[]),
        field_branch(
            "select",
            json!({"values": strings("Allowed values."), "max_select": int(1, "Values chosen at once (default 1; above 1 the field holds an array).")}),
            &["values"],
        ),
        field_branch(
            "json",
            json!({"max_size": int(0, "Maximum serialised size in bytes.")}),
            &[],
        ),
        field_branch(
            "array",
            json!({"items": {"enum": ["text", "number", "bool", "json"]}, "min_items": int(0, "Minimum items."), "max_items": int(0, "Maximum items.")}),
            &[],
        ),
        field_branch(
            "relation",
            json!({
                "collection": {"type": "string", "minLength": 1, "description": "Target collection name."},
                "max_select": int(1, "Records referenced at once (default 1)."),
                "cascade_delete": {"type": "boolean", "description": "Delete this record when the referenced record is deleted."}
            }),
            &["collection"],
        ),
        field_branch(
            "file",
            json!({"max_select": int(1, "Files referenced at once (default 1).")}),
            &[],
        ),
        field_branch(
            "password",
            json!({"min_length": int(0, "Minimum length (default 8).")}),
            &[],
        ),
        field_branch("geo", none, &[]),
        field_branch(
            "vector",
            json!({"dimensions": int(1, "Number of dimensions (1 to 16 000).")}),
            &["dimensions"],
        ),
    ]
}

/// `collections/<name>.json`.
pub fn collection() -> Value {
    let field_rule = json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {"read": rule("Reading the field"), "write": rule("Writing the field")}
    });
    doc(
        "collection.schema.json",
        "ForgeBase collection",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "$schema": {"type": "string"},
                "name": {"type": "string", "minLength": 1, "description": "Collection name; defaults to the file name."},
                "renamed_from": {"type": "string", "description": "Rename: the collection's previous name on the server (records are kept)."},
                "type": {"enum": ["base", "auth", "view"], "description": "Kind (default base). Cannot change: a change replaces the collection."},
                "schema_mode": {"enum": ["strict", "flexible"], "description": "strict (declared fields only, default) or flexible (JSON documents)."},
                "versioned": {"type": "boolean", "description": "Keep every version of every record."},
                "view_query": {"type": ["string", "null"], "description": "SELECT of a view collection."},
                "options": {"type": "object", "description": "Options owned by other packages (e.g. tenant_scoped). Only the keys listed here are managed."},
                "fields": {"type": "array", "items": {"$ref": "#/$defs/field"}, "description": "Fields, in order (views: none, they come from the query)."},
                "indexes": {
                    "type": "array",
                    "description": "Manual indexes. Omit the key to leave the server's indexes alone.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["name", "keys"],
                        "properties": {
                            "name": {"type": "string", "minLength": 1},
                            "keys": {
                                "type": "array",
                                "items": {
                                    "type": "object",
                                    "additionalProperties": false,
                                    "required": ["path"],
                                    "properties": {
                                        "path": {"type": "string", "minLength": 1, "description": "Field path in filter syntax: price, metadata.brand, title:lower."},
                                        "desc": {"type": "boolean"}
                                    }
                                }
                            },
                            "unique": {"type": "boolean"},
                            "kind": {"enum": ["btree", "fulltext", "trigram"]},
                            "partial": {"type": ["string", "null"], "description": "Filter expression of a partial index."}
                        }
                    }
                },
                "rules": {
                    "type": "object",
                    "additionalProperties": false,
                    "description": "Access rules. Omit the key to leave the server's rules alone.",
                    "properties": {
                        "list_rule": rule("Listing and searching records"),
                        "view_rule": rule("Reading one record"),
                        "create_rule": rule("Creating records"),
                        "update_rule": rule("Updating records"),
                        "delete_rule": rule("Deleting records"),
                        "fields": {"type": "object", "additionalProperties": field_rule, "description": "Per-field rules by field name."}
                    }
                }
            },
            "$defs": {"field": {"oneOf": field_types()}}
        }),
    )
}

/// `functions/<name>/function.toml`.
pub fn function() -> Value {
    doc(
        "function.schema.json",
        "ForgeBase function",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "name": {"type": "string", "description": "Function name; defaults to the directory name."},
                "description": {"type": "string"},
                "access": {"enum": ["public", "authenticated", "permission"], "description": "Who may invoke over HTTP (default permission)."},
                "entry": {"type": "string", "description": "Source (.ts, .mts, .js, .mjs) or WebAssembly module (.wasm), relative to the function directory."},
                "build": {"type": "string", "description": "Command `forgebase deploy` runs in the function directory before the upload (e.g. a bundler)."},
                "capabilities": strings("Granted capabilities: db.read, db.write, kv, log, env, secrets, fetch, ..."),
                "fetch_hosts": strings("With fetch: hosts the function may call."),
                "env": string_map("Plain (non-secret) environment values."),
                "secrets": string_map("Secret name -> name of the environment variable that holds the value at apply time. Values are never stored in files."),
                "limits": {
                    "type": "object",
                    "additionalProperties": false,
                    "description": "Limits; keys left out keep the server's value.",
                    "properties": {
                        "fuel": int(0, "Fuel per execution."),
                        "memory_bytes": int(0, "Largest linear memory in bytes."),
                        "timeout_ms": int(0, "Wall-clock limit in milliseconds."),
                        "max_concurrency": int(0, "Concurrent executions.")
                    }
                },
                "hooks": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["collection", "events"],
                        "properties": {
                            "collection": {"type": "string", "minLength": 1},
                            "events": {"type": "array", "items": {"enum": ["before_create", "after_create", "before_update", "after_update", "before_delete", "after_delete"]}}
                        }
                    }
                },
                "kv_rules": {
                    "type": "array",
                    "description": "Key-value namespaces a JavaScript function may reach. Omit to leave the server's rules alone.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["namespace", "access"],
                        "properties": {"namespace": {"type": "string", "minLength": 1}, "access": {"enum": ["read", "write"]}}
                    }
                }
            }
        }),
    )
}

/// `queues/topics/<name>.json`.
pub fn topic() -> Value {
    doc(
        "topic.schema.json",
        "ForgeBase queue topic",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "$schema": {"type": "string"},
                "name": {"type": "string", "minLength": 1, "description": "Topic name; defaults to the file name."},
                "retention_ms": int(1, "How long messages are kept (server default 7 days). Left out: the server's value."),
                "retention_bytes": opt_int(0, "Most bytes kept (null: no limit). Left out: the server's value."),
                "max_message_bytes": int(1, "Largest message. Left out: the server's value."),
                "dedup_window_ms": int(0, "Dedup window. Left out: the server's value."),
                "labels": string_map("Free-form labels."),
                "schema": {"type": ["object", "null"], "description": "JSON Schema every message's data must satisfy."},
                "source": {
                    "type": ["object", "null"],
                    "additionalProperties": false,
                    "required": ["collection"],
                    "description": "Feed the topic from a collection's record changes. Cannot change: a change replaces the topic.",
                    "properties": {
                        "collection": {"type": "string", "minLength": 1},
                        "ops": {"type": "array", "items": {"enum": ["create", "update", "delete"]}}
                    }
                }
            }
        }),
    )
}

/// `queues/subscriptions/<name>.json`.
pub fn subscription() -> Value {
    doc(
        "subscription.schema.json",
        "ForgeBase queue subscription",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["topic"],
            "properties": {
                "$schema": {"type": "string"},
                "name": {"type": "string", "minLength": 1, "description": "Subscription name; defaults to the file name."},
                "topic": {"type": "string", "minLength": 1, "description": "Topic. Cannot change: a change replaces the subscription."},
                "filter": {"type": ["object", "null"], "description": "Attribute filter. Cannot change: a change replaces the subscription."},
                "ordering": {"type": "boolean", "description": "Ordered delivery per ordering key. Cannot change."},
                "start": {
                    "description": "Where a new subscription starts (create only): latest (default), earliest or {\"time\": RFC 3339}.",
                    "oneOf": [
                        {"enum": ["latest", "earliest"]},
                        {"type": "object", "additionalProperties": false, "required": ["time"], "properties": {"time": {"type": "string"}}}
                    ]
                },
                "ack_deadline_ms": int(1000, "Ack deadline. Left out: the server's value."),
                "backoff_base_ms": int(0, "Redelivery backoff base. Left out: the server's value."),
                "backoff_max_ms": int(0, "Backoff cap. Left out: the server's value."),
                "max_delivery_attempts": opt_int(1, "Dead-letter after this many deliveries (null: unlimited)."),
                "dead_letter_topic": {"type": ["string", "null"]},
                "labels": string_map("Free-form labels."),
                "push": {
                    "type": ["object", "null"],
                    "additionalProperties": false,
                    "required": ["kind", "endpoint"],
                    "description": "Push delivery instead of pull.",
                    "properties": {
                        "kind": {"enum": ["webhook", "function"]},
                        "endpoint": {"type": "string", "minLength": 1, "description": "https:// URL (webhook) or function name."},
                        "secret_env": {"type": "string", "description": "Environment variable the webhook signing secret belongs in. The secret is never written to project files; `apply --secrets-out FILE` exports a newly issued one under this name."}
                    }
                }
            }
        }),
    )
}

/// `storage/buckets/<name>.json`.
pub fn bucket() -> Value {
    doc(
        "bucket.schema.json",
        "ForgeBase storage bucket",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "$schema": {"type": "string"},
                "name": {"type": "string", "minLength": 1, "description": "Bucket name; defaults to the file name."},
                "visibility": {"enum": ["private", "authenticated", "public"], "description": "Default read access (default private)."},
                "versioning": {"type": "boolean", "description": "Keep earlier versions on overwrite and delete."},
                "max_object_bytes": opt_int(1, "Largest object (null: the server limit)."),
                "allowed_content_types": {"type": ["array", "null"], "items": {"type": "string"}, "description": "Allowed content types (image/* patterns); null allows all."},
                "acl": {
                    "type": "array",
                    "description": "The bucket's whole ACL. Omit the key to leave the server's ACL alone.",
                    "items": {
                        "type": "object",
                        "additionalProperties": false,
                        "required": ["grantee", "permission"],
                        "properties": {
                            "grantee": {"type": "string", "description": "user:<collection>/<id>, service:<id>, role:<name>, authenticated or public."},
                            "permission": {"enum": ["read", "write", "admin"]},
                            "prefix": {"type": "string", "description": "Key prefix the grant covers (\"\": the whole bucket)."}
                        }
                    }
                }
            }
        }),
    )
}

/// `policies/<resource kind>.json`.
pub fn policy() -> Value {
    doc(
        "policy.schema.json",
        "ForgeBase engine resource rules",
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["rules"],
            "properties": {
                "$schema": {"type": "string"},
                "rules": {
                    "type": "object",
                    "description": "Resource name (or *) -> action (list, view, create, update, delete, read, write) -> rule (null locks it, \"\" opens it).",
                    "additionalProperties": {
                        "type": "object",
                        "additionalProperties": {"type": ["string", "null"]}
                    }
                }
            }
        }),
    )
}

/// `oauth-clients/<name>.json`.
pub fn oauth_client() -> Value {
    doc(
        "oauth-client.schema.json",
        "ForgeBase OAuth client app",
        json!({
            "type": "object",
            "additionalProperties": false,
            "properties": {
                "$schema": {"type": "string"},
                "name": {"type": "string", "minLength": 1, "description": "Display name (the identity of the client in this project); defaults to the file name."},
                "type": {"enum": ["confidential", "public"], "description": "Cannot change: a change replaces the client (new client_id)."},
                "description": {"type": ["string", "null"]},
                "redirect_uris": strings("Allowed redirect URIs (exact match)."),
                "post_logout_redirect_uris": strings("Allowed post-logout redirect URIs."),
                "grant_types": strings("Allowed grant types. Left out: the server's value."),
                "scopes": strings("Scopes the client may request. Left out: the server's value."),
                "skip_consent": {"type": "boolean"},
                "access_token_ttl": opt_int(60, "Access-token lifetime in seconds."),
                "refresh_token_ttl": opt_int(300, "Refresh-token lifetime in seconds."),
                "disabled": {"type": "boolean"},
                "secret_env": {"type": "string", "description": "Environment variable the client secret belongs in; `apply --secrets-out FILE` exports a newly issued secret under this name."}
            }
        }),
    )
}

// ---------------------------------------------------------------------------
// Validation

/// One step of a JSON path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seg {
    /// Object member.
    Key(String),
    /// Array item.
    Index(usize),
}

impl std::fmt::Display for Seg {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Seg::Key(k) => write!(f, ".{k}"),
            Seg::Index(i) => write!(f, "[{i}]"),
        }
    }
}

/// A validation failure at a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Violation {
    /// Where.
    pub path: Vec<Seg>,
    /// What.
    pub message: String,
}

/// Renders a path as `fields[2].max_length` (empty: the document).
pub fn path_text(path: &[Seg]) -> String {
    let s: String = path.iter().map(ToString::to_string).collect();
    s.strip_prefix('.').map(str::to_owned).unwrap_or(s)
}

/// Checks `value` against `schema` (a document of [`all`]).
pub fn validate(schema: &Value, value: &Value) -> Vec<Violation> {
    let mut out = Vec::new();
    check(schema, schema, value, &mut Vec::new(), &mut out);
    out
}

fn type_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(n) if n.is_i64() || n.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn type_matches(want: &str, v: &Value) -> bool {
    match want {
        "number" => v.is_number(),
        "integer" => v.is_i64() || v.is_u64(),
        other => type_name(v) == other,
    }
}

fn resolve<'a>(root: &'a Value, schema: &'a Value) -> &'a Value {
    match schema.get("$ref").and_then(Value::as_str) {
        Some(r) => r
            .strip_prefix("#/")
            .map(|p| p.split('/').fold(root, |acc, seg| &acc[seg]))
            .unwrap_or(&Value::Null),
        None => schema,
    }
}

fn short(v: &Value) -> String {
    let s = v.to_string();
    if s.chars().count() > 60 {
        format!("{}…", s.chars().take(60).collect::<String>())
    } else {
        s
    }
}

fn check(root: &Value, schema: &Value, v: &Value, path: &mut Vec<Seg>, out: &mut Vec<Violation>) {
    let schema = resolve(root, schema);
    let fail = |path: &Vec<Seg>, out: &mut Vec<Violation>, message: String| {
        out.push(Violation {
            path: path.clone(),
            message,
        });
    };
    if let Some(c) = schema.get("const")
        && c != v
    {
        fail(path, out, format!("must be {c}, found {}", short(v)));
        return;
    }
    if let Some(Value::Array(options)) = schema.get("enum")
        && !options.contains(v)
    {
        let list: Vec<String> = options.iter().map(Value::to_string).collect();
        fail(
            path,
            out,
            format!("must be one of {}, found {}", list.join(", "), short(v)),
        );
        return;
    }
    if let Some(t) = schema.get("type") {
        let wanted: Vec<&str> = match t {
            Value::String(s) => vec![s.as_str()],
            Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
            _ => Vec::new(),
        };
        if !wanted.is_empty() && !wanted.iter().any(|w| type_matches(w, v)) {
            fail(
                path,
                out,
                format!("must be {}, found {}", wanted.join(" or "), type_name(v)),
            );
            return;
        }
    }
    if let Some(Value::Array(branches)) = schema.get("oneOf") {
        one_of(root, branches, v, path, out);
        return;
    }
    if let Some(min) = schema.get("minimum").and_then(Value::as_f64)
        && let Some(n) = v.as_f64()
        && n < min
    {
        fail(path, out, format!("must be at least {min}, found {n}"));
    }
    if let Some(min) = schema.get("minLength").and_then(Value::as_u64)
        && let Some(s) = v.as_str()
        && (s.chars().count() as u64) < min
    {
        fail(path, out, "must not be empty".to_owned());
    }
    if let Value::Object(map) = v {
        object(root, schema, map, path, out);
    }
    if let (Value::Array(items), Some(item_schema)) = (v, schema.get("items")) {
        for (i, item) in items.iter().enumerate() {
            path.push(Seg::Index(i));
            check(root, item_schema, item, path, out);
            path.pop();
        }
    }
}

fn object(root: &Value, schema: &Value, map: &Map<String, Value>, path: &mut Vec<Seg>, out: &mut Vec<Violation>) {
    let props = schema.get("properties").and_then(Value::as_object);
    if let Some(Value::Array(req)) = schema.get("required") {
        for r in req.iter().filter_map(Value::as_str) {
            if !map.contains_key(r) {
                out.push(Violation {
                    path: path.clone(),
                    message: format!("missing required key \"{r}\""),
                });
            }
        }
    }
    for (k, val) in map {
        path.push(Seg::Key(k.clone()));
        match props.and_then(|p| p.get(k)) {
            Some(s) => check(root, s, val, path, out),
            None => match schema.get("additionalProperties") {
                Some(Value::Bool(false)) => {
                    let mut known: Vec<&str> = props
                        .map(|p| p.keys().map(String::as_str).collect())
                        .unwrap_or_default();
                    known.retain(|k| *k != "$schema");
                    out.push(Violation {
                        path: path.clone(),
                        message: if known.is_empty() {
                            format!("unknown key \"{k}\"")
                        } else {
                            format!("unknown key \"{k}\" (expected one of: {})", known.join(", "))
                        },
                    });
                }
                Some(s @ Value::Object(_)) => check(root, s, val, path, out),
                _ => {}
            },
        }
        path.pop();
    }
}

fn one_of(root: &Value, branches: &[Value], v: &Value, path: &mut Vec<Seg>, out: &mut Vec<Violation>) {
    let resolved: Vec<&Value> = branches.iter().map(|b| resolve(root, b)).collect();
    let discriminators: Vec<Option<&Value>> = resolved
        .iter()
        .map(|b| {
            b.get("properties")
                .and_then(|p| p.get("type"))
                .and_then(|t| t.get("const"))
        })
        .collect();
    if discriminators.iter().all(Option::is_some) {
        let names: Vec<String> = discriminators.iter().flatten().map(|d| d.to_string()).collect();
        let Some(tag) = v.get("type") else {
            out.push(Violation {
                path: path.clone(),
                message: format!("missing required key \"type\" (one of {})", names.join(", ")),
            });
            return;
        };
        match discriminators.iter().position(|d| *d == Some(tag)) {
            Some(i) => check(root, resolved[i], v, path, out),
            None => {
                path.push(Seg::Key("type".into()));
                out.push(Violation {
                    path: path.clone(),
                    message: format!("must be one of {}, found {}", names.join(", "), short(tag)),
                });
                path.pop();
            }
        }
        return;
    }
    let mut first_errors: Option<Vec<Violation>> = None;
    for b in &resolved {
        let mut errs = Vec::new();
        check(root, b, v, path, &mut errs);
        if errs.is_empty() {
            return;
        }
        if first_errors.as_ref().is_none_or(|f| errs.len() < f.len()) {
            first_errors = Some(errs);
        }
    }
    out.extend(first_errors.unwrap_or_default());
}

// ---------------------------------------------------------------------------
// Locating a path in the source text

/// 1-based line of `path` in a JSON document (the key of the deepest member
/// found, or the start of the item), `None` when the text does not parse.
pub fn locate_json(text: &str, path: &[Seg]) -> Option<usize> {
    let mut s = Scanner {
        b: text.as_bytes(),
        i: 0,
        line: 1,
    };
    s.ws();
    let start = s.line;
    Some(s.descend(path).unwrap_or(start))
}

struct Scanner<'a> {
    b: &'a [u8],
    i: usize,
    line: usize,
}

impl Scanner<'_> {
    fn peek(&self) -> Option<u8> {
        self.b.get(self.i).copied()
    }

    fn bump(&mut self) -> Option<u8> {
        let c = self.peek()?;
        self.i += 1;
        if c == b'\n' {
            self.line += 1;
        }
        Some(c)
    }

    fn ws(&mut self) {
        while matches!(self.peek(), Some(b' ' | b'\t' | b'\r' | b'\n')) {
            self.bump();
        }
    }

    fn string(&mut self) -> Option<String> {
        if self.bump()? != b'"' {
            return None;
        }
        let start = self.i;
        let mut escaped = false;
        loop {
            let c = self.bump()?;
            if escaped {
                escaped = false;
            } else if c == b'\\' {
                escaped = true;
            } else if c == b'"' {
                break;
            }
        }
        let raw = std::str::from_utf8(&self.b[start..self.i - 1]).ok()?;
        serde_json::from_str::<String>(&format!("\"{raw}\"")).ok()
    }

    fn skip(&mut self) -> Option<()> {
        self.ws();
        match self.peek()? {
            b'"' => {
                self.string()?;
            }
            b'{' | b'[' => {
                let mut depth = 0_usize;
                loop {
                    match self.peek()? {
                        b'"' => {
                            self.string()?;
                            continue;
                        }
                        b'{' | b'[' => depth += 1,
                        b'}' | b']' => {
                            depth -= 1;
                            if depth == 0 {
                                self.bump();
                                return Some(());
                            }
                        }
                        _ => {}
                    }
                    self.bump();
                }
            }
            _ => {
                while !matches!(
                    self.peek(),
                    None | Some(b',' | b'}' | b']' | b' ' | b'\n' | b'\r' | b'\t')
                ) {
                    self.bump();
                }
            }
        }
        Some(())
    }

    /// Walks to `path`; returns the line of the last segment found.
    fn descend(&mut self, path: &[Seg]) -> Option<usize> {
        let Some((first, rest)) = path.split_first() else {
            self.ws();
            return Some(self.line);
        };
        self.ws();
        match (first, self.peek()?) {
            (Seg::Key(want), b'{') => {
                self.bump();
                loop {
                    self.ws();
                    if self.peek()? == b'}' {
                        return None;
                    }
                    let key_line = self.line;
                    let key = self.string()?;
                    self.ws();
                    if self.bump()? != b':' {
                        return None;
                    }
                    if &key == want {
                        return Some(self.descend(rest).unwrap_or(key_line).max(key_line));
                    }
                    self.skip()?;
                    self.ws();
                    if self.peek()? == b',' {
                        self.bump();
                    }
                }
            }
            (Seg::Index(want), b'[') => {
                self.bump();
                let mut n = 0;
                loop {
                    self.ws();
                    if self.peek()? == b']' {
                        return None;
                    }
                    if n == *want {
                        let line = self.line;
                        return Some(self.descend(rest).unwrap_or(line).max(line));
                    }
                    self.skip()?;
                    self.ws();
                    if self.peek()? == b',' {
                        self.bump();
                    }
                    n += 1;
                }
            }
            _ => None,
        }
    }
}

/// Best-effort 1-based line of `path` in a TOML document of the shapes the
/// project files use (top-level keys, `[table]` sections, `[[array]]`
/// sections, inline values).
pub fn locate_toml(text: &str, path: &[Seg]) -> Option<usize> {
    let key_of = |line: &str| -> Option<String> {
        let t = line.trim_start();
        if t.starts_with('#') || t.starts_with('[') {
            return None;
        }
        let (k, _) = t.split_once('=')?;
        Some(k.trim().trim_matches('"').to_owned())
    };
    let header_of = |line: &str| -> Option<(bool, String)> {
        let t = line.trim();
        if let Some(h) = t.strip_prefix("[[") {
            return Some((true, h.split("]]").next()?.trim().to_owned()));
        }
        let h = t.strip_prefix('[')?;
        Some((false, h.split(']').next()?.trim().to_owned()))
    };
    let lines: Vec<&str> = text.lines().collect();
    let Some(Seg::Key(top)) = path.first() else {
        return Some(1);
    };
    // A top-level key before any header, or a header naming the table.
    let mut in_root = true;
    let mut table_count = 0_usize;
    let wanted_index = match path.get(1) {
        Some(Seg::Index(i)) => Some(*i),
        _ => None,
    };
    let inner_key = match (path.get(1), path.get(2)) {
        (Some(Seg::Key(k)), _) | (Some(Seg::Index(_)), Some(Seg::Key(k))) => Some(k.as_str()),
        _ => None,
    };
    let mut found: Option<usize> = None;
    let mut in_target = false;
    for (n, line) in lines.iter().enumerate() {
        if let Some((array, h)) = header_of(line) {
            in_root = false;
            in_target = false;
            if &h == top {
                if array {
                    if wanted_index.is_none_or(|w| w == table_count) {
                        in_target = true;
                        found = Some(n + 1);
                    }
                    table_count += 1;
                } else {
                    in_target = true;
                    found = Some(n + 1);
                }
            }
            continue;
        }
        let Some(k) = key_of(line) else { continue };
        if in_root && &k == top {
            return Some(n + 1);
        }
        if in_target && inner_key == Some(k.as_str()) {
            return Some(n + 1);
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    fn errs(schema: &Value, v: Value) -> Vec<String> {
        validate(schema, &v)
            .into_iter()
            .map(|e| format!("{}: {}", path_text(&e.path), e.message))
            .collect()
    }

    #[test]
    fn collections_validate_fields_by_type() {
        let s = collection();
        assert!(errs(&s, json!({"fields": [{"name": "t", "type": "text", "max_length": 5}]})).is_empty());
        let e = errs(&s, json!({"fields": [{"name": "t", "type": "text", "maxlength": 5}]}));
        assert_eq!(e.len(), 1, "{e:?}");
        assert!(e[0].starts_with("fields[0].maxlength: unknown key"), "{e:?}");
        let e = errs(&s, json!({"fields": [{"name": "t", "type": "txt"}]}));
        assert!(e[0].starts_with("fields[0].type: must be one of"), "{e:?}");
        let e = errs(&s, json!({"fields": [{"name": "r", "type": "relation"}]}));
        assert!(e[0].contains("missing required key \"collection\""), "{e:?}");
        let e = errs(&s, json!({"type": "table", "versioned": "yes"}));
        assert_eq!(e.len(), 2, "{e:?}");
        let e = errs(&s, json!({"rules": {"list_rule": 3}}));
        assert!(e[0].contains("must be string or null"), "{e:?}");
    }

    #[test]
    fn other_kinds_reject_unknown_keys_and_bad_values() {
        assert!(errs(&subscription(), json!({"topic": "t", "start": "earliest"})).is_empty());
        assert!(
            errs(
                &subscription(),
                json!({"topic": "t", "start": {"time": "2026-01-01T00:00:00Z"}})
            )
            .is_empty()
        );
        assert!(!errs(&subscription(), json!({"topic": "t", "start": "middle"})).is_empty());
        assert!(errs(&subscription(), json!({})).iter().any(|e| e.contains("\"topic\"")));
        assert!(!errs(&bucket(), json!({"visibility": "secret"})).is_empty());
        assert!(!errs(&function(), json!({"limits": {"fuel": -1}})).is_empty());
        assert!(!errs(&project(), json!({"format": 2, "name": "x"})).is_empty());
        assert!(
            errs(
                &project(),
                json!({"format": 1, "name": "x", "environments": {"prod": {"url": "https://x"}}})
            )
            .is_empty()
        );
        assert!(!errs(&policy(), json!({"rules": {"b": {"read": 1}}})).is_empty());
    }

    #[test]
    fn json_paths_map_to_lines() {
        let text = "{\n  \"name\": \"posts\",\n  \"fields\": [\n    {\"name\": \"a\", \"type\": \"text\"},\n    {\n      \"name\": \"b\",\n      \"maxlength\": 3\n    }\n  ]\n}\n";
        let p = |segs: &[Seg]| locate_json(text, segs);
        assert_eq!(p(&[Seg::Key("name".into())]), Some(2));
        assert_eq!(p(&[Seg::Key("fields".into()), Seg::Index(0)]), Some(4));
        assert_eq!(
            p(&[Seg::Key("fields".into()), Seg::Index(1), Seg::Key("maxlength".into())]),
            Some(7)
        );
        assert_eq!(p(&[]), Some(1));
        assert_eq!(p(&[Seg::Key("missing".into())]), Some(1));
    }

    #[test]
    fn toml_paths_map_to_lines() {
        let text = "description = \"x\"\naccess = \"public\"\n\n[limits]\nfuel = 5\nbogus = 1\n\n[[hooks]]\ncollection = \"a\"\n\n[[hooks]]\ncollection = \"b\"\nevents = [\"x\"]\n";
        let p = |segs: &[Seg]| locate_toml(text, segs);
        assert_eq!(p(&[Seg::Key("access".into())]), Some(2));
        assert_eq!(p(&[Seg::Key("limits".into()), Seg::Key("bogus".into())]), Some(6));
        assert_eq!(p(&[Seg::Key("limits".into())]), Some(4));
        assert_eq!(
            p(&[Seg::Key("hooks".into()), Seg::Index(1), Seg::Key("events".into())]),
            Some(13)
        );
    }
}

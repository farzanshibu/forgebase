//! The resource model shared by the layout reader, the server reader and the
//! planner: resource kinds, canonical specs, the stable JSON printer and the
//! structural diff.
//!
//! A *spec* is the canonical JSON of one resource: the same shape whether it
//! comes from a project file or from the server, with defaults filled in and
//! server-managed values (ids, timestamps, versions, counters) left out, so
//! two specs are equal exactly when the resource needs no change.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fmt::Write as _;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

/// Kinds of resources, in the order changes are applied (deletes run in reverse).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    /// `collections/<name>.json` (fields, indexes, access rules).
    Collection,
    /// `storage/buckets/<name>.json`.
    Bucket,
    /// `queues/topics/<name>.json`.
    Topic,
    /// `functions/<name>/function.toml`.
    Function,
    /// `queues/subscriptions/<name>.json`.
    Subscription,
    /// One entry of `policies/<resource kind>.json`; the name is `<resource kind>/<name>`.
    Policy,
    /// `oauth-clients/<name>.json`.
    OauthClient,
}

impl Kind {
    /// Every kind in apply order.
    pub const ALL: [Kind; 7] = [
        Kind::Collection,
        Kind::Bucket,
        Kind::Topic,
        Kind::Function,
        Kind::Subscription,
        Kind::Policy,
        Kind::OauthClient,
    ];

    /// Name in output and plan files.
    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Collection => "collection",
            Kind::Bucket => "bucket",
            Kind::Topic => "topic",
            Kind::Function => "function",
            Kind::Subscription => "subscription",
            Kind::Policy => "policy",
            Kind::OauthClient => "oauth_client",
        }
    }

    /// The `manage` area (project file) the kind belongs to.
    pub fn area(self) -> Area {
        match self {
            Kind::Collection => Area::Collections,
            Kind::Bucket => Area::Storage,
            Kind::Topic | Kind::Subscription => Area::Queues,
            Kind::Function => Area::Functions,
            Kind::Policy => Area::Policies,
            Kind::OauthClient => Area::OauthClients,
        }
    }
}

impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Groups of kinds a project can own (`manage` in `forgebase.project.toml`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Area {
    /// Collections.
    Collections,
    /// Functions.
    Functions,
    /// Topics and subscriptions.
    Queues,
    /// Buckets.
    Storage,
    /// Engine resource rules.
    Policies,
    /// OAuth client apps.
    OauthClients,
}

impl Area {
    /// Every area.
    pub const ALL: [Area; 6] = [
        Area::Collections,
        Area::Functions,
        Area::Queues,
        Area::Storage,
        Area::Policies,
        Area::OauthClients,
    ];

    /// Name in `manage`.
    pub fn as_str(self) -> &'static str {
        match self {
            Area::Collections => "collections",
            Area::Functions => "functions",
            Area::Queues => "queues",
            Area::Storage => "storage",
            Area::Policies => "policies",
            Area::OauthClients => "oauth-clients",
        }
    }

    /// Parses a `manage` entry.
    pub fn parse(s: &str) -> Option<Area> {
        Area::ALL.into_iter().find(|a| a.as_str() == s)
    }
}

/// Identity of a resource.
pub type Key = (Kind, String);

/// Code of a function as it will be uploaded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Code {
    /// A WebAssembly module.
    Wasm(Vec<u8>),
    /// JavaScript or TypeScript source.
    Source {
        /// `javascript` or `typescript`.
        language: &'static str,
        /// The text.
        text: String,
    },
}

impl Code {
    /// `wasm`, `javascript` or `typescript` (the server's names).
    pub fn language(&self) -> &'static str {
        match self {
            Code::Wasm(_) => "wasm",
            Code::Source { language, .. } => language,
        }
    }

    /// SHA-256 (hex) the server records for the version: of the module, or of the source as sent.
    pub fn sha256(&self) -> String {
        match self {
            Code::Wasm(b) => hex(&Sha256::digest(b)),
            Code::Source { text, .. } => hex(&Sha256::digest(text.as_bytes())),
        }
    }
}

/// Lower-case hex.
pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().fold(String::with_capacity(bytes.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

/// SHA-256 (hex) of a value's canonical text.
pub fn digest(v: &Value) -> String {
    hex(&Sha256::digest(canonical_text(v).as_bytes()))
}

/// Kind-specific data of a desired resource that is not part of its spec.
#[derive(Debug, Clone, Default)]
pub enum Extra {
    /// Nothing.
    #[default]
    None,
    /// A collection.
    Collection {
        /// Previous name of the collection.
        renamed_from: Option<String>,
        /// Field renames: new name -> previous name.
        field_renames: BTreeMap<String, String>,
    },
    /// A function.
    Function(Box<FunctionExtra>),
    /// A subscription.
    Subscription {
        /// Where a new subscription starts (create only).
        start: Option<Value>,
        /// Variable for a newly issued webhook secret.
        secret_env: Option<String>,
    },
    /// An OAuth client.
    OauthClient {
        /// Variable for a newly issued client secret.
        secret_env: Option<String>,
    },
}

/// What a function deploy needs besides its spec.
#[derive(Debug, Clone)]
pub struct FunctionExtra {
    /// The function directory.
    pub dir: PathBuf,
    /// The entry file.
    pub entry: PathBuf,
    /// Build command (`forgebase deploy`).
    pub build: Option<String>,
    /// The code (absent when the entry file does not exist yet and a build will create it).
    pub code: Option<Code>,
    /// Secret name -> environment variable.
    pub secrets: BTreeMap<String, String>,
}

/// A resource as the project declares it.
#[derive(Debug, Clone)]
pub struct Desired {
    /// Kind.
    pub kind: Kind,
    /// Name.
    pub name: String,
    /// Canonical spec.
    pub spec: Value,
    /// File that declares it (relative to the project directory).
    pub file: PathBuf,
    /// Kind-specific extras.
    pub extra: Extra,
}

/// A resource as the server has it.
#[derive(Debug, Clone)]
pub struct Actual {
    /// Kind.
    pub kind: Kind,
    /// Name.
    pub name: String,
    /// Canonical spec.
    pub spec: Value,
    /// Server revision (schema version, latest function version, update time).
    pub revision: String,
    /// Server representation the apply step needs (collection definition, client id, ...).
    pub raw: Value,
}

impl Actual {
    /// Fingerprint of the server state: changes whenever the resource changes on the server.
    pub fn fingerprint(&self) -> String {
        digest(&json!({"spec": self.spec, "revision": self.revision}))
    }
}

/// Resources the server creates itself: never reported as unmanaged, never
/// pruned, and pulled only when they carry project changes (the default auth
/// collection with fields of its own). Declared in a project, they are managed
/// like any other resource.
pub fn is_builtin(kind: Kind, name: &str) -> bool {
    matches!(
        (kind, name),
        (Kind::Collection, "users") | (Kind::Bucket, "forgebase-job-results")
    )
}

/// Whole-valued floats as integers (`0.0` -> `0`): the server stores number
/// options as floats, files usually hold integers.
pub fn canon_numbers(v: &mut Value) {
    match v {
        Value::Number(n) if !n.is_i64() && !n.is_u64() => {
            if let Some(f) = n.as_f64()
                && f.fract() == 0.0
                && f.abs() < 9_007_199_254_740_992.0
            {
                #[allow(clippy::cast_possible_truncation)]
                let i = f as i64;
                *v = Value::from(i);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(canon_numbers),
        Value::Object(m) => m.values_mut().for_each(canon_numbers),
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Stable JSON text

/// Keys printed first, in this order; the rest follow alphabetically.
const FIRST_KEYS: &[&str] = &["$schema", "name", "type", "topic", "path"];

fn ordered_keys(m: &Map<String, Value>) -> Vec<&String> {
    let mut keys: Vec<&String> = m.keys().collect();
    keys.sort_by(|a, b| {
        let rank = |k: &str| FIRST_KEYS.iter().position(|f| *f == k).unwrap_or(FIRST_KEYS.len());
        rank(a).cmp(&rank(b)).then_with(|| a.cmp(b))
    });
    keys
}

/// One-line compact text with keys in printing order (also the canonical text for digests).
pub fn canonical_text(v: &Value) -> String {
    let mut s = String::new();
    inline(v, &mut s);
    s
}

fn inline(v: &Value, out: &mut String) {
    match v {
        Value::Array(a) => {
            out.push('[');
            for (i, x) in a.iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                inline(x, out);
            }
            out.push(']');
        }
        Value::Object(m) => {
            if m.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            for (i, k) in ordered_keys(m).into_iter().enumerate() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                inline(&m[k], out);
            }
            out.push_str(" }");
        }
        scalar => out.push_str(&scalar.to_string()),
    }
}

/// Whether a value prints on one line: scalars, and containers of scalars
/// (or of short scalar arrays) that fit.
fn fits_inline(v: &Value, width: usize) -> bool {
    let shallow = match v {
        Value::Array(a) => a.iter().all(|x| !x.is_array() && !x.is_object()),
        Value::Object(m) => m.values().all(|x| match x {
            Value::Array(a) => a.iter().all(|y| !y.is_array() && !y.is_object()),
            Value::Object(o) => o.is_empty(),
            _ => true,
        }),
        _ => true,
    };
    shallow && canonical_text(v).len() <= width
}

/// Pretty JSON for project files: two-space indent, `name`/`type` first,
/// short objects and arrays on one line, a trailing newline.
pub fn pretty(v: &Value) -> String {
    let mut out = String::new();
    block(v, 0, &mut out);
    out.push('\n');
    out
}

fn block(v: &Value, indent: usize, out: &mut String) {
    let pad = |n: usize| "  ".repeat(n);
    match v {
        Value::Array(a) if !a.is_empty() && !fits_inline(v, 96usize.saturating_sub(indent * 2)) => {
            out.push_str("[\n");
            for (i, x) in a.iter().enumerate() {
                out.push_str(&pad(indent + 1));
                block(x, indent + 1, out);
                if i + 1 < a.len() {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push(']');
        }
        Value::Object(m) if !m.is_empty() && (indent == 0 || !fits_inline(v, 96usize.saturating_sub(indent * 2))) => {
            out.push_str("{\n");
            let keys = ordered_keys(m);
            let n = keys.len();
            for (i, k) in keys.into_iter().enumerate() {
                out.push_str(&pad(indent + 1));
                out.push_str(&Value::String(k.clone()).to_string());
                out.push_str(": ");
                block(&m[k], indent + 1, out);
                if i + 1 < n {
                    out.push(',');
                }
                out.push('\n');
            }
            out.push_str(&pad(indent));
            out.push('}');
        }
        other => inline(other, out),
    }
}

// ---------------------------------------------------------------------------
// Canonical specs

fn s(v: &Value, k: &str) -> Option<String> {
    v.get(k).and_then(Value::as_str).map(str::to_owned)
}

fn b(v: &Value, k: &str) -> bool {
    v.get(k).and_then(Value::as_bool).unwrap_or(false)
}

fn or(v: &Value, k: &str, default: Value) -> Value {
    match v.get(k) {
        Some(x) if !x.is_null() => x.clone(),
        _ => default,
    }
}

fn opt(v: &Value, k: &str) -> Value {
    v.get(k).cloned().unwrap_or(Value::Null)
}

fn sorted_strings(v: Option<&Value>) -> Value {
    let mut items: Vec<String> = v
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
        .unwrap_or_default();
    items.sort();
    items.dedup();
    json!(items)
}

fn string_map(v: Option<&Value>) -> Value {
    let m: BTreeMap<String, Value> = v
        .and_then(Value::as_object)
        .map(|o| o.iter().map(|(k, x)| (k.clone(), x.clone())).collect())
        .unwrap_or_default();
    json!(m)
}

/// An option key and its default (`None`: no default, omitted when unset).
type OptionDefault = (&'static str, Option<fn() -> Value>);

/// Option keys of a field type with their defaults.
fn field_options(ty: &str) -> &'static [OptionDefault] {
    fn one() -> Value {
        json!(1)
    }
    fn f() -> Value {
        json!(false)
    }
    fn eight() -> Value {
        json!(8)
    }
    fn json_items() -> Value {
        json!("json")
    }
    match ty {
        "text" => &[("min_length", None), ("max_length", None)],
        "number" => &[("min", None), ("max", None), ("integer", Some(f))],
        "select" => &[("values", None), ("max_select", Some(one))],
        "json" => &[("max_size", None)],
        "array" => &[("items", Some(json_items)), ("min_items", None), ("max_items", None)],
        "relation" => &[
            ("collection", None),
            ("max_select", Some(one)),
            ("cascade_delete", Some(f)),
        ],
        "file" => &[("max_select", Some(one))],
        "password" => &[("min_length", Some(eight))],
        "vector" => &[("dimensions", None)],
        _ => &[],
    }
}

/// Canonical field: common keys, then type options with defaults filled.
/// `collection_names` maps a relation's `collection_id` (server form) to a name.
pub fn field_spec(f: &Value, collection_names: Option<&HashMap<String, String>>) -> Value {
    let ty = s(f, "type").unwrap_or_default();
    let mut out = Map::new();
    out.insert("name".into(), json!(s(f, "name").unwrap_or_default()));
    out.insert("type".into(), json!(ty));
    out.insert("required".into(), json!(b(f, "required")));
    out.insert("unique".into(), json!(b(f, "unique")));
    out.insert("hidden".into(), json!(b(f, "hidden")));
    if let Some(d) = f.get("default").filter(|d| !d.is_null()) {
        out.insert("default".into(), d.clone());
    }
    for (key, default) in field_options(&ty) {
        let value = if *key == "collection" {
            match collection_names {
                Some(names) => f
                    .get("collection_id")
                    .and_then(Value::as_str)
                    .map(|id| json!(names.get(id).cloned().unwrap_or_else(|| format!("#{id}")))),
                None => f.get("collection").cloned(),
            }
        } else {
            f.get(*key).filter(|x| !x.is_null()).cloned()
        };
        match (value, default) {
            (Some(v), _) => {
                out.insert((*key).into(), v);
            }
            (None, Some(d)) => {
                out.insert((*key).into(), d());
            }
            (None, None) => {}
        }
    }
    Value::Object(out)
}

/// Canonical index (definition of a manual index).
pub fn index_spec(i: &Value) -> Value {
    let keys: Vec<Value> = i
        .get("keys")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|k| {
                    let mut m = Map::new();
                    m.insert("path".into(), json!(s(k, "path").unwrap_or_default()));
                    if b(k, "desc") {
                        m.insert("desc".into(), json!(true));
                    }
                    Value::Object(m)
                })
                .collect()
        })
        .unwrap_or_default();
    json!({
        "name": s(i, "name").unwrap_or_default(),
        "keys": keys,
        "unique": b(i, "unique"),
        "kind": or(i, "kind", json!("btree")),
        "partial": opt(i, "partial"),
    })
}

/// Canonical access rules of a collection.
pub fn rules_spec(r: &Value) -> Value {
    let mut fields = BTreeMap::new();
    if let Some(m) = r.get("fields").and_then(Value::as_object) {
        for (k, v) in m {
            let read = opt(v, "read");
            let write = opt(v, "write");
            if read.is_null() && write.is_null() {
                continue;
            }
            fields.insert(k.to_ascii_lowercase(), json!({"read": read, "write": write}));
        }
    }
    json!({
        "list_rule": opt(r, "list_rule"),
        "view_rule": opt(r, "view_rule"),
        "create_rule": opt(r, "create_rule"),
        "update_rule": opt(r, "update_rule"),
        "delete_rule": opt(r, "delete_rule"),
        "fields": fields,
    })
}

/// Canonical collection from a project file (indexes and rules only when the file has the key).
pub fn collection_spec(f: &Value) -> Value {
    let ty = s(f, "type").unwrap_or_else(|| "base".into());
    let view = ty == "view";
    let mut out = Map::new();
    out.insert("type".into(), json!(ty));
    out.insert("schema_mode".into(), or(f, "schema_mode", json!("strict")));
    out.insert("versioned".into(), json!(b(f, "versioned")));
    out.insert("view_query".into(), opt(f, "view_query"));
    out.insert("options".into(), string_map(f.get("options")));
    let fields: Vec<Value> = if view {
        Vec::new()
    } else {
        f.get("fields")
            .and_then(Value::as_array)
            .map(|a| a.iter().map(|x| field_spec(x, None)).collect())
            .unwrap_or_default()
    };
    out.insert("fields".into(), json!(fields));
    if let Some(ix) = f.get("indexes").and_then(Value::as_array) {
        let mut ix: Vec<Value> = ix.iter().map(index_spec).collect();
        ix.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
        out.insert("indexes".into(), json!(ix));
    }
    if let Some(r) = f.get("rules") {
        out.insert("rules".into(), rules_spec(r));
    }
    Value::Object(out)
}

/// Canonical collection from the server: system fields left out, relation ids as names.
pub fn collection_spec_from_server(
    c: &Value,
    names: &HashMap<String, String>,
    indexes: &[Value],
    rules: Option<&Value>,
) -> Value {
    let ty = s(c, "type").unwrap_or_else(|| "base".into());
    let view = ty == "view";
    let fields: Vec<Value> = if view {
        Vec::new()
    } else {
        c.get("fields")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter(|f| !b(f, "system"))
                    .map(|f| field_spec(f, Some(names)))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut ix: Vec<Value> = indexes.iter().map(index_spec).collect();
    ix.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    let mut out = json!({
        "type": ty,
        "schema_mode": or(c, "schema_mode", json!("strict")),
        "versioned": b(c, "versioned"),
        "view_query": opt(c, "view_query"),
        "options": string_map(c.get("options")),
        "fields": fields,
        "indexes": ix,
    });
    if let Some(r) = rules {
        out["rules"] = rules_spec(r);
    }
    out
}

/// Canonical bucket (`acl` only when given).
pub fn bucket_spec(f: &Value, acl: Option<&Value>) -> Value {
    let types = match f.get("allowed_content_types").and_then(Value::as_array) {
        Some(a) if !a.is_empty() => sorted_strings(Some(&Value::Array(a.clone()))),
        _ => Value::Null,
    };
    let max = match f.get("max_object_bytes") {
        Some(Value::Number(n)) if n.as_i64() != Some(0) => Value::Number(n.clone()),
        _ => Value::Null,
    };
    let mut out = json!({
        "visibility": or(f, "visibility", json!("private")),
        "versioning": b(f, "versioning"),
        "max_object_bytes": max,
        "allowed_content_types": types,
    });
    if let Some(a) = acl {
        let mut entries: Vec<Value> = a
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|e| {
                        json!({
                            "grantee": s(e, "grantee").unwrap_or_default(),
                            "permission": s(e, "permission").unwrap_or_default(),
                            "prefix": s(e, "prefix").unwrap_or_default(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        entries.sort_by_key(canonical_text);
        entries.dedup();
        out["acl"] = json!(entries);
    }
    out
}

/// Settings of a topic that stay unmanaged when a file leaves them out.
pub const TOPIC_OPTIONAL: &[&str] = &[
    "retention_ms",
    "retention_bytes",
    "max_message_bytes",
    "dedup_window_ms",
];
/// Settings of a subscription that stay unmanaged when a file leaves them out.
pub const SUBSCRIPTION_OPTIONAL: &[&str] = &["ack_deadline_ms", "backoff_base_ms", "backoff_max_ms"];

fn source_spec(v: &Value) -> Value {
    if !v.is_object() {
        return Value::Null;
    }
    let mut ops = sorted_strings(v.get("ops"));
    if ops.as_array().is_some_and(Vec::is_empty) {
        ops = json!(["create", "delete", "update"]);
    }
    json!({"collection": s(v, "collection").unwrap_or_default(), "ops": ops})
}

/// Canonical topic; `from_file` keeps only the optional settings the file has.
pub fn topic_spec(f: &Value, from_file: bool) -> Value {
    let mut out = json!({
        "labels": string_map(f.get("labels")),
        "schema": opt(f, "schema"),
        "source": source_spec(&opt(f, "source")),
    });
    for k in TOPIC_OPTIONAL {
        if !from_file || f.get(*k).is_some() {
            out[*k] = opt(f, k);
        }
    }
    out
}

/// Canonical subscription; `from_file` keeps only the optional settings the file has.
pub fn subscription_spec(f: &Value, from_file: bool) -> Value {
    let push = match f.get("push") {
        Some(p @ Value::Object(_)) => {
            json!({"kind": s(p, "kind").unwrap_or_default(), "endpoint": s(p, "endpoint").unwrap_or_default()})
        }
        _ => Value::Null,
    };
    let mut out = json!({
        "topic": s(f, "topic").unwrap_or_default(),
        "filter": opt(f, "filter"),
        "ordering": b(f, "ordering"),
        "push": push,
        "dead_letter_topic": opt(f, "dead_letter_topic"),
        "max_delivery_attempts": match f.get("max_delivery_attempts") {
            Some(Value::Number(n)) if n.as_i64() != Some(0) => Value::Number(n.clone()),
            _ => Value::Null,
        },
        "labels": string_map(f.get("labels")),
    });
    for k in SUBSCRIPTION_OPTIONAL {
        if !from_file || f.get(*k).is_some() {
            out[*k] = opt(f, k);
        }
    }
    out
}

/// Canonical function. `config` is a version configuration (server) or the
/// file itself; `limits` keeps only the keys present.
pub fn function_spec(
    description: &str,
    access: &str,
    config: &Value,
    code: (&str, &str),
    secrets: &BTreeSet<String>,
    kv_rules: Option<&Value>,
) -> Value {
    let mut hooks: Vec<Value> = config
        .get("hooks")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|h| json!({"collection": s(h, "collection").unwrap_or_default(), "events": sorted_strings(h.get("events"))}))
                .collect()
        })
        .unwrap_or_default();
    hooks.sort_by_key(canonical_text);
    let mut out = json!({
        "description": description,
        "access": access,
        "capabilities": sorted_strings(config.get("capabilities")),
        "fetch_hosts": sorted_strings(config.get("fetch_hosts")),
        "env": string_map(config.get("env")),
        "hooks": hooks,
        "limits": string_map(config.get("limits")),
        "code": {"language": code.0, "sha256": code.1},
        "secrets": secrets.iter().collect::<Vec<_>>(),
    });
    if let Some(k) = kv_rules {
        let mut rules: Vec<Value> = k
            .as_array()
            .map(|a| {
                a.iter()
                    .map(|r| json!({"namespace": s(r, "namespace").unwrap_or_default(), "access": s(r, "access").unwrap_or_default()}))
                    .collect()
            })
            .unwrap_or_default();
        rules.sort_by_key(canonical_text);
        out["kv_rules"] = json!(rules);
    }
    out
}

/// Settings of an OAuth client that stay unmanaged when a file leaves them out.
pub const OAUTH_OPTIONAL: &[&str] = &["grant_types", "scopes"];

/// Canonical OAuth client; `from_file` keeps only the optional settings the file has.
pub fn oauth_spec(f: &Value, from_file: bool) -> Value {
    let mut out = json!({
        "type": or(f, "type", json!("confidential")),
        "description": opt(f, "description"),
        "redirect_uris": sorted_strings(f.get("redirect_uris")),
        "post_logout_redirect_uris": sorted_strings(f.get("post_logout_redirect_uris")),
        "skip_consent": b(f, "skip_consent"),
        "access_token_ttl": opt(f, "access_token_ttl"),
        "refresh_token_ttl": opt(f, "refresh_token_ttl"),
        "disabled": b(f, "disabled"),
    });
    for k in OAUTH_OPTIONAL {
        if !from_file || f.get(*k).is_some() {
            out[*k] = sorted_strings(f.get(*k));
        }
    }
    out
}

/// Canonical engine resource rules (action -> rule).
pub fn policy_spec(rules: &Value) -> Value {
    json!({"rules": string_map(Some(rules))})
}

/// The server spec restricted to what the desired spec manages: settings a
/// file leaves out keep the server's value and are not a difference.
pub fn managed_view(kind: Kind, desired: &Value, actual: &Value) -> Value {
    let mut a = actual.clone();
    let Some(am) = a.as_object_mut() else { return a };
    let has = |k: &str| desired.get(k).is_some();
    let drop_absent = |am: &mut Map<String, Value>, keys: &[&str]| {
        for k in keys {
            if !has(k) {
                am.remove(*k);
            }
        }
    };
    match kind {
        Kind::Collection => {
            drop_absent(am, &["indexes", "rules"]);
            if let (Some(Value::Object(opts)), Some(want)) =
                (am.get_mut("options"), desired.get("options").and_then(Value::as_object))
            {
                opts.retain(|k, _| want.contains_key(k));
            }
        }
        Kind::Bucket => drop_absent(am, &["acl"]),
        Kind::Topic => drop_absent(am, TOPIC_OPTIONAL),
        Kind::Subscription => drop_absent(am, SUBSCRIPTION_OPTIONAL),
        Kind::OauthClient => drop_absent(am, OAUTH_OPTIONAL),
        Kind::Function => {
            drop_absent(am, &["kv_rules"]);
            if let (Some(Value::Object(limits)), Some(want)) =
                (am.get_mut("limits"), desired.get("limits").and_then(Value::as_object))
            {
                limits.retain(|k, _| want.contains_key(k));
            }
        }
        Kind::Policy => {}
    }
    a
}

// ---------------------------------------------------------------------------
// Structural diff

/// One difference between two specs.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Delta {
    /// Dotted path (`fields.title.max_length`, `fields[order]`).
    pub path: String,
    /// Server value (`None`: absent).
    pub before: Option<Value>,
    /// Project value (`None`: absent).
    pub after: Option<Value>,
}

fn named(a: &[Value]) -> Option<Vec<(&str, &Value)>> {
    a.iter()
        .map(|x| x.get("name").and_then(Value::as_str).map(|n| (n, x)))
        .collect()
}

fn join(path: &str, k: &str) -> String {
    if path.is_empty() {
        k.to_owned()
    } else {
        format!("{path}.{k}")
    }
}

/// Differences from `before` (server) to `after` (project).
pub fn diff(before: &Value, after: &Value) -> Vec<Delta> {
    let mut out = Vec::new();
    walk("", Some(before), Some(after), &mut out);
    out
}

fn walk(path: &str, before: Option<&Value>, after: Option<&Value>, out: &mut Vec<Delta>) {
    if before == after {
        return;
    }
    if let (Some(Value::Number(x)), Some(Value::Number(y))) = (before, after)
        && x.as_f64() == y.as_f64()
    {
        return;
    }
    match (before, after) {
        (Some(Value::Object(x)), Some(Value::Object(y))) => {
            let keys: BTreeSet<&String> = x.keys().chain(y.keys()).collect();
            for k in keys {
                walk(&join(path, k), x.get(k), y.get(k), out);
            }
        }
        (Some(Value::Array(x)), Some(Value::Array(y))) => match (named(x), named(y)) {
            (Some(nx), Some(ny)) if !nx.is_empty() || !ny.is_empty() => {
                let bx: BTreeMap<&str, &Value> = nx.iter().copied().collect();
                let by: BTreeMap<&str, &Value> = ny.iter().copied().collect();
                // In project order, then the ones only the server has.
                let mut names: Vec<&str> = ny.iter().map(|(n, _)| *n).collect();
                names.extend(nx.iter().map(|(n, _)| *n).filter(|n| !by.contains_key(n)));
                for n in names {
                    walk(&join(path, n), bx.get(n).copied(), by.get(n).copied(), out);
                }
                let ox: Vec<&str> = nx.iter().map(|(n, _)| *n).filter(|n| by.contains_key(n)).collect();
                let oy: Vec<&str> = ny.iter().map(|(n, _)| *n).filter(|n| bx.contains_key(n)).collect();
                if ox != oy {
                    out.push(Delta {
                        path: format!("{path}[order]"),
                        before: Some(json!(ox)),
                        after: Some(json!(oy)),
                    });
                }
            }
            _ => out.push(Delta {
                path: path.to_owned(),
                before: before.cloned(),
                after: after.cloned(),
            }),
        },
        _ => out.push(Delta {
            path: path.to_owned(),
            before: before.cloned(),
            after: after.cloned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_is_stable_and_puts_names_first() {
        let v = json!({"fields": [{"type": "text", "required": true, "name": "title", "max_length": 200}], "$schema": "../x", "name": "posts", "versioned": false});
        let text = pretty(&v);
        assert_eq!(
            text,
            "{\n  \"$schema\": \"../x\",\n  \"name\": \"posts\",\n  \"fields\": [\n    { \"name\": \"title\", \"type\": \"text\", \"max_length\": 200, \"required\": true }\n  ],\n  \"versioned\": false\n}\n"
        );
        let back: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(back, v);
        assert_eq!(pretty(&back), text);
    }

    #[test]
    fn field_specs_fill_defaults_and_name_relations() {
        let names: HashMap<String, String> = [("c1".to_owned(), "users".to_owned())].into();
        let server = json!({"id": "f1", "name": "owner", "type": "relation", "collection_id": "c1", "max_select": 1, "cascade_delete": false, "required": false, "unique": false, "hidden": false, "system": false});
        let file = json!({"name": "owner", "type": "relation", "collection": "users"});
        assert_eq!(field_spec(&server, Some(&names)), field_spec(&file, None));
        let sel = field_spec(&json!({"name": "s", "type": "select", "values": ["a"]}), None);
        assert_eq!(sel["max_select"], 1);
        assert_eq!(
            field_spec(&json!({"name": "p", "type": "password"}), None)["min_length"],
            8
        );
    }

    #[test]
    fn diffs_name_fields_and_report_order() {
        let a = json!({"fields": [{"name": "a", "type": "text"}, {"name": "b", "type": "text"}], "versioned": false});
        let b = json!({"fields": [{"name": "b", "type": "number"}, {"name": "a", "type": "text"}, {"name": "c", "type": "bool"}], "versioned": false});
        let d = diff(&a, &b);
        let paths: Vec<&str> = d.iter().map(|d| d.path.as_str()).collect();
        assert_eq!(paths, vec!["fields.b.type", "fields.c", "fields[order]"]);
        assert!(d[1].before.is_none());
        assert!(diff(&a, &a).is_empty());
    }

    #[test]
    fn managed_view_ignores_settings_the_file_leaves_out() {
        let desired = topic_spec(&json!({"labels": {"a": "b"}}), true);
        let actual = topic_spec(
            &json!({"labels": {"a": "b"}, "retention_ms": 5, "dedup_window_ms": 7}),
            false,
        );
        assert_eq!(managed_view(Kind::Topic, &desired, &actual), desired);
        let desired = topic_spec(&json!({"labels": {"a": "b"}, "retention_ms": 6}), true);
        assert_eq!(diff(&managed_view(Kind::Topic, &desired, &actual), &desired).len(), 1);
    }
}

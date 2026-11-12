//! The operations an agent can reach through the generic MCP tools, read from
//! the server's own OpenAPI document (the one `GET /api/openapi.json` serves,
//! so a disabled module's operations are already gone).
//!
//! Left out: the MCP endpoint itself (no recursion), streaming and browser
//! flows that a single request/response cannot carry (realtime, the KV
//! subscribe stream, OAuth/OIDC redirects), first-run setup, and the database
//! node agent's own routes.

use std::collections::{BTreeMap, HashMap};

use axum::http::Method;
use serde_json::{Map, Value as Json, json};

/// Paths no MCP tool may call, by prefix.
const EXCLUDED_PREFIXES: &[&str] = &[
    "/api/mcp",
    "/api/v1/realtime",
    "/api/setup",
    "/api/auth/oidc",
    "/api/system/database/agent",
    "/admin",
];

/// Path segments that make a `POST` as hard to undo as a `DELETE`.
const DESTRUCTIVE_VERBS: &[&str] = &[
    "revoke", "rotate", "purge", "flush", "restore", "promote", "rebuild", "rollback", "reset",
];

/// Deepest `$ref` chain [`Catalog::describe`] inlines.
const MAX_SCHEMA_DEPTH: usize = 6;

/// Whether `path` is one an MCP tool may call.
pub fn allowed_path(path: &str) -> bool {
    !EXCLUDED_PREFIXES
        .iter()
        .any(|p| path == *p || path.starts_with(&format!("{p}/")))
        && !path.contains("/subscribe/")
}

/// Whether a call is destructive: a `DELETE`, or a `POST` to a revoke, rotate,
/// purge… action. Destructive calls need `confirm: true`.
pub fn destructive(method: &Method, path_template: &str) -> bool {
    *method == Method::DELETE
        || (*method == Method::POST
            && path_template
                .rsplit('/')
                .next()
                .is_some_and(|last| DESTRUCTIVE_VERBS.contains(&last)))
}

/// One documented operation.
#[derive(Debug, Clone)]
pub struct Operation {
    /// `operationId`.
    pub id: String,
    /// HTTP method.
    pub method: Method,
    /// Path template, `{name}` for each path parameter.
    pub path: String,
    /// First tag.
    pub tag: String,
    /// One-line summary.
    pub summary: String,
    /// The operation object from the document.
    raw: Json,
}

impl Operation {
    /// Names of the header parameters the operation declares, lower case.
    pub fn header_params(&self) -> Vec<String> {
        self.raw["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|p| p["in"] == "header")
            .filter_map(|p| p["name"].as_str().map(str::to_ascii_lowercase))
            .collect()
    }

    /// Whether calling it is destructive (see [`destructive`]).
    pub fn destructive(&self) -> bool {
        destructive(&self.method, &self.path)
    }

    fn brief(&self) -> Json {
        json!({
            "operation": self.id,
            "method": self.method.as_str(),
            "path": self.path,
            "tag": self.tag,
            "summary": self.summary,
        })
    }
}

/// Every callable operation of the document, with its schemas.
#[derive(Debug, Default)]
pub struct Catalog {
    ops: Vec<Operation>,
    by_id: HashMap<String, usize>,
    by_route: HashMap<(Method, String), usize>,
    tag_info: BTreeMap<String, String>,
    schemas: Map<String, Json>,
}

impl Catalog {
    /// Indexes `doc` (an OpenAPI 3 document as JSON).
    pub fn new(doc: &Json) -> Self {
        let mut c = Self {
            schemas: doc["components"]["schemas"].as_object().cloned().unwrap_or_default(),
            ..Self::default()
        };
        for t in doc["tags"].as_array().into_iter().flatten() {
            if let Some(name) = t["name"].as_str() {
                c.tag_info
                    .insert(name.to_owned(), t["description"].as_str().unwrap_or("").to_owned());
            }
        }
        let Some(paths) = doc["paths"].as_object() else {
            return c;
        };
        for (path, item) in paths {
            if !allowed_path(path) {
                continue;
            }
            let Some(item) = item.as_object() else { continue };
            for (m, op) in item {
                let Ok(method) = Method::from_bytes(m.to_ascii_uppercase().as_bytes()) else {
                    continue;
                };
                if !matches!(
                    method,
                    Method::GET | Method::POST | Method::PUT | Method::PATCH | Method::DELETE
                ) {
                    continue;
                }
                let id = op["operationId"]
                    .as_str()
                    .map_or_else(|| format!("{} {path}", method.as_str()), str::to_owned);
                let summary = op["summary"]
                    .as_str()
                    .or_else(|| op["description"].as_str())
                    .unwrap_or("")
                    .lines()
                    .next()
                    .unwrap_or("")
                    .to_owned();
                let tag = op["tags"][0].as_str().unwrap_or("other").to_owned();
                let at = c.ops.len();
                c.by_id.insert(id.clone(), at);
                c.by_route.insert((method.clone(), path.clone()), at);
                c.ops.push(Operation {
                    id,
                    method,
                    path: path.clone(),
                    tag,
                    summary,
                    raw: op.clone(),
                });
            }
        }
        c
    }

    /// How many operations are callable.
    pub fn len(&self) -> usize {
        self.ops.len()
    }

    /// Whether no operation is callable.
    pub fn is_empty(&self) -> bool {
        self.ops.is_empty()
    }

    /// The operation an agent names: its `operationId`, or `"METHOD /path"`
    /// with the documented path template.
    pub fn find(&self, name: &str) -> Option<&Operation> {
        let name = name.trim();
        if let Some(&i) = self.by_id.get(name) {
            return self.ops.get(i);
        }
        let (m, p) = name.split_once(' ')?;
        let method = Method::from_bytes(m.trim().to_ascii_uppercase().as_bytes()).ok()?;
        self.by_route
            .get(&(method, p.trim().to_owned()))
            .and_then(|&i| self.ops.get(i))
    }

    /// The operation documented for `method` and `path_template`.
    pub fn route(&self, method: &Method, path_template: &str) -> Option<&Operation> {
        self.by_route
            .get(&(method.clone(), path_template.to_owned()))
            .and_then(|&i| self.ops.get(i))
    }

    /// Without a filter, the tags and how many operations each has (500+
    /// operations do not fit one answer); with one, the matching operations.
    pub fn list(&self, tag: Option<&str>, search: Option<&str>, limit: usize) -> Json {
        let tag = tag.map(str::trim).filter(|t| !t.is_empty());
        let search = search.map(|s| s.trim().to_ascii_lowercase()).filter(|s| !s.is_empty());
        if tag.is_none() && search.is_none() {
            let mut counts: BTreeMap<&str, usize> = BTreeMap::new();
            for op in &self.ops {
                *counts.entry(op.tag.as_str()).or_default() += 1;
            }
            let tags: Vec<Json> = counts
                .into_iter()
                .map(|(t, n)| json!({"tag": t, "operations": n, "description": self.tag_info.get(t).map_or("", String::as_str)}))
                .collect();
            return json!({
                "total": self.ops.len(),
                "tags": tags,
                "hint": "Pass `tag` (one of the tags above) or `search` (words from a path, operation id or summary) to list operations.",
            });
        }
        let words: Vec<&str> = search
            .as_deref()
            .map(|s| s.split_whitespace().collect())
            .unwrap_or_default();
        let hits: Vec<&Operation> = self
            .ops
            .iter()
            .filter(|op| tag.is_none_or(|t| op.tag.eq_ignore_ascii_case(t)))
            .filter(|op| {
                let hay = format!("{} {} {} {}", op.id, op.method, op.path, op.summary).to_ascii_lowercase();
                words.iter().all(|w| hay.contains(w))
            })
            .collect();
        let shown: Vec<Json> = hits.iter().take(limit).map(|op| op.brief()).collect();
        json!({
            "matches": hits.len(),
            "shown": shown.len(),
            "operations": shown,
            "hint": "Call describe_operation with an `operation` from this list for its parameters and body, then call_operation.",
        })
    }

    /// Everything needed to call `op`: parameters, request body and response
    /// schemas, with `#/components/schemas/*` references written out.
    pub fn describe(&self, op: &Operation) -> Json {
        let mut out = op.brief();
        let o = &op.raw;
        out["description"] = o["description"].clone();
        out["destructive"] = Json::Bool(op.destructive());
        let params: Vec<Json> = o["parameters"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|p| {
                json!({
                    "name": p["name"],
                    "in": p["in"],
                    "required": p["required"].as_bool().unwrap_or(false),
                    "description": p["description"],
                    "schema": self.resolve(&p["schema"], 0, &mut Vec::new()),
                })
            })
            .collect();
        out["parameters"] = Json::Array(params);
        if let Some(content) = o["requestBody"]["content"].as_object() {
            let body: Map<String, Json> = content
                .iter()
                .map(|(ct, v)| (ct.clone(), self.resolve(&v["schema"], 0, &mut Vec::new())))
                .collect();
            out["request_body"] = json!({
                "required": o["requestBody"]["required"].as_bool().unwrap_or(false),
                "content": body,
            });
        }
        if let Some(responses) = o["responses"].as_object() {
            let r: Map<String, Json> = responses
                .iter()
                .map(|(status, v)| {
                    let schema = v["content"]
                        .as_object()
                        .and_then(|c| c.values().next())
                        .map(|c| self.resolve(&c["schema"], 0, &mut Vec::new()));
                    (
                        status.clone(),
                        json!({"description": v["description"], "schema": schema}),
                    )
                })
                .collect();
            out["responses"] = Json::Object(r);
        }
        out["call_with"] = json!({
            "tool": "call_operation",
            "operation": op.id,
            "path_params": "each {name} in the path",
            "query": "query parameters, as an object",
            "body": "the JSON request body",
        });
        out
    }

    /// `schema` with component references inlined, up to
    /// [`MAX_SCHEMA_DEPTH`] levels and never into a cycle.
    fn resolve(&self, schema: &Json, depth: usize, seen: &mut Vec<String>) -> Json {
        match schema {
            Json::Object(m) => {
                if let Some(r) = m.get("$ref").and_then(Json::as_str)
                    && let Some(name) = r.strip_prefix("#/components/schemas/")
                {
                    if depth >= MAX_SCHEMA_DEPTH || seen.iter().any(|s| s == name) {
                        return json!({"$ref": r});
                    }
                    let Some(target) = self.schemas.get(name) else {
                        return json!({"$ref": r});
                    };
                    seen.push(name.to_owned());
                    let mut out = self.resolve(target, depth + 1, seen);
                    seen.pop();
                    if let Json::Object(o) = &mut out {
                        o.entry("title").or_insert_with(|| Json::String(name.to_owned()));
                    }
                    return out;
                }
                Json::Object(
                    m.iter()
                        .map(|(k, v)| (k.clone(), self.resolve(v, depth, seen)))
                        .collect(),
                )
            }
            Json::Array(a) => Json::Array(a.iter().map(|v| self.resolve(v, depth, seen)).collect()),
            other => other.clone(),
        }
    }
}

/// Characters a path segment keeps as they are (RFC 3986 unreserved).
fn encode_segment(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

/// The concrete path for `template` with each `{name}` taken from `params`.
/// A parameter that is the last segment may hold slashes (an object key, a
/// function sub-path); its pieces are encoded one by one.
///
/// # Errors
/// The name of a parameter `params` lacks, or one that is empty.
pub fn fill_path(template: &str, params: &Map<String, Json>) -> Result<String, String> {
    let segments: Vec<&str> = template.split('/').collect();
    let last = segments.len().saturating_sub(1);
    let mut out = Vec::with_capacity(segments.len());
    for (i, seg) in segments.iter().enumerate() {
        let Some(name) = seg.strip_prefix('{').and_then(|s| s.strip_suffix('}')) else {
            out.push((*seg).to_owned());
            continue;
        };
        let name = name.trim_start_matches('*');
        let value = match params.get(name) {
            Some(Json::String(s)) => s.clone(),
            Some(Json::Number(n)) => n.to_string(),
            Some(Json::Bool(b)) => b.to_string(),
            _ => return Err(name.to_owned()),
        };
        if value.is_empty() {
            return Err(name.to_owned());
        }
        if i == last {
            out.push(value.split('/').map(encode_segment).collect::<Vec<_>>().join("/"));
        } else {
            out.push(encode_segment(&value));
        }
    }
    Ok(out.join("/"))
}

/// `query` as a URL query string: arrays repeat the key, objects are JSON.
pub fn query_string(query: &Map<String, Json>) -> String {
    let mut q = url::form_urlencoded::Serializer::new(String::new());
    for (k, v) in query {
        match v {
            Json::Null => {}
            Json::Array(items) => {
                for item in items {
                    q.append_pair(k, &scalar(item));
                }
            }
            other => {
                q.append_pair(k, &scalar(other));
            }
        }
    }
    q.finish()
}

fn scalar(v: &Json) -> String {
    match v {
        Json::String(s) => s.clone(),
        Json::Null => String::new(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn doc() -> Json {
        json!({
            "tags": [{"name": "records", "description": "Records"}],
            "components": {"schemas": {
                "Node": {"type": "object", "properties": {"child": {"$ref": "#/components/schemas/Node"}}},
                "Body": {"type": "object", "properties": {"name": {"type": "string"}}},
            }},
            "paths": {
                "/api/v1/collections/{collection}/records/{id}": {
                    "get": {"operationId": "records_get", "tags": ["records"], "summary": "Get one record"},
                    "delete": {"operationId": "records_delete", "tags": ["records"],
                        "parameters": [{"name": "If-Match", "in": "header"}]},
                },
                "/api/v1/storage/buckets/{bucket}/objects/{key}": {
                    "put": {"operationId": "put_object", "tags": ["storage"],
                        "requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Body"}}}}},
                },
                "/api/v1/tree": {"post": {"operationId": "tree", "tags": ["x"],
                    "requestBody": {"content": {"application/json": {"schema": {"$ref": "#/components/schemas/Node"}}}}}},
                "/api/auth/keys/rotate": {"post": {"operationId": "rotate", "tags": ["admin"]}},
                "/api/mcp": {"post": {"operationId": "mcp", "tags": ["mcp"]}},
                "/api/v1/realtime/sse": {"get": {"operationId": "sse", "tags": ["realtime"]}},
                "/api/v1/kv/{ns}/subscribe/{channel}": {"get": {"operationId": "sub", "tags": ["kv"]}},
            }
        })
    }

    #[test]
    fn indexes_callable_operations_only() {
        let c = Catalog::new(&doc());
        assert_eq!(c.len(), 5, "mcp, realtime and subscribe are left out");
        assert!(c.find("mcp").is_none() && c.find("sse").is_none() && c.find("sub").is_none());
        assert_eq!(
            c.find("records_get").unwrap().path,
            "/api/v1/collections/{collection}/records/{id}"
        );
        assert_eq!(
            c.find("delete /api/v1/collections/{collection}/records/{id}")
                .unwrap()
                .id,
            "records_delete"
        );
        assert_eq!(c.find("records_delete").unwrap().header_params(), vec!["if-match"]);
    }

    #[test]
    fn destructive_means_delete_or_an_irreversible_action() {
        let c = Catalog::new(&doc());
        assert!(c.find("records_delete").unwrap().destructive());
        assert!(c.find("rotate").unwrap().destructive());
        assert!(!c.find("records_get").unwrap().destructive());
        assert!(!c.find("put_object").unwrap().destructive());
    }

    #[test]
    fn lists_tags_without_a_filter_and_operations_with_one() {
        let c = Catalog::new(&doc());
        let all = c.list(None, None, 50);
        assert_eq!(all["total"], 5);
        assert!(
            all["tags"]
                .as_array()
                .unwrap()
                .iter()
                .any(|t| t["tag"] == "records" && t["operations"] == 2)
        );
        let r = c.list(Some("records"), None, 50);
        assert_eq!(r["matches"], 2);
        let s = c.list(None, Some("ONE record"), 50);
        assert_eq!(s["operations"][0]["operation"], "records_get");
    }

    #[test]
    fn describe_inlines_references_without_looping() {
        let c = Catalog::new(&doc());
        let d = c.describe(c.find("tree").unwrap());
        let s = &d["request_body"]["content"]["application/json"];
        assert_eq!(s["title"], "Node");
        assert_eq!(
            s["properties"]["child"]["$ref"], "#/components/schemas/Node",
            "cycle stops"
        );
        let p = c.describe(c.find("put_object").unwrap());
        assert_eq!(
            p["request_body"]["content"]["application/json"]["properties"]["name"]["type"],
            "string"
        );
    }

    #[test]
    fn fills_and_encodes_path_parameters() {
        let p = |v: Json| v.as_object().unwrap().clone();
        assert_eq!(
            fill_path(
                "/api/v1/collections/{collection}/records/{id}",
                &p(json!({"collection": "posts", "id": 7}))
            )
            .unwrap(),
            "/api/v1/collections/posts/records/7"
        );
        assert_eq!(
            fill_path(
                "/api/v1/storage/buckets/{bucket}/objects/{key}",
                &p(json!({"bucket": "a b", "key": "dir/x y.png"}))
            )
            .unwrap(),
            "/api/v1/storage/buckets/a%20b/objects/dir/x%20y.png",
            "the last parameter keeps its slashes"
        );
        assert_eq!(
            fill_path(
                "/api/v1/collections/{collection}/records",
                &p(json!({"collection": "a/b"}))
            )
            .unwrap(),
            "/api/v1/collections/a%2Fb/records",
            "a middle parameter cannot add segments"
        );
        assert_eq!(fill_path("/x/{id}", &p(json!({}))).unwrap_err(), "id");
        assert_eq!(fill_path("/x/{id}", &p(json!({"id": ""}))).unwrap_err(), "id");
    }

    #[test]
    fn query_strings_repeat_array_keys() {
        let q = json!({"filter": "a = 1", "fields": ["id", "title"], "limit": 5, "skip": null});
        assert_eq!(
            query_string(q.as_object().unwrap()),
            "fields=id&fields=title&filter=a+%3D+1&limit=5"
        );
    }
}

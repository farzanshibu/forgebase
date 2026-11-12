//! The MCP tools: a curated set for the jobs an agent does most (each one a
//! single REST operation, declared in [`CURATED`]) and three generic tools
//! that reach every other operation through the OpenAPI document
//! (`list_operations`, `describe_operation`, `call_operation`).
//!
//! A tool never acts by itself: it becomes one HTTP request ([`Outgoing`])
//! that the endpoint sends through the server's own router with the caller's
//! credentials, so authentication, scopes, policy rules, feature switches and
//! rate limits apply exactly as they do to the same call over HTTP.

use axum::http::{HeaderName, HeaderValue, Method};
use base64::Engine as _;
use serde_json::{Map, Value as Json, json};

use super::catalog::{self, Catalog, Operation};

/// Most operations `list_operations` returns at once.
const LIST_LIMIT: usize = 100;

/// Headers a tool may set on its request (besides the operation's own
/// documented header parameters). Never credentials: those come from the
/// MCP request itself.
const TOOL_HEADERS: &[&str] = &["if-match", "if-none-match", "idempotency-key"];

/// Headers a tool may never set.
const FORBIDDEN_HEADERS: &[&str] = &[
    "authorization",
    "cookie",
    "x-api-key",
    "x-csrf-token",
    "host",
    "content-length",
    "content-type",
    "transfer-encoding",
    "connection",
];

/// Where a curated tool's argument goes.
#[derive(Debug, Clone, Copy)]
enum Loc {
    /// A `{name}` segment of the path.
    Path,
    /// A query parameter.
    Query,
    /// A field of a JSON request body built from the arguments.
    Field,
    /// The whole JSON request body.
    Body,
    /// A request header with this name.
    Header(&'static str),
    /// An object of extra query parameters.
    QueryObject,
    /// Raw request bytes, as UTF-8 text.
    RawText,
    /// Raw request bytes, base64.
    RawBase64,
    /// The content type of the raw bytes.
    ContentType,
}

/// One argument of a curated tool.
#[derive(Debug)]
struct Arg {
    name: &'static str,
    loc: Loc,
    /// JSON Schema type.
    ty: &'static str,
    desc: &'static str,
    required: bool,
}

const fn arg(name: &'static str, loc: Loc, ty: &'static str, desc: &'static str, required: bool) -> Arg {
    Arg {
        name,
        loc,
        ty,
        desc,
        required,
    }
}

/// A curated tool: one documented REST operation with friendlier arguments.
#[derive(Debug)]
struct Curated {
    name: &'static str,
    title: &'static str,
    desc: &'static str,
    method: &'static str,
    path: &'static str,
    args: &'static [Arg],
}

const COLLECTION: Arg = arg("collection", Loc::Path, "string", "Collection name.", true);
const RECORD_ID: Arg = arg("id", Loc::Path, "string", "Record id.", true);
const BUCKET: Arg = arg("bucket", Loc::Path, "string", "Bucket name.", true);
const OBJECT_KEY: Arg = arg(
    "key",
    Loc::Path,
    "string",
    "Object key; may contain slashes (folder/file.png).",
    true,
);
const FUNCTION: Arg = arg("name", Loc::Path, "string", "Function name.", true);
const JOB_ID: Arg = arg("id", Loc::Path, "string", "Job id.", true);
const IF_MATCH: Arg = arg(
    "if_match",
    Loc::Header("if-match"),
    "string",
    "Optional ETag from a previous read: the change applies only if the record has not changed since.",
    false,
);

/// The curated tools, in the order `tools/list` shows them.
static CURATED: &[Curated] = &[
    Curated {
        name: "whoami",
        title: "Who am I",
        desc: "The principal this connection acts as (administrator, service account or user), its realm and scopes. Call it first to know what you are allowed to do.",
        method: "GET",
        path: "/api/auth/me",
        args: &[],
    },
    Curated {
        name: "server_info",
        title: "Server information",
        desc: "Name, version, data directory, uptime and a database summary of this ForgeBase server.",
        method: "GET",
        path: "/api/system/info",
        args: &[],
    },
    Curated {
        name: "get_features",
        title: "Features and settings",
        desc: "Every module (storage, functions, jobs, queues, billing…), whether it is on, and each setting with its value, default and where it came from (file, environment, flag). Secrets read as null. Administrators only.",
        method: "GET",
        path: "/api/system/features",
        args: &[],
    },
    Curated {
        name: "update_config",
        title: "Change settings",
        desc: "Writes settings to the server's config file, e.g. {\"set\": {\"storage.enabled\": false}}. Keys are the dotted setting keys from get_features. Every key is validated first; nothing is written if one is invalid. Most changes take effect after the server restarts (the result says restart_required). Settings fixed by an environment variable or flag cannot be changed here. Administrators only.",
        method: "PATCH",
        path: "/api/system/config",
        args: &[arg(
            "set",
            Loc::Field,
            "object",
            "Dotted setting keys and their new values, e.g. {\"jobs.enabled\": true}.",
            true,
        )],
    },
    Curated {
        name: "list_collections",
        title: "List collections",
        desc: "Every collection (table) with its fields and options.",
        method: "GET",
        path: "/api/v1/collections",
        args: &[],
    },
    Curated {
        name: "get_collection",
        title: "Get a collection",
        desc: "One collection's definition: fields, types, indexes and options.",
        method: "GET",
        path: "/api/v1/collections/{collection}",
        args: &[COLLECTION],
    },
    Curated {
        name: "create_collection",
        title: "Create a collection",
        desc: "Creates a collection. The definition is {\"name\": \"posts\", \"fields\": [{\"name\": \"title\", \"type\": \"text\", \"required\": true}, …]}; describe_operation collections_create gives the full schema (field types, options, schema_mode).",
        method: "POST",
        path: "/api/v1/collections",
        args: &[arg(
            "definition",
            Loc::Body,
            "object",
            "The collection definition.",
            true,
        )],
    },
    Curated {
        name: "update_collection",
        title: "Change a collection",
        desc: "Replaces a collection's definition (add, rename or change fields). Existing records keep their data. Send the whole definition, as get_collection returns it, with your changes.",
        method: "PUT",
        path: "/api/v1/collections/{collection}",
        args: &[
            COLLECTION,
            arg("definition", Loc::Body, "object", "The full new definition.", true),
        ],
    },
    Curated {
        name: "delete_collection",
        title: "Delete a collection",
        desc: "Deletes a collection and every record in it. Cannot be undone.",
        method: "DELETE",
        path: "/api/v1/collections/{collection}",
        args: &[COLLECTION],
    },
    Curated {
        name: "list_records",
        title: "List records",
        desc: "A page of a collection's records, with optional filter, sort, projection and pagination. Filter syntax: `status = 'published' && views > 10`.",
        method: "GET",
        path: "/api/v1/collections/{collection}/records",
        args: &[
            COLLECTION,
            arg(
                "filter",
                Loc::Query,
                "string",
                "Filter expression, e.g. `owner = 'u1' && done = false`.",
                false,
            ),
            arg(
                "sort",
                Loc::Query,
                "string",
                "Sort fields, `-` for descending, e.g. `-created,title`.",
                false,
            ),
            arg(
                "fields",
                Loc::Query,
                "string",
                "Comma-separated fields to return.",
                false,
            ),
            arg(
                "expand",
                Loc::Query,
                "string",
                "Comma-separated relation fields to expand.",
                false,
            ),
            arg("search", Loc::Query, "string", "Full-text search terms.", false),
            arg("page", Loc::Query, "integer", "Page number, from 1.", false),
            arg("per_page", Loc::Query, "integer", "Records per page.", false),
            arg(
                "cursor",
                Loc::Query,
                "string",
                "Cursor from a previous page, for keyset pagination.",
                false,
            ),
        ],
    },
    Curated {
        name: "get_record",
        title: "Get a record",
        desc: "One record by id.",
        method: "GET",
        path: "/api/v1/collections/{collection}/records/{id}",
        args: &[COLLECTION, RECORD_ID],
    },
    Curated {
        name: "create_record",
        title: "Create a record",
        desc: "Adds a record to a collection. Field values are checked against the collection's schema.",
        method: "POST",
        path: "/api/v1/collections/{collection}/records",
        args: &[
            COLLECTION,
            arg(
                "record",
                Loc::Body,
                "object",
                "Field values, e.g. {\"title\": \"Hello\"}.",
                true,
            ),
        ],
    },
    Curated {
        name: "update_record",
        title: "Update a record",
        desc: "Changes some fields of a record (JSON merge patch: fields you leave out keep their values; null removes an optional field).",
        method: "PATCH",
        path: "/api/v1/collections/{collection}/records/{id}",
        args: &[
            COLLECTION,
            RECORD_ID,
            arg("changes", Loc::Body, "object", "The fields to change.", true),
            IF_MATCH,
        ],
    },
    Curated {
        name: "delete_record",
        title: "Delete a record",
        desc: "Deletes one record. Cannot be undone.",
        method: "DELETE",
        path: "/api/v1/collections/{collection}/records/{id}",
        args: &[COLLECTION, RECORD_ID, IF_MATCH],
    },
    Curated {
        name: "run_sql",
        title: "Run SQL",
        desc: "Runs one SQL statement on the database and returns columns and rows. Use ?1, ?2… placeholders with `params` for values. Prefer the record tools for app data: they apply the collection's rules.",
        method: "POST",
        path: "/api/v1/sql",
        args: &[
            arg("sql", Loc::Field, "string", "The SQL statement.", true),
            arg(
                "params",
                Loc::Field,
                "array",
                "Values for the ?1, ?2… placeholders.",
                false,
            ),
        ],
    },
    Curated {
        name: "list_tables",
        title: "List tables",
        desc: "Every table of the database with its columns, including ForgeBase's own.",
        method: "GET",
        path: "/api/v1/sql/tables",
        args: &[],
    },
    Curated {
        name: "list_buckets",
        title: "List buckets",
        desc: "Every storage bucket with its settings.",
        method: "GET",
        path: "/api/v1/storage/buckets",
        args: &[],
    },
    Curated {
        name: "list_objects",
        title: "List files",
        desc: "Objects (files) in a bucket, optionally under a prefix (folder).",
        method: "GET",
        path: "/api/v1/storage/buckets/{bucket}/objects",
        args: &[
            BUCKET,
            arg(
                "prefix",
                Loc::Query,
                "string",
                "Only keys that start with this, e.g. `avatars/`.",
                false,
            ),
            arg(
                "delimiter",
                Loc::Query,
                "string",
                "`/` to list one folder level.",
                false,
            ),
            arg("limit", Loc::Query, "integer", "Most objects to return.", false),
            arg("cursor", Loc::Query, "string", "Cursor from a previous page.", false),
        ],
    },
    Curated {
        name: "read_object",
        title: "Read a file",
        desc: "Downloads an object. Text comes back as text, other content as base64 (up to 1 MiB).",
        method: "GET",
        path: "/api/v1/storage/buckets/{bucket}/objects/{key}",
        args: &[BUCKET, OBJECT_KEY],
    },
    Curated {
        name: "write_object",
        title: "Write a file",
        desc: "Uploads an object, replacing one with the same key. Give `content` for text or `content_base64` for bytes.",
        method: "PUT",
        path: "/api/v1/storage/buckets/{bucket}/objects/{key}",
        args: &[
            BUCKET,
            OBJECT_KEY,
            arg("content", Loc::RawText, "string", "The file's content as text.", false),
            arg(
                "content_base64",
                Loc::RawBase64,
                "string",
                "The file's content, base64-encoded.",
                false,
            ),
            arg(
                "content_type",
                Loc::ContentType,
                "string",
                "MIME type, e.g. `text/plain` (default `application/octet-stream`).",
                false,
            ),
        ],
    },
    Curated {
        name: "delete_object",
        title: "Delete a file",
        desc: "Deletes an object from a bucket. Cannot be undone unless the bucket keeps versions.",
        method: "DELETE",
        path: "/api/v1/storage/buckets/{bucket}/objects/{key}",
        args: &[BUCKET, OBJECT_KEY],
    },
    Curated {
        name: "list_functions",
        title: "List functions",
        desc: "Every deployed edge function with its runtime, version and capabilities.",
        method: "GET",
        path: "/api/v1/functions",
        args: &[],
    },
    Curated {
        name: "get_function",
        title: "Get a function",
        desc: "One function: its current version, settings and capabilities.",
        method: "GET",
        path: "/api/v1/functions/{name}",
        args: &[FUNCTION],
    },
    Curated {
        name: "deploy_function",
        title: "Deploy a function",
        desc: "Deploys a new version of an edge function (creating it if needed): {\"name\": \"hello\", \"source\": \"export default …\"} for JavaScript or TypeScript, or `wasm` (base64). describe_operation deploy gives every option (capabilities, env, limits, hooks).",
        method: "POST",
        path: "/api/v1/functions",
        args: &[arg("function", Loc::Body, "object", "The deploy request.", true)],
    },
    Curated {
        name: "invoke_function",
        title: "Invoke a function",
        desc: "Calls a function with a JSON payload and returns what it answered.",
        method: "POST",
        path: "/api/v1/functions/{name}/invoke",
        args: &[
            FUNCTION,
            arg("payload", Loc::Body, "object", "The JSON the function receives.", false),
            arg(
                "version",
                Loc::Query,
                "integer",
                "A specific version instead of the current one.",
                false,
            ),
        ],
    },
    Curated {
        name: "function_logs",
        title: "Function logs",
        desc: "The latest log lines and invocations of a function, newest first.",
        method: "GET",
        path: "/api/v1/functions/{name}/logs",
        args: &[
            FUNCTION,
            arg("limit", Loc::Query, "integer", "How many entries (default 50).", false),
        ],
    },
    Curated {
        name: "delete_function",
        title: "Delete a function",
        desc: "Deletes a function and all its versions. Cannot be undone.",
        method: "DELETE",
        path: "/api/v1/functions/{name}",
        args: &[FUNCTION],
    },
    Curated {
        name: "list_jobs",
        title: "List jobs",
        desc: "Background jobs, filtered by status, queue or kind, newest first.",
        method: "GET",
        path: "/api/v1/jobs",
        args: &[
            arg(
                "status",
                Loc::Query,
                "string",
                "queued, running, succeeded, failed, cancelled…",
                false,
            ),
            arg("queue", Loc::Query, "string", "Queue name.", false),
            arg("kind", Loc::Query, "string", "Job kind.", false),
            arg("limit", Loc::Query, "integer", "Most jobs to return.", false),
            arg("offset", Loc::Query, "integer", "Jobs to skip.", false),
        ],
    },
    Curated {
        name: "enqueue_job",
        title: "Enqueue a job",
        desc: "Adds a background job, e.g. {\"kind\": \"send-report\", \"payload\": {…}}. describe_operation enqueue gives every option (queue, priority, run_at or delay_ms, max_attempts, depends_on).",
        method: "POST",
        path: "/api/v1/jobs",
        args: &[arg("job", Loc::Body, "object", "The job.", true)],
    },
    Curated {
        name: "get_job",
        title: "Get a job",
        desc: "One job: status, attempts, progress, result or error.",
        method: "GET",
        path: "/api/v1/jobs/{id}",
        args: &[JOB_ID],
    },
    Curated {
        name: "cancel_job",
        title: "Cancel a job",
        desc: "Cancels a queued or running job.",
        method: "POST",
        path: "/api/v1/jobs/{id}/cancel",
        args: &[JOB_ID],
    },
    Curated {
        name: "retry_job",
        title: "Retry a job",
        desc: "Runs a failed or cancelled job again.",
        method: "POST",
        path: "/api/v1/jobs/{id}/retry",
        args: &[JOB_ID],
    },
    Curated {
        name: "list_service_accounts",
        title: "List service accounts",
        desc: "Service accounts: the non-human principals that API keys belong to.",
        method: "GET",
        path: "/api/auth/service-accounts",
        args: &[],
    },
    Curated {
        name: "create_service_account",
        title: "Create a service account",
        desc: "Creates a service account, e.g. {\"name\": \"backend\", \"description\": \"…\"}; `role` is `service` (the default) or `admin` for full access. Give it keys with create_api_key.",
        method: "POST",
        path: "/api/auth/service-accounts",
        args: &[arg("account", Loc::Body, "object", "The service account.", true)],
    },
    Curated {
        name: "create_api_key",
        title: "Create an API key",
        desc: "Creates an API key for a service account. The secret is shown only in this answer: hand it to the user, do not store it elsewhere. A key cannot have broader scopes than the credential creating it.",
        method: "POST",
        path: "/api/auth/service-accounts/{id}/keys",
        args: &[
            arg("id", Loc::Path, "string", "Service account id.", true),
            arg("name", Loc::Field, "string", "A name for the key.", true),
            arg(
                "scopes",
                Loc::Field,
                "array",
                "Scopes, e.g. [\"collections:read\", \"storage:write:avatars\"]; empty for the account's full access.",
                false,
            ),
            arg(
                "expires_in_days",
                Loc::Field,
                "integer",
                "Days until it expires; leave out for no expiry.",
                false,
            ),
            arg(
                "tenant",
                Loc::Field,
                "string",
                "Bind the key to this tenant (environment id).",
                false,
            ),
        ],
    },
    Curated {
        name: "list_api_keys",
        title: "List API keys",
        desc: "API keys and personal access tokens: name, scopes, owner, last use and expiry. Never the secrets.",
        method: "GET",
        path: "/api/auth/api-keys",
        args: &[],
    },
    Curated {
        name: "revoke_api_key",
        title: "Revoke an API key",
        desc: "Revokes an API key at once; apps using it start getting 401.",
        method: "DELETE",
        path: "/api/auth/api-keys/{id}",
        args: &[arg("id", Loc::Path, "string", "Key id.", true)],
    },
    Curated {
        name: "audit_log",
        title: "Audit log",
        desc: "Sign-ins, key and token changes, administrator actions and MCP tool calls, newest first.",
        method: "GET",
        path: "/api/auth/audit",
        args: &[arg(
            "query",
            Loc::QueryObject,
            "object",
            "Filters as query parameters, e.g. {\"limit\": 50}.",
            false,
        )],
    },
];

/// The request a tool call becomes.
#[derive(Debug)]
pub struct Outgoing {
    /// HTTP method.
    pub method: Method,
    /// Documented path template, for the audit log.
    pub template: String,
    /// Concrete, encoded path.
    pub path: String,
    /// Query string without `?`.
    pub query: String,
    /// Request headers set by the tool (already checked).
    pub headers: Vec<(HeaderName, HeaderValue)>,
    /// Body bytes and their content type.
    pub body: Option<(Vec<u8>, String)>,
}

/// What a `tools/call` needs to do.
#[derive(Debug)]
pub enum Plan {
    /// Answer from the catalog, without a request.
    Answer(Json),
    /// Send this request.
    Send(Outgoing),
}

/// Why a call cannot be planned: shown to the agent as a tool error.
pub type ToolError = String;

/// `tools/list`: the curated tools whose operation this server documents
/// (a disabled module's tools are left out), then the generic tools.
pub fn list(catalog: &Catalog) -> Vec<Json> {
    let mut tools: Vec<Json> = CURATED
        .iter()
        .filter_map(|t| {
            let method = Method::from_bytes(t.method.as_bytes()).ok()?;
            catalog.route(&method, t.path)?;
            Some(describe_curated(t, &method))
        })
        .collect();
    tools.extend(generic_tools(catalog));
    tools
}

/// Names of the curated tools whose operation is documented.
pub fn curated_names(catalog: &Catalog) -> Vec<&'static str> {
    CURATED
        .iter()
        .filter(|t| Method::from_bytes(t.method.as_bytes()).is_ok_and(|m| catalog.route(&m, t.path).is_some()))
        .map(|t| t.name)
        .collect()
}

fn describe_curated(t: &Curated, method: &Method) -> Json {
    let destructive = catalog::destructive(method, t.path);
    let mut props = Map::new();
    let mut required = Vec::new();
    for a in t.args {
        let mut s = json!({"type": a.ty, "description": a.desc});
        if a.ty == "array" {
            s["items"] = json!({});
        }
        props.insert(a.name.to_owned(), s);
        if a.required {
            required.push(a.name);
        }
    }
    if destructive {
        props.insert(
            "confirm".to_owned(),
            json!({"type": "boolean", "description": "Must be true. Ask the user before deleting or revoking anything, then call again with confirm: true."}),
        );
        required.push("confirm");
    }
    let read_only = *method == Method::GET;
    json!({
        "name": t.name,
        "title": t.title,
        "description": t.desc,
        "inputSchema": {"type": "object", "properties": props, "required": required, "additionalProperties": false},
        "annotations": {
            "title": t.title,
            "readOnlyHint": read_only,
            "destructiveHint": destructive,
            "idempotentHint": read_only || matches!(*method, Method::PUT | Method::DELETE),
            "openWorldHint": false,
        },
    })
}

fn generic_tools(catalog: &Catalog) -> Vec<Json> {
    let n = catalog.len();
    vec![
        json!({
            "name": "list_operations",
            "title": "Find an API operation",
            "description": format!("Searches the {n} operations of the ForgeBase API that the tools above do not cover (users, realms, roles and rules, storage buckets and signed URLs, queues, key-value, vector, graph, billing, backups, indexes, database topology…). Without arguments it lists the tags. Then describe_operation and call_operation."),
            "inputSchema": {"type": "object", "properties": {
                "tag": {"type": "string", "description": "Only operations with this tag, e.g. `storage`."},
                "search": {"type": "string", "description": "Words that must all appear in the path, operation id or summary, e.g. `bucket policy`."},
            }, "additionalProperties": false},
            "annotations": {"title": "Find an API operation", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        }),
        json!({
            "name": "describe_operation",
            "title": "Describe an API operation",
            "description": "Parameters, request body schema and responses of one operation, from the server's OpenAPI document.",
            "inputSchema": {"type": "object", "properties": {
                "operation": {"type": "string", "description": "The operation id from list_operations, or `METHOD /path` with the documented path, e.g. `POST /api/v1/storage/buckets`."},
            }, "required": ["operation"], "additionalProperties": false},
            "annotations": {"title": "Describe an API operation", "readOnlyHint": true, "destructiveHint": false, "idempotentHint": true, "openWorldHint": false},
        }),
        json!({
            "name": "call_operation",
            "title": "Call an API operation",
            "description": "Calls any operation of the ForgeBase API with this connection's credentials. Deleting, revoking, rotating, purging, flushing, restoring, promoting, rebuilding or rolling back needs confirm: true: ask the user first.",
            "inputSchema": {"type": "object", "properties": {
                "operation": {"type": "string", "description": "Operation id, or `METHOD /path`."},
                "path_params": {"type": "object", "description": "A value for each {name} in the path."},
                "query": {"type": "object", "description": "Query parameters; an array repeats the parameter."},
                "body": {"description": "The JSON request body."},
                "body_text": {"type": "string", "description": "A non-JSON body as text (use with content_type)."},
                "body_base64": {"type": "string", "description": "A binary body, base64-encoded (use with content_type)."},
                "content_type": {"type": "string", "description": "Content type of body_text or body_base64."},
                "headers": {"type": "object", "description": "Extra headers: If-Match, If-None-Match, Idempotency-Key, X-ForgeBase-* or one the operation documents."},
                "confirm": {"type": "boolean", "description": "true to allow a destructive operation, after the user agreed."},
            }, "required": ["operation"], "additionalProperties": false},
            "annotations": {"title": "Call an API operation", "readOnlyHint": false, "destructiveHint": true, "idempotentHint": false, "openWorldHint": false},
        }),
    ]
}

/// Plans `tools/call` of `name` with `args`. `Ok(None)` for an unknown tool.
///
/// # Errors
/// A [`ToolError`] for arguments the tool cannot use.
pub fn plan(catalog: &Catalog, name: &str, args: &Map<String, Json>) -> Result<Option<Plan>, ToolError> {
    match name {
        "list_operations" => {
            let tag = args.get("tag").and_then(Json::as_str);
            let search = args.get("search").and_then(Json::as_str);
            Ok(Some(Plan::Answer(catalog.list(tag, search, LIST_LIMIT))))
        }
        "describe_operation" => {
            let op = find(catalog, args)?;
            Ok(Some(Plan::Answer(catalog.describe(op))))
        }
        "call_operation" => plan_generic(catalog, args).map(|o| Some(Plan::Send(o))),
        other => match CURATED.iter().find(|t| t.name == other) {
            Some(t) => plan_curated(catalog, t, args).map(|o| Some(Plan::Send(o))),
            None => Ok(None),
        },
    }
}

fn find<'c>(catalog: &'c Catalog, args: &Map<String, Json>) -> Result<&'c Operation, ToolError> {
    let name = args
        .get("operation")
        .and_then(Json::as_str)
        .ok_or("`operation` is required: an operation id from list_operations, or `METHOD /path`.")?;
    catalog.find(name).ok_or_else(|| {
        format!(
            "No operation `{name}` on this server (or it cannot be called over MCP). Find one with list_operations."
        )
    })
}

fn require_confirm(destructive: bool, args: &Map<String, Json>, what: &str) -> Result<(), ToolError> {
    if destructive && args.get("confirm").and_then(Json::as_bool) != Some(true) {
        return Err(format!(
            "{what} deletes or revokes something and cannot be undone. Ask the user to confirm, then call again with confirm: true."
        ));
    }
    Ok(())
}

fn obj<'a>(args: &'a Map<String, Json>, key: &str) -> Result<Option<&'a Map<String, Json>>, ToolError> {
    match args.get(key) {
        None | Some(Json::Null) => Ok(None),
        Some(Json::Object(m)) => Ok(Some(m)),
        Some(_) => Err(format!("`{key}` must be an object.")),
    }
}

fn header(name: &str, value: &Json, allowed: &[String]) -> Result<(HeaderName, HeaderValue), ToolError> {
    let lower = name.trim().to_ascii_lowercase();
    if FORBIDDEN_HEADERS.contains(&lower.as_str()) {
        return Err(format!(
            "Header `{name}` cannot be set: credentials come from this MCP connection."
        ));
    }
    let ok = TOOL_HEADERS.contains(&lower.as_str()) || lower.starts_with("x-forgebase-") || allowed.contains(&lower);
    if !ok {
        return Err(format!(
            "Header `{name}` is not allowed. Allowed: If-Match, If-None-Match, Idempotency-Key, X-ForgeBase-*, and the operation's own header parameters."
        ));
    }
    let text = match value {
        Json::String(s) => s.clone(),
        other => other.to_string(),
    };
    let n = HeaderName::from_bytes(lower.as_bytes()).map_err(|_| format!("`{name}` is not a valid header name."))?;
    let v = HeaderValue::from_str(&text).map_err(|_| format!("The value of `{name}` is not a valid header value."))?;
    Ok((n, v))
}

fn plan_generic(catalog: &Catalog, args: &Map<String, Json>) -> Result<Outgoing, ToolError> {
    let op = find(catalog, args)?;
    require_confirm(op.destructive(), args, &format!("`{}`", op.id))?;
    let empty = Map::new();
    let params = obj(args, "path_params")?.unwrap_or(&empty);
    let path = catalog::fill_path(&op.path, params)
        .map_err(|p| format!("`path_params.{p}` is required for {} {}.", op.method, op.path))?;
    let query = obj(args, "query")?.map(catalog::query_string).unwrap_or_default();
    let allowed = op.header_params();
    let headers = obj(args, "headers")?
        .into_iter()
        .flatten()
        .map(|(k, v)| header(k, v, &allowed))
        .collect::<Result<Vec<_>, _>>()?;
    let content_type = args.get("content_type").and_then(Json::as_str);
    let body = match (args.get("body"), args.get("body_text"), args.get("body_base64")) {
        (Some(b), None, None) if !b.is_null() => Some((b.to_string().into_bytes(), "application/json".to_owned())),
        (None | Some(Json::Null), Some(Json::String(t)), None) => Some((
            t.clone().into_bytes(),
            content_type.unwrap_or("text/plain; charset=utf-8").to_owned(),
        )),
        (None | Some(Json::Null), None, Some(Json::String(b))) => Some((
            decode_base64(b)?,
            content_type.unwrap_or("application/octet-stream").to_owned(),
        )),
        (None | Some(Json::Null), None, None) => None,
        _ => return Err("Give at most one of `body`, `body_text` (a string) and `body_base64` (a string).".to_owned()),
    };
    Ok(Outgoing {
        method: op.method.clone(),
        template: op.path.clone(),
        path,
        query,
        headers,
        body,
    })
}

fn decode_base64(s: &str) -> Result<Vec<u8>, ToolError> {
    base64::engine::general_purpose::STANDARD
        .decode(s.trim())
        .map_err(|_| "`content_base64`/`body_base64` is not valid base64.".to_owned())
}

fn plan_curated(catalog: &Catalog, t: &Curated, args: &Map<String, Json>) -> Result<Outgoing, ToolError> {
    let method = Method::from_bytes(t.method.as_bytes()).map_err(|_| "bad method".to_owned())?;
    if catalog.route(&method, t.path).is_none() {
        return Err(format!(
            "`{}` is not available: its module is switched off on this server.",
            t.name
        ));
    }
    require_confirm(catalog::destructive(&method, t.path), args, &format!("`{}`", t.name))?;
    if let Some(unknown) = args
        .keys()
        .find(|k| *k != "confirm" && !t.args.iter().any(|a| a.name == k.as_str()))
    {
        return Err(format!("`{}` has no argument `{unknown}`.", t.name));
    }
    let mut path_params = Map::new();
    let mut query = Map::new();
    let mut fields = Map::new();
    let mut whole: Option<Json> = None;
    let mut headers = Vec::new();
    let mut raw: Option<Vec<u8>> = None;
    let mut content_type: Option<String> = None;
    for a in t.args {
        let v = match args.get(a.name) {
            Some(Json::Null) | None if a.required => {
                return Err(format!("`{}` needs `{}`: {}", t.name, a.name, a.desc));
            }
            Some(Json::Null) | None => continue,
            Some(v) => v,
        };
        match a.loc {
            Loc::Path => {
                path_params.insert(a.name.to_owned(), v.clone());
            }
            Loc::Query => {
                query.insert(a.name.to_owned(), v.clone());
            }
            Loc::QueryObject => {
                let m = v
                    .as_object()
                    .ok_or_else(|| format!("`{}` must be an object.", a.name))?;
                query.extend(m.clone());
            }
            Loc::Field => {
                fields.insert(a.name.to_owned(), v.clone());
            }
            Loc::Body => whole = Some(v.clone()),
            Loc::Header(h) => headers.push(header(h, v, &[])?),
            Loc::RawText => {
                let s = v.as_str().ok_or_else(|| format!("`{}` must be a string.", a.name))?;
                raw = Some(s.as_bytes().to_vec());
            }
            Loc::RawBase64 => {
                if raw.is_some() {
                    return Err("Give `content` or `content_base64`, not both.".to_owned());
                }
                let s = v.as_str().ok_or_else(|| format!("`{}` must be a string.", a.name))?;
                raw = Some(decode_base64(s)?);
            }
            Loc::ContentType => content_type = v.as_str().map(str::to_owned),
        }
    }
    let path = catalog::fill_path(t.path, &path_params).map_err(|p| format!("`{}` needs `{p}`.", t.name))?;
    let body = if let Some(bytes) = raw {
        Some((
            bytes,
            content_type.unwrap_or_else(|| "application/octet-stream".to_owned()),
        ))
    } else if t.args.iter().any(|a| matches!(a.loc, Loc::RawText | Loc::RawBase64)) {
        return Err(format!("`{}` needs `content` or `content_base64`.", t.name));
    } else if let Some(b) = whole {
        Some((b.to_string().into_bytes(), "application/json".to_owned()))
    } else if !fields.is_empty() {
        Some((
            Json::Object(fields).to_string().into_bytes(),
            "application/json".to_owned(),
        ))
    } else {
        None
    };
    Ok(Outgoing {
        method,
        template: t.path.to_owned(),
        path,
        query: catalog::query_string(&query),
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn catalog() -> Catalog {
        let mut paths = Map::new();
        for t in CURATED {
            let entry = paths.entry(t.path.to_owned()).or_insert_with(|| json!({}));
            entry[t.method.to_ascii_lowercase()] = json!({"operationId": t.name, "tags": ["x"]});
        }
        paths.insert(
            "/api/v1/storage/buckets/{bucket}".to_owned(),
            json!({"delete": {"operationId": "delete_bucket", "tags": ["storage"],
                "parameters": [{"name": "X-Custom", "in": "header"}]}}),
        );
        Catalog::new(&json!({"paths": paths}))
    }

    fn args(v: Json) -> Map<String, Json> {
        v.as_object().unwrap().clone()
    }

    fn send(p: Option<Plan>) -> Outgoing {
        match p.unwrap() {
            Plan::Send(o) => o,
            Plan::Answer(a) => panic!("expected a request, got {a}"),
        }
    }

    #[test]
    fn every_curated_tool_is_listed_with_a_schema() {
        let c = catalog();
        let tools = list(&c);
        assert_eq!(tools.len(), CURATED.len() + 3);
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        let mut unique = names.clone();
        unique.sort_unstable();
        unique.dedup();
        assert_eq!(unique.len(), names.len(), "tool names are unique");
        let del = tools.iter().find(|t| t["name"] == "delete_record").unwrap();
        assert_eq!(del["annotations"]["destructiveHint"], true);
        assert!(
            del["inputSchema"]["required"]
                .as_array()
                .unwrap()
                .contains(&json!("confirm"))
        );
        let get = tools.iter().find(|t| t["name"] == "get_record").unwrap();
        assert_eq!(get["annotations"]["readOnlyHint"], true);
    }

    #[test]
    fn a_disabled_module_hides_its_tools() {
        let c = Catalog::new(&json!({"paths": {"/api/v1/collections": {"get": {"operationId": "l", "tags": ["c"]}}}}));
        assert_eq!(curated_names(&c), vec!["list_collections"]);
        assert!(
            plan(&c, "list_buckets", &Map::new())
                .unwrap_err()
                .contains("switched off")
        );
    }

    #[test]
    fn curated_arguments_become_path_query_body_and_headers() {
        let c = catalog();
        let o = send(
            plan(
                &c,
                "list_records",
                &args(json!({"collection": "posts", "filter": "done = false", "per_page": 5})),
            )
            .unwrap(),
        );
        assert_eq!(
            (o.method.as_str(), o.path.as_str()),
            ("GET", "/api/v1/collections/posts/records")
        );
        assert_eq!(o.query, "filter=done+%3D+false&per_page=5");
        assert!(o.body.is_none());

        let o = send(
            plan(
                &c,
                "update_record",
                &args(json!({"collection": "posts", "id": "r1", "changes": {"title": "x"}, "if_match": "\"v2\""})),
            )
            .unwrap(),
        );
        assert_eq!(o.method, Method::PATCH);
        assert_eq!(o.body.as_ref().unwrap().0, br#"{"title":"x"}"#);
        assert_eq!(o.headers[0].0.as_str(), "if-match");

        let o = send(plan(&c, "run_sql", &args(json!({"sql": "select 1"}))).unwrap());
        assert_eq!(o.body.unwrap().0, br#"{"sql":"select 1"}"#);

        let o = send(
            plan(
                &c,
                "write_object",
                &args(json!({"bucket": "b", "key": "a/b.txt", "content_base64": "aGk=", "content_type": "text/plain"})),
            )
            .unwrap(),
        );
        assert_eq!(o.path, "/api/v1/storage/buckets/b/objects/a/b.txt");
        assert_eq!(o.body.unwrap(), (b"hi".to_vec(), "text/plain".to_owned()));
    }

    #[test]
    fn curated_tools_check_their_arguments() {
        let c = catalog();
        assert!(
            plan(&c, "get_record", &args(json!({"collection": "posts"})))
                .unwrap_err()
                .contains("`id`")
        );
        assert!(
            plan(&c, "get_record", &args(json!({"collection": "p", "id": "1", "x": 1})))
                .unwrap_err()
                .contains("no argument `x`")
        );
        assert!(
            plan(&c, "write_object", &args(json!({"bucket": "b", "key": "k"})))
                .unwrap_err()
                .contains("content")
        );
        assert!(plan(&c, "nope", &Map::new()).unwrap().is_none(), "unknown tool");
    }

    #[test]
    fn destructive_calls_need_confirmation() {
        let c = catalog();
        let a = args(json!({"collection": "posts", "id": "r1"}));
        assert!(plan(&c, "delete_record", &a).unwrap_err().contains("confirm: true"));
        let mut ok = a.clone();
        ok.insert("confirm".into(), json!(true));
        assert_eq!(send(plan(&c, "delete_record", &ok).unwrap()).method, Method::DELETE);
        let g = args(json!({"operation": "delete_bucket", "path_params": {"bucket": "b"}}));
        assert!(plan(&c, "call_operation", &g).unwrap_err().contains("confirm: true"));
    }

    #[test]
    fn the_generic_call_builds_the_documented_request() {
        let c = catalog();
        let o = send(
            plan(
                &c,
                "call_operation",
                &args(json!({
                    "operation": "DELETE /api/v1/storage/buckets/{bucket}",
                    "path_params": {"bucket": "avatars"},
                    "query": {"force": true},
                    "headers": {"X-Custom": "1", "X-ForgeBase-Tenant": "env1"},
                    "confirm": true,
                })),
            )
            .unwrap(),
        );
        assert_eq!(
            (o.path.as_str(), o.query.as_str()),
            ("/api/v1/storage/buckets/avatars", "force=true")
        );
        assert_eq!(o.headers.len(), 2);
        let bad = |h: Json| {
            plan(
                &c,
                "call_operation",
                &args(json!({"operation": "list_collections", "headers": h})),
            )
            .unwrap_err()
        };
        assert!(bad(json!({"Authorization": "Bearer x"})).contains("cannot be set"));
        assert!(bad(json!({"Cookie": "a=b"})).contains("cannot be set"));
        assert!(bad(json!({"X-Other": "1"})).contains("not allowed"));
        assert!(
            plan(
                &c,
                "call_operation",
                &args(json!({"operation": "get_record", "path_params": {"collection": "p"}}))
            )
            .unwrap_err()
            .contains("path_params.id")
        );
        assert!(
            plan(&c, "call_operation", &args(json!({"operation": "nope"})))
                .unwrap_err()
                .contains("list_operations")
        );
    }
}

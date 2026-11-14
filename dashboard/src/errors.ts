/**
 * Plain words for every error the dashboard can show (dashboard-ux.md rule 4). ErrorPanel reads `explain`,
 * forms read `fieldErrors`, and `notify.failed` uses the same title.
 *
 * Usage:
 *   const { title, next, requestId } = explain(error);   // any thrown value: ApiError, Error, string
 *   const byField = fieldErrors(error);                   // { title: "This field is required" }
 *   <FormField label="Title" error={byField.title}>…</FormField>
 *
 * Add a code here, not in a page: the map is the one place the wording lives.
 */
import { ApiError } from "./api";

export interface ErrorWords {
  /** One plain sentence: what went wrong. */
  title: string;
  /** What to do next. */
  next: string;
}

/** Where in a query text the server found a problem (1-based). */
export interface QueryPosition {
  line: number;
  column: number;
}

export type ErrorKind = "session" | "forbidden" | "network" | "disabled" | "invalid" | "missing" | "server" | "other";

export interface ExplainedError extends ErrorWords {
  kind: ErrorKind;
  status?: number;
  code?: string;
  requestId?: string;
  /** The server's own message, for Details. */
  message: string;
  /** The line and column of an openCypher problem, when the server named them. */
  position?: QueryPosition;
  /** The parsed response body (or the thrown value), for Details. */
  raw: unknown;
}

/** Error code (from `{"error":{"code"}}`) → plain words. Codes are stable API values (docs/api.md section 6). */
export const ERROR_WORDS = {
  invalid_name: { title: "That name is not allowed", next: "Use letters, digits and underscores, starting with a letter (at most 63 characters)." },
  invalid_bucket_name: { title: "That bucket name is not allowed", next: "Use 1 to 63 lower-case letters, digits, dots, dashes or underscores, starting with a letter or digit." },
  invalid_function_name: { title: "That function name is not allowed", next: "Use 1 to 48 lower-case letters, digits, dashes or underscores, starting with a letter." },
  reserved_name: { title: "That name is reserved", next: "Names starting with _forgebase or sqlite_ belong to the server. Pick another name." },
  validation_failed: { title: "Some fields need fixing", next: "Check the fields marked below and try again." },
  field_required: { title: "A required field is empty", next: "Fill in the fields marked below and try again." },
  invalid_body: { title: "The server could not read this request", next: "Check the values you entered. If it keeps happening, copy the request id and report it." },
  invalid_json: { title: "That is not valid JSON", next: "Look for a missing comma, quote or bracket." },
  invalid_filter: { title: "The filter could not be read", next: 'Check the filter syntax, for example title = "Pen" && price > 2.' },
  invalid_parameter: { title: "One of the options is not valid", next: "Check the values you entered and try again." },
  not_found: { title: "Not found", next: "It may have been deleted or renamed. Go back to the list and pick it again." },
  conflict: { title: "That clashes with something that already exists", next: "Pick another name, or reload to see the latest version." },
  already_exists: { title: "That name is already taken", next: "Pick another name." },
  version_conflict: { title: "Someone else changed this first", next: "Reload to get the latest version, then make your change again." },
  unique_violation: { title: "That value is already used", next: "This field must be unique. Enter a different value." },
  bucket_not_empty: { title: "The bucket still holds files", next: "Delete its files first, or choose to delete them with the bucket." },
  payload_too_large: { title: "That is too large", next: "Send a smaller file or body, or raise the limit in Features and settings." },
  unsupported_media_type: { title: "That file type is not accepted", next: "Check the file's content type, or allow it in the bucket's settings." },
  content_type_not_allowed: { title: "That file type is not allowed in this bucket", next: "Upload a file of an allowed type, or change the bucket's allowed types." },
  rate_limited: { title: "Too many requests", next: "Wait a moment and try again." },
  quota_exceeded: { title: "A plan limit was reached", next: "Raise the limit, or wait for it to reset." },
  feature_disabled: { title: "This feature is switched off", next: "An administrator can switch it on in System › Features and settings." },
  invalid_credentials: { title: "The email or password is wrong", next: "Check both and try again." },
  csrf_failed: { title: "The page's security token is out of date", next: "Reload the page and try again." },
  unavailable: { title: "The server is not ready", next: "Wait a moment and try again. System shows what is still starting." },
  timeout: { title: "The request took too long", next: "Try again. If it keeps happening, narrow the query." },
  method_not_allowed: { title: "The server does not allow that action here", next: "The dashboard and the server may be different versions. Check the version in System." },
  db_logins_unavailable: { title: "Direct logins need PostgreSQL", next: "This project runs on SQLite. Move it with `forgebase db convert --to postgres`, then create logins here." },
  extensions_unavailable: { title: "Extensions need PostgreSQL", next: "This project runs on SQLite. Move it with `forgebase db convert --to postgres`." },
  extension_managed: { title: "ForgeBase manages this extension", next: "Change it in Features and settings (database extensions), not here." },
  extension_not_allowed: { title: "That extension is not on the allow list", next: "Pick one from the list on this page." },
  extension_unavailable: { title: "This PostgreSQL engine does not ship that extension", next: "Use an engine that includes it, or pick another extension." },
  run_as_ddl: { title: "Run as does not run schema changes", next: "Run reads and writes as the user. To change the schema, switch Run as back to Administrator." },
  run_as_unavailable: { title: "Run as needs PostgreSQL with row-level security", next: "Switch Run as back to Administrator, or move the project to PostgreSQL." },
  // Reserved since ADR-0034: fuzzy search runs on every database, so a current server no longer answers this. An older one still can.
  fuzzy_unsupported: { title: "This server version cannot do fuzzy search", next: "Current versions run fuzzy search on every database (pg_trgm where PostgreSQL has it, a portable engine elsewhere, ADR-0034). Update the server, or turn Fuzzy off." },
  no_trigram_index: { title: "No trigram index covers that field", next: "Create an index of kind trigram on the field in Indexes, then search again." },
  // Reserved since ADR-0034: the trigram index kind is built on every database now.
  index_kind_unsupported: { title: "This server version cannot build that kind of index", next: "Current versions build the trigram kind on every database (ADR-0034). Update the server, or pick btree." },
  invalid_fuzzy_threshold: { title: "The fuzzy threshold must be between 0 and 1", next: "Use a number such as 0.3." },
  cypher_unsupported: { title: "That query uses openCypher the built-in engine does not support", next: "The message says which construct and where. Rewrite the query without it (WITH, OPTIONAL MATCH, UNWIND, UNION, CASE, arithmetic, path variables and unbounded * are outside the subset), or use the Traversal and Shortest path queries." },
  cypher_too_complex: { title: "That query is too big or too deep for the built-in engine", next: "Use at most 10 hops in a variable-length path, constrain its start node (a property or a label), or split the query. The message says which limit and where." },
  cypher_invalid: { title: "That query could not be understood", next: "Check the syntax. A query needs a final RETURN with named items, and RETURN * is not allowed." },
  cypher_read_only: { title: "That query changes data", next: "Cypher here only reads; write through the REST graph API (Nodes and Edges tabs)." },
  cypher_error: { title: "The query failed while running", next: "Check the labels, properties and parameters it uses. The server message below says what it hit." },
  cypher_timeout: { title: "The query ran out of time", next: "Narrow it with a label or a filter, lower the depth, or raise the timeout (at most 30 000 ms)." },
  invalid_limit: { title: "The limit is out of range", next: "Use a whole number from 1 to 1000." },
  invalid_timeout: { title: "The timeout is out of range", next: "Use a whole number of milliseconds from 1 to 30000." },
  cypher_rules_unsupported: { title: "Your graph access rules cannot be applied to openCypher", next: "Ask an administrator to simplify the label rules for your role, or use the REST graph API (Nodes, Edges, Traversal)." },
  graph_materialising: { title: "The openCypher graph is still being built", next: "It is built once from your nodes and edges. Wait for it to finish, then run the query again." },
  tenant_moving: { title: "That tenant is being moved right now", next: "Wait for the move to finish, then try again. The openCypher graph is rebuilt on the new location." },
  operation_in_progress: { title: "Another change to this set of servers is already running", next: "Wait for it to finish in Operations below (or cancel it), then try again." },
  shard_not_empty: { title: "That shard still holds tenants", next: "Move its tenants to another shard first, or drain it, then remove it." },
  node_mismatch: { title: "That server is not a copy of this set's primary", next: "Pick a server that follows this set's primary, or add the replica from this screen." },
  sharding_unsupported: { title: "That server cannot hold shards", next: "Set max_prepared_transactions above 0 on it (PostgreSQL setting), restart it and try again." },
  node_unreachable: { title: "ForgeBase cannot reach that server", next: "Check its address, that PostgreSQL is running and that the network and firewall let this server connect." },
  postgres_required: { title: "Replicas and shards need PostgreSQL", next: "Move this project with `forgebase db convert --to postgres`." },
  mfa_step_up_required: { title: "Sign in again with your second factor", next: "This change needs a sign-in with your authenticator code from the last 10 minutes. Sign in again below, then retry." },
  operation_not_found: { title: "That operation no longer exists", next: "Reload the page to see the current operations." },
  operation_not_cancellable: { title: "This operation can no longer be cancelled", next: "It has passed its last safe stopping point. Let it finish." },
  node_from_config: { title: "forgebase.toml declares this server", next: "Change or remove it in the config file and restart the server." },
  shard_exists: { title: "A shard with that name already exists", next: "Pick another shard name." },
  topology_unavailable: { title: "This server cannot manage replicas and shards right now", next: "Wait a moment and reload. If it keeps happening, check System." },
  not_implemented: { title: "Coming in this release", next: "Tenant moves and rebalancing are not switched on in this build yet." },
  internal: { title: "Something went wrong on the server", next: "Copy the request id and look it up in the server log." },
  network_error: { title: "Cannot reach the server", next: "Check that the server is running and the network is up, then try again." },
  invalid_response: { title: "The server sent an answer the dashboard could not read", next: "The dashboard and the server may be different versions. Check the version in System." },
} satisfies Record<string, ErrorWords>;

const WORDS: Record<string, ErrorWords | undefined> = ERROR_WORDS;

/** Next step by HTTP status, for codes the map does not know. */
function nextFor(status: number | undefined): string {
  if (status === undefined) return "Try again.";
  if (status === 400 || status === 422) return "Check the values you entered and try again.";
  if (status === 404) return ERROR_WORDS.not_found.next;
  if (status === 409) return ERROR_WORDS.conflict.next;
  if (status === 413) return ERROR_WORDS.payload_too_large.next;
  if (status === 429) return ERROR_WORDS.rate_limited.next;
  if (status >= 500) return "Try again. If it keeps happening, copy the request id and look it up in the server log.";
  return "Try again.";
}

/**
 * The line and column of an openCypher error (`cypher_invalid`, `cypher_unsupported`, `cypher_too_complex`): from
 * `error.details.line` and `column` when the server sends them, else from the "(line 2, column 14)" its message ends with.
 */
export function queryPosition(error: unknown): QueryPosition | undefined {
  if (!(error instanceof ApiError) || !error.code.startsWith("cypher_")) return undefined;
  const d = (error.body as { error?: { details?: { line?: unknown; column?: unknown } } } | undefined)?.error?.details;
  if (typeof d?.line === "number" && typeof d.column === "number") return { line: d.line, column: d.column };
  const m = /\(line (\d+), column (\d+)\)/.exec(error.message);
  return m ? { line: Number(m[1]), column: Number(m[2]) } : undefined;
}

/** The permission a 403 names, from "missing permission collections:read" and similar messages. */
export function missingPermission(error: unknown): string | undefined {
  if (!(error instanceof ApiError)) return undefined;
  const m = /permission[s]?\s+[`"']?([a-z_][\w.:*-]*)/i.exec(error.message) ?? /scope[s]?\s+[`"']?([a-z_][\w.:*-]*)/i.exec(error.message);
  return m?.[1];
}

/** Explain any thrown value in plain words. Never returns raw JSON in `title` or `next`. */
export function explain(error: unknown): ExplainedError {
  if (error instanceof ApiError) {
    const base = { status: error.status, code: error.code, requestId: error.requestId, message: error.message, position: queryPosition(error), raw: error.body ?? { error: { code: error.code, message: error.message } } };
    // A failed sign-in is a 401 too: its own code (invalid_credentials) has better words than "session ended".
    if (error.status === 401 && error.code === "invalid_credentials") return { ...base, kind: "invalid", ...ERROR_WORDS.invalid_credentials };
    if (error.status === 401) return { ...base, kind: "session", title: "Your session ended", next: "Sign in again to carry on." };
    if (error.status === 403 && error.code === "cypher_rules_unsupported") return { ...base, kind: "forbidden", ...ERROR_WORDS.cypher_rules_unsupported };
    if (error.status === 403 && error.code === "mfa_step_up_required") return { ...base, kind: "forbidden", ...ERROR_WORDS.mfa_step_up_required };
    if (error.status === 403) {
      const perm = missingPermission(error);
      return {
        ...base,
        kind: "forbidden",
        title: "You do not have permission to do this",
        next: perm ? `Your account lacks the permission ${perm}. Ask an administrator to grant it.` : "Your account lacks the permission. Ask an administrator to grant it.",
      };
    }
    const words = WORDS[error.code];
    const kind: ErrorKind = error.isNetwork ? "network" : error.code === "feature_disabled" ? "disabled" : error.status === 404 ? "missing" : error.status >= 500 ? "server" : error.status >= 400 ? "invalid" : "other";
    if (words) return { ...base, kind, ...words };
    // An unknown code: the server's message is the best title we have (kept word for word, so it can be searched).
    return { ...base, kind, title: error.message || `Request failed (${error.status})`, next: nextFor(error.status) };
  }
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong";
  return { kind: "other", title: message, next: "Try again.", message, raw: error instanceof Error ? { name: error.name, message: error.message } : error };
}

/** Field name → plain message, from a 400 `validation_failed` (`error.details.fields`). Empty for other errors. */
export function fieldErrors(error: unknown): Record<string, string> {
  if (!(error instanceof ApiError)) return {};
  const out: Record<string, string> = {};
  for (const [name, f] of Object.entries(error.fields)) out[name] = f.message || FIELD_CODE_WORDS[f.code] || "This value is not valid";
  return out;
}

const FIELD_CODE_WORDS: Record<string, string> = {
  required: "This field is required",
  invalid: "This value is not valid",
  too_long: "This value is too long",
  too_short: "This value is too short",
  unique: "This value is already used",
  type: "This value has the wrong type",
};

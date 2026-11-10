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

export type ErrorKind = "session" | "forbidden" | "network" | "disabled" | "invalid" | "missing" | "server" | "other";

export interface ExplainedError extends ErrorWords {
  kind: ErrorKind;
  status?: number;
  code?: string;
  requestId?: string;
  /** The server's own message, for Details. */
  message: string;
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

const sentence = (s: string) => (s ? s.charAt(0).toUpperCase() + s.slice(1) : s);

/** The permission a 403 names, from "missing permission collections:read" and similar messages. */
export function missingPermission(error: unknown): string | undefined {
  if (!(error instanceof ApiError)) return undefined;
  const m = /permission[s]?\s+[`"']?([a-z_][\w.:*-]*)/i.exec(error.message) ?? /scope[s]?\s+[`"']?([a-z_][\w.:*-]*)/i.exec(error.message);
  return m?.[1];
}

/** Explain any thrown value in plain words. Never returns raw JSON in `title` or `next`. */
export function explain(error: unknown): ExplainedError {
  if (error instanceof ApiError) {
    const base = { status: error.status, code: error.code, requestId: error.requestId, message: error.message, raw: error.body ?? { error: { code: error.code, message: error.message } } };
    if (error.status === 401) return { ...base, kind: "session", title: "Your session ended", next: "Sign in again to carry on." };
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
    return { ...base, kind, title: sentence(error.message) || `Request failed (${error.status})`, next: nextFor(error.status) };
  }
  const message = error instanceof Error ? error.message : typeof error === "string" ? error : "Something went wrong";
  return { kind: "other", title: sentence(message), next: "Try again.", message, raw: error instanceof Error ? { name: error.name, message: error.message } : error };
}

/** Field name → plain message, from a 400 `validation_failed` (`error.details.fields`). Empty for other errors. */
export function fieldErrors(error: unknown): Record<string, string> {
  if (!(error instanceof ApiError)) return {};
  const out: Record<string, string> = {};
  for (const [name, f] of Object.entries(error.fields)) out[name] = sentence(f.message) || FIELD_CODE_WORDS[f.code] || "This value is not valid";
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

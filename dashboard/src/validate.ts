/**
 * Validators that mirror the server's rules, so forms can say what is wrong as you type (dashboard-ux.md rule 6).
 * Each returns `undefined` when the value is fine, or one plain sentence for FormField's `error`.
 *
 * Usage:
 *   const nameError = touched ? validate.collectionName(name) : undefined;
 *   <FormField label="Name" error={nameError ?? fieldErrors(saveError).name}>…</FormField>
 *   <Button disabled={!!validate.collectionName(name)}>Create</Button>
 *
 * The server stays the judge: a rule here only saves a round trip. When the server changes a rule, change it here.
 */

export type Validator = (value: string) => string | undefined;

const empty = (v: string, what: string) => (v.trim() === "" ? `Enter a ${what}.` : undefined);

/** Collection and field names (crates/forgebase-schema): a letter, then letters, digits or underscores; at most 63. */
export const collectionName: Validator = (v) =>
  empty(v, "name") ??
  (/^(_forgebase|sqlite_)/i.test(v) ? "Names starting with _forgebase or sqlite_ are reserved." : undefined) ??
  (v.length > 63 ? "Use at most 63 characters." : undefined) ??
  (/^[A-Za-z][A-Za-z0-9_]*$/.test(v) ? undefined : "Use letters, digits and underscores, starting with a letter.");

/** Field names follow the collection rule. */
export const fieldName: Validator = collectionName;

/** Bucket names (crates/forgebase-storage): 1-63 of a-z, 0-9, '.', '_' and '-', starting with a letter or digit. */
export const bucketName: Validator = (v) =>
  empty(v, "bucket name") ??
  (v.length > 63 ? "Use at most 63 characters." : undefined) ??
  (/^[a-z0-9][a-z0-9._-]*$/.test(v) ? undefined : "Use lower-case letters, digits, dots, dashes or underscores, starting with a letter or digit.");

/** Function names (crates/forgebase-functions): 1-48 of a-z, 0-9, '_' and '-', starting with a lower-case letter. */
export const functionName: Validator = (v) =>
  empty(v, "function name") ??
  (v.length > 48 ? "Use at most 48 characters." : undefined) ??
  (/^[a-z][a-z0-9_-]*$/.test(v) ? undefined : "Use lower-case letters, digits, dashes or underscores, starting with a letter.");

/** Queue topic and subscription names (crates/forgebase-queue): 1-128 letters, digits, '.', '_', ':' or '-', starting with a letter or digit. */
export const topicName: Validator = (v) =>
  empty(v, "name") ??
  (v.length > 128 ? "Use at most 128 characters." : undefined) ??
  (/^[A-Za-z0-9][A-Za-z0-9._:-]*$/.test(v) ? undefined : "Use letters, digits, dots, colons, dashes or underscores, starting with a letter or digit.");

/** Key-value namespaces (crates/forgebase-kv): 1-64 letters, digits, '_', '.', '-' or ':', starting with a letter or digit. */
export const namespaceName: Validator = (v) =>
  empty(v, "namespace") ??
  (v.length > 64 ? "Use at most 64 characters." : undefined) ??
  (/^[A-Za-z0-9][A-Za-z0-9_.:-]*$/.test(v) ? undefined : "Use letters, digits, dots, colons, dashes or underscores, starting with a letter or digit.");

/** A non-empty value. */
export const required =
  (what = "a value"): Validator =>
  (v) =>
    v.trim() === "" ? `Enter ${what}.` : undefined;

/** Valid JSON (empty passes; combine with `required` when it must be set). */
export const json: Validator = (v) => {
  if (v.trim() === "") return undefined;
  try {
    JSON.parse(v);
    return undefined;
  } catch (e) {
    return `That is not valid JSON: ${e instanceof Error ? e.message : "check the brackets and quotes"}.`;
  }
};

/** A whole number in [min, max]; empty passes. */
export const integer =
  (min = Number.MIN_SAFE_INTEGER, max = Number.MAX_SAFE_INTEGER): Validator =>
  (v) => {
    if (v.trim() === "") return undefined;
    if (!/^-?\d+$/.test(v.trim())) return "Enter a whole number.";
    const n = Number(v);
    if (n < min) return `Enter ${min} or more.`;
    if (n > max) return `Enter ${max} or less.`;
    return undefined;
  };

/** The first message from several validators. */
export const all =
  (...vs: Validator[]): Validator =>
  (v) => {
    for (const f of vs) {
      const m = f(v);
      if (m) return m;
    }
    return undefined;
  };

export const validate = { collectionName, fieldName, bucketName, functionName, topicName, namespaceName, required, json, integer, all };
export default validate;

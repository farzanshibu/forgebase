/** Minimal OpenAPI helpers: route matching and response-shape validation against the real /api/openapi.json. */
import spec from "./fixtures/openapi.json";

type Json = Record<string, unknown>;
const doc = spec as unknown as { paths: Record<string, Record<string, Json>>; components: { schemas: Record<string, Json> } };

export function specOperations(): string[] {
  const out: string[] = [];
  for (const [path, item] of Object.entries(doc.paths))
    for (const m of ["get", "post", "put", "patch", "delete"]) if (item[m]) out.push(`${m} ${path}`);
  return out;
}

export interface Route {
  template: string;
  params: Record<string, string>;
  op: Json;
}

/** Find the operation for a request, or undefined when the document does not describe it. */
export function matchRoute(method: string, pathname: string): Route | undefined {
  const parts = pathname.split("/");
  for (const [template, item] of Object.entries(doc.paths)) {
    const tp = template.split("/");
    if (tp.length !== parts.length) continue;
    const params: Record<string, string> = {};
    const ok = tp.every((seg, i) => {
      if (seg.startsWith("{")) {
        params[seg.slice(1, -1)] = decodeURIComponent(parts[i] ?? "");
        return true;
      }
      return seg === parts[i];
    });
    const op = item[method.toLowerCase()];
    if (ok && op) return { template, params, op };
  }
  return undefined;
}

/** The documented schema of the success response, as a schema name (when it is a `$ref`). */
export function responseSchemaName(op: Json, status: number): string | undefined {
  const res = (op.responses as Record<string, Json> | undefined)?.[String(status)];
  const schema = ((res?.content as Json | undefined)?.["application/json"] as Json | undefined)?.schema as Json | undefined;
  const ref = schema?.$ref;
  return typeof ref === "string" ? ref.split("/").pop() : undefined;
}

export function hasResponse(op: Json, status: number): boolean {
  return !!(op.responses as Record<string, unknown> | undefined)?.[String(status)];
}

/** Returns a list of problems; empty when `value` fits `schema`. */
export function validate(schema: Json, value: unknown, at = "$"): string[] {
  if (typeof schema.$ref === "string") {
    const name = schema.$ref.split("/").pop() as string;
    return validate(doc.components.schemas[name] ?? {}, value, at);
  }
  if (Array.isArray(schema.oneOf) || Array.isArray(schema.anyOf)) {
    const alts = (schema.oneOf ?? schema.anyOf) as Json[];
    return alts.some((a) => validate(a, value, at).length === 0) ? [] : [`${at}: matches no alternative`];
  }
  const types = ([] as unknown[]).concat(schema.type ?? []) as string[];
  if (types.includes("object") && !schema.properties && !schema.additionalProperties) return [];
  if (value === null) return types.length === 0 || types.includes("null") ? [] : [`${at}: null not allowed`];
  if (types.length === 0) return [];
  const actual = Array.isArray(value) ? "array" : typeof value === "number" ? (Number.isInteger(value) ? "integer" : "number") : typeof value;
  const okType = types.some((t) => t === actual || (t === "number" && actual === "integer"));
  if (!okType) return [`${at}: expected ${types.join("|")}, got ${actual}`];
  const problems: string[] = [];
  // utoipa `Object` = any JSON value (the document uses it for free-form data such as SQL cells), so no shape to check.
  if (actual === "object" && schema.properties) {
    const obj = value as Json;
    for (const req of (schema.required as string[] | undefined) ?? []) if (!(req in obj)) problems.push(`${at}.${req}: required but missing`);
    for (const [k, sub] of Object.entries(schema.properties as Record<string, Json>)) if (k in obj) problems.push(...validate(sub, obj[k], `${at}.${k}`));
  }
  if (actual === "array" && schema.items) (value as unknown[]).forEach((v, i) => problems.push(...validate(schema.items as Json, v, `${at}[${i}]`)));
  return problems;
}

export function validateBody(schemaName: string, body: unknown): string[] {
  return validate({ $ref: `#/components/schemas/${schemaName}` }, body);
}

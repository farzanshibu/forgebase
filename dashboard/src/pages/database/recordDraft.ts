/**
 * The model behind RecordForm: which control a field gets, and the two-way mapping between what the user typed (a
 * "draft": strings, plus booleans for bool fields) and the JSON body the server takes.
 */
import type { FieldDef, RecordObject } from "../../api";
import { READ_ONLY_KEYS } from "./values";

export type Control = "text" | "email" | "url" | "password" | "number" | "bool" | "date" | "select" | "selectMany" | "relation" | "list" | "json";
export type Draft = Record<string, string | boolean>;

/** Types whose value is a JSON document the user edits as text. */
const JSON_TYPES = ["json", "array", "geo", "vector"];

const maxSelect = (f: FieldDef) => (typeof f.max_select === "number" ? f.max_select : 1);

/** The control for a field: one per type (relations pick a record; multi-value file and relation fields take a list). */
export function controlFor(f: FieldDef): Control {
  const t = String(f.type);
  if (JSON_TYPES.includes(t)) return "json";
  if (t === "select") return maxSelect(f) > 1 ? "selectMany" : "select";
  if (t === "relation") return maxSelect(f) > 1 ? "list" : "relation";
  if (t === "file") return "list";
  if (t === "bool" || t === "number" || t === "date" || t === "email" || t === "url" || t === "password") return t;
  return "text";
}

/** Fields the form edits: the ones the user owns (not the system fields). */
export const editableFields = (fields: FieldDef[]) => fields.filter((f) => !f.system && f.name !== "id");

const pad = (n: number) => String(n).padStart(2, "0");
/** ISO timestamp to the `datetime-local` value (local time, minutes). */
export function isoToLocal(iso: string): string {
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}
/** `datetime-local` value to an ISO timestamp, or undefined when it is not a date. */
export function localToIso(local: string): string | undefined {
  const d = new Date(local);
  return Number.isNaN(d.getTime()) ? undefined : d.toISOString();
}

function toText(v: unknown, c: Control): string | boolean {
  if (c === "bool") return v === true;
  if (v === null || v === undefined) return "";
  if (c === "json") return JSON.stringify(v, null, 2);
  if (c === "list" || c === "selectMany") return Array.isArray(v) ? v.join(", ") : String(v);
  if (c === "date") return typeof v === "string" ? isoToLocal(v) : "";
  return String(v);
}

/** One draft entry per editable field, from an existing record (or the field defaults for a new one). */
export function toDraft(fields: FieldDef[], record?: RecordObject): Draft {
  const out: Draft = {};
  for (const f of editableFields(fields)) {
    const c = controlFor(f);
    const v = record ? record[f.name] : f.default;
    out[f.name] = c === "password" ? "" : toText(v, c);
  }
  return out;
}

const splitList = (s: string) => s.split(",").map((x) => x.trim()).filter(Boolean);

export interface BodyResult { body: Record<string, unknown>; errors: Record<string, string> }

/**
 * The JSON body for a draft, and one message per field that cannot be sent. An empty optional field is left out.
 * A password left empty on an existing record is left out too (it is write-only), so it keeps its value.
 */
export function toBody(fields: FieldDef[], draft: Draft): BodyResult {
  const body: Record<string, unknown> = {};
  const errors: Record<string, string> = {};
  for (const f of editableFields(fields)) {
    const c = controlFor(f);
    const raw = draft[f.name];
    if (c === "bool") {
      body[f.name] = raw === true;
      continue;
    }
    const text = typeof raw === "string" ? raw : "";
    if (text.trim() === "") {
      if (f.required && c !== "password") errors[f.name] = "This field is required.";
      continue;
    }
    if (c === "number") {
      const n = Number(text);
      if (Number.isNaN(n)) errors[f.name] = "Enter a number.";
      else body[f.name] = n;
    } else if (c === "date") {
      const iso = localToIso(text);
      if (iso) body[f.name] = iso;
      else errors[f.name] = "Enter a date and time.";
    } else if (c === "json") {
      try {
        const v = JSON.parse(text) as unknown;
        if ((f.type === "array" || f.type === "vector") && !Array.isArray(v)) errors[f.name] = "Enter a JSON array, such as [1, 2].";
        else body[f.name] = v;
      } catch {
        errors[f.name] = "Not valid JSON.";
      }
    } else if (c === "list" || c === "selectMany") {
      body[f.name] = splitList(text);
    } else {
      body[f.name] = text;
    }
  }
  return { body, errors };
}

/** Parse the "Edit as JSON" text into a record body; throws a readable Error. Read-only keys are dropped. */
export function parseJsonBody(text: string): Record<string, unknown> {
  let doc: unknown;
  try {
    doc = JSON.parse(text);
  } catch (e) {
    throw new Error(`Invalid JSON: ${(e as Error).message}`);
  }
  if (typeof doc !== "object" || doc === null || Array.isArray(doc)) throw new Error("A record must be a JSON object.");
  const body = { ...(doc as Record<string, unknown>) };
  for (const k of READ_ONLY_KEYS) delete body[k];
  return body;
}

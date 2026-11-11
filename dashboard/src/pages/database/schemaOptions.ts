/** Typed options per field type for the schema designer (crates/forgebase-schema `FieldKind`), and the mapping to and from the wire field. */
import type { FieldDef } from "../../api";

export type OptionKind = "int" | "num" | "bool" | "list" | "collection";
export interface OptionSpec { key: string; label: string; kind: OptionKind; help?: string }

/** The options each field type takes (crates/forgebase-schema `FieldKind`). Other options on a field are kept as they are. */
export const OPTIONS: Record<string, OptionSpec[]> = {
  text: [{ key: "min_length", label: "Min length", kind: "int" }, { key: "max_length", label: "Max length", kind: "int" }],
  number: [{ key: "min", label: "Min", kind: "num" }, { key: "max", label: "Max", kind: "num" }, { key: "integer", label: "Whole numbers only", kind: "bool" }],
  select: [{ key: "values", label: "Allowed values", kind: "list", help: "Separate with commas" }, { key: "max_select", label: "Max choices", kind: "int" }],
  json: [{ key: "max_size", label: "Max size (bytes)", kind: "int" }],
  array: [{ key: "min_items", label: "Min items", kind: "int" }, { key: "max_items", label: "Max items", kind: "int" }],
  relation: [{ key: "collection_id", label: "Points to", kind: "collection" }, { key: "max_select", label: "Max records", kind: "int" }, { key: "cascade_delete", label: "Delete with the referenced record", kind: "bool" }],
  file: [{ key: "max_select", label: "Max files", kind: "int" }],
  password: [{ key: "min_length", label: "Min length", kind: "int" }],
  vector: [{ key: "dimensions", label: "Dimensions", kind: "int" }],
};

export type OptValue = string | boolean;
export interface Row {
  base: FieldDef;
  name: string;
  type: string;
  required: boolean;
  unique: boolean;
  opts: Record<string, OptValue>;
}

export function toRow(f: FieldDef): Row {
  const opts: Record<string, OptValue> = {};
  for (const o of OPTIONS[String(f.type)] ?? []) {
    const v = f[o.key];
    if (o.kind === "bool") opts[o.key] = v === true;
    else if (v !== undefined && v !== null) opts[o.key] = Array.isArray(v) ? v.join(", ") : String(v);
  }
  return { base: f, name: f.name, type: String(f.type), required: !!f.required, unique: !!f.unique, opts };
}

/** The wire field for a row: unknown options kept, this type's options typed, unset ones left out. Returns an error message when an option is not valid. */
export function buildField(r: Row): { field?: FieldDef; error?: string } {
  const field: FieldDef = { ...r.base, name: r.name, type: r.type, required: r.required, unique: r.unique };
  const specs = OPTIONS[r.type] ?? [];
  for (const known of Object.values(OPTIONS).flat()) if (!specs.some((s) => s.key === known.key)) delete field[known.key];
  for (const o of specs) {
    const v = r.opts[o.key];
    delete field[o.key];
    if (o.kind === "bool") {
      if (v === true) field[o.key] = true;
    } else if (typeof v === "string" && v.trim() !== "") {
      if (o.kind === "list") field[o.key] = v.split(",").map((x) => x.trim()).filter(Boolean);
      else if (o.kind === "collection") field[o.key] = v.trim();
      else {
        const n = Number(v);
        if (Number.isNaN(n) || (o.kind === "int" && !Number.isInteger(n))) return { error: `${o.label} of field "${r.name}" must be ${o.kind === "int" ? "a whole number" : "a number"}.` };
        field[o.key] = n;
      }
    }
  }
  return { field };
}


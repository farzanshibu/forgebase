/**
 * RecordForm: one typed control per field of a collection (audit A-07). Text, email and URL fields get an Input, numbers
 * a number input, bool a switch, dates a date-time input, select a Select, relations a record picker, and JSON, array,
 * geo and vector fields a small JSON box. Required fields are marked; an error shows under its field.
 *
 * Usage (controlled; `recordForm.ts` has the draft helpers):
 *   const [draft, setDraft] = useState(() => toDraft(collection.fields, record));
 *   <RecordForm collection={collection} draft={draft} onChange={(name, v) => setDraft((d) => ({ ...d, [name]: v }))}
 *               errors={{ title: "This field is required." }} />
 */
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { listRecords } from "../../api";
import type { CollectionDef, FieldDef } from "../../api";
import { Check, Sel, Toggle } from "../../components/controls";
import { FormField, type ControlProps } from "../../components/FormField";
import { useLoad } from "../../hooks";
import { controlFor, editableFields, type Draft } from "./recordDraft";

export interface RecordFormProps {
  collection: CollectionDef;
  draft: Draft;
  onChange: (field: string, value: string | boolean) => void;
  /** One message per field name (client checks and the server's `details.fields`). */
  errors?: Record<string, string>;
  /** Every collection, to resolve a relation field's target. */
  collections?: CollectionDef[];
  /** Only a new record may set a field that is unique and required, so none today; reserved for read-only forms. */
  disabled?: boolean;
}

const stringValues = (f: FieldDef): string[] => (Array.isArray(f.values) ? f.values.map(String) : []);

export function RecordForm({ collection, draft, onChange, errors = {}, collections = [], disabled }: RecordFormProps) {
  const fields = editableFields(collection.fields);
  if (fields.length === 0) {
    return <p className="text-sm text-muted-foreground">This collection has no fields yet. Add some on the Schema tab, or use “Edit as JSON” when the schema is flexible.</p>;
  }
  return (
    <div className="space-y-4">
      {fields.map((f) => (
        <FormField key={f.name} label={f.name} required={!!f.required && controlFor(f) !== "password"} error={errors[f.name]} help={helpFor(f)}>
          {(p) => <FieldControl field={f} p={p} value={draft[f.name]} onChange={(v) => onChange(f.name, v)} collections={collections} disabled={disabled} />}
        </FormField>
      ))}
    </div>
  );
}

function helpFor(f: FieldDef): string | undefined {
  const c = controlFor(f);
  if (c === "password") return "Write-only. Leave empty to keep the current value.";
  if (c === "list") return f.type === "file" ? "File names, separated by commas." : "Record ids, separated by commas.";
  if (c === "json" && f.type === "geo") return 'A point such as {"lon": 2.35, "lat": 48.85}.';
  if (c === "json" && f.type === "vector") return `A JSON array of ${f.dimensions ?? "n"} numbers.`;
  if (c === "json" && f.type === "array") return "A JSON array, such as [1, 2, 3].";
  if (c === "json") return "Any JSON value.";
  if (c === "date") return "Your local time; stored as UTC.";
  return undefined;
}

function FieldControl({ field: f, p, value, onChange, collections, disabled }: { field: FieldDef; p: ControlProps; value: string | boolean | undefined; onChange: (v: string | boolean) => void; collections: CollectionDef[]; disabled?: boolean }) {
  const c = controlFor(f);
  const text = typeof value === "string" ? value : "";
  switch (c) {
    case "bool":
      return <Toggle {...p} checked={value === true} onChange={onChange} aria-label={f.name} disabled={disabled} />;
    case "select": {
      const values = stringValues(f);
      return <Sel {...p} aria-label={f.name} value={text} onChange={onChange} placeholder="Choose…" options={f.required ? values : ["", ...values].map((v) => ({ value: v, label: v || "None" }))} disabled={disabled} />;
    }
    case "selectMany": {
      const chosen = new Set(text.split(",").map((x) => x.trim()).filter(Boolean));
      return (
        <div role="group" aria-label={f.name} className="flex flex-wrap gap-x-4 gap-y-1">
          {stringValues(f).map((v) => (
            <Check key={v} label={v} checked={chosen.has(v)} disabled={disabled} onChange={(on) => { const n = new Set(chosen); if (on) n.add(v); else n.delete(v); onChange([...n].join(", ")); }} />
          ))}
        </div>
      );
    }
    case "relation":
      return <RelationPicker f={f} p={p} text={text} onChange={onChange} collections={collections} disabled={disabled} />;
    case "json":
      return <Textarea {...p} size="sm" autoResize={false} spellCheck={false} className="h-24 font-mono text-xs" value={text} onChange={(e) => onChange(e.target.value)} disabled={disabled} />;
    default: {
      const type = c === "number" ? "number" : c === "date" ? "datetime-local" : c === "email" ? "email" : c === "url" ? "url" : c === "password" ? "password" : "text";
      return <Input {...p} size="sm" type={type} step={c === "number" ? "any" : undefined} autoComplete={c === "password" ? "new-password" : "off"} value={text} onChange={(e) => onChange(e.target.value)} disabled={disabled} />;
    }
  }
}

/** A Select of the target collection's first records; falls back to a plain id Input when they cannot be listed. */
function RelationPicker({ f, p, text, onChange, collections, disabled }: { f: FieldDef; p: ControlProps; text: string; onChange: (v: string) => void; collections: CollectionDef[]; disabled?: boolean }) {
  const target = collections.find((c) => c.id === f.collection_id || c.name === f.collection_id);
  const list = useLoad((s) => (target ? listRecords(target.name, { per_page: 100 }, s) : Promise.reject(new Error("unknown target"))), `relation:${target?.name ?? f.collection_id}`);
  if (!target || list.error || !list.data) {
    return <Input {...p} size="sm" placeholder="Record id" value={text} onChange={(e) => onChange(e.target.value)} disabled={disabled || (!!target && !list.error)} />;
  }
  const label = (r: Record<string, unknown>) => {
    const first = target.fields.find((x) => !x.system && x.type === "text");
    const v = first ? r[first.name] : undefined;
    return v ? `${String(v)} (${String(r.id)})` : String(r.id);
  };
  const options = list.data.items.map((r) => ({ value: String(r.id), label: label(r) }));
  if (text && !options.some((o) => o.value === text)) options.unshift({ value: text, label: text });
  return <Sel {...p} aria-label={f.name} value={text} onChange={onChange} placeholder={`Choose a ${target.name} record…`} options={f.required ? options : [{ value: "", label: "None" }, ...options]} disabled={disabled} />;
}

export default RecordForm;

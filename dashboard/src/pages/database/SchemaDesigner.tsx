import { useState } from "react";
import { useNavigate } from "react-router-dom";
import { createCollection, deleteCollection, updateCollection, FIELD_TYPES } from "../../api";
import type { CollectionDef, FieldDef } from "../../api";
import { Button, ConfirmButton, ErrorView, inputCls } from "../../components/ui";

const CORE_KEYS = ["id", "name", "type", "required", "unique", "hidden", "system", "default"];

interface Row {
  base: FieldDef;
  name: string;
  type: string;
  required: boolean;
  unique: boolean;
  options: string;
}

function toRow(f: FieldDef): Row {
  const opts: Record<string, unknown> = {};
  for (const [k, v] of Object.entries(f)) if (!CORE_KEYS.includes(k)) opts[k] = v;
  return { base: f, name: f.name, type: String(f.type), required: !!f.required, unique: !!f.unique, options: Object.keys(opts).length ? JSON.stringify(opts) : "" };
}

export default function SchemaDesigner({ existing, onSaved }: { existing?: CollectionDef; onSaved?: () => void }) {
  const nav = useNavigate();
  const [name, setName] = useState(existing?.name ?? "");
  const [kind, setKind] = useState(existing?.type ?? "base");
  const [mode, setMode] = useState(existing?.schema_mode ?? "strict");
  const [versioned, setVersioned] = useState(!!existing?.versioned);
  const [rows, setRows] = useState<Row[]>((existing?.fields ?? []).map(toRow));
  const [problem, setProblem] = useState<unknown>();
  const [saved, setSaved] = useState(false);

  const patch = (i: number, p: Partial<Row>) => setRows((rs) => rs.map((r, j) => (j === i ? { ...r, ...p } : r)));

  function build(): CollectionDef | null {
    const fields: FieldDef[] = [];
    for (const r of rows) {
      if (r.base.system) {
        fields.push(r.base);
        continue;
      }
      let opts: Record<string, unknown> = {};
      if (r.options.trim()) {
        try {
          opts = JSON.parse(r.options) as Record<string, unknown>;
        } catch {
          setProblem(new Error(`Options of field "${r.name}" are not valid JSON`));
          return null;
        }
      }
      fields.push({ ...r.base, ...opts, name: r.name, type: r.type, required: r.required, unique: r.unique });
    }
    return { ...(existing ?? {}), name, type: kind, schema_mode: mode, versioned, fields };
  }

  async function save() {
    setSaved(false);
    const def = build();
    if (!def) return;
    try {
      if (existing) {
        await updateCollection(existing.name, def);
        setProblem(undefined);
        setSaved(true);
        onSaved?.();
      } else {
        await createCollection(def);
        nav(`/database/c/${encodeURIComponent(name)}`);
      }
    } catch (e) {
      setProblem(e);
    }
  }

  return (
    <div className="max-w-4xl space-y-4">
      <h2 className="text-lg font-medium">{existing ? "Schema" : "New collection"}</h2>
      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label className="flex flex-col gap-1">Name
          <input className={inputCls} value={name} onChange={(e) => setName(e.target.value)} />
        </label>
        <label className="flex flex-col gap-1">Type
          <select className={inputCls} value={kind} onChange={(e) => setKind(e.target.value)}>
            <option>base</option><option>auth</option><option>view</option>
          </select>
        </label>
        <label className="flex flex-col gap-1">Schema mode
          <select className={inputCls} value={mode} onChange={(e) => setMode(e.target.value)}>
            <option>strict</option><option>flexible</option>
          </select>
        </label>
        <label className="flex items-center gap-2 pb-2">
          <input type="checkbox" checked={versioned} onChange={(e) => setVersioned(e.target.checked)} /> Keep record versions
        </label>
      </div>

      <table className="w-full text-left text-sm">
        <thead className="text-xs uppercase tracking-wide text-zinc-500">
          <tr><th className="py-1">Field</th><th>Type</th><th>Required</th><th>Unique</th><th>Options (JSON)</th><th /></tr>
        </thead>
        <tbody>
          {rows.map((r, i) => {
            const locked = !!r.base.system;
            return (
              <tr key={r.base.id ?? `new-${i}`} className="border-t border-zinc-800">
                <td className="py-1 pr-2">
                  <input aria-label={`Field ${i + 1} name`} disabled={locked} className={`${inputCls} w-full`} value={r.name} onChange={(e) => patch(i, { name: e.target.value })} />
                </td>
                <td className="pr-2">
                  <select aria-label={`Field ${i + 1} type`} disabled={locked} className={inputCls} value={r.type} onChange={(e) => patch(i, { type: e.target.value })}>
                    {[...new Set([...FIELD_TYPES, r.type])].map((t) => <option key={t}>{t}</option>)}
                  </select>
                </td>
                <td><input type="checkbox" aria-label={`Field ${i + 1} required`} disabled={locked} checked={r.required} onChange={(e) => patch(i, { required: e.target.checked })} /></td>
                <td><input type="checkbox" aria-label={`Field ${i + 1} unique`} disabled={locked} checked={r.unique} onChange={(e) => patch(i, { unique: e.target.checked })} /></td>
                <td className="pr-2">
                  <input aria-label={`Field ${i + 1} options`} disabled={locked} className={`${inputCls} w-full font-mono text-xs`} placeholder='{"max_length":80}' value={r.options} onChange={(e) => patch(i, { options: e.target.value })} />
                </td>
                <td>{locked ? <span className="text-xs text-zinc-500">system</span> : <Button onClick={() => setRows((rs) => rs.filter((_, j) => j !== i))}>Remove</Button>}</td>
              </tr>
            );
          })}
        </tbody>
      </table>
      <Button onClick={() => setRows((rs) => [...rs, toRow({ name: "", type: "text" })])}>Add field</Button>

      <ErrorView error={problem} />
      {saved && <p role="status" className="text-sm text-emerald-300">Schema saved.</p>}
      <div className="flex gap-2">
        <Button tone="primary" onClick={() => void save()}>{existing ? "Save schema" : "Create collection"}</Button>
        {existing && !existing.system && (
          <ConfirmButton
            label="Delete collection"
            onConfirm={() => deleteCollection(existing.name).then(() => nav("/database"), setProblem)}
          />
        )}
      </div>
    </div>
  );
}

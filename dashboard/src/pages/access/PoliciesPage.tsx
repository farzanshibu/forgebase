import { Textarea } from "panelui-web/textarea";
import { EmptyState } from "../../components/EmptyState";
import { useState } from "react";
import { deletePolicy, getPolicy, listCollections, putPolicy, type CollectionDef, type Policy } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { ActionError } from "./shared";
import { useRun } from "./util";

type Mode = "none" | "open" | "expr";
const modeOf = (v: string | null | undefined): Mode => (v === null || v === undefined ? "none" : v.trim() === "" ? "open" : "expr");

/** One rule: `null` (locked / no rule), `""` (everyone) or an expression. */
export function RuleInput({ label, value, onChange, noneLabel }: { label: string; value: string | null | undefined; onChange: (v: string | null) => void; noneLabel: string }) {
  // The mode is state of its own: an expression being edited may be empty without turning into "Everyone".
  const [mode, setMode] = useState<Mode>(modeOf(value));
  return (
    <div className="flex flex-wrap items-start gap-2">
      <span className="w-28 pt-1.5 text-sm text-foreground/80">{label}</span>
      <Sel
        aria-label={`${label} mode`}
        value={mode}
        onChange={(v) => {
          const m = v as Mode;
          setMode(m);
          onChange(m === "none" ? null : m === "open" ? "" : (mode === "expr" ? value : null) ?? "@request.auth.id != \"\"");
        }}
        options={[{ value: "none", label: noneLabel }, { value: "open", label: "Everyone" }, { value: "expr", label: "Expression" }]}
      />
      {mode === "expr" && (
        <Textarea aria-label={`${label} expression`} minRows={2} className="min-w-72 flex-1 font-mono" value={value ?? ""} onChange={(e) => onChange(e.target.value)} />
      )}
    </div>
  );
}

const RULES = [["list_rule", "List"], ["view_rule", "View"], ["create_rule", "Create"], ["update_rule", "Update"], ["delete_rule", "Delete"]] as const;

export default function PoliciesPage() {
  const cols = useLoad(listCollections, "collections");
  const [name, setName] = useState("");
  const def = cols.data?.find((c) => c.name === name);
  const [saved, setSaved] = useState(false);
  const policy = useLoad((s) => (name ? getPolicy(name, s) : Promise.resolve(undefined)), `policy:${name}`);
  return (
    <div className="space-y-4">
      <ErrorView error={cols.error} />
      {cols.loading && !cols.error && <Loading what="Loading collections" />}
      {cols.data?.length === 0 && <EmptyState compact title="No collections yet." why="Collection rules apply to collections. Create a collection in Database first." />}
      {cols.data && cols.data.length > 0 && (
        <label className="flex max-w-xs flex-col gap-1 text-xs text-muted-foreground">Collection
          <Sel value={name} onChange={(v) => { setName(v); setSaved(false); }} options={[{ value: "", label: "Choose a collection…" }, ...cols.data.filter((c) => !c.system).map((c) => ({ value: c.name, label: c.name }))]} />
        </label>
      )}
      {name && <ErrorView error={policy.error} />}
      {name && policy.loading && !policy.error && <Loading what="Loading rules" />}
      {name && policy.data && def && <PolicyEditor key={`${name}:${JSON.stringify(policy.data)}`} policy={policy.data} def={def} saved={saved} onSaved={() => setSaved(true)} onChanged={policy.reload} />}
    </div>
  );
}

function PolicyEditor({ policy, def, saved, onSaved, onChanged }: { policy: Policy; def: CollectionDef; saved: boolean; onSaved: () => void; onChanged: () => void }) {
  const [p, setP] = useState<Policy>(policy);
  const { run, error } = useRun();
  const fields = def.fields.filter((f) => !f.system);
  const setField = (f: string, k: "read" | "write", v: string | null) => setP({ ...p, fields: { ...p.fields, [f]: { ...p.fields[f], [k]: v } } });
  const save = () => {
    const out: Policy["fields"] = {};
    for (const [k, v] of Object.entries(p.fields)) if (v.read != null || v.write != null) out[k] = { read: v.read ?? null, write: v.write ?? null };
    void run(async () => { await putPolicy(def.name, { list_rule: p.list_rule, view_rule: p.view_rule, create_rule: p.create_rule, update_rule: p.update_rule, delete_rule: p.delete_rule, fields: out }); onSaved(); });
  };
  return (
    <div className="space-y-4">
      <Panel title={`Rules of ${def.name}`}>
        <p className="mb-3 text-xs text-muted-foreground">Locked = privileged callers only. Everyone = no restriction. An expression is evaluated per request.</p>
        <div className="space-y-3">
          {RULES.map(([k, label]) => <RuleInput key={k} label={label} noneLabel="Locked" value={p[k]} onChange={(v) => setP({ ...p, [k]: v })} />)}
        </div>
      </Panel>
      {fields.length > 0 && (
        <Panel title="Field rules">
          <div className="space-y-3">
            {fields.map((f) => (
              <div key={f.name} className="rounded border border-border p-2">
                <p className="mb-1 font-mono text-sm">{f.name}</p>
                <RuleInput label={`${f.name} read`} noneLabel="No rule" value={p.fields[f.name]?.read} onChange={(v) => setField(f.name, "read", v)} />
                <RuleInput label={`${f.name} write`} noneLabel="No rule" value={p.fields[f.name]?.write} onChange={(v) => setField(f.name, "write", v)} />
              </div>
            ))}
          </div>
        </Panel>
      )}
      <div className="flex gap-2">
        <Button tone="primary" onClick={save}>Save rules</Button>
        <ConfirmButton label="Lock all rules" onConfirm={() => void run(async () => { await deletePolicy(def.name); onChanged(); })} />
        {saved && !error && <span role="status" className="self-center text-sm text-success">Saved</span>}
      </div>
      <ActionError error={error} />
    </div>
  );
}

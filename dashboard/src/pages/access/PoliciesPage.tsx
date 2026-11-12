import { Textarea } from "panelui-web/textarea";
import { useState } from "react";
import { Link } from "react-router-dom";
import { deletePolicy, getPolicy, listCollections, putPolicy, type CollectionDef, type Policy } from "../../api";
import { Sel } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonLines } from "../../components/Skeleton";
import { Button, ConfirmButton } from "../../components/ui";
import { useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { ActionError, Field } from "./shared";
import { useRun } from "./util";
import { CardBody, TableCard } from "../../components/TableCard";

type Mode = "none" | "open" | "expr";
const modeOf = (v: string | null | undefined): Mode => (v === null || v === undefined ? "none" : v.trim() === "" ? "open" : "expr");

/** One rule: `null` (locked / no rule), `""` (everyone) or an expression. `display` is the visible name when it differs from `label`. */
export function RuleInput({ label, display, hint, value, onChange, noneLabel, noneHint }: {
  label: string;
  display?: string;
  hint?: string;
  value: string | null | undefined;
  onChange: (v: string | null) => void;
  noneLabel: string;
  noneHint?: string;
}) {
  // The mode is state of its own: an expression being edited may be empty without turning into "Everyone".
  const [mode, setMode] = useState<Mode>(modeOf(value));
  return (
    <div className="grid items-start gap-x-4 gap-y-2 px-4 py-3 sm:grid-cols-[10rem_11rem_minmax(0,1fr)]">
      <div className="pt-1">
        <p className="text-sm font-medium">{display ?? label}</p>
        {hint && <p className="text-xs text-muted-foreground">{hint}</p>}
      </div>
      <Sel
        aria-label={`${label} mode`}
        value={mode}
        className="w-full"
        onChange={(v) => {
          const m = v as Mode;
          setMode(m);
          onChange(m === "none" ? null : m === "open" ? "" : (mode === "expr" ? value : null) ?? "@request.auth.id != \"\"");
        }}
        options={[{ value: "none", label: noneLabel }, { value: "open", label: "Everyone" }, { value: "expr", label: "Expression" }]}
      />
      {mode === "expr" ? (
        <Textarea aria-label={`${label} expression`} minRows={1} className="w-full font-mono text-xs" value={value ?? ""} spellCheck={false} onChange={(e) => onChange(e.target.value)} />
      ) : (
        <p className="pt-1.5 text-xs text-muted-foreground">{mode === "none" ? noneHint ?? "No rule." : "No restriction: anyone may do this."}</p>
      )}
    </div>
  );
}

const RULES = [
  ["list_rule", "List", "Find and page through records"],
  ["view_rule", "View", "Read one record by id"],
  ["create_rule", "Create", "Add a record"],
  ["update_rule", "Update", "Change a record"],
  ["delete_rule", "Delete", "Remove a record"],
] as const;

export default function PoliciesPage() {
  const cols = useLoad(listCollections, "collections");
  const [name, setName] = useState("");
  const def = cols.data?.find((c) => c.name === name);
  const policy = useLoad((s) => (name ? getPolicy(name, s) : Promise.resolve(undefined)), `policy:${name}`);
  const choices = (cols.data ?? []).filter((c) => !c.system);
  return (
    <div className="space-y-4">
      {!!cols.error && <ErrorPanel error={cols.error} onRetry={cols.reload} />}
      {cols.loading && !cols.data && !cols.error && <SkeletonLines lines={2} label="Loading collections" />}
      {cols.data && choices.length === 0 && (
        <EmptyState
          title="No collections yet"
          why="Collection rules decide who may list, view, create, update and delete the records of a collection. Create a collection in Database first."
          action={<Link to="/database/new" className="text-sm font-medium text-primary underline-offset-2 hover:underline">Create a collection</Link>}
        />
      )}
      {choices.length > 0 && (
        <div className="flex flex-wrap items-end justify-between gap-3 rounded-xl border border-border bg-card px-4 py-3">
          <Field label="Collection" className="w-64">
            <Sel value={name} onChange={setName} options={[{ value: "", label: "Choose a collection…" }, ...choices.map((c) => ({ value: c.name, label: c.name }))]} className="w-full" />
          </Field>
          <p className="max-w-lg text-xs text-muted-foreground">
            Rules use the filter syntax, such as <code className="font-mono">owner = @request.auth.id</code>. Try one in the Rule tester before you rely on it.
          </p>
        </div>
      )}
      {name && !!policy.error && <ErrorPanel error={policy.error} onRetry={policy.reload} />}
      {name && policy.loading && !policy.data && !policy.error && <SkeletonLines lines={5} label="Loading rules" />}
      {name && policy.data && def && <PolicyEditor key={`${name}:${JSON.stringify(policy.data)}`} policy={policy.data} def={def} onChanged={policy.reload} />}
      {!name && choices.length > 0 && (
        <EmptyState compact title="Pick a collection" why="Its rules for listing, viewing, creating, updating and deleting records, and per-field rules, show here." />
      )}
    </div>
  );
}

const bodyOf = (p: Policy) => {
  const out: Policy["fields"] = {};
  for (const [k, v] of Object.entries(p.fields)) if (v.read != null || v.write != null) out[k] = { read: v.read ?? null, write: v.write ?? null };
  return { list_rule: p.list_rule, view_rule: p.view_rule, create_rule: p.create_rule, update_rule: p.update_rule, delete_rule: p.delete_rule, fields: out };
};

function PolicyEditor({ policy, def, onChanged }: { policy: Policy; def: CollectionDef; onChanged: () => void }) {
  const [p, setP] = useState<Policy>(policy);
  const [base, setBase] = useState(() => JSON.stringify(bodyOf(policy)));
  const [saved, setSaved] = useState(false);
  const { run, error, busy } = useRun();
  const fields = def.fields.filter((f) => !f.system);
  const dirty = JSON.stringify(bodyOf(p)) !== base;
  const setField = (f: string, k: "read" | "write", v: string | null) => setP({ ...p, fields: { ...p.fields, [f]: { ...p.fields[f], [k]: v } } });
  const save = () => {
    const body = bodyOf(p);
    void run(async () => {
      await putPolicy(def.name, body);
      setBase(JSON.stringify(body));
      setSaved(true);
      notify.saved(`Rules of ${def.name}`);
    });
  };
  return (
    <div className="space-y-4">
      <TableCard title={`Rules of ${def.name}`} description="Locked: only administrators and service keys. Everyone: no restriction. An expression is checked on every request.">
        <div className="divide-y divide-border">
          {RULES.map(([k, label, hint]) => (
            <RuleInput key={k} label={label} hint={hint} noneLabel="Locked" noneHint="Locked: administrators and service keys only." value={p[k]} onChange={(v) => setP({ ...p, [k]: v })} />
          ))}
        </div>
      </TableCard>
      {fields.length > 0 && (
        <TableCard title="Field rules" description="Optional. Hide a field from readers or stop writers from setting it, on top of the record rules.">
          <div className="divide-y divide-border">
            {fields.map((f) => (
              <div key={f.name}>
                <p className="bg-muted/30 px-4 py-1.5 font-mono text-xs font-medium">{f.name}</p>
                <div className="divide-y divide-border">
                  <RuleInput label={`${f.name} read`} display="Read" hint="Who sees this field" noneLabel="No rule" noneHint="No extra rule: the record rules decide." value={p.fields[f.name]?.read} onChange={(v) => setField(f.name, "read", v)} />
                  <RuleInput label={`${f.name} write`} display="Write" hint="Who may set this field" noneLabel="No rule" noneHint="No extra rule: the record rules decide." value={p.fields[f.name]?.write} onChange={(v) => setField(f.name, "write", v)} />
                </div>
              </div>
            ))}
          </div>
        </TableCard>
      )}
      {fields.length === 0 && <CardBody className="rounded-xl border border-dashed border-border text-xs text-muted-foreground">This collection has no fields of its own, so there are no field rules.</CardBody>}
      <div className="sticky bottom-0 z-10 flex flex-wrap items-center justify-between gap-3 rounded-xl border border-border bg-card/95 px-4 py-3 shadow-sm backdrop-blur">
        <p className="text-sm">
          {dirty ? <span className="font-medium text-warning">Unsaved changes</span> : saved && !error ? <span role="status" className="text-success">Saved</span> : <span className="text-muted-foreground">No changes</span>}
        </p>
        <div className="flex items-center gap-2">
          <ConfirmButton label="Lock all rules" consequence={`Every rule of ${def.name} is removed: only administrators and service keys can reach its records until you add rules again.`} onConfirm={() => void run(async () => { await deletePolicy(def.name); onChanged(); })} />
          <Button tone="primary" disabled={busy} onClick={save}>{busy ? "Saving…" : "Save rules"}</Button>
        </div>
      </div>
      <ActionError error={error} />
    </div>
  );
}

/**
 * ResourceRules: who may do what with one engine resource (a key-value namespace, a vector collection, a graph label,
 * a bucket). One panel for every studio that has such resources (FB-134 rules; FB-138 to FB-140 apply them).
 *
 * Usage:
 *   <ResourceRules kind="kv_namespace" name={ns} noun="namespace" />
 *
 * Each operation is Locked (nobody but an administrator), Everyone, or an expression. Expressions read `@request.*`
 * (the caller) and, for kinds with item fields, the item's fields. The name `*` holds the rules of every resource of
 * the kind that has none of its own. Only super-administrators can read or change rules; others see the server's
 * "no permission" panel.
 */
import { useState } from "react";
import { Input } from "panelui-web/input";
import { get, send } from "../../api/client";
import { useLoad } from "../../hooks";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { notify } from "../../toaster";

export type RuleKind = "kv_namespace" | "vector_collection" | "graph_label" | "graph_edge_type" | "bucket";
export type RuleOp = "list" | "view" | "create" | "update" | "delete";
export type RuleSet = Record<string, string | null>;

export interface RuleKindInfo {
  kind: string;
  fields: { name: string; type: string }[];
  operations: string[];
  actions: string[];
  items: { name: string; rules: RuleSet }[];
}

const R = "/api/v1/policies/resources";
const enc = encodeURIComponent;
export const getRuleKind = (kind: string, signal?: AbortSignal) => get<RuleKindInfo>(`${R}/${enc(kind)}`, signal);
export const putResourceRules = (kind: string, name: string, rules: RuleSet) => send<unknown>("PUT", `${R}/${enc(kind)}/${enc(name)}`, { rules });
export const deleteResourceRules = (kind: string, name: string) => send<void>("DELETE", `${R}/${enc(kind)}/${enc(name)}`);

const OP_TEXT: Record<RuleOp, string> = {
  list: "List (scan or search)",
  view: "View (read one)",
  create: "Create",
  update: "Update",
  delete: "Delete",
};

type Mode = "locked" | "everyone" | "expr";
interface Row { mode: Mode; expr: string }

/** The stored `read` and `write` aliases cover several operations; an explicit operation wins. */
export function expand(stored: RuleSet): Record<RuleOp, string | null | undefined> {
  const out: Record<RuleOp, string | null | undefined> = { list: undefined, view: undefined, create: undefined, update: undefined, delete: undefined };
  const groups: Record<string, RuleOp[]> = { read: ["list", "view"], write: ["create", "update", "delete"] };
  for (const [k, ops] of Object.entries(groups)) if (k in stored) for (const o of ops) out[o] = stored[k];
  for (const o of Object.keys(out) as RuleOp[]) if (o in stored) out[o] = stored[o];
  return out;
}

const toRow = (v: string | null | undefined): Row => (v === undefined || v === null ? { mode: "locked", expr: "" } : v === "" ? { mode: "everyone", expr: "" } : { mode: "expr", expr: v });

/** The body for PUT: only operations that are not locked (a locked operation has no rule). */
export function toRules(rows: Partial<Record<RuleOp, Row>>): RuleSet {
  const out: RuleSet = {};
  for (const [op, r] of Object.entries(rows) as [RuleOp, Row][]) {
    if (r.mode === "everyone") out[op] = "";
    else if (r.mode === "expr") out[op] = r.expr.trim();
  }
  return out;
}

export function ResourceRules({ kind, name, noun }: { kind: RuleKind; name: string; noun: string }) {
  const info = useLoad((s) => getRuleKind(kind, s), `rules:${kind}`);
  if (info.error) return <Panel title="Access rules"><ErrorView error={info.error} onRetry={info.reload} /></Panel>;
  if (!info.data) return <Panel title="Access rules"><Loading what="Loading rules" /></Panel>;
  const own = info.data.items.find((i) => i.name === name);
  const fallback = info.data.items.find((i) => i.name === "*");
  return <Editor key={`${kind}/${name}`} kind={kind} name={name} noun={noun} info={info.data} stored={own?.rules} fallback={fallback?.rules} onSaved={info.reload} />;
}

function Editor({ kind, name, noun, info, stored, fallback, onSaved }: { kind: RuleKind; name: string; noun: string; info: RuleKindInfo; stored?: RuleSet; fallback?: RuleSet; onSaved: () => void }) {
  const ops = (["list", "view", "create", "update", "delete"] as RuleOp[]).filter((o) => info.operations.includes(o));
  const start = expand(stored ?? {});
  const [rows, setRows] = useState<Partial<Record<RuleOp, Row>>>(() => Object.fromEntries(ops.map((o) => [o, toRow(start[o])])));
  const [problem, setProblem] = useState<unknown>();
  const [busy, setBusy] = useState(false);
  const patch = (op: RuleOp, p: Partial<Row>) => setRows((r) => ({ ...r, [op]: { ...r[op]!, ...p } }));
  const rules = toRules(rows);
  const blank = ops.find((o) => rows[o]?.mode === "expr" && !rows[o]!.expr.trim());

  async function save() {
    setBusy(true);
    setProblem(undefined);
    try {
      if (Object.keys(rules).length === 0) await deleteResourceRules(kind, name);
      else await putResourceRules(kind, name, rules);
      notify.saved("Access rules");
      onSaved();
    } catch (e) {
      setProblem(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <Panel title="Access rules">
      <div className="space-y-3 text-sm">
        <p className="text-muted-foreground">
          Rules say who may use this {noun} besides administrators. A locked operation is refused for everyone else.
          {!stored && fallback && <> This {noun} has no rules of its own, so the shared rules for every {noun} (name <code className="font-mono">*</code>) apply.</>}
          {!stored && !fallback && <> This {noun} has no rules yet, so everything is locked.</>}
        </p>
        <ul className="space-y-2" aria-label={`Rules for ${name}`}>
          {ops.map((op) => {
            const row = rows[op]!;
            return (
              <li key={op} className="flex flex-wrap items-center gap-2">
                <span className="w-44 shrink-0">{OP_TEXT[op]}</span>
                <Sel
                  aria-label={`${OP_TEXT[op]} rule`}
                  value={row.mode}
                  onChange={(m) => patch(op, { mode: m as Mode, expr: m === "expr" && !row.expr ? '@request.auth.id != ""' : row.expr })}
                  options={[{ value: "locked", label: "Locked" }, { value: "everyone", label: "Everyone" }, { value: "expr", label: "Expression" }]}
                />
                {row.mode === "expr" && (
                  <Input size="sm" aria-label={`${OP_TEXT[op]} expression`} className="min-w-64 flex-1 font-mono text-xs" value={row.expr} onChange={(e) => patch(op, { expr: e.target.value })} />
                )}
              </li>
            );
          })}
        </ul>
        <details className="rounded-md border border-border p-2 text-xs">
          <summary className="cursor-pointer font-medium">What can an expression use?</summary>
          <div className="mt-2 space-y-1 text-muted-foreground">
            <p>
              <code className="font-mono">@request.auth.id</code> is the caller&apos;s id, empty when signed out. <code className="font-mono">@request.auth.type</code> is the kind of caller, such as <code className="font-mono">admin</code>. Join tests with <code className="font-mono">&amp;&amp;</code> and <code className="font-mono">||</code>.
            </p>
            <p>Example: <code className="font-mono">@request.auth.id != &quot;&quot;</code> lets every signed-in caller through.</p>
            {info.fields.length > 0 ? (
              <p>
                An expression may also test the item&apos;s fields: {info.fields.map((f, i) => <span key={f.name}>{i > 0 && ", "}<code className="font-mono">{f.name}</code> ({f.type})</span>)}. These narrow what the caller sees and may change instead of refusing the whole call.
              </p>
            ) : (
              <p>This kind has no item fields, so expressions can only test the caller with <code className="font-mono">@request</code>.</p>
            )}
            <p>Administrators always pass. A rule that no longer parses locks its operation.</p>
          </div>
        </details>
        <ErrorView error={problem} />
        <div className="flex items-center gap-2">
          <Button tone="primary" disabled={busy || !!blank} onClick={() => void save()}>{busy ? "Saving…" : "Save rules"}</Button>
          {blank && <span className="text-xs text-muted-foreground">Write an expression for “{OP_TEXT[blank]}” or choose Locked.</span>}
        </div>
      </div>
    </Panel>
  );
}

export default ResourceRules;

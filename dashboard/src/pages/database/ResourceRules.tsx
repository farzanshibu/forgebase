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
import { useLoad } from "../../hooks";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { Sel } from "../../components/controls";
import { notify } from "../../toaster";
import { OP_TEXT, deleteResourceRules, expand, getRuleKind, putResourceRules, toRow, toRules, type Mode, type Row, type RuleKind, type RuleKindInfo, type RuleOp, type RuleSet } from "./rules";

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

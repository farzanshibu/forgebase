import { useState } from "react";
import { Table } from "panelui-web/table";
import { deleteRule, listRules, patchRule } from "../../api";
import type { AlertRule, RuleSource } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { EmptyState } from "../../components/EmptyState";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Toggle } from "../../components/controls";
import { Button, ErrorView } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import { notify, toaster } from "../../toaster";
import RuleSheet from "./RuleSheet";
import { SeverityBadge } from "./shared";

const SOURCE_WORDS: Record<RuleSource, { label: string; title: string }> = {
  builtin: { label: "built-in", title: "Part of the rule pack that ships with ForgeBase. You can switch it off, not change it." },
  config: { label: "config file", title: "Declared in [[alerts.rules]] in forgebase.toml. Change it there; you can switch it off here." },
  api: { label: "custom", title: "Created here or through the API. You can change and delete it." },
  slo: { label: "SLO", title: "A burn-rate rule of a service level objective. Change the SLO to change it." },
};

/** Every rule with an enable switch; custom rules can be created, changed and deleted. */
export default function Rules() {
  const [creating, setCreating] = useCreateParam();
  const list = useLoad(listRules, "alert-rules");
  const [editing, setEditing] = useState<AlertRule>();
  const [pending, setPending] = useState<string>();
  const [error, setError] = useState<unknown>();

  async function toggle(r: AlertRule, enabled: boolean) {
    setPending(r.id);
    setError(undefined);
    try {
      await patchRule(r.id, { enabled });
      toaster.add({ title: `Rule ${r.name} ${enabled ? "enabled" : "disabled"}`, type: "success" });
      list.reload();
    } catch (e) {
      setError(e);
    } finally {
      setPending(undefined);
    }
  }

  const rules = list.data ?? [];
  return (
    <>
      <ErrorView error={error} />
      <TableCard title="Rules" count={list.data?.length} description="Built-in rules can be switched off. Create your own for anything on /metrics." actions={<Button onClick={list.reload}>Refresh</Button>}>
        <CardBody><ErrorView error={list.error} onRetry={list.reload} /></CardBody>
        {list.loading && !list.data && <CardBody><SkeletonTable rows={4} columns={6} label="Loading rules" /></CardBody>}
        {list.data && rules.length === 0 && <CardBody><EmptyState compact title="No rules" why="Every built-in rule is switched off in the config file, and none of your own exists yet." action={<Button tone="primary" onClick={() => setCreating(true)}>New rule</Button>} /></CardBody>}
        {rules.length > 0 && (
          <Table framed density="compact" aria-label="Alert rules">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head>Rule</Table.Head><Table.Head>Source</Table.Head><Table.Head>Condition</Table.Head><Table.Head>Severity</Table.Head><Table.Head>Enabled</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {rules.map((r) => (
                <Table.Row key={r.id}>
                  <Table.Cell className="max-w-96 py-2">
                    <span className="font-medium">{r.name}</span>
                    <code className="block truncate text-xs text-muted-foreground" title={r.expr}>{r.expr}</code>
                  </Table.Cell>
                  <Table.Cell><Badge title={SOURCE_WORDS[r.source].title}>{SOURCE_WORDS[r.source].label}</Badge></Table.Cell>
                  <Table.Cell className="whitespace-nowrap font-mono text-xs">{r.op} {r.threshold}{r.for_ms > 0 ? ` for ${r.for}` : ""}</Table.Cell>
                  <Table.Cell><SeverityBadge severity={r.severity} /></Table.Cell>
                  <Table.Cell>
                    <span className="flex items-center gap-2">
                      <Toggle aria-label={`Enable ${r.name}`} checked={r.enabled} disabled={r.disabled_by_config || pending === r.id} pending={pending === r.id} onChange={(c) => void toggle(r, c)} />
                      {r.disabled_by_config && <Badge tone="neutral" title="Listed in [alerts] disabled_rules: only the config file can switch it on.">off in config</Badge>}
                    </span>
                  </Table.Cell>
                  <Table.Cell className="text-right">
                    {r.source === "api" && (
                      <span className="inline-flex gap-2">
                        <Button aria-label={`Edit ${r.name}`} onClick={() => setEditing(r)}>Edit</Button>
                        <ConfirmDialog
                          trigger={<Button tone="danger" aria-label={`Delete ${r.name}`}>Delete</Button>}
                          title={`Delete rule ${r.name}?`}
                          consequence="The rule stops being evaluated and its open alerts resolve. Alert history is kept."
                          confirmLabel="Delete rule"
                          onConfirm={async () => { await deleteRule(r.id); notify.deleted(`Rule ${r.name}`); list.reload(); }}
                        />
                      </span>
                    )}
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      {creating && <RuleSheet open onOpenChange={setCreating} onSaved={list.reload} />}
      {editing && <RuleSheet key={editing.id} open rule={editing} onOpenChange={(o) => !o && setEditing(undefined)} onSaved={list.reload} />}
    </>
  );
}

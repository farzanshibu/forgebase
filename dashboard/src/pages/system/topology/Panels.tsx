/** Summary strip, operations list, tenants table with the move dialog, and the rebalance plan (ADR-0029 §5). */
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { useEffect, useState } from "react";
import { Link } from "react-router-dom";
import {
  cancelOperation, isFinished, isNotImplemented, listPlacements, moveTenant, planRebalance, runRebalance,
  type Placement, type RebalancePlan, type TopologyOperation, type TopologySet, type TopologyView,
} from "../../../api";
import { Sel } from "../../../components/controls";
import { EmptyState } from "../../../components/EmptyState";
import { ErrorPanel } from "../../../components/ErrorPanel";
import { HelpTip } from "../../../components/HelpTip";
import { Pager } from "../../../components/Pager";
import { SkeletonTable } from "../../../components/Skeleton";
import { Button, Panel } from "../../../components/ui";
import { formatBytes } from "../../../format";
import { useCursorList } from "../../../hooks";
import { toaster } from "../../../toaster";
import { Chip, ActionDialog } from "./ui";
import { StatGrid, TH, type StatItem } from "../../jobs/ops";
import { formatLag, freezeText, newestFirst, operationLabel, setName, statusInfo } from "./util";

const done = (title: string, description?: string) => toaster.add({ title, description, type: "success" });

/** Backend and engine, servers healthy, highest lag, shards, tenants, last operation. */
export function SummaryStrip({ view, operations }: { view: TopologyView; operations: TopologyOperation[] }) {
  const nodes = view.nodes;
  const healthy = nodes.filter((n) => n.state === "ready" || n.state === "streaming").length;
  const lags = nodes.filter((n) => n.role === "replica" && typeof n.lag_ms === "number").map((n) => n.lag_ms as number);
  const last = newestFirst(operations)[0];
  const shards = view.sets.filter((s) => s.kind === "shard").length;
  const tenants = view.sets.reduce((a, s) => a + s.tenants, 0);
  const lastTone = last ? statusInfo(last.status).tone : undefined;
  const items: StatItem[] = [
    { label: "Backend", value: `${view.backend === "postgres" ? "PostgreSQL" : "SQLite"}${view.engine_version ? ` ${view.engine_version}` : ""}` },
    { label: "Servers healthy", value: `${healthy} of ${nodes.length}`, tone: healthy < nodes.length ? "warn" : "ok" },
    { label: "Highest replica lag", value: lags.length ? formatLag(Math.max(...lags)) : "no replicas" },
    { label: "Shards", value: String(shards) },
    { label: "Tenants", value: String(tenants) },
    { label: "Last operation", value: last ? `${operationLabel(last.kind)}: ${statusInfo(last.status).label}` : "none yet", tone: lastTone === "bad" ? "bad" : undefined, title: last ? `${operationLabel(last.kind)}: ${statusInfo(last.status).label}` : undefined },
  ];
  return <StatGrid label="Summary" items={items} />;
}

/** One operation: stage, progress bar, message, cancel while it is allowed, and a link to its job. */
function OperationRow({ op, onChanged }: { op: TopologyOperation; onChanged: () => void }) {
  const [cancelling, setCancelling] = useState(false);
  const st = statusInfo(op.status);
  const finished = isFinished(op);
  const pct = typeof op.progress?.percent === "number" ? Math.max(0, Math.min(100, Math.round(op.progress.percent))) : undefined;
  const target = [op.node, op.set].filter(Boolean).join(" in ");
  return (
    <li className="space-y-1.5 py-3" data-operation={op.id}>
      <div className="flex flex-wrap items-center gap-2">
        <span className="font-medium">{operationLabel(op.kind)}</span>
        {target && <span className="text-xs text-muted-foreground">{target}</span>}
        <Chip tone={st.tone}>{st.label}</Chip>
        {op.cancel_requested && !finished && <span className="text-xs text-muted-foreground">Cancelling at the next safe point…</span>}
        <span className="ml-auto flex items-center gap-2">
          <Link to={`/jobs/j/${op.id}`} className="text-xs text-primary hover:underline">Job<span className="sr-only"> of {operationLabel(op.kind)}</span></Link>
          {!finished && op.cancellable && !op.cancel_requested && <Button aria-label={`Cancel ${operationLabel(op.kind)}`} onClick={() => setCancelling(true)}>Cancel</Button>}
        </span>
      </div>
      {!finished && (
        <div className="space-y-1">
          <div role="progressbar" aria-label={`${operationLabel(op.kind)} progress`} aria-valuemin={0} aria-valuemax={100} aria-valuenow={pct} aria-valuetext={pct === undefined ? "working" : `${pct} percent`} className="h-2 overflow-hidden rounded-full bg-muted">
            <div className={`h-full rounded-full bg-primary ${pct === undefined ? "w-1/3 animate-pulse" : ""}`} style={pct === undefined ? undefined : { width: `${pct}%` }} />
          </div>
          <p className="text-xs text-muted-foreground">{[op.progress?.stage, pct === undefined ? undefined : `${pct}%`, op.progress?.message].filter(Boolean).join(" · ") || "Waiting to start"}</p>
        </div>
      )}
      {op.status === "failed" && op.last_error && <p className="break-words text-xs text-destructive">{op.last_error}</p>}
      {cancelling && (
        <ActionDialog
          title={`Cancel ${operationLabel(op.kind).toLowerCase()}?`}
          consequence="ForgeBase stops at the next safe point and leaves everything as it was before. You can start it again later."
          confirmLabel="Cancel operation"
          tone="primary"
          onConfirm={async () => { await cancelOperation(op.id); done("Cancelling"); onChanged(); }}
          onClose={() => setCancelling(false)}
        />
      )}
    </li>
  );
}

export function OperationsPanel({ operations, error, loading, onChanged }: { operations: TopologyOperation[] | undefined; error: unknown; loading: boolean; onChanged: () => void }) {
  const sorted = newestFirst(operations ?? []);
  const active = sorted.filter((o) => !isFinished(o));
  const shown = [...active, ...sorted.filter((o) => isFinished(o)).slice(0, 5)];
  return (
    <Panel title="Operations">
      <ErrorPanel error={error} onRetry={onChanged} />
      {loading && !operations && !error && <SkeletonTable rows={2} columns={3} label="Loading operations" />}
      {operations && shown.length === 0 && <EmptyState compact title="No operations yet" why="Adding a replica or a shard, promoting and rebuilding run in the background. They show up here with their progress." />}
      {shown.length > 0 && <ul aria-label="Operations" className="divide-y divide-border text-sm">{shown.map((o) => <OperationRow key={o.id} op={o} onChanged={onChanged} />)}</ul>}
    </Panel>
  );
}

const PLACEMENT_STATE: Record<string, string> = { active: "Active", moving: "Moving", frozen: "Frozen for the switch" };

function MoveDialog({ placement, sets, onClose, onChanged }: { placement: Placement; sets: TopologySet[]; onClose: () => void; onChanged: () => void }) {
  const targets = sets.filter((s) => s.id !== placement.set && s.hosts_tenants && s.state === "active");
  const [to, setTo] = useState(targets[0]?.id ?? "");
  const [unavailable, setUnavailable] = useState(false);
  return (
    <ActionDialog
      title={`Move ${placement.tenant}?`}
      consequence={<>ForgeBase copies this tenant's data ({formatBytes(placement.est_bytes)}) to the other set while it keeps working, then switches over. Its <HelpTip term="freeze">writes pause</HelpTip> for {freezeText(null)} at the switch; reads and other tenants carry on. If anything fails, it stays where it is.</>}
      confirmLabel="Move tenant"
      confirmText={placement.tenant}
      tone="primary"
      canConfirm={Boolean(to)}
      onConfirm={async () => {
        try {
          await moveTenant(placement.tenant, to, placement.tenant);
        } catch (e) {
          if (isNotImplemented(e)) { setUnavailable(true); throw e; }
          throw e;
        }
        done(`Moving ${placement.tenant}`, "Follow it in Operations.");
        onChanged();
      }}
      onClose={onClose}
    >
      <p>From <strong>{setName(sets, placement.set)}</strong> to:</p>
      <Sel aria-label="Move to set" size="md" className="w-full" value={to} onChange={setTo} options={targets.map((s) => ({ value: s.id, label: s.name }))} placeholder="Pick a set" />
      {targets.length === 0 && <p className="text-xs text-muted-foreground">No other set takes new tenants. Add a shard first, or let one take tenants again.</p>}
      {unavailable && <p role="note" className="text-xs text-muted-foreground">Coming in this release: moving tenants is not switched on in this build yet. Nothing was changed.</p>}
    </ActionDialog>
  );
}

/** Tenants with their set, estimated size and state; filter by set and project, search, "Load more", and "Move to…". */
export function PlacementsPanel({ sets, onChanged }: { sets: TopologySet[]; onChanged: () => void }) {
  const [set, setSet] = useState("");
  const [project, setProject] = useState("");
  const [search, setSearch] = useState("");
  const [q, setQ] = useState("");
  const [moving, setMoving] = useState<Placement>();
  useEffect(() => {
    const t = setTimeout(() => setQ(search.trim()), 300);
    return () => clearTimeout(t);
  }, [search]);
  const list = useCursorList<Placement>(
    (cursor, signal) => listPlacements({ set, project: project.trim(), q, cursor, limit: 25 }, signal).then((p) => ({ items: p.items, next: p.next_cursor })),
    `placements:${set}:${project.trim()}:${q}`,
  );
  const filtered = Boolean(set || project.trim() || q);
  return (
    <Panel title="Tenants">
      <div className="mb-3 flex flex-wrap items-end gap-3 text-sm">
        <label className="flex flex-col gap-1">Set<Sel aria-label="Filter by set" className="w-40" value={set} onChange={setSet} options={[{ value: "", label: "All sets" }, ...sets.map((s) => ({ value: s.id, label: s.name }))]} /></label>
        <label className="flex flex-col gap-1">Project<Input size="sm" aria-label="Filter by project" className="w-40" value={project} onChange={(e) => setProject(e.target.value)} /></label>
        <label className="flex flex-col gap-1">Search<Input size="sm" type="search" aria-label="Search tenants" className="w-56" placeholder="Tenant, project or organization" value={search} onChange={(e) => setSearch(e.target.value)} /></label>
      </div>
      <ErrorPanel error={list.error} onRetry={list.reload} />
      {list.loading && !list.error && <SkeletonTable rows={3} columns={6} label="Loading tenants" />}
      {!list.loading && !list.error && list.items.length === 0 && (
        <EmptyState compact title={filtered ? "No tenants match" : "No tenants placed yet"} why={filtered ? "Clear the filters or the search to see every tenant." : "Each tenant lives in one set. Tenants appear here once they are created."} />
      )}
      {list.items.length > 0 && (
        <div className="overflow-x-auto">
          <Table aria-label="Tenants" className="w-full text-left text-sm">
            <Table.Header className={TH}>
              <Table.Row><Table.Head className="py-1">Tenant</Table.Head><Table.Head>Project</Table.Head><Table.Head>Organization</Table.Head><Table.Head>Set</Table.Head><Table.Head>Size</Table.Head><Table.Head>State</Table.Head><Table.Head><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {list.items.map((p) => (
                <Table.Row key={p.tenant} className="border-t border-border" data-tenant={p.tenant}>
                  <Table.Cell className="py-1.5 font-mono text-xs">{p.tenant}</Table.Cell>
                  <Table.Cell>{p.project ?? "—"}</Table.Cell>
                  <Table.Cell>{p.organization ?? "—"}</Table.Cell>
                  <Table.Cell>{setName(sets, p.set)}</Table.Cell>
                  <Table.Cell>{formatBytes(p.est_bytes)}</Table.Cell>
                  <Table.Cell><Chip tone={p.state === "active" ? "ok" : "warn"}>{PLACEMENT_STATE[p.state] ?? p.state}</Chip></Table.Cell>
                  <Table.Cell className="text-right">{sets.length > 1 && <Button aria-label={`Move ${p.tenant} to another set`} disabled={p.state !== "active"} onClick={() => setMoving(p)}>Move to…</Button>}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </div>
      )}
      {list.items.length > 0 && <Pager mode="more" shown={list.items.length} hasMore={list.hasMore} loading={list.loadingMore} onMore={list.loadMore} noun="tenants" label="Tenants pages" />}
      {moving && <MoveDialog placement={moving} sets={sets} onClose={() => setMoving(undefined)} onChanged={() => { list.reload(); onChanged(); }} />}
    </Panel>
  );
}

/** "Plan rebalance" shows what would move; "Run plan" confirms with the plan id. */
export function RebalancePanel({ sets, onChanged }: { sets: TopologySet[]; onChanged: () => void }) {
  const [plan, setPlan] = useState<RebalancePlan>();
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const [soon, setSoon] = useState(false);
  const [running, setRunning] = useState(false);

  async function build() {
    setBusy(true);
    setError(undefined);
    setSoon(false);
    try {
      setPlan(await planRebalance());
    } catch (e) {
      if (isNotImplemented(e)) setSoon(true);
      else setError(e);
    } finally {
      setBusy(false);
    }
  }

  const totalFreeze = plan?.freeze_estimate_ms ?? null;
  return (
    <Panel title="Rebalance" actions={<Button disabled={busy} onClick={() => void build()}>{busy ? "Planning…" : "Plan rebalance"}</Button>}>
      <p className="text-sm text-muted-foreground"><HelpTip term="rebalance">Rebalancing</HelpTip> moves tenants between shards so each holds a fair share. You see the plan first; nothing moves until you run it.</p>
      {soon && <p role="status" className="mt-2 rounded-md border border-border p-3 text-sm"><strong>Coming in this release.</strong> Planning and running a rebalance is not switched on in this build yet. Nothing was changed.</p>}
      <ErrorPanel error={error} />
      {plan && plan.moves.length === 0 && <p role="status" className="mt-2 text-sm">The shards are balanced. Nothing needs to move.</p>}
      {plan && plan.moves.length > 0 && (
        <div className="mt-3 space-y-3">
          <Table aria-label="Rebalance plan" className="w-full text-left text-sm">
            <Table.Header className={TH}><Table.Row><Table.Head className="py-1">Tenant</Table.Head><Table.Head>From</Table.Head><Table.Head>To</Table.Head><Table.Head>Size</Table.Head><Table.Head>Writes pause</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {plan.moves.map((m) => (
                <Table.Row key={m.tenant} className="border-t border-border">
                  <Table.Cell className="py-1.5 font-mono text-xs">{m.tenant}</Table.Cell>
                  <Table.Cell>{setName(sets, m.from)}</Table.Cell>
                  <Table.Cell>{setName(sets, m.to)}</Table.Cell>
                  <Table.Cell>{formatBytes(m.est_bytes)}</Table.Cell>
                  <Table.Cell>{freezeText(m.freeze_estimate_ms)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
          <Button tone="primary" onClick={() => setRunning(true)}>Run plan ({plan.moves.length} move{plan.moves.length === 1 ? "" : "s"})</Button>
          {running && (
            <ActionDialog
              title={`Run this plan of ${plan.moves.length} move${plan.moves.length === 1 ? "" : "s"}?`}
              consequence={<>ForgeBase moves the tenants one after another while they keep working. Each tenant's <HelpTip term="freeze">writes pause</HelpTip> {totalFreeze === null ? "for a second or two" : `for ${freezeText(totalFreeze)}`} at its switch. A failed move leaves that tenant where it was.</>}
              confirmLabel="Run plan"
              confirmText={plan.plan_id}
              tone="primary"
              onConfirm={async () => { await runRebalance(plan.plan_id, plan.plan_id); done("Rebalancing started", "Follow it in Operations."); setPlan(undefined); onChanged(); }}
              onClose={() => setRunning(false)}
            />
          )}
        </div>
      )}
    </Panel>
  );
}

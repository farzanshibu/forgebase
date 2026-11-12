/**
 * System studio, "Database topology" tab (FB-176, ADR-0029 §5): where your data lives. The primary server, its read
 * replicas and your shards, with their health, and the controls to add, promote, rebuild, remove and move.
 * Polled every 5 s while the tab is visible; operations follow their `jobs:<id>` channel.
 */
import { ArrowDown01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Card } from "panelui-web/card";
import { Menu } from "panelui-web/menu";
import { useEffect, useMemo, useRef, useState } from "react";
import { ApiError, isFinished, type TopologyNode, type TopologySet, type TopologyView } from "../../../api";
import { EmptyState } from "../../../components/EmptyState";
import { ErrorPanel } from "../../../components/ErrorPanel";
import { PageHeader } from "../../../components/PageHeader";
import { SkeletonCards } from "../../../components/Skeleton";
import { UseFromCode } from "../../../components/UseFromCode";
import { Button, CopyButton } from "../../../components/ui";
import { serverUrl, type Snippet } from "../../../snippets";
import { AddReplicaDialog, AddShardDialog, DrainDialog, PromoteDialog, RebuildDialog, RemoveReplicaDialog, RemoveShardDialog, WeightDialog } from "./Dialogs";
import { OperationsPanel, PlacementsPanel, RebalancePanel, SummaryStrip } from "./Panels";
import { SetCard, type NodeAction, type SetAction } from "./SetCard";
import { activeIds, useJobChannels, useOperations, useTopology } from "./hooks";
import { StateChip } from "./ui";
import { Section } from "../../jobs/ops";
import { TOPOLOGY_DOCS, kindLabel, nodesOf } from "./util";

const CONVERT = "forgebase db convert --to postgres";

type Open =
  | { kind: "add_replica"; set: string }
  | { kind: "add_shard" }
  | { kind: "node"; action: NodeAction; node: TopologyNode }
  | { kind: "set"; action: SetAction; set: TopologySet };

function consistencySnippet(url: string): Snippet {
  return {
    js: `import { ForgeBase } from "@forgebase/sdk";

// Default "session": a read sees your own earlier writes, even when a replica answers it.
const fb = new ForgeBase(${JSON.stringify(url)}, { apiKey: "<YOUR_API_KEY>" });

// Read this one call from the primary:
const page = await fb.collection("orders").list({ consistency: "strong" });`,
    rust: `use forgebase_sdk::{Consistency, ForgeBase};

// Default session consistency reads your own writes through the commit tokens the client keeps.
let fb = ForgeBase::builder(${JSON.stringify(url)})
    .api_key("<YOUR_API_KEY>")
    .consistency(Consistency::Strong) // every read on the primary
    .build()?;`,
    curl: `# Read from the primary:
curl -H "X-API-Key: <YOUR_API_KEY>" \\
  -H "X-ForgeBase-Consistency: strong" \\
  ${url}/api/v1/collections/orders/records

# Or hold a read back until a replica has your write (the value comes from the X-ForgeBase-Commit header of the write):
curl -H "X-API-Key: <YOUR_API_KEY>" \\
  -H "X-ForgeBase-Read-After: <set>/<position>" \\
  ${url}/api/v1/collections/orders/records`,
  };
}

function PostgresOnly({ view }: { view?: TopologyView }) {
  return (
    <div className="space-y-4">
      <EmptyState
        title="Replicas and shards need PostgreSQL"
        why={<>This project stores its data in SQLite, one file on one server, so there is nothing to copy or split. Move to PostgreSQL to add read replicas and shards. Your data comes with you.</>}
        action={<div className="flex flex-wrap items-center justify-center gap-2"><code className="rounded-md bg-muted px-2 py-1 font-mono text-xs">{CONVERT}</code><CopyButton text={CONVERT} label="Copy command" /></div>}
        docs="docs/database.md"
      />
      {view && view.nodes.length > 0 && (
        <Card aria-label="Your server">
          <Card.Header><h3 className="font-medium">Your server</h3></Card.Header>
          <Card.Content className="text-sm">
            {view.nodes.map((n) => <p key={n.id} className="flex flex-wrap items-center gap-2"><span className="font-medium">{n.name}</span><span className="text-muted-foreground">{kindLabel(n.kind)} · SQLite{n.engine_version ? ` ${n.engine_version}` : ""}</span><StateChip state={n.state} /></p>)}
          </Card.Content>
        </Card>
      )}
    </div>
  );
}

export default function TopologyPage() {
  const topo = useTopology();
  const view = topo.data;
  const ops = useOperations((view?.operations ?? []).some((o) => !isFinished(o)));
  const [open, setOpen] = useState<Open>();
  const close = () => setOpen(undefined);

  // Refresh the topology when an operation finishes or a new one starts: the nodes change with it.
  const running = activeIds(ops.data);
  const seen = useRef<string>("");
  useEffect(() => {
    const key = running.slice().sort().join(",");
    if (seen.current !== key) {
      if (seen.current !== "") topo.refresh();
      seen.current = key;
    }
  }, [running, topo]);
  useJobChannels(running, () => ops.refresh());

  const changed = () => { topo.refresh(); ops.refresh(); };
  const sets = view?.sets ?? [];
  const sqlite = view?.backend === "sqlite" || (topo.error instanceof ApiError && topo.error.code === "postgres_required");
  const single = useMemo(() => sets.length <= 1 && (view?.nodes.length ?? 0) <= 1, [sets.length, view?.nodes.length]);

  const primary = !sqlite && view ? (
    <div className="flex items-center">
      <Button tone="primary" className="rounded-r-none" onClick={() => setOpen({ kind: "add_replica", set: "control" })}>Add replica</Button>
      <Menu>
        <Menu.Trigger render={<Button tone="primary" aria-label="More ways to add" className="-ml-px rounded-l-none border-l border-primary-foreground/30" />}>
          <HugeiconsIcon icon={ArrowDown01Icon} size={16} strokeWidth={1.8} aria-hidden="true" />
        </Menu.Trigger>
        <Menu.Content align="end">
          <Menu.Item onClick={() => setOpen({ kind: "add_shard" })}>Add shard…</Menu.Item>
        </Menu.Content>
      </Menu>
    </div>
  ) : undefined;

  const onNodeAction = (action: NodeAction, node: TopologyNode) => setOpen({ kind: "node", action, node });
  const onSetAction = (action: SetAction, set: TopologySet) => (action === "add_replica" ? setOpen({ kind: "add_replica", set: set.id }) : setOpen({ kind: "set", action, set }));

  return (
    <div className="max-w-6xl space-y-4">
      <PageHeader
        title="Database topology"
        description="Where your data lives: the primary server, its read replicas and your shards, with their health."
        docs={TOPOLOGY_DOCS}
        primary={primary}
      />
      <ErrorPanel error={sqlite ? undefined : topo.error} onRetry={topo.refresh} />
      {topo.loading && !view && !topo.error && <SkeletonCards count={2} label="Loading topology" />}

      {sqlite && !topo.loading && <PostgresOnly view={view} />}

      {view && !sqlite && (
        <>
          {view.restart_required && (
            <div role="alert" className="rounded-xl border border-warning/50 bg-warning/10 p-4 text-sm">
              <p className="font-medium">Restart the server to finish</p>
              <p className="text-muted-foreground">{view.restart_required} Until then, requests that need the control database fail. Restart ForgeBase and it continues by itself.</p>
            </div>
          )}
          <SummaryStrip view={view} operations={ops.data ?? view.operations} />
          {sets.map((s) => <SetCard key={s.id} set={s} nodes={nodesOf(view.nodes, s.id)} onNodeAction={onNodeAction} onSetAction={onSetAction} />)}
          {single && (
            <EmptyState
              title="One server, and that is fine"
              why={<>A <strong>read replica</strong> is a copy of your database on another server that answers reads and can take over if the primary fails. A <strong>shard</strong> is another server that holds some of your tenants, so one server does not have to hold them all. Add a replica when reads are slow or you want a standby; add a shard when one server is too small. Nothing changes until you add one.</>}
              action={<Button tone="primary" onClick={() => setOpen({ kind: "add_replica", set: "control" })}>Add replica</Button>}
              sample={<Button onClick={() => setOpen({ kind: "add_shard" })}>Add shard</Button>}
              docs={TOPOLOGY_DOCS}
            />
          )}
          <OperationsPanel operations={ops.data} error={ops.error} loading={ops.loading} onChanged={changed} />
          {sets.length > 1 && <PlacementsPanel sets={sets} onChanged={changed} />}
          {sets.length > 1 && <RebalancePanel sets={sets} onChanged={changed} />}
          <Section label="Reading from replicas" title="Reading from replicas in your app" description={<>By default a read sees your own earlier writes, even when a replica answers it. Ask for <code>strong</code> to read from the primary, or send the <code>X-ForgeBase-Read-After</code> header yourself.</>}>
            <UseFromCode title={null} snippet={consistencySnippet(serverUrl())} />
          </Section>
        </>
      )}

      {open?.kind === "add_replica" && <AddReplicaDialog sets={sets} initialSet={open.set} onClose={close} onChanged={changed} />}
      {open?.kind === "add_shard" && <AddShardDialog onClose={close} onChanged={changed} />}
      {open?.kind === "node" && open.action === "promote" && <PromoteDialog node={open.node} onClose={close} onChanged={changed} />}
      {open?.kind === "node" && open.action === "rebuild" && <RebuildDialog node={open.node} onClose={close} onChanged={changed} />}
      {open?.kind === "node" && open.action === "remove" && <RemoveReplicaDialog node={open.node} onClose={close} onChanged={changed} />}
      {open?.kind === "node" && open.action === "weight" && <WeightDialog target={{ kind: "node", node: open.node }} onClose={close} onChanged={changed} />}
      {open?.kind === "set" && (open.action === "drain" || open.action === "resume") && <DrainDialog set={open.set} resume={open.action === "resume"} onClose={close} onChanged={changed} />}
      {open?.kind === "set" && open.action === "remove" && <RemoveShardDialog set={open.set} onClose={close} onChanged={changed} />}
      {open?.kind === "set" && open.action === "weight" && <WeightDialog target={{ kind: "set", set: open.set }} onClose={close} onChanged={changed} />}
    </div>
  );
}

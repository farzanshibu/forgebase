/** One card per set (Control, Shard 1, …) with its servers, state chips, lag sparkline and row and card menus (ADR-0029 §5). */
import { MoreVerticalIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Card } from "panelui-web/card";
import { Menu } from "panelui-web/menu";
import { Table } from "panelui-web/table";
import type { TopologyNode, TopologySet } from "../../../api";
import { getTopologyNode } from "../../../api";
import { HelpTip } from "../../../components/HelpTip";
import { Button } from "../../../components/ui";
import { formatBytes } from "../../../format";
import { usePolling } from "../../../hooks";
import { Chip, Sparkline, StateChip } from "./ui";
import { kindLabel, lagText, roleLabel } from "./util";
import { TH } from "../../jobs/ops";

/** What a menu can ask the page to do. */
export type NodeAction = "promote" | "rebuild" | "remove" | "weight";
export type SetAction = "add_replica" | "drain" | "resume" | "weight" | "remove";

function LagCell({ node }: { node: TopologyNode }) {
  const history = usePolling((s) => getTopologyNode(node.id, s), 30000);
  if (node.role === "primary") return <span className="text-muted-foreground">—</span>;
  return (
    <div className="flex flex-col gap-0.5">
      <span>{lagText(node)}</span>
      <Sparkline samples={history.data?.lag_history ?? []} label={node.name} />
    </div>
  );
}

function NodeMenu({ node, onAction }: { node: TopologyNode; onAction: (a: NodeAction, n: TopologyNode) => void }) {
  if (node.origin === "config") return <Chip tone="neutral" title="Declared in forgebase.toml. Change it in the file and restart the server.">From forgebase.toml</Chip>;
  const replica = node.role === "replica";
  if (!replica) return null;
  return (
    <Menu>
      <Menu.Trigger render={<Button aria-label={`Actions for ${node.name}`} />}>
        <HugeiconsIcon icon={MoreVerticalIcon} size={16} strokeWidth={1.8} aria-hidden="true" />
      </Menu.Trigger>
      <Menu.Content align="end">
        <Menu.Item onClick={() => onAction("promote", node)}>Promote to primary…</Menu.Item>
        <Menu.Item onClick={() => onAction("rebuild", node)}>Rebuild</Menu.Item>
        <Menu.Item onClick={() => onAction("weight", node)}>Set read share…</Menu.Item>
        <Menu.Separator />
        <Menu.Item variant="destructive" onClick={() => onAction("remove", node)}>Remove replica…</Menu.Item>
      </Menu.Content>
    </Menu>
  );
}

function SetMenu({ set, onAction }: { set: TopologySet; onAction: (a: SetAction, s: TopologySet) => void }) {
  const shard = set.kind === "shard";
  return (
    <Menu>
      <Menu.Trigger render={<Button aria-label={`Actions for ${set.name}`} />}>
        <HugeiconsIcon icon={MoreVerticalIcon} size={16} strokeWidth={1.8} aria-hidden="true" />
        <span className="ml-1">Actions</span>
      </Menu.Trigger>
      <Menu.Content align="end">
        <Menu.Item onClick={() => onAction("add_replica", set)}>Add replica…</Menu.Item>
        {shard && (set.state === "draining" ? <Menu.Item onClick={() => onAction("resume", set)}>Take new tenants again…</Menu.Item> : <Menu.Item onClick={() => onAction("drain", set)}>Drain…</Menu.Item>)}
        {shard && <Menu.Item onClick={() => onAction("weight", set)}>Set capacity share…</Menu.Item>}
        {shard && <Menu.Separator />}
        {shard && <Menu.Item variant="destructive" onClick={() => onAction("remove", set)}>Remove shard…</Menu.Item>}
      </Menu.Content>
    </Menu>
  );
}

const SET_STATE: Record<string, { label: string; tone: "ok" | "warn" | "bad" | "neutral" }> = {
  active: { label: "Active", tone: "ok" },
  provisioning: { label: "Setting up", tone: "warn" },
  draining: { label: "Draining", tone: "warn" },
  migrating: { label: "Moving data", tone: "warn" },
  retired: { label: "Retired", tone: "neutral" },
};

export function SetCard({ set, nodes, onNodeAction, onSetAction }: {
  set: TopologySet;
  nodes: TopologyNode[];
  onNodeAction: (a: NodeAction, n: TopologyNode) => void;
  onSetAction: (a: SetAction, s: TopologySet) => void;
}) {
  const st = SET_STATE[set.state] ?? { label: set.state, tone: "neutral" as const };
  const healthy = nodes.filter((n) => n.state === "ready" || n.state === "streaming").length;
  return (
    <Card aria-label={set.name} data-set={set.id}>
      <Card.Header className="flex flex-wrap items-center justify-between gap-2">
        <div className="min-w-0">
          <h3 className="flex flex-wrap items-center gap-2 font-medium">
            {set.name}
            <Chip tone={st.tone}>{st.label}</Chip>
            {set.kind === "shard" && !set.hosts_tenants && set.state === "active" && <Chip tone="neutral" title="This shard takes no new tenants.">No new tenants</Chip>}
            {nodes.length > 0 && <Chip tone={healthy === nodes.length ? "ok" : healthy === 0 ? "bad" : "warn"} title="Servers that are ready or streaming">{healthy} of {nodes.length} servers healthy</Chip>}
          </h3>
          <p className="text-xs text-muted-foreground">
            {set.kind === "control" ? "The primary server and its replicas. Also holds the server's own data." : `Shard ${set.number}`}
            {" · "}{set.tenants} tenant{set.tenants === 1 ? "" : "s"} · about {formatBytes(set.est_bytes)}
            {set.kind === "shard" && ` · capacity share ${set.weight}`}
          </p>
        </div>
        <SetMenu set={set} onAction={onSetAction} />
      </Card.Header>
      <Card.Content className="overflow-x-auto">
        <Table aria-label={`${set.name} servers`} className="w-full text-left text-sm">
          <Table.Header className={TH}>
            <Table.Row>
              <Table.Head className="py-1">Server</Table.Head>
              <Table.Head>Where</Table.Head>
              <Table.Head>State</Table.Head>
              <Table.Head><span className="inline-flex items-center gap-1">Lag <HelpTip term="lag" /></span></Table.Head>
              <Table.Head>Disk free</Table.Head>
              <Table.Head>Engine</Table.Head>
              <Table.Head><span className="sr-only">Actions</span></Table.Head>
            </Table.Row>
          </Table.Header>
          <Table.Body>
            {nodes.map((n) => (
              <Table.Row key={n.id} className="border-t border-border align-top hover:bg-muted/40" data-node={n.name}>
                <Table.Cell className="py-2">
                  <span className="font-medium">{n.name}</span>
                  <span className="ml-2 text-xs text-muted-foreground">{roleLabel(n.role)}</span>
                  {n.role === "replica" && n.in_rotation === false && <span className="ml-2 text-xs text-muted-foreground">(not answering reads)</span>}
                  {n.last_error && <p className="mt-0.5 max-w-xs break-words text-xs text-destructive">{n.last_error}</p>}
                </Table.Cell>
                <Table.Cell className="text-muted-foreground">{kindLabel(n.kind)}</Table.Cell>
                <Table.Cell><StateChip state={n.state} /></Table.Cell>
                <Table.Cell><LagCell node={n} /></Table.Cell>
                <Table.Cell className="text-muted-foreground">{typeof n.agent?.disk_free === "number" ? formatBytes(n.agent.disk_free) : "—"}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{n.engine_version ?? "—"}</Table.Cell>
                <Table.Cell className="text-right"><NodeMenu node={n} onAction={onNodeAction} /></Table.Cell>
              </Table.Row>
            ))}
            {nodes.length === 0 && <Table.Row><Table.Cell colSpan={7} className="py-3 text-muted-foreground">No servers in this set yet.</Table.Cell></Table.Row>}
          </Table.Body>
        </Table>
      </Card.Content>
    </Card>
  );
}

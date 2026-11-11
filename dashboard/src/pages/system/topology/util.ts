/** Words and numbers for the Database topology screen (FB-176): plain names for the server's codes. */
import type { NodeKind, NodeState, TopologyNode, TopologyOperation, TopologySet } from "../../../api";
import { formatBytes } from "../../../format";

export type Tone = "ok" | "warn" | "bad" | "neutral";

export const STATE_LABEL: Record<NodeState, { label: string; tone: Tone; why: string }> = {
  streaming: { label: "Streaming", tone: "ok", why: "The replica is connected and applying changes as they happen." },
  ready: { label: "Ready", tone: "ok", why: "The server is up and serving." },
  catching_up: { label: "Catching up", tone: "warn", why: "The replica is applying changes it missed. It answers reads again once it is close enough." },
  lagging: { label: "Lagging", tone: "warn", why: "The replica is further behind than your limit, so reads skip it until it catches up." },
  unreachable: { label: "Unreachable", tone: "bad", why: "ForgeBase cannot connect to this server. Check that it is running and reachable." },
  needs_rebuild: { label: "Needs rebuild", tone: "bad", why: "This replica can no longer follow the primary. Rebuild it to copy the data again." },
  provisioning: { label: "Provisioning", tone: "neutral", why: "The server is being set up." },
  promoting: { label: "Promoting", tone: "warn", why: "This replica is becoming the primary." },
};

export const stateInfo = (s: string) => STATE_LABEL[s as NodeState] ?? { label: s.replace(/_/g, " "), tone: "neutral" as Tone, why: "" };

export const KIND_LABEL: Record<NodeKind, string> = { local: "This server", remote: "Other host", external: "External" };
export const kindLabel = (k: string) => KIND_LABEL[k as NodeKind] ?? k;

export const roleLabel = (r: string) => (r === "primary" ? "Primary" : "Replica");

/** `0.4 s`, `12 s`, `3 min`: a lag in plain time. */
export function formatLag(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "—";
  if (ms < 1000) return ms === 0 ? "0 s" : `${(ms / 1000).toFixed(1)} s`;
  const s = ms / 1000;
  if (s < 60) return `${Math.round(s)} s`;
  if (s < 3600) return `${Math.round(s / 60)} min`;
  return `${(s / 3600).toFixed(1)} h`;
}

/** The lag of a node as "1.2 s · 48 KB behind". */
export function lagText(n: Pick<TopologyNode, "lag_ms" | "lag_bytes" | "role">): string {
  if (n.role === "primary") return "—";
  if (n.lag_ms === null || n.lag_ms === undefined) return "not measured";
  return `${formatLag(n.lag_ms)} · ${formatBytes(n.lag_bytes ?? 0)} behind`;
}

/** The set's display name: the server's, with "Control" for the control set. */
export const setName = (sets: TopologySet[], id: string) => sets.find((s) => s.id === id)?.name ?? id;

export function freezeText(ms: number | null | undefined): string {
  if (ms === null || ms === undefined) return "about a second or two";
  if (ms < 1000) return "under a second";
  return `about ${Math.round(ms / 1000)} s`;
}

export const OPERATION_LABEL: Record<string, string> = {
  "database.node.add": "Adding a replica",
  "database.node.rebuild": "Rebuilding a replica",
  "database.promote": "Promoting a replica",
  "database.shard.add": "Adding a shard",
  "database.shard.drain": "Draining a shard",
  "database.tenant.move": "Moving a tenant",
  "database.rebalance": "Rebalancing tenants",
};
export const operationLabel = (kind: string) => OPERATION_LABEL[kind] ?? kind;

export const STATUS_LABEL: Record<string, { label: string; tone: Tone }> = {
  queued: { label: "Queued", tone: "neutral" },
  pending: { label: "Queued", tone: "neutral" },
  running: { label: "Running", tone: "warn" },
  succeeded: { label: "Done", tone: "ok" },
  failed: { label: "Failed", tone: "bad" },
  dead: { label: "Failed", tone: "bad" },
  cancelled: { label: "Cancelled", tone: "neutral" },
  aborted: { label: "Stopped, nothing changed", tone: "neutral" },
};
export const statusInfo = (s: string) => STATUS_LABEL[s] ?? { label: s, tone: "neutral" as Tone };

/** The newest operation first. */
export const newestFirst = (ops: TopologyOperation[]) => [...ops].sort((a, b) => b.created_at.localeCompare(a.created_at));

/** Replicas of a set, then its primary first. */
export function nodesOf(nodes: TopologyNode[], setId: string): TopologyNode[] {
  return nodes.filter((n) => n.set === setId).sort((a, b) => (a.role === b.role ? a.name.localeCompare(b.name) : a.role === "primary" ? -1 : 1));
}

/** "a-z 0-9 - _", 1 to 32 characters, not "primary" (the server's rule for node and shard names). */
export function validName(name: string): string | undefined {
  if (!name) return "Enter a name.";
  if (name.length > 32) return "Use at most 32 characters.";
  if (!/^[a-z0-9_-]+$/.test(name)) return "Use lower-case letters, digits, dashes and underscores.";
  if (name === "primary") return "“primary” is reserved. Pick another name.";
  return undefined;
}

/** A PostgreSQL URL the server will accept at all (it checks the connection itself). */
export function validUrl(url: string): string | undefined {
  if (!url.trim()) return "Enter the server's address.";
  if (!/^postgres(ql)?:\/\/.+/.test(url.trim())) return "Start with postgres:// (for example postgres://user:password@host:5432/forgebase).";
  return undefined;
}

export const TOPOLOGY_DOCS = "docs/adr/0029-postgres-extensions-replicas-sharding.md";

import { Table } from "panelui-web/table";
import { getCapabilities } from "../../api";
import type { Capability, CapabilityState } from "../../api";
import { Chip } from "../../components/Chips";
import { EmptyState } from "../../components/EmptyState";
import { HelpTip } from "../../components/HelpTip";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonTable } from "../../components/Skeleton";
import { Badge, type Tone } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { BACKEND_NAME } from "../database/info";

const STATE_TONE: Record<CapabilityState, Tone> = { active: "ok", native: "ok", portable: "info", off: "neutral", unavailable: "bad" };
const STATE_WORDS: Record<CapabilityState, string> = {
  active: "Runs on its PostgreSQL extension.",
  portable: "Runs on ForgeBase's own engine, which works on every database and gives the same answers.",
  native: "Part of the server itself; nothing to configure.",
  off: "Switched off.",
  unavailable: "Cannot run on this server. The reason says why.",
};

/** "Accelerated" on a PostgreSQL extension, "portable" on ForgeBase's own engine, "built in" for what needs neither. */
function Runs({ c }: { c: Capability }) {
  if (c.accelerated) return <Badge tone="ok" title="Runs on a PostgreSQL extension">accelerated</Badge>;
  if (c.state === "portable") return <Badge tone="info" title="Runs on ForgeBase's own engine">portable</Badge>;
  if (c.state === "native") return <Badge tone="neutral" title="Part of the server itself">built in</Badge>;
  return <span className="text-muted-foreground">—</span>;
}

/** Limits as `name: value` chips (values can be numbers, text or lists). */
function Limits({ limits }: { limits: Record<string, unknown> }) {
  const items = Object.entries(limits);
  if (items.length === 0) return <span className="text-muted-foreground">—</span>;
  return (
    <ul aria-label="Limits" className="flex flex-wrap gap-1">
      {items.map(([k, v]) => <li key={k}><Chip mono={false}>{k.replace(/_/g, " ")}: {typeof v === "object" ? JSON.stringify(v) : String(v)}</Chip></li>)}
    </ul>
  );
}

/**
 * Which engine runs each feature on this server (`GET /api/system/capabilities`, ADR-0034): the extension and its version
 * or the portable engine, its state, the configured mode, why it is not accelerated, and the limits of the engine in use.
 */
export default function CapabilityMatrix() {
  const caps = useLoad(getCapabilities, "capabilities");
  const features = caps.data?.features ?? [];
  const counts = (s: CapabilityState) => features.filter((f) => f.state === s).length;
  return (
    <div className="max-w-6xl space-y-4">
      <PageHeader title="Capabilities" description="The engine that runs each feature on this server, and why. Portable features give the same answers as their PostgreSQL extension." docs="docs/feature-parity.md" term="accelerated" secondary={<Button onClick={caps.reload}>Refresh</Button>} />
      <TableCard
        title="Features"
        count={caps.data ? features.length : undefined}
        description={caps.data ? <>Database: <strong>{BACKEND_NAME[caps.data.backend] ?? caps.data.backend}</strong>. {counts("active")} accelerated, {counts("portable")} portable, {counts("off")} off{counts("unavailable") ? `, ${counts("unavailable")} unavailable` : ""}.</> : undefined}
      >
        <CardBody><ErrorView error={caps.error} onRetry={caps.reload} /></CardBody>
        {caps.loading && !caps.data && <CardBody><SkeletonTable rows={6} columns={6} label="Loading capabilities" /></CardBody>}
        {caps.data && features.length === 0 && <CardBody><EmptyState compact title="No capabilities reported" why="This server did not list its features. It may be older than the capability matrix." /></CardBody>}
        {features.length > 0 && (
          <Table framed density="compact" aria-label="Capability matrix">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head>Feature</Table.Head><Table.Head>Engine</Table.Head><Table.Head>Runs on <HelpTip term="accelerated" /></Table.Head><Table.Head>State</Table.Head><Table.Head>Mode</Table.Head><Table.Head>Why</Table.Head><Table.Head>Limits</Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {features.map((c) => (
                <Table.Row key={c.feature} data-feature={c.feature}>
                  <Table.Cell className="py-2"><span className="font-medium">{c.label}</span><code className="block text-xs text-muted-foreground">{c.feature}</code></Table.Cell>
                  <Table.Cell className="max-w-56 text-sm">{c.engine}</Table.Cell>
                  <Table.Cell><Runs c={c} /></Table.Cell>
                  <Table.Cell><Badge tone={STATE_TONE[c.state] ?? "default"} dot title={STATE_WORDS[c.state]}>{c.state}</Badge></Table.Cell>
                  <Table.Cell className="text-muted-foreground">{c.mode ?? "—"}{c.extension && <span className="block text-xs">{c.extension}</span>}</Table.Cell>
                  <Table.Cell className="max-w-64 text-xs text-muted-foreground">{c.reason ?? "—"}</Table.Cell>
                  <Table.Cell className="max-w-72"><Limits limits={c.limits} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <ul aria-label="What each state means" className="grid gap-2 text-xs text-muted-foreground sm:grid-cols-2">
        {(Object.keys(STATE_WORDS) as CapabilityState[]).map((s) => <li key={s}><Badge tone={STATE_TONE[s]}>{s}</Badge> {STATE_WORDS[s]}</li>)}
      </ul>
    </div>
  );
}

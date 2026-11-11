import { Link } from "react-router-dom";
import { listQueueSubscriptions } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import Tenant from "../jobs/Tenant";
import { duration } from "./util";
import { Table } from "panelui-web/table";

export default function SubscriptionList() {
  const subs = useLoad((s) => listQueueSubscriptions(undefined, s), "queue-subs");
  return (
    <Panel title="Subscriptions" actions={<Button onClick={subs.reload}>Refresh</Button>}>
      <ErrorPanel error={subs.error} onRetry={subs.reload} />
      {subs.loading && !subs.error && <SkeletonTable rows={3} columns={8} label="Loading subscriptions" />}
      {subs.data && subs.data.length === 0 && (
        <EmptyState
          title="No subscriptions yet"
          why="A subscription is one reader of a topic: it gets its own copy of every message and keeps its place. Open a topic and choose New subscription."
          docs="docs/adr/0022-queues-pubsub.md"
        />
      )}
      {subs.data && subs.data.length > 0 && (
        <Table aria-label="Subscriptions" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-1">Subscription</Table.Head><Table.Head>Topic</Table.Head><Table.Head>Owner</Table.Head><Table.Head>Tenant</Table.Head><Table.Head>Delivery</Table.Head><Table.Head>Ack deadline</Table.Head><Table.Head>Max attempts</Table.Head><Table.Head>Dead-letter topic</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {subs.data.map((s) => (
              <Table.Row key={s.name} className="border-t border-border">
                <Table.Cell className="py-1.5"><Link className="text-primary hover:underline" to={`/queues/s/${encodeURIComponent(s.name)}`}>{s.name}</Link></Table.Cell>
                <Table.Cell><Link className="hover:underline" to={`/queues/t/${encodeURIComponent(s.topic)}`}>{s.topic}</Link></Table.Cell>
                <Table.Cell className="font-mono text-xs">{s.created_by}</Table.Cell>
                <Table.Cell><Tenant value={s.tenant} /></Table.Cell>
                <Table.Cell>{s.push ? `push (${s.push.kind})` : s.ordering ? "pull, ordered" : "pull"}</Table.Cell>
                <Table.Cell>{duration(s.ack_deadline_ms)}</Table.Cell><Table.Cell>{s.max_delivery_attempts ?? "unlimited"}</Table.Cell><Table.Cell>{s.dead_letter_topic ?? "none"}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </Panel>
  );
}

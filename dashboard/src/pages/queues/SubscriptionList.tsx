import { Link } from "react-router-dom";
import { listQueueSubscriptions } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import Tenant from "../jobs/Tenant";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { NUM } from "../jobs/ops";
import { duration } from "./util";
import { Table } from "panelui-web/table";

export default function SubscriptionList() {
  const subs = useLoad((s) => listQueueSubscriptions(undefined, s), "queue-subs");
  return (
    <TableCard title="Subscriptions" count={subs.data?.length} actions={<Button onClick={subs.reload}>Refresh</Button>}>
      <CardBody className="space-y-3">
        <ErrorPanel error={subs.error} onRetry={subs.reload} />
        {subs.loading && !subs.data && !subs.error && <SkeletonTable rows={3} columns={8} label="Loading subscriptions" />}
        {subs.data && subs.data.length === 0 && (
          <EmptyState
            title="No subscriptions yet"
            why="A subscription is one reader of a topic: it gets its own copy of every message and keeps its place. Open a topic and choose New subscription."
            docs="docs/adr/0022-queues-pubsub.md"
          />
        )}
      </CardBody>
      {subs.data && subs.data.length > 0 && (
          <Table framed density="compact" aria-label="Subscriptions">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head className="py-1">Subscription</Table.Head><Table.Head>Topic</Table.Head><Table.Head>Delivery</Table.Head><Table.Head className={NUM}>Ack deadline</Table.Head><Table.Head className={NUM}>Max attempts</Table.Head><Table.Head>Dead-letter topic</Table.Head><Table.Head>Owner</Table.Head><Table.Head>Tenant</Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {subs.data.map((s) => (
                <Table.Row key={s.name}>
                  <Table.Cell className="py-2"><Link className="font-medium text-primary hover:underline" to={`/queues/s/${encodeURIComponent(s.name)}`}>{s.name}</Link></Table.Cell>
                  <Table.Cell><Link className="hover:underline" to={`/queues/t/${encodeURIComponent(s.topic)}`}>{s.topic}</Link></Table.Cell>
                  <Table.Cell className="whitespace-nowrap">{s.push ? `push (${s.push.kind})` : s.ordering ? "pull, ordered" : "pull"}</Table.Cell>
                  <Table.Cell className={NUM}>{duration(s.ack_deadline_ms)}</Table.Cell>
                  <Table.Cell className={`${NUM} ${s.max_delivery_attempts == null ? "text-muted-foreground" : ""}`}>{s.max_delivery_attempts ?? "unlimited"}</Table.Cell>
                  <Table.Cell>{s.dead_letter_topic ? <Link className="hover:underline" to={`/queues/t/${encodeURIComponent(s.dead_letter_topic)}`}>{s.dead_letter_topic}</Link> : <span className="text-muted-foreground">none</span>}</Table.Cell>
                  <Table.Cell className="max-w-40 truncate font-mono text-xs text-muted-foreground" title={s.created_by}>{s.created_by}</Table.Cell>
                  <Table.Cell><Tenant value={s.tenant} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
      )}
    </TableCard>
  );
}

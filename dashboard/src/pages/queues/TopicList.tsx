import { Link, useOutletContext } from "react-router-dom";
import { listQueueTopics } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { useLoad } from "../../hooks";
import Tenant from "../jobs/Tenant";
import { CardBody, HEAD_ROW, TableCard } from "../../components/TableCard";
import { NUM, When } from "../jobs/ops";
import { bytes, duration } from "./util";
import { Table } from "panelui-web/table";

export default function TopicList() {
  const topics = useLoad(listQueueTopics, "queue-topics");
  const { newTopic } = useOutletContext<{ newTopic: () => void }>();
  return (
    <TableCard title="Topics" count={topics.data?.length} actions={<Button onClick={topics.reload}>Refresh</Button>}>
      <CardBody className="space-y-3">
        <ErrorPanel error={topics.error} onRetry={topics.reload} />
        {topics.loading && !topics.data && !topics.error && <SkeletonTable rows={3} columns={7} label="Loading topics" />}
        {topics.data && topics.data.length === 0 && (
          <EmptyState
            title="No topics yet"
            why="A topic is a named stream of messages, like orders or signups. Your code publishes to it; each subscription receives every message and keeps its own place, so a slow reader never loses one."
            action={<Button tone="primary" onClick={newTopic}>New topic</Button>}
            docs="docs/adr/0022-queues-pubsub.md"
          />
        )}
      </CardBody>
      {topics.data && topics.data.length > 0 && (
          <Table framed density="compact" aria-label="Topics">
            <Table.Header className={HEAD_ROW}>
              <Table.Row><Table.Head className="py-1">Topic</Table.Head><Table.Head>Source</Table.Head><Table.Head>Tenant</Table.Head><Table.Head className={NUM}>Published</Table.Head><Table.Head className={NUM}>Size</Table.Head><Table.Head className={NUM}>Retention</Table.Head><Table.Head className="text-right">Created</Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {topics.data.map((t) => (
                <Table.Row key={t.name}>
                  <Table.Cell className="py-2"><Link className="font-medium text-primary hover:underline" to={`/queues/t/${encodeURIComponent(t.name)}`}>{t.name}</Link></Table.Cell>
                  <Table.Cell>
                    {t.source
                      ? <span className="rounded-md border border-info/30 bg-info/10 px-1.5 py-0.5 text-xs text-info">records of {t.source.collection}</span>
                      : <span className="text-muted-foreground">publish</span>}
                  </Table.Cell>
                  <Table.Cell><Tenant value={t.tenant} /></Table.Cell>
                  <Table.Cell className={NUM}>{t.published_total.toLocaleString()}</Table.Cell>
                  <Table.Cell className={NUM}>{bytes(t.published_bytes)}</Table.Cell>
                  <Table.Cell className={NUM}>{duration(t.retention_ms)}</Table.Cell>
                  <Table.Cell className="text-right text-muted-foreground"><When iso={t.created_at} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
      )}
    </TableCard>
  );
}

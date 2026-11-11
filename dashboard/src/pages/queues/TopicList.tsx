import { Link, useOutletContext } from "react-router-dom";
import { listQueueTopics } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import Tenant from "../jobs/Tenant";
import { bytes, duration } from "./util";
import { Table } from "panelui-web/table";

export default function TopicList() {
  const topics = useLoad(listQueueTopics, "queue-topics");
  const { newTopic } = useOutletContext<{ newTopic: () => void }>();
  return (
    <Panel title="Topics" actions={<Button onClick={topics.reload}>Refresh</Button>}>
      <ErrorPanel error={topics.error} onRetry={topics.reload} />
      {topics.loading && !topics.error && <SkeletonTable rows={3} columns={7} label="Loading topics" />}
      {topics.data && topics.data.length === 0 && (
        <EmptyState
          title="No topics yet"
          why="A topic is a named stream of messages, like orders or signups. Your code publishes to it; each subscription receives every message and keeps its own place, so a slow reader never loses one."
          action={<Button tone="primary" onClick={newTopic}>New topic</Button>}
          docs="docs/adr/0022-queues-pubsub.md"
        />
      )}
      {topics.data && topics.data.length > 0 && (
        <Table aria-label="Topics" className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
            <Table.Row><Table.Head className="py-1">Topic</Table.Head><Table.Head>Tenant</Table.Head><Table.Head>Published</Table.Head><Table.Head>Bytes</Table.Head><Table.Head>Retention</Table.Head><Table.Head>Source</Table.Head><Table.Head>Created</Table.Head></Table.Row>
          </Table.Header>
          <Table.Body>
            {topics.data.map((t) => (
              <Table.Row key={t.name} className="border-t border-border">
                <Table.Cell className="py-1.5"><Link className="text-primary hover:underline" to={`/queues/t/${encodeURIComponent(t.name)}`}>{t.name}</Link></Table.Cell>
                <Table.Cell><Tenant value={t.tenant} /></Table.Cell>
                <Table.Cell>{t.published_total}</Table.Cell><Table.Cell>{bytes(t.published_bytes)}</Table.Cell><Table.Cell>{duration(t.retention_ms)}</Table.Cell>
                <Table.Cell>{t.source ? `records of ${t.source.collection}` : "publish"}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{when(t.created_at)}</Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      )}
    </Panel>
  );
}

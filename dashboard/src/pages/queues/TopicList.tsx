import { useState } from "react";
import { Link } from "react-router-dom";
import { createQueueTopic, listQueueTopics } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { bytes, duration } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";

export default function TopicList() {
  const topics = useLoad(listQueueTopics, "queue-topics");
  const [name, setName] = useState("");
  const [days, setDays] = useState("");
  const [error, setError] = useState<unknown>();

  async function create(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    try {
      await createQueueTopic({ name: name.trim(), ...(days ? { retention_ms: Math.round(Number(days) * 86_400_000) } : {}) });
      setName("");
      setDays("");
      topics.reload();
    } catch (err) {
      setError(err);
    }
  }

  return (
    <div className="space-y-4">
      <Panel title="New topic">
        <form onSubmit={(e) => void create(e)} className="flex flex-wrap items-end gap-3 text-sm">
          <label>Name<Input size="sm" className="ml-2 w-48" value={name} onChange={(e) => setName(e.target.value)} /></label>
          <label>Retention (days, optional)<Input size="sm" className="ml-2 w-20" inputMode="decimal" value={days} onChange={(e) => setDays(e.target.value)} /></label>
          <Button type="submit" tone="primary" disabled={!name.trim()}>Create topic</Button>
        </form>
        <div className="mt-3"><ErrorView error={error} /></div>
      </Panel>
      <Panel title="Topics" actions={<Button onClick={topics.reload}>Refresh</Button>}>
        <ErrorView error={topics.error} />
        {topics.loading && !topics.error && <Loading what="Loading topics" />}
        {topics.data && topics.data.length === 0 && <p className="text-sm text-muted-foreground">No topics yet. Create one above, then add a subscription to receive its messages.</p>}
        {topics.data && topics.data.length > 0 && (
          <Table aria-label="Topics" className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground">
              <Table.Row><Table.Head className="py-1">Topic</Table.Head><Table.Head>Published</Table.Head><Table.Head>Bytes</Table.Head><Table.Head>Retention</Table.Head><Table.Head>Source</Table.Head><Table.Head>Created</Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {topics.data.map((t) => (
                <Table.Row key={t.name} className="border-t border-border">
                  <Table.Cell className="py-1.5"><Link className="text-primary hover:underline" to={`/queues/t/${encodeURIComponent(t.name)}`}>{t.name}</Link></Table.Cell>
                  <Table.Cell>{t.published_total}</Table.Cell><Table.Cell>{bytes(t.published_bytes)}</Table.Cell><Table.Cell>{duration(t.retention_ms)}</Table.Cell>
                  <Table.Cell>{t.source ? `records of ${t.source.collection}` : "publish"}</Table.Cell>
                  <Table.Cell className="text-muted-foreground">{when(t.created_at)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </Panel>
    </div>
  );
}

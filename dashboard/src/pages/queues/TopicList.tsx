import { useState } from "react";
import { Link } from "react-router-dom";
import { createQueueTopic, listQueueTopics } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";
import { bytes, duration } from "./util";

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
          <label>Name<input className={`${inputCls} ml-2 w-48`} value={name} onChange={(e) => setName(e.target.value)} /></label>
          <label>Retention (days, optional)<input className={`${inputCls} ml-2 w-20`} inputMode="decimal" value={days} onChange={(e) => setDays(e.target.value)} /></label>
          <Button type="submit" tone="primary" disabled={!name.trim()}>Create topic</Button>
        </form>
        <div className="mt-3"><ErrorView error={error} /></div>
      </Panel>
      <Panel title="Topics" actions={<Button onClick={topics.reload}>Refresh</Button>}>
        <ErrorView error={topics.error} />
        {topics.loading && !topics.error && <Loading what="Loading topics" />}
        {topics.data && topics.data.length === 0 && <p className="text-sm text-zinc-400">No topics yet. Create one above, then add a subscription to receive its messages.</p>}
        {topics.data && topics.data.length > 0 && (
          <table aria-label="Topics" className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500">
              <tr><th className="py-1">Topic</th><th>Published</th><th>Bytes</th><th>Retention</th><th>Source</th><th>Created</th></tr>
            </thead>
            <tbody>
              {topics.data.map((t) => (
                <tr key={t.name} className="border-t border-zinc-800">
                  <td className="py-1.5"><Link className="text-amber-300 hover:underline" to={`/queues/t/${encodeURIComponent(t.name)}`}>{t.name}</Link></td>
                  <td>{t.published_total}</td><td>{bytes(t.published_bytes)}</td><td>{duration(t.retention_ms)}</td>
                  <td>{t.source ? `records of ${t.source.collection}` : "publish"}</td>
                  <td className="text-zinc-400">{when(t.created_at)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </Panel>
    </div>
  );
}

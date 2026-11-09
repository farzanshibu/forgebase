import { Link } from "react-router-dom";
import { listQueueSubscriptions } from "../../api";
import { Button, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { duration } from "./util";

export default function SubscriptionList() {
  const subs = useLoad((s) => listQueueSubscriptions(undefined, s), "queue-subs");
  return (
    <Panel title="Subscriptions" actions={<Button onClick={subs.reload}>Refresh</Button>}>
      <ErrorView error={subs.error} />
      {subs.loading && !subs.error && <Loading what="Loading subscriptions" />}
      {subs.data && subs.data.length === 0 && <p className="text-sm text-zinc-400">No subscriptions yet: open a topic to add one.</p>}
      {subs.data && subs.data.length > 0 && (
        <table aria-label="Subscriptions" className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500">
            <tr><th className="py-1">Subscription</th><th>Topic</th><th>Delivery</th><th>Ack deadline</th><th>Max attempts</th><th>Dead-letter topic</th></tr>
          </thead>
          <tbody>
            {subs.data.map((s) => (
              <tr key={s.name} className="border-t border-zinc-800">
                <td className="py-1.5"><Link className="text-amber-300 hover:underline" to={`/queues/s/${encodeURIComponent(s.name)}`}>{s.name}</Link></td>
                <td><Link className="hover:underline" to={`/queues/t/${encodeURIComponent(s.topic)}`}>{s.topic}</Link></td>
                <td>{s.push ? `push (${s.push.kind})` : s.ordering ? "pull, ordered" : "pull"}</td>
                <td>{duration(s.ack_deadline_ms)}</td><td>{s.max_delivery_attempts ?? "unlimited"}</td><td>{s.dead_letter_topic ?? "none"}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </Panel>
  );
}

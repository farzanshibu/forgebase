import { listSchedules } from "../../api";
import { ErrorView, Loading } from "../../components/ui";
import { useLoad } from "../../hooks";
import { when } from "../functions/util";

export default function Schedules() {
  const list = useLoad(listSchedules, "job-schedules");
  return (
    <div className="space-y-3">
      <p className="text-sm text-zinc-400">Recurring jobs (cron, UTC).</p>
      <ErrorView error={list.error} />
      {list.loading && !list.error && <Loading what="Loading schedules" />}
      {list.data && list.data.length === 0 && <p className="text-sm text-zinc-400">No schedules.</p>}
      {list.data && list.data.length > 0 && (
        <table aria-label="Schedules" className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Name</th><th>Cron</th><th>Queue</th><th>Kind</th><th>Enabled</th><th>Next run</th><th>Last run</th></tr></thead>
          <tbody>
            {list.data.map((s) => (
              <tr key={s.id} className="border-t border-zinc-800">
                <td className="py-2">{s.name}</td><td className="font-mono text-xs">{s.cron}</td><td>{s.queue}</td><td>{s.kind}</td>
                <td>{s.enabled ? "yes" : "no"}</td><td className="text-zinc-400">{when(s.next_run_at)}</td><td className="text-zinc-400">{when(s.last_run_at)}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
    </div>
  );
}

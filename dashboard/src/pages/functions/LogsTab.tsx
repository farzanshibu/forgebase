import { useState } from "react";
import { functionLogs } from "../../api";
import { Button, ErrorView, Loading } from "../../components/ui";
import { usePolling } from "../../hooks";
import { when } from "./util";
import { Sel } from "../../components/controls";

const STEP = 50;
const MAX = 500;

export default function LogsTab({ name }: { name: string }) {
  const [limit, setLimit] = useState(STEP);
  const [live, setLive] = useState(true);
  const [status, setStatus] = useState("");
  const logs = usePolling((s) => functionLogs(name, limit, s), live ? 3000 : 3_600_000);
  // A different page size must refetch at once, not on the next tick.
  const [seen, setSeen] = useState(limit);
  if (seen !== limit) {
    setSeen(limit);
    logs.refresh();
  }
  const shown = (logs.data ?? []).filter((l) => !status || l.status === status);
  const statuses = [...new Set((logs.data ?? []).map((l) => l.status))];

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-3 text-sm">
        <label className="flex items-center gap-1.5"><input type="checkbox" checked={live} onChange={(e) => setLive(e.target.checked)} />Live (every 3 s)</label>
        <label className="flex items-center gap-1.5">Status
          <Sel aria-label="Status" value={status} onChange={setStatus} options={[{ value: "", label: "all" }, ...statuses.map((s) => ({ value: s, label: s }))]} />
        </label>
        <Button onClick={logs.refresh}>Refresh</Button>
        <span className="text-muted-foreground">{logs.data ? `${shown.length} of the newest ${limit} executions` : ""}{logs.updatedAt ? ` · updated ${logs.updatedAt.toLocaleTimeString()}` : ""}</span>
      </div>
      <ErrorView error={logs.error} />
      {logs.loading && !logs.error && <Loading what="Loading logs" />}
      {logs.data && shown.length === 0 && <p className="text-sm text-muted-foreground">No executions logged.</p>}
      <ul aria-label="Executions" className="space-y-2">
        {shown.map((l) => (
          <li key={l.id} className="rounded-md border border-border bg-card p-3 text-sm">
            <p className="flex flex-wrap gap-x-3">
              <span className={l.status === "ok" ? "text-success" : "text-destructive"}>{l.status}</span>
              <span>v{l.version}</span><span>{l.trigger}</span>
              <span>{l.duration_ms} ms</span><span>{l.fuel_used} fuel</span>
              <span className="text-muted-foreground">{when(l.created_at)}</span>
              {l.request_id && <span className="font-mono text-xs text-muted-foreground">{l.request_id}</span>}
            </p>
            {l.error && <p className="mt-1 text-destructive">{l.error}</p>}
            {l.logs.length > 0 && (
              <pre className="mt-2 overflow-x-auto rounded bg-background p-2 font-mono text-xs">{l.logs.map((x) => `[${x.level}] ${x.message}`).join("\n")}</pre>
            )}
          </li>
        ))}
      </ul>
      {logs.data && logs.data.length >= limit && limit < MAX && <Button onClick={() => setLimit(Math.min(MAX, limit + STEP))}>Load older</Button>}
    </div>
  );
}

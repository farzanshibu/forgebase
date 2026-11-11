import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { ApiError, functionLogs } from "../../api";
import type { ExecutionLog } from "../../api";
import { Button, CopyButton } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { Pager } from "../../components/Pager";
import { SkeletonLines } from "../../components/Skeleton";
import { usePolling } from "../../hooks";
import { when } from "./util";
import { Sel } from "../../components/controls";
import { Input } from "panelui-web/input";

const STEP = 50;
const MAX = 500;

/** A run's error text as an ErrorPanel error; the request id is its handle in these logs. */
const runError = (l: ExecutionLog) => new ApiError({ status: 500, code: "function_error", message: l.error ?? "The function failed", requestId: l.request_id ?? undefined });

/** One execution: status, version, timing, fuel, a copyable request id, its error (in an ErrorPanel) and its log lines. */
function Row({ l, onRequest }: { l: ExecutionLog; onRequest: (id: string) => void }) {
  return (
    <li className="rounded-md border border-border bg-card p-3 text-sm">
      <p className="flex flex-wrap items-center gap-x-3 gap-y-1">
        <span className={l.status === "ok" ? "text-success" : "text-destructive"}>{l.status}</span>
        <span>v{l.version}</span><span>{l.trigger}</span>
        <span>{l.duration_ms} ms</span><span>{l.fuel_used} fuel</span>
        <span className="text-muted-foreground">{when(l.created_at)}</span>
        {l.request_id && (
          <>
            <button type="button" className="font-mono text-xs text-primary hover:underline" aria-label={`Show only request ${l.request_id}`} onClick={() => onRequest(l.request_id!)}>{l.request_id}</button>
            <CopyButton text={l.request_id} label="Copy request id" />
          </>
        )}
      </p>
      {l.error && <div className="mt-2"><ErrorPanel error={runError(l)} /></div>}
      {l.logs.length > 0 && (
        <pre className="mt-2 overflow-x-auto rounded bg-background p-2 font-mono text-xs">{l.logs.map((x) => `[${x.level}] ${x.message}`).join("\n")}</pre>
      )}
    </li>
  );
}

export default function LogsTab({ name }: { name: string }) {
  const [params, setParams] = useSearchParams();
  const request = params.get("request") ?? "";
  const setRequest = (v: string) => setParams((p) => { const n = new URLSearchParams(p); if (v) n.set("request", v); else n.delete("request"); return n; }, { replace: true });
  const [limit, setLimit] = useState(STEP);
  const [live, setLive] = useState(true);
  const [status, setStatus] = useState("");
  const logs = usePolling((s) => functionLogs(name, request ? MAX : limit, s), live ? 3000 : 3_600_000);
  // A different page size or filter must refetch at once, not on the next tick.
  const [seen, setSeen] = useState(`${limit}:${request ? 1 : 0}`);
  const want = `${limit}:${request ? 1 : 0}`;
  if (seen !== want) {
    setSeen(want);
    logs.refresh();
  }
  const shown = (logs.data ?? []).filter((l) => (!status || l.status === status) && (!request || (l.request_id ?? "").includes(request.trim())));
  const statuses = [...new Set((logs.data ?? []).map((l) => l.status))];

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-end gap-3 text-sm">
        <label className="flex items-center gap-1.5"><input type="checkbox" checked={live} onChange={(e) => setLive(e.target.checked)} />Live (every 3 s)</label>
        <label className="flex items-center gap-1.5">Status
          <Sel aria-label="Status" value={status} onChange={setStatus} options={[{ value: "", label: "all" }, ...statuses.map((s) => ({ value: s, label: s }))]} />
        </label>
        <label className="flex items-center gap-1.5">Request id
          <Input size="sm" className="w-72 font-mono text-xs" value={request} onChange={(e) => setRequest(e.target.value)} placeholder="Paste a request id" />
        </label>
        {request && <Button onClick={() => setRequest("")}>Clear filter</Button>}
        <Button onClick={logs.refresh}>Refresh</Button>
        <span className="flex items-center gap-1 text-muted-foreground">
          Fuel <HelpTip term="fuel" />
          {logs.data ? ` · ${shown.length} of the newest ${request ? MAX : limit} executions` : ""}{logs.updatedAt ? ` · updated ${logs.updatedAt.toLocaleTimeString()}` : ""}
        </span>
      </div>
      <ErrorPanel error={logs.error} onRetry={logs.refresh} />
      {logs.loading && !logs.error && <SkeletonLines lines={5} label="Loading logs" />}
      {logs.data && shown.length === 0 && (
        <EmptyState compact
          title={request || status ? "No executions match" : "No executions logged"}
          why={request || status ? "Clear the filter to see every execution, or paste the request id from a Test run or an error." : "Run the function from the Test tab, or call it from your app: each call is logged here with its console output and any error."} />
      )}
      <ul aria-label="Executions" className="space-y-2">
        {shown.map((l) => <Row key={l.id} l={l} onRequest={setRequest} />)}
      </ul>
      {!request && logs.data && <Pager mode="more" shown={logs.data.length} hasMore={logs.data.length >= limit && limit < MAX} onMore={() => setLimit(Math.min(MAX, limit + STEP))} noun="executions" label="More executions" />}
    </div>
  );
}

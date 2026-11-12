import { useState } from "react";
import { useSearchParams } from "react-router-dom";
import { Input } from "panelui-web/input";
import { Search01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { ApiError, functionLogs } from "../../api";
import type { ExecutionLog } from "../../api";
import { Button, CopyButton } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { Pager } from "../../components/Pager";
import { SkeletonLines } from "../../components/Skeleton";
import { usePolling } from "../../hooks";
import { Sel, Toggle } from "../../components/controls";
import { LogLines, RunStatus } from "./parts";
import { count, when } from "./util";

const STEP = 50;
const MAX = 500;
const ALL = "all";

/** A run's error text as an ErrorPanel error; the request id is its handle in these logs. */
const runError = (l: ExecutionLog) => new ApiError({ status: 500, code: "function_error", message: l.error ?? "The function failed", requestId: l.request_id ?? undefined });

/** One execution: a header line (status, time, version, trigger, duration, fuel, request id), its error and its log lines. */
function Row({ l, onRequest }: { l: ExecutionLog; onRequest: (id: string) => void }) {
  return (
    <li className={`overflow-hidden rounded-xl border bg-card text-sm ${l.status === "ok" ? "border-border" : "border-destructive/40"}`}>
      <div className="flex flex-wrap items-center gap-x-4 gap-y-1.5 px-4 py-2.5">
        <RunStatus status={l.status} />
        <span className="font-mono text-xs text-muted-foreground" title="When the run started (UTC)">{when(l.created_at)}</span>
        <span className="font-mono text-xs">v{l.version}</span>
        <span className="rounded bg-muted px-1.5 py-0.5 text-xs text-muted-foreground">{l.trigger}</span>
        <span className="text-xs tabular-nums">{l.duration_ms} ms</span>
        <span className="text-xs tabular-nums text-muted-foreground">{count(l.fuel_used)} fuel</span>
        {l.request_id && (
          <span className="ml-auto flex min-w-0 items-center gap-2">
            <button type="button" className="max-w-[18rem] truncate rounded font-mono text-xs text-primary hover:underline focus-visible:outline-2 focus-visible:outline-ring" title="Show only this request" aria-label={`Show only request ${l.request_id}`} onClick={() => onRequest(l.request_id!)}>{l.request_id}</button>
            <CopyButton text={l.request_id} label="Copy request id" className="h-7 px-2 text-xs" />
          </span>
        )}
      </div>
      {(l.error || l.logs.length > 0) && (
        <div className="space-y-2 border-t border-border px-4 py-3">
          {l.error && <ErrorPanel error={runError(l)} />}
          {l.logs.length > 0 && <LogLines lines={l.logs} />}
        </div>
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
  const [status, setStatus] = useState(ALL);
  const logs = usePolling((s) => functionLogs(name, request ? MAX : limit, s), live ? 3000 : 3_600_000);
  // A different page size or filter must refetch at once, not on the next tick.
  const [seen, setSeen] = useState(`${limit}:${request ? 1 : 0}`);
  const want = `${limit}:${request ? 1 : 0}`;
  if (seen !== want) {
    setSeen(want);
    logs.refresh();
  }
  const shown = (logs.data ?? []).filter((l) => (status === ALL || l.status === status) && (!request || (l.request_id ?? "").includes(request.trim())));
  const statuses = [...new Set((logs.data ?? []).map((l) => l.status))];
  const failed = (logs.data ?? []).filter((l) => l.status !== "ok").length;

  return (
    <div className="space-y-3">
      <div className="flex flex-wrap items-center gap-2 rounded-xl border border-border bg-card px-3 py-2">
        <Toggle checked={live} onChange={setLive} label={<span className="flex items-center gap-1.5 whitespace-nowrap">Live <span className="text-xs text-muted-foreground">every 3 s</span>{live && <span aria-hidden="true" className="size-1.5 animate-pulse rounded-full bg-success motion-reduce:animate-none" />}</span>} className="mr-2 items-center" />
        <Sel aria-label="Status" className="w-36" value={status} onChange={setStatus} options={[{ value: ALL, label: "All statuses" }, ...statuses.map((s) => ({ value: s, label: s }))]} />
        <span className="relative flex min-w-[14rem] flex-1 items-center">
          <HugeiconsIcon icon={Search01Icon} size={14} strokeWidth={2} aria-hidden="true" className="pointer-events-none absolute left-2.5 text-muted-foreground" />
          <Input size="sm" aria-label="Request id" className="w-full pl-8 font-mono text-xs" value={request} onChange={(e) => setRequest(e.target.value)} placeholder="Filter by request id" />
        </span>
        {request && <Button onClick={() => setRequest("")}>Clear filter</Button>}
        <Button onClick={logs.refresh}>Refresh</Button>
      </div>
      <p className="flex flex-wrap items-center gap-x-1 text-xs text-muted-foreground">
        {logs.data ? <span className="tabular-nums">{shown.length} of the newest {request ? MAX : limit} executions{failed > 0 ? ` · ${failed} failed` : ""}</span> : null}
        {logs.updatedAt ? <span>· updated {logs.updatedAt.toLocaleTimeString()}</span> : null}
        <span className="ml-auto flex items-center gap-1">Fuel is the work a run did <HelpTip term="fuel" /></span>
      </p>
      <ErrorPanel error={logs.error} onRetry={logs.refresh} />
      {logs.loading && !logs.error && <SkeletonLines lines={5} label="Loading logs" />}
      {logs.data && shown.length === 0 && (
        <EmptyState compact
          title={request || status !== ALL ? "No executions match" : "No executions logged"}
          why={request || status !== ALL ? "Clear the filter to see every execution, or paste the request id from a Test run or an error." : "Run the function from the Test tab, or call it from your app: each call is logged here with its console output and any error."} />
      )}
      <ul aria-label="Executions" className="space-y-2">
        {shown.map((l) => <Row key={l.id} l={l} onRequest={setRequest} />)}
      </ul>
      {!request && logs.data && <Pager mode="more" shown={logs.data.length} hasMore={logs.data.length >= limit && limit < MAX} onMore={() => setLimit(Math.min(MAX, limit + STEP))} noun="executions" label="More executions" />}
    </div>
  );
}

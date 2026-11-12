import { useState } from "react";
import { Link, useSearchParams } from "react-router-dom";
import { Input } from "panelui-web/input";
import { functionLogs, listFunctions } from "../../api";
import type { ExecutionLog } from "../../api";
import { Button, CopyButton } from "../../components/ui";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { PageHeader } from "../../components/PageHeader";
import { SkeletonLines } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { RunStatus } from "./parts";
import { when } from "./util";

const NEWEST = 500;

interface Match { fn: string; log: ExecutionLog }

/** Every function's newest executions whose request id contains `id` (the log API has no request-id filter, so this reads each function's newest 500). */
async function find(id: string, signal: AbortSignal): Promise<Match[]> {
  const fns = await listFunctions(signal);
  const per = await Promise.all(fns.map(async (f) => (await functionLogs(f.name, NEWEST, signal).catch(() => [] as ExecutionLog[])).filter((l) => (l.request_id ?? "").includes(id)).map((log) => ({ fn: f.name, log }))));
  return per.flat();
}

/**
 * Find in logs: the page behind ErrorPanel's "Find in logs" link (`/functions/logs?request=<id>`). Paste a request id from
 * an error and see which function run carried it, with a link to that function's logs filtered to it.
 */
export default function RequestSearch() {
  const [params, setParams] = useSearchParams();
  const id = (params.get("request") ?? "").trim();
  const [draft, setDraft] = useState(id);
  const result = useLoad((s) => (id ? find(id, s) : Promise.resolve([] as Match[])), id);
  return (
    <div className="space-y-4">
      <PageHeader title="Find a request in the logs" term="request_id" description="Paste a request id from an error to see the function run that carried it." docs={null} />
      <form
        aria-label="Find a request"
        className="flex max-w-2xl items-end gap-2 text-sm"
        onSubmit={(e) => { e.preventDefault(); const v = draft.trim(); setParams(v ? { request: v } : {}, { replace: true }); }}
      >
        <label className="flex flex-1 flex-col gap-1 font-medium">
          Request id
          <Input size="sm" className="w-full font-mono text-xs" value={draft} placeholder="Paste a request id" onChange={(e) => setDraft(e.target.value)} />
        </label>
        <Button type="submit" tone="primary">Search</Button>
      </form>
      <ErrorPanel error={result.error} onRetry={result.reload} />
      {id && result.loading && !result.error && <SkeletonLines lines={3} label="Searching the logs" />}
      {id && result.data && result.data.length === 0 && (
        <EmptyState
          compact
          title="No function run has this request id"
          why="Only function runs are logged by request id here, and only the newest 500 runs of each function. A request to the database, storage or another studio is in the server log: search it for this id."
        />
      )}
      <ul aria-label="Matching executions" className="space-y-2">
        {(result.data ?? []).map(({ fn, log }) => (
          <li key={log.id} className={`rounded-xl border bg-card px-4 py-3 text-sm ${log.status === "ok" ? "border-border" : "border-destructive/40"}`}>
            <p className="flex flex-wrap items-center gap-x-4 gap-y-1.5">
              <Link className="font-mono font-medium text-primary hover:underline" to={`/functions/f/${encodeURIComponent(fn)}/logs?request=${encodeURIComponent(log.request_id ?? id)}`}>{fn}</Link>
              <RunStatus status={log.status} />
              <span className="font-mono text-xs text-muted-foreground">{when(log.created_at)}</span>
              <span className="text-xs tabular-nums">{log.duration_ms} ms</span>
              {log.request_id && <span className="ml-auto flex min-w-0 items-center gap-2"><span className="truncate font-mono text-xs">{log.request_id}</span><CopyButton text={log.request_id} label="Copy request id" className="h-7 px-2 text-xs" /></span>}
            </p>
            {log.error && <p className="mt-2 rounded-lg bg-destructive/5 px-3 py-2 font-mono text-xs text-destructive">{log.error}</p>}
          </li>
        ))}
      </ul>
    </div>
  );
}

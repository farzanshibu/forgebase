import { useState } from "react";
import { Link } from "react-router-dom";
import { ApiError, functionLogs, invokeFunction } from "../../api";
import type { ExecutionLog, FunctionInfo, InvokeResult, VersionInfo } from "../../api";
import { Button, CopyButton, Loading } from "../../components/ui";
import { ErrorPanel } from "../../components/ErrorPanel";
import { FormField } from "../../components/FormField";
import { Sel } from "../../components/controls";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { HttpStatus, LogLines } from "./parts";
import { parseHeaderLines } from "./util";

const pretty = (s: string) => {
  try {
    return JSON.stringify(JSON.parse(s), null, 2);
  } catch {
    return s;
  }
};

/** An HTTP answer that is an error, as an ApiError, so ErrorPanel explains it like any other error. */
function asApiError(r: InvokeResult, id: string): ApiError | undefined {
  if (r.status < 400) return undefined;
  let code = "function_failed", message = `The function answered HTTP ${r.status}`;
  try {
    const e = (JSON.parse(r.body) as { error?: { code?: string; message?: string } }).error;
    if (e?.code) code = e.code;
    if (e?.message) message = e.message;
  } catch {
    if (r.body.trim()) message = r.body.trim().slice(0, 300);
  }
  return new ApiError({ status: r.status, code, message, requestId: r.requestId ?? id });
}

/** Test-invoke, request on the left and response on the right: method, path, query, headers and JSON body in; status, body, the run's log lines and headers out. */
export default function TestPanel({ info, versions }: { info: FunctionInfo; versions: VersionInfo[] }) {
  const [method, setMethod] = useState("POST");
  const [path, setPath] = useState("");
  const [query, setQuery] = useState("");
  const [headers, setHeaders] = useState("");
  const [body, setBody] = useState("");
  const [version, setVersion] = useState("active");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<unknown>();
  const [result, setResult] = useState<InvokeResult>();
  const [logs, setLogs] = useState<ExecutionLog[]>();

  async function run(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    setResult(undefined);
    setLogs(undefined);
    setBusy(true);
    try {
      const h = parseHeaderLines(headers);
      if (body.trim()) {
        try {
          JSON.parse(body);
        } catch {
          throw new Error("The body is not valid JSON");
        }
        if (!Object.keys(h).some((k) => k.toLowerCase() === "content-type")) h["Content-Type"] = "application/json";
      }
      // Our own request id ties this run to its log lines; the server's answer wins when it sets one.
      const sent = crypto.randomUUID();
      if (!Object.keys(h).some((k) => k.toLowerCase() === "x-request-id")) h["x-request-id"] = sent;
      const res = await invokeFunction(info.name, { method, path, query, headers: h, body: body.trim() ? body : undefined, version: version === "active" ? undefined : Number(version) });
      const id = res.requestId ?? sent;
      setResult({ ...res, requestId: id });
      try {
        const recent = await functionLogs(info.name, 50);
        setLogs(recent.filter((l) => l.request_id === id));
      } catch {
        setLogs([]);
      }
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  const lines = (logs ?? []).flatMap((l) => [...l.logs, ...(l.error ? [{ level: "error", message: l.error }] : [])]);
  const runLink = (id: string) => `/functions/f/${encodeURIComponent(info.name)}/logs?request=${encodeURIComponent(id)}`;
  return (
    <div className="grid items-start gap-4 xl:grid-cols-2">
      <section aria-labelledby="fn-test-request" className="overflow-hidden rounded-xl border border-border bg-card">
        <header className="flex items-center justify-between gap-3 border-b border-border bg-muted/40 px-4 py-2.5">
          <h2 id="fn-test-request" className="text-sm font-medium">Request</h2>
          <span className="text-xs text-muted-foreground"><kbd className="rounded border border-border bg-background px-1 font-mono">Ctrl/Cmd+Enter</kbd> runs it</span>
        </header>
        <form
          aria-label="Test run"
          onSubmit={(e) => void run(e)}
          onKeyDown={(e) => { if ((e.ctrlKey || e.metaKey) && e.key === "Enter") { e.preventDefault(); e.currentTarget.requestSubmit(); } }}
          className="grid gap-3 p-4 sm:grid-cols-[8rem_minmax(0,1fr)]"
        >
          <FormField label="Method">
            {(p) => <Sel {...p} aria-label="Method" className="w-full font-mono" value={method} onChange={setMethod} options={["GET", "POST", "PUT", "PATCH", "DELETE"]} />}
          </FormField>
          <FormField label="Path" help={`Sent to /api/v1/functions/${info.name}/invoke followed by this path.`}>
            {(p) => <Input {...p} size="sm" className="w-full font-mono" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/" />}
          </FormField>
          <FormField label="Version" className="sm:col-span-1">
            {(p) => <Sel {...p} aria-label="Version" className="w-full" value={version} onChange={setVersion}
              options={[{ value: "active", label: `Active (v${info.active_version})` }, ...versions.map((v) => ({ value: String(v.version), label: `v${v.version}` }))]} />}
          </FormField>
          <FormField label="Query">
            {(p) => <Input {...p} size="sm" className="w-full font-mono" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="name=Ada&x=1" />}
          </FormField>
          <FormField label="Headers" help="One Name: value per line." className="sm:col-span-2">
            {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-20 w-full font-mono text-xs" value={headers} onChange={(e) => setHeaders(e.target.value)} placeholder="X-Tenant: acme" />}
          </FormField>
          <FormField label="JSON body" help="Sent with Content-Type: application/json unless you set it." className="sm:col-span-2">
            {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-28 w-full font-mono text-xs" value={body} onChange={(e) => setBody(e.target.value)} placeholder='{"name": "Ada"}' />}
          </FormField>
          <div className="flex items-center gap-3 sm:col-span-2">
            <Button tone="primary" type="submit" disabled={busy}>{busy ? "Running…" : "Run test"}</Button>
            <span className="text-xs text-muted-foreground">Sent with your session. The run is logged like any other call.</span>
          </div>
        </form>
      </section>
      <section aria-labelledby="fn-test-response" className="overflow-hidden rounded-xl border border-border bg-card">
        <header className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-muted/40 px-4 py-2.5">
          <h2 id="fn-test-response" className="text-sm font-medium">Response</h2>
          {result && <span className="text-xs tabular-nums text-muted-foreground">{result.durationMs} ms</span>}
        </header>
        <div className="space-y-4 p-4">
          <ErrorPanel error={error} />
          {!result && !error && (busy ? <Loading what="Running the function" /> : (
            <p className="py-8 text-center text-sm text-muted-foreground">Run a test to see the status, the body, the headers and the log lines of that run here.</p>
          ))}
          {result && (
            <div className="space-y-4" aria-label="Test result" role="group">
              <div className="flex flex-wrap items-center gap-x-3 gap-y-2 text-sm">
                <HttpStatus status={result.status} />
                {result.requestId && (
                  <span className="flex min-w-0 items-center gap-2">
                    <span className="text-xs text-muted-foreground">Request</span>
                    <span className="truncate font-mono text-xs" aria-label="Request id" title={result.requestId}>{result.requestId}</span>
                    <CopyButton text={result.requestId} label="Copy request id" className="h-7 px-2 text-xs" />
                    <Link className="text-xs font-medium text-primary hover:underline" to={runLink(result.requestId)}>Find in logs</Link>
                  </span>
                )}
              </div>
              <ErrorPanel error={asApiError(result, result.requestId ?? "")} />
              <div>
                <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">Body</h3>
                <pre aria-label="Response body" className="max-h-80 overflow-auto rounded-lg border border-border bg-muted/40 p-3 font-mono text-xs leading-5">{pretty(result.body) || "(empty)"}</pre>
              </div>
              <div>
                <h3 className="mb-1.5 text-xs font-medium text-muted-foreground">Logs of this run</h3>
                {logs === undefined ? <Loading what="Loading logs" /> : lines.length === 0 ? <p className="text-sm text-muted-foreground">No log lines for this run.</p> : <LogLines lines={lines} label="Run logs" />}
              </div>
              <details className="group">
                <summary className="cursor-pointer text-xs font-medium text-muted-foreground hover:text-foreground">Headers ({Object.keys(result.headers).length})</summary>
                <pre aria-label="Response headers" className="mt-1.5 overflow-x-auto rounded-lg border border-border bg-muted/40 p-3 font-mono text-xs leading-5">{Object.entries(result.headers).map(([k, v]) => `${k}: ${v}`).join("\n") || "none"}</pre>
              </details>
            </div>
          )}
        </div>
      </section>
    </div>
  );
}

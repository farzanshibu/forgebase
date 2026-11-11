import { useState } from "react";
import { Link } from "react-router-dom";
import { ApiError, functionLogs, invokeFunction } from "../../api";
import type { ExecutionLog, FunctionInfo, InvokeResult, VersionInfo } from "../../api";
import { Button, CopyButton, Panel } from "../../components/ui";
import { ErrorPanel } from "../../components/ErrorPanel";
import { Sel } from "../../components/controls";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
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

/** Test-invoke: method, path, query, headers and JSON body in; status, headers, body and the execution's logs out. */
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

  const lines = (logs ?? []).flatMap((l) => [
    ...l.logs.map((x, i) => ({ key: `${l.id}-${i}`, text: `[${x.level}] ${x.message}`, error: false })),
    ...(l.error ? [{ key: `e${l.id}`, text: `error: ${l.error}`, error: true }] : []),
  ]);
  return (
    <Panel title="Test run">
      <form aria-label="Test run" onSubmit={(e) => void run(e)} className="grid gap-3 md:grid-cols-4">
        <label className="text-sm">Method
          <Sel aria-label="Method" className="mt-1" value={method} onChange={setMethod} options={["GET", "POST", "PUT", "PATCH", "DELETE"]} />
        </label>
        <label className="text-sm">Path<Input size="sm" className="mt-1 w-full" value={path} onChange={(e) => setPath(e.target.value)} placeholder="/" /></label>
        <label className="text-sm">Query<Input size="sm" className="mt-1 w-full" value={query} onChange={(e) => setQuery(e.target.value)} placeholder="name=Ada&x=1" /></label>
        <label className="text-sm">Version
          <Sel aria-label="Version" className="mt-1" value={version} onChange={setVersion}
            options={[{ value: "active", label: `active (v${info.active_version})` }, ...versions.map((v) => ({ value: String(v.version), label: `v${v.version}` }))]} />
        </label>
        <label className="text-sm md:col-span-2">Headers (Name: value per line)
          <Textarea size="sm" autoResize={false} className="mt-1 h-24 w-full font-mono text-xs" value={headers} onChange={(e) => setHeaders(e.target.value)} />
        </label>
        <label className="text-sm md:col-span-2">JSON body
          <Textarea size="sm" autoResize={false} className="mt-1 h-24 w-full font-mono text-xs" value={body} onChange={(e) => setBody(e.target.value)} placeholder='{"name": "Ada"}' />
        </label>
        <div className="md:col-span-4"><Button tone="primary" type="submit" disabled={busy}>Run test</Button></div>
      </form>
      <div className="mt-3"><ErrorPanel error={error} /></div>
      {result && (
        <div className="mt-4 space-y-3" aria-label="Test result" role="group">
          <p className="flex flex-wrap items-center gap-x-2 gap-y-1 text-sm">
            <span className="font-mono font-semibold">HTTP {result.status}</span>
            <span className="text-muted-foreground">· {result.durationMs} ms · request</span>
            {result.requestId && <><span className="font-mono text-xs" aria-label="Request id">{result.requestId}</span><CopyButton text={result.requestId} label="Copy request id" /></>}
            {result.requestId && <Link className="text-primary hover:underline" to={`/functions/f/${encodeURIComponent(info.name)}/logs?request=${encodeURIComponent(result.requestId)}`}>Find in logs</Link>}
          </p>
          <ErrorPanel error={asApiError(result, result.requestId ?? "")} />
          <div>
            <h4 className="text-xs uppercase tracking-wide text-muted-foreground">Headers</h4>
            <pre aria-label="Response headers" className="overflow-x-auto rounded-md border border-border p-2 font-mono text-xs">{Object.entries(result.headers).map(([k, v]) => `${k}: ${v}`).join("\n") || "none"}</pre>
          </div>
          <div>
            <h4 className="text-xs uppercase tracking-wide text-muted-foreground">Body</h4>
            <pre aria-label="Response body" className="overflow-x-auto rounded-md border border-border p-2 font-mono text-xs">{pretty(result.body) || "(empty)"}</pre>
          </div>
          <div>
            <h4 className="text-xs uppercase tracking-wide text-muted-foreground">Logs of this run</h4>
            {logs === undefined ? <p className="text-sm text-muted-foreground">Loading logs…</p> : lines.length === 0 ? <p className="text-sm text-muted-foreground">No log lines for this run.</p> : (
              <ul aria-label="Run logs" className="rounded-md border border-border p-2 font-mono text-xs">
                {lines.map((l) => <li key={l.key} className={l.error ? "text-destructive" : undefined}>{l.text}</li>)}
              </ul>
            )}
          </div>
        </div>
      )}
    </Panel>
  );
}

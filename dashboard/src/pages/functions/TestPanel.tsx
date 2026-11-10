import { useState } from "react";
import { functionLogs, invokeFunction } from "../../api";
import type { ExecutionLog, FunctionInfo, InvokeResult, VersionInfo } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
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
      const res = await invokeFunction(info.name, { method, path, query, headers: h, body: body.trim() ? body : undefined, version: version === "active" ? undefined : Number(version) });
      setResult(res);
      try {
        const recent = await functionLogs(info.name, 20);
        setLogs(res.requestId ? recent.filter((l) => l.request_id === res.requestId) : recent.slice(0, 1));
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
        <div className="md:col-span-4"><Button tone="primary" type="submit" disabled={busy}>Send test request</Button></div>
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
      {result && (
        <div className="mt-4 space-y-3" aria-label="Test result" role="group">
          <p className="text-sm"><span className="font-mono font-semibold">HTTP {result.status}</span> <span className="text-muted-foreground">· {result.durationMs} ms{result.requestId ? ` · request ${result.requestId}` : ""}</span></p>
          <div>
            <h4 className="text-xs uppercase tracking-wide text-muted-foreground">Headers</h4>
            <pre aria-label="Response headers" className="overflow-x-auto rounded-md border border-border p-2 font-mono text-xs">{Object.entries(result.headers).map(([k, v]) => `${k}: ${v}`).join("\n") || "none"}</pre>
          </div>
          <div>
            <h4 className="text-xs uppercase tracking-wide text-muted-foreground">Body</h4>
            <pre aria-label="Response body" className="overflow-x-auto rounded-md border border-border p-2 font-mono text-xs">{pretty(result.body) || "(empty)"}</pre>
          </div>
          <div>
            <h4 className="text-xs uppercase tracking-wide text-muted-foreground">Logs</h4>
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

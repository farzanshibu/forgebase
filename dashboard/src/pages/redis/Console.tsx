import { useEffect, useRef, useState } from "react";
import { ApiError, kvExec } from "../../api";
import { Button } from "../../components/ui";
import { formatReply, tokenize } from "./util";
import { Input } from "panelui-web/input";

interface Entry { id: number; line: string; out: string; ok: boolean; ms: number }

const EXAMPLES = ["SET greeting hello", "GET greeting", "KEYS *", "HSET user:1 name Ada", "TTL greeting"];

/** Command console over `POST /kv/{ns}/exec`: any supported command, redis-cli style output, history with the arrow keys. */
export default function Console({ ns }: { ns: string }) {
  const [line, setLine] = useState("");
  const [log, setLog] = useState<Entry[]>([]);
  const [hist, setHist] = useState<string[]>([]);
  const [pos, setPos] = useState<number>(-1);
  const [busy, setBusy] = useState(false);
  const out = useRef<HTMLDivElement>(null);
  const input = useRef<HTMLInputElement>(null);

  // Keep the newest reply in view, the way a terminal does.
  useEffect(() => {
    const el = out.current;
    if (el && typeof el.scrollTo === "function") el.scrollTo({ top: el.scrollHeight });
  }, [log]);

  const run = async (e: React.FormEvent) => {
    e.preventDefault();
    const text = line.trim();
    if (!text) return;
    setBusy(true);
    const t0 = performance.now();
    let out: string, ok = true;
    try {
      const args = tokenize(text);
      out = formatReply(await kvExec(ns, args));
    } catch (err) {
      ok = false;
      out = err instanceof ApiError ? `(error) ${err.code}: ${err.message}` : `(error) ${err instanceof Error ? err.message : String(err)}`;
    }
    const ms = Math.round(performance.now() - t0);
    setLog((l) => [...l, { id: l.length, line: text, out, ok, ms }]);
    setHist((h) => [text, ...h]);
    setPos(-1); setLine(""); setBusy(false);
  };
  const key = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowUp" && hist.length) { e.preventDefault(); const p = Math.min(pos + 1, hist.length - 1); setPos(p); setLine(hist[p]!); }
    else if (e.key === "ArrowDown") { e.preventDefault(); const p = pos - 1; setPos(p); setLine(p >= 0 ? hist[p]! : ""); }
  };
  return (
    <section aria-labelledby="kv-console" className="overflow-hidden rounded-xl border border-border bg-card">
      <header className="flex flex-wrap items-center justify-between gap-2 border-b border-border px-4 py-2.5">
        <h2 id="kv-console" className="flex items-center gap-2 text-sm font-medium">
          Command console
          <span className="rounded bg-muted px-1.5 py-0.5 font-mono text-xs font-normal text-muted-foreground">namespace {ns}</span>
        </h2>
        <Button onClick={() => setLog([])} disabled={log.length === 0}>Clear</Button>
      </header>
      <div ref={out} aria-label="Console output" role="log" className="h-96 overflow-auto bg-muted/40 px-4 py-3 font-mono text-xs leading-5">
        {log.length === 0 && (
          <div className="text-muted-foreground">
            <p>Type a command, for example SET greeting hello or KEYS *. Quote arguments with spaces.</p>
            <p className="mt-2 flex flex-wrap items-center gap-1.5 font-sans">
              Try:
              {EXAMPLES.map((x) => (
                <button key={x} type="button" className="rounded border border-border bg-background px-1.5 py-0.5 font-mono text-[0.6875rem] text-foreground hover:bg-accent focus-visible:outline-2 focus-visible:outline-ring" onClick={() => { setLine(x); input.current?.focus(); }}>{x}</button>
              ))}
            </p>
          </div>
        )}
        {log.map((e) => (
          <div key={e.id} className="mb-2.5">
            <p className="flex items-baseline justify-between gap-3">
              <span className="min-w-0 break-all"><span className="select-none text-primary">{ns}&gt; </span><span className="text-foreground">{e.line}</span></span>
              <span className="shrink-0 text-[0.6875rem] tabular-nums text-muted-foreground">{e.ms} ms</span>
            </p>
            <pre className={`whitespace-pre-wrap break-all ${e.ok ? (/^\((nil|empty array)\)/.test(e.out) ? "text-muted-foreground" : "text-foreground") : "text-destructive"}`}>{e.out}</pre>
          </div>
        ))}
      </div>
      <form onSubmit={(e) => void run(e)} className="flex items-center gap-2 border-t border-border px-3 py-2.5">
        <span aria-hidden="true" className="select-none font-mono text-xs text-primary">{ns}&gt;</span>
        <Input ref={input} size="sm" aria-label="Command" className="flex-1 font-mono" placeholder="GET key" value={line} onChange={(e) => setLine(e.target.value)} onKeyDown={key} autoComplete="off" spellCheck={false} />
        <span className="hidden text-xs text-muted-foreground sm:inline">↑ ↓ history</span>
        <Button type="submit" tone="primary" disabled={busy || !line.trim()}>{busy ? "Running…" : "Run"}</Button>
      </form>
    </section>
  );
}

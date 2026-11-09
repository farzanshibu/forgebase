import { useState } from "react";
import { ApiError, kvExec } from "../../api";
import { Button, Panel, inputCls } from "../../components/ui";
import { formatReply, tokenize } from "./util";

interface Entry { id: number; line: string; out: string; ok: boolean }

/** Command console over `POST /kv/{ns}/exec`: any supported command, redis-cli style output, history with the arrow keys. */
export default function Console({ ns }: { ns: string }) {
  const [line, setLine] = useState("");
  const [log, setLog] = useState<Entry[]>([]);
  const [hist, setHist] = useState<string[]>([]);
  const [pos, setPos] = useState<number>(-1);
  const [busy, setBusy] = useState(false);
  const run = async (e: React.FormEvent) => {
    e.preventDefault();
    const text = line.trim();
    if (!text) return;
    setBusy(true);
    let out: string, ok = true;
    try {
      const args = tokenize(text);
      out = formatReply(await kvExec(ns, args));
    } catch (err) {
      ok = false;
      out = err instanceof ApiError ? `(error) ${err.code}: ${err.message}` : `(error) ${err instanceof Error ? err.message : String(err)}`;
    }
    setLog((l) => [...l, { id: l.length, line: text, out, ok }]);
    setHist((h) => [text, ...h]);
    setPos(-1); setLine(""); setBusy(false);
  };
  const key = (e: React.KeyboardEvent) => {
    if (e.key === "ArrowUp" && hist.length) { e.preventDefault(); const p = Math.min(pos + 1, hist.length - 1); setPos(p); setLine(hist[p]!); }
    else if (e.key === "ArrowDown") { e.preventDefault(); const p = pos - 1; setPos(p); setLine(p >= 0 ? hist[p]! : ""); }
  };
  return (
    <Panel title={`Command console (namespace ${ns})`} actions={<Button onClick={() => setLog([])} disabled={log.length === 0}>Clear</Button>}>
      <div aria-label="Console output" role="log" className="mb-3 max-h-96 overflow-auto rounded bg-zinc-950 p-3 font-mono text-xs">
        {log.length === 0 && <p className="text-zinc-500">Type a command, for example SET greeting hello or KEYS *. Quote arguments with spaces.</p>}
        {log.map((e) => (
          <div key={e.id} className="mb-2">
            <p className="text-amber-300">&gt; {e.line}</p>
            <pre className={`whitespace-pre-wrap ${e.ok ? "text-zinc-200" : "text-red-300"}`}>{e.out}</pre>
          </div>
        ))}
      </div>
      <form onSubmit={(e) => void run(e)} className="flex gap-2">
        <input aria-label="Command" className={`${inputCls} flex-1 font-mono`} placeholder="GET key" value={line} onChange={(e) => setLine(e.target.value)} onKeyDown={key} autoComplete="off" spellCheck={false} />
        <Button type="submit" tone="primary" disabled={busy || !line.trim()}>Run</Button>
      </form>
    </Panel>
  );
}

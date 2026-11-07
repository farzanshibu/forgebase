import { useState } from "react";
import { clearSlowQueries, setSlowThreshold, slowQueries } from "../../api";
import { useLoad } from "../../hooks";
import { Button, ConfirmButton, ErrorView, Loading, inputCls } from "../../components/ui";

export default function SlowQueries() {
  const { data, error, loading, reload } = useLoad(slowQueries, "slow");
  const [threshold, setThreshold] = useState("");
  const [problem, setProblem] = useState<unknown>();

  async function act(fn: () => Promise<unknown>) {
    setProblem(undefined);
    try {
      await fn();
      reload();
    } catch (e) {
      setProblem(e);
    }
  }

  return (
    <div className="space-y-4">
      <ErrorView error={error} />
      <ErrorView error={problem} />
      {loading && !data && !error && <Loading what="Loading slow queries" />}
      {data && (
        <>
          <div className="flex flex-wrap items-end gap-3 text-sm">
            <span className="text-zinc-400">Statements slower than <strong className="text-zinc-100">{data.threshold_ms} ms</strong> are logged.</span>
            <input aria-label="Threshold ms" className={`${inputCls} w-24`} inputMode="numeric" value={threshold} onChange={(e) => setThreshold(e.target.value)} placeholder="ms" />
            <Button disabled={!/^\d+$/.test(threshold)} onClick={() => void act(() => setSlowThreshold(Number(threshold)))}>Set threshold</Button>
            <Button onClick={reload}>Refresh</Button>
            <ConfirmButton label="Clear log" disabled={data.items.length === 0} onConfirm={() => void act(clearSlowQueries)} />
          </div>
          {data.items.length === 0 ? (
            <p className="text-sm text-zinc-400">No slow queries recorded.</p>
          ) : (
            <table className="w-full text-left text-sm">
              <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-1">SQL</th><th>Duration</th><th>Rows</th><th>When</th></tr></thead>
              <tbody>
                {data.items.map((q, i) => (
                  <tr key={i} className="border-t border-zinc-800 align-top">
                    <td className="max-w-xl py-1 font-mono text-xs break-all">{q.sql}{q.label && <span className="ml-2 text-zinc-500">[{q.label.collection}]</span>}</td>
                    <td>{q.duration_ms.toFixed(1)} ms</td>
                    <td>{q.rows}</td>
                    <td className="text-xs text-zinc-400">{q.at}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </>
      )}
    </div>
  );
}

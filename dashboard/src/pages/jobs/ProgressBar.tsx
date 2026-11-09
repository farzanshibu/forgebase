import type { Progress } from "../../api";

/** Live progress of a task: percent bar, stage and message. Without a percent only the stage and message show. */
export default function ProgressBar({ progress, label = "Task progress" }: { progress: Progress | null | undefined; label?: string }) {
  if (!progress) return <p className="text-sm text-zinc-400">No progress reported yet.</p>;
  const pct = progress.percent == null ? null : Math.max(0, Math.min(100, progress.percent));
  return (
    <div className="space-y-1 text-sm">
      {pct !== null && (
        <div role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)} className="h-2.5 w-full overflow-hidden rounded bg-zinc-800">
          <div className="h-full bg-amber-400 transition-[width]" style={{ width: `${pct}%` }} />
        </div>
      )}
      <p>
        {pct !== null && <span className="mr-2 font-medium">{Math.round(pct)}%</span>}
        {progress.stage && <span className="mr-2 rounded bg-zinc-800 px-1.5 py-0.5 text-xs">{progress.stage}</span>}
        {progress.message && <span className="text-zinc-300">{progress.message}</span>}
      </p>
    </div>
  );
}

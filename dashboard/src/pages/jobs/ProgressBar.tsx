import type { Progress } from "../../api";

/** Live progress of a task: percent bar, stage and message. Without a percent only the stage and message show. */
export default function ProgressBar({ progress, label = "Task progress" }: { progress: Progress | null | undefined; label?: string }) {
  if (!progress) return <p className="text-sm text-muted-foreground">No progress reported yet.</p>;
  const pct = progress.percent == null ? null : Math.max(0, Math.min(100, progress.percent));
  return (
    <div className="space-y-1.5 text-sm">
      {pct !== null && (
        <div className="flex items-center gap-3">
          <div role="progressbar" aria-label={label} aria-valuemin={0} aria-valuemax={100} aria-valuenow={Math.round(pct)} className="h-2 min-w-16 flex-1 overflow-hidden rounded-full bg-muted">
            <div className={`h-full rounded-full transition-[width] duration-500 ${pct >= 100 ? "bg-success" : "bg-primary"}`} style={{ width: `${pct}%` }} />
          </div>
          <span className="w-10 shrink-0 text-right text-xs font-medium tabular-nums">{Math.round(pct)}%</span>
        </div>
      )}
      {(progress.stage || progress.message) && (
        <p className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          {progress.stage && <span className="rounded-md bg-muted px-1.5 py-0.5 font-mono text-xs">{progress.stage}</span>}
          {progress.message && <span className="text-xs text-muted-foreground">{progress.message}</span>}
        </p>
      )}
    </div>
  );
}

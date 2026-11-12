/**
 * Skeletons: grey shapes where content is about to appear (dashboard-ux.md rule 6, audit A-12). Use them for
 * lists, tables and cards instead of a "Loading…" line; keep `Loading` (ui.tsx) for a small inline wait.
 *
 * Usage:
 *   {list.loading && <SkeletonTable rows={5} columns={4} label="Loading buckets" />}
 *   {stats.loading && <SkeletonCards count={3} />}
 *   <Skeleton className="h-4 w-40" />                         // one bar
 *
 * Each group is one `role="status"` with a label, so a screen reader hears "Loading buckets" once, not every bar.
 */
import type { CSSProperties } from "react";

export function Skeleton({ className = "", style }: { className?: string; style?: CSSProperties }) {
  return <span aria-hidden="true" style={style} className={`block animate-pulse rounded-md bg-muted motion-reduce:animate-none ${className}`.trim()} />;
}

export function SkeletonTable({ rows = 5, columns = 4, label = "Loading" }: { rows?: number; columns?: number; label?: string }) {
  return (
    <div role="status" aria-label={label} className="w-full space-y-3 py-1">
      <div className="flex gap-4">
        {Array.from({ length: columns }, (_, c) => (
          <Skeleton key={c} className="h-3 flex-1 opacity-70" />
        ))}
      </div>
      {Array.from({ length: rows }, (_, r) => (
        <div key={r} className="flex gap-4 border-t border-border pt-3">
          {Array.from({ length: columns }, (_, c) => (
            <Skeleton key={c} className={`h-4 flex-1 ${c === 0 ? "max-w-48" : ""}`} />
          ))}
        </div>
      ))}
      <span className="sr-only">{label}…</span>
    </div>
  );
}

export function SkeletonCards({ count = 3, label = "Loading" }: { count?: number; label?: string }) {
  return (
    <div role="status" aria-label={label} className="grid gap-3 sm:grid-cols-2 lg:grid-cols-3">
      {Array.from({ length: count }, (_, i) => (
        <div key={i} className="space-y-3 rounded-xl border border-border p-4">
          <Skeleton className="h-3 w-24" />
          <Skeleton className="h-6 w-32" />
          <Skeleton className="h-3 w-full" />
        </div>
      ))}
      <span className="sr-only">{label}…</span>
    </div>
  );
}

export function SkeletonLines({ lines = 3, label = "Loading" }: { lines?: number; label?: string }) {
  return (
    <div role="status" aria-label={label} className="space-y-2">
      {Array.from({ length: lines }, (_, i) => (
        <Skeleton key={i} className={`h-4 ${i === lines - 1 ? "w-2/3" : "w-full"}`} />
      ))}
      <span className="sr-only">{label}…</span>
    </div>
  );
}

export default Skeleton;

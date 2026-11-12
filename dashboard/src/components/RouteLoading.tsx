import { Spinner } from "panelui-web/spinner";
import { BrandMark } from "./AuthShell";
import { Skeleton } from "./Skeleton";

/**
 * Themed fallback while a lazily loaded chunk (a studio, sign-in, setup) is fetched.
 * - `page` (default, inside the shell): the shape of a studio page, a header then a table, so nothing jumps when it lands.
 * - `screen` (before the shell exists): the ForgeBase mark and a spinner, centred.
 */
export default function RouteLoading({ variant = "page" }: { variant?: "page" | "screen" }) {
  if (variant === "screen")
    return (
      <div role="status" className="flex min-h-svh flex-col items-center justify-center gap-4 bg-background">
        <BrandMark size="lg" />
        <span className="flex items-center gap-2 text-sm text-muted-foreground">
          <Spinner size="sm" aria-hidden="true" role="presentation" /> Loading…
        </span>
      </div>
    );
  return (
    <div role="status" className="space-y-8">
      <div className="space-y-2.5">
        <Skeleton className="h-7 w-48" />
        <Skeleton className="h-4 w-96 max-w-full" />
      </div>
      <div className="flex gap-6 border-b border-border pb-3">
        {[64, 80, 72, 56].map((w) => (
          <Skeleton key={w} className="h-4" style={{ width: w }} />
        ))}
      </div>
      <div className="space-y-3 rounded-xl border border-border bg-card p-5">
        {Array.from({ length: 5 }, (_, i) => (
          <div key={i} className="flex gap-4">
            <Skeleton className="h-4 flex-1 max-w-56" />
            <Skeleton className="h-4 flex-1" />
            <Skeleton className="h-4 w-20" />
          </div>
        ))}
      </div>
      <span className="sr-only">Loading…</span>
    </div>
  );
}

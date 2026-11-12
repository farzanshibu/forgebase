/**
 * When: a time as "5 min ago" (or a day with `day`), the exact local and UTC time on hover, kept fresh by one shared
 * 30 s timer. The one way to show a time from the API in every studio. The words come from timeFormat.ts.
 *
 * Usage:
 *   <When value={job.created_at} />                  // "5 min ago"; title "Nov 12, 2026, 10:55:00 AM (2026-11-12 09:55:00Z)"
 *   <When value={key.last_used_at} fallback="Never" /> // words for a missing time (default "—")
 *   <When value={sub.current_period_end} day />      // "Nov 12, 2026"
 *   <When value={user.created} />                    // a number is Unix seconds
 *
 * `iso` is the same as `value`, and `empty` the same as `fallback` (older call sites). A string that is not a time
 * shows as it is.
 */
import type { ReactNode } from "react";
import { absolute, dayText, relative, toDate, useNow } from "./timeFormat";

export interface WhenProps {
  /** An ISO time, or Unix seconds. */
  value?: string | number | null;
  /** Same as `value`. */
  iso?: string | null;
  /** Shown for a missing time. Default "—". */
  fallback?: ReactNode;
  /** Same as `fallback`. */
  empty?: ReactNode;
  /** Show the day ("Nov 12, 2026") instead of a relative time. */
  day?: boolean;
  className?: string;
}

export function When({ value, iso, fallback, empty, day = false, className = "" }: WhenProps) {
  const now = useNow();
  const v = value ?? iso;
  if (v === null || v === undefined || v === "") return <span className={`text-muted-foreground ${className}`.trim()}>{fallback ?? empty ?? "—"}</span>;
  const d = toDate(v);
  if (!d) return <span className={className}>{String(v)}</span>;
  return (
    <time dateTime={typeof v === "string" ? v : d.toISOString()} title={absolute(d)} className={`whitespace-nowrap tabular-nums ${className}`.trim()}>
      {day ? dayText(d) : relative(d, now)}
    </time>
  );
}

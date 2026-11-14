import type { InvocationProfile } from "../../api";
import { fmtUs } from "./util";

/**
 * The host calls of one execution as a timeline: one row per call (the first 64 are kept), a bar from when it started
 * to when it ended on a shared scale, red when it failed.
 */
export default function Timeline({ profile }: { profile: InvocationProfile }) {
  const calls = profile.timeline;
  const total = Math.max(profile.duration_us, ...calls.map((c) => c.at_us + c.dur_us), 1);
  if (calls.length === 0) return <p className="text-sm text-muted-foreground">This run made no host calls.</p>;
  return (
    <div className="space-y-1">
      <ol aria-label="Host call timeline" className="space-y-1">
        {calls.map((c, i) => (
          <li key={i} aria-label={`${c.op} at ${fmtUs(c.at_us)} for ${fmtUs(c.dur_us)}, ${c.ok ? "ok" : "failed"}`} className="grid grid-cols-[7rem_1fr_5rem] items-center gap-2 text-xs">
            <span className="truncate font-mono" title={c.op}>{c.op}</span>
            <span className="relative h-3 rounded bg-muted" aria-hidden="true">
              <span className={`absolute top-0 h-3 rounded ${c.ok ? "bg-primary" : "bg-destructive"}`} style={{ left: `${(c.at_us / total) * 100}%`, width: `${Math.max((c.dur_us / total) * 100, 0.8)}%` }} />
            </span>
            <span className="text-right tabular-nums text-muted-foreground">{fmtUs(c.dur_us)}</span>
          </li>
        ))}
      </ol>
      <p className="flex justify-between text-xs text-muted-foreground"><span>0</span><span>{fmtUs(total)}</span></p>
      {profile.timeline_dropped > 0 && <p className="text-xs text-muted-foreground">{profile.timeline_dropped} more call{profile.timeline_dropped === 1 ? "" : "s"} are counted but not drawn (the timeline keeps the first 64).</p>}
    </div>
  );
}

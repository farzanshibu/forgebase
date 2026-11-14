import type { Invocation } from "../../api";
import { DetailSheet } from "../../components/CreateSheet";
import { Facts } from "../../components/Facts";
import { HelpTip } from "../../components/HelpTip";
import { Badge } from "../../components/StatusBadge";
import { formatBytes } from "../../format";
import { When } from "../jobs/ops";
import Timeline from "./Timeline";
import { fmtInt, fmtMs, fmtUs } from "./util";

/** One execution: its outcome, cold start, fuel and memory against their limits, and the timeline of its host calls. */
export default function InvocationSheet({ invocation: i, onClose }: { invocation: Invocation; onClose: () => void }) {
  const p = i.profile;
  return (
    <DetailSheet open onOpenChange={(o) => !o && onClose()} title={`${i.function} v${i.version}`} description={<span className="flex flex-wrap items-center gap-2"><Badge status={i.status}>{i.status}</Badge><When iso={i.created_at} />{p?.cold_start && <Badge tone="warn">cold start</Badge>}</span>}>
      {i.error && <p role="alert" className="rounded-lg border border-destructive/40 bg-destructive/10 p-2 text-sm text-destructive">{i.error}</p>}
      <Facts
        label="Invocation facts"
        columns={2}
        items={[
          ["Trigger", i.trigger],
          ["Took", fmtMs(i.duration_ms)],
          ["Request id", i.request_id ?? "—"],
          ["Trace id", i.trace_id ?? "—"],
          p && ["Fuel", `${fmtInt(p.fuel_used)} of ${fmtInt(p.fuel_limit)}`],
          p && ["Peak memory", `${formatBytes(p.memory_peak_bytes)} of ${formatBytes(p.memory_limit_bytes)}`],
          p?.cold_start && ["Load time (cold)", fmtUs(p.prepare_us)],
          p && ["Host calls", fmtInt(p.host_calls)],
        ]}
      />
      {p ? (
        <section aria-label="Host calls">
          <h3 className="mb-1 flex items-center gap-1 text-sm font-medium">Host calls <HelpTip term="host_call" /></h3>
          <Timeline profile={p} />
        </section>
      ) : (
        <p className="text-sm text-muted-foreground">No profile was recorded for this run. Function profiling is switched off, or the run predates it.</p>
      )}
      {i.logs.length > 0 && (
        <section aria-label="Log lines">
          <h3 className="mb-1 text-sm font-medium">Log lines</h3>
          <pre className="max-h-48 overflow-auto rounded-lg border border-border bg-muted/40 p-2 font-mono text-xs">{i.logs.map((l) => `${l.level ?? "log"}  ${l.message ?? ""}`).join("\n")}</pre>
        </section>
      )}
    </DetailSheet>
  );
}

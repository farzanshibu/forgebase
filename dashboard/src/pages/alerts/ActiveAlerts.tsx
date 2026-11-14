import { useEffect, useState } from "react";
import { listAlerts } from "../../api";
import type { Alert, AlertSeverity } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { Sel } from "../../components/controls";
import { SkeletonCards } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { Button, ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Section, When } from "../jobs/ops";
import AlertDetail from "./AlertDetail";
import SilenceSheet from "./SilenceSheet";
import { LabelChips, SeverityBadge, StateBadge } from "./shared";
import { fmtValue, matchersFromAlert, runbookUrl } from "./util";

const REFRESH_MS = 30_000;

/** Open alerts (pending and firing), most severe first. Silence one from here: its labels become the matchers. */
export default function ActiveAlerts() {
  const [state, setState] = useState<"" | "firing" | "pending">("");
  const [severity, setSeverity] = useState<"" | AlertSeverity>("");
  const [open, setOpen] = useState<Alert>();
  const [silencing, setSilencing] = useState<Alert>();
  const list = useLoad((s) => listAlerts({ state: state || undefined, severity: severity || undefined }, s), `alerts:${state}:${severity}`);
  const { reload } = list;
  useEffect(() => {
    const t = setInterval(reload, REFRESH_MS);
    return () => clearInterval(t);
  }, [reload]);

  const items = list.data ?? [];
  return (
    <div className="space-y-4">
      <Section
        title="Open alerts"
        count={list.data?.length}
        description="Firing alerts are sending notifications; pending ones are waiting out their hold time. Refreshed every 30 seconds."
        actions={
          <span className="flex flex-wrap items-center gap-2">
            <Sel aria-label="State" value={state} onChange={setState} options={[{ value: "", label: "Any state" }, "firing", "pending"]} />
            <Sel aria-label="Severity" value={severity} onChange={setSeverity} options={[{ value: "", label: "Any severity" }, "critical", "warning", "info"]} />
            <Button onClick={reload}>Refresh</Button>
          </span>
        }
      >
        <ErrorView error={list.error} onRetry={reload} />
        {list.loading && !list.data && <SkeletonCards count={2} label="Loading alerts" />}
        {list.data && items.length === 0 && (
          <EmptyState compact title="Nothing is firing" why={state || severity ? "No open alert matches these filters." : "Every rule is quiet. Alerts that start to fire, or that are waiting to, show here."} />
        )}
        <ul className="space-y-3" aria-label="Alerts">
          {items.map((a) => {
            const runbook = runbookUrl(a.annotations);
            const summary = a.annotations.summary ?? a.annotations.description;
            return (
              <li key={a.id}>
                <article aria-label={`${a.name} ${a.state}`} className="rounded-lg border border-border p-3">
                  <div className="flex flex-wrap items-start justify-between gap-2">
                    <div className="min-w-0 space-y-1">
                      <p className="flex flex-wrap items-center gap-2 font-medium">
                        <SeverityBadge severity={a.severity} />
                        {a.name}
                        <StateBadge state={a.state} />
                        {a.silenced_by.length > 0 && <Badge tone="neutral" title={`Muted by ${a.silenced_by.join(", ")}`}>silenced</Badge>}
                      </p>
                      {summary && <p className="text-sm text-foreground/80">{summary}</p>}
                    </div>
                    <div className="flex shrink-0 items-center gap-2">
                      {runbook && <a className="text-sm font-medium text-primary underline-offset-2 hover:underline" href={runbook} target="_blank" rel="noreferrer noopener">Runbook</a>}
                      <Button aria-label={`Details of ${a.name}`} onClick={() => setOpen(a)}>Details</Button>
                      <Button aria-label={`Silence ${a.name}`} onClick={() => setSilencing(a)}>Silence</Button>
                    </div>
                  </div>
                  <div className="mt-2"><LabelChips labels={a.labels} /></div>
                  <p className="mt-2 text-xs text-muted-foreground">
                    Value {fmtValue(a.value)} · {a.state === "firing" ? <>firing since <When iso={a.fired_at} /></> : <>active since <When iso={a.active_at} /></>}
                  </p>
                </article>
              </li>
            );
          })}
        </ul>
      </Section>
      {open && <AlertDetail alert={open} onClose={() => setOpen(undefined)} />}
      {silencing && (
        <SilenceSheet
          key={silencing.id}
          open
          onOpenChange={(o) => !o && setSilencing(undefined)}
          initial={{ matchers: matchersFromAlert(silencing), comment: `Silencing ${silencing.name}` }}
          onCreated={reload}
        />
      )}
    </div>
  );
}

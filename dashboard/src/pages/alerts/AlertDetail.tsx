import { getAlert } from "../../api";
import type { Alert } from "../../api";
import { DetailSheet } from "../../components/CreateSheet";
import { Facts } from "../../components/Facts";
import { Skeleton } from "../../components/Skeleton";
import { ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { When } from "../jobs/ops";
import { LabelChips, SeverityBadge, StateBadge } from "./shared";
import { fmtValue, runbookUrl } from "./util";

/** An alert's details and its state changes (`GET /api/v1/alerts/{id}`). */
export default function AlertDetail({ alert, onClose }: { alert: Alert; onClose: () => void }) {
  const detail = useLoad((s) => getAlert(alert.id, s), `alert:${alert.id}`);
  const runbook = runbookUrl(alert.annotations);
  return (
    <DetailSheet open onOpenChange={(o) => !o && onClose()} title={alert.name} description={<span className="flex flex-wrap items-center gap-2"><SeverityBadge severity={alert.severity} /><StateBadge state={alert.state} /></span>}>
      <Facts
        label="Alert facts"
        columns={2}
        items={[
          ["Value", fmtValue(alert.value)],
          ["Rule", alert.rule_id],
          ["Active since", <When key="a" iso={alert.active_at} />],
          ["Firing since", <When key="f" iso={alert.fired_at} fallback="Not yet" />],
          ["Resolved", <When key="r" iso={alert.resolved_at} fallback="No" />],
          ["Last checked", <When key="l" iso={alert.last_evaluated_at} />],
        ]}
      />
      <LabelChips labels={alert.labels} />
      {Object.entries(alert.annotations).length > 0 && (
        <dl aria-label="Annotations" className="space-y-1 text-sm">
          {Object.entries(alert.annotations).map(([k, v]) => (
            <div key={k}><dt className="text-xs uppercase tracking-wide text-muted-foreground">{k}</dt><dd>{v}</dd></div>
          ))}
        </dl>
      )}
      {runbook && <p><a className="text-sm font-medium text-primary underline-offset-2 hover:underline" href={runbook} target="_blank" rel="noreferrer noopener">Open the runbook</a></p>}
      <section aria-label="State changes">
        <h3 className="mb-1 text-sm font-medium">State changes</h3>
        {detail.loading && !detail.data && <Skeleton className="h-16 w-full" />}
        <ErrorView error={detail.error} onRetry={detail.reload} />
        {detail.data && (
          <ol className="space-y-1 text-sm">
            {detail.data.events.map((e, i) => (
              <li key={i} className="flex items-center gap-2"><StateBadge state={e.state} /><When iso={e.at} /><span className="text-muted-foreground">value {fmtValue(e.value)}</span></li>
            ))}
          </ol>
        )}
      </section>
    </DetailSheet>
  );
}

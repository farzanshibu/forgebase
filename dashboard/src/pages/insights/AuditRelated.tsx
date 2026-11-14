import { auditRelated } from "../../api";
import type { InsightAuditEntry } from "../../api";
import { DetailSheet } from "../../components/CreateSheet";
import { Facts } from "../../components/Facts";
import { SkeletonLines } from "../../components/Skeleton";
import { Badge } from "../../components/StatusBadge";
import { ErrorView } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Code, When } from "../jobs/ops";
import { resourceOf } from "./util";

function Entries({ title, items, empty }: { title: string; items: InsightAuditEntry[]; empty: string }) {
  return (
    <section aria-label={title}>
      <h3 className="mb-1 text-sm font-medium">{title} <span className="font-normal text-muted-foreground">({items.length})</span></h3>
      {items.length === 0 ? <p className="text-sm text-muted-foreground">{empty}</p> : (
        <ul className="space-y-1 text-sm">
          {items.map((e) => (
            <li key={e.id} className="flex flex-wrap items-center gap-2"><When iso={e.at} /><span className="font-mono text-xs">{e.event}</span><Badge status={e.outcome}>{e.outcome}</Badge><span className="truncate text-xs text-muted-foreground">{e.actor}</span></li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** One audit entry with the entries of the same request and of the same actor around it (`/audit/{id}/related`). */
export default function AuditRelated({ entry, onClose }: { entry: InsightAuditEntry; onClose: () => void }) {
  const r = useLoad((s) => auditRelated(entry.id, {}, s), `audit-related:${entry.id}`);
  const e = r.data?.event ?? entry;
  return (
    <DetailSheet open onOpenChange={(o) => !o && onClose()} title={e.event} description={<span className="flex flex-wrap items-center gap-2"><Badge status={e.outcome}>{e.outcome}</Badge><When iso={e.at} /></span>}>
      <Facts
        label="Audit entry facts"
        columns={2}
        items={[
          ["Actor", e.actor],
          e.impersonator && ["Impersonated by", e.impersonator],
          ["Resource", resourceOf(e)],
          ["IP", e.ip ?? "—"],
          ["Request id", e.request_id ?? "—"],
          ["Entry id", String(e.id)],
        ]}
      />
      {e.user_agent && <p className="truncate text-xs text-muted-foreground" title={e.user_agent}>{e.user_agent}</p>}
      {e.detail !== null && e.detail !== undefined && <Code label="Entry detail">{JSON.stringify(e.detail, null, 2)}</Code>}
      <ErrorView error={r.error} onRetry={r.reload} />
      {r.loading && !r.data && <SkeletonLines lines={3} label="Loading related events" />}
      {r.data && (
        <>
          <Entries title="Same request" items={r.data.same_request} empty="Nothing else was logged for this request." />
          <Entries title="Same actor, around the same time" items={r.data.same_actor} empty="This actor did nothing else within five minutes." />
        </>
      )}
    </DetailSheet>
  );
}

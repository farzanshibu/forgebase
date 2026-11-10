/**
 * Recent activity: the newest entries of the authentication audit log (sign-ins, administrators, keys), with a link
 * to the full log in Authentication.
 *
 * Usage:
 *   <RecentActivity />
 */
import { useId } from "react";
import { authAudit } from "../../api";
import { useLoad } from "../../hooks";
import { Card } from "panelui-web/card";
import { Badge } from "panelui-web/badge";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonLines } from "../../components/Skeleton";
import { LinkButton } from "./LinkButton";
import { actorWords, ago, EVENT_WORDS, eventWords } from "./model";

const SHOWN = 6;

export function RecentActivity({ adminEmail, adminId }: { adminEmail?: string; adminId?: string }) {
  const headingId = useId();
  const list = useLoad((s) => authAudit({ limit: SHOWN }, s), "overview:activity");
  const rows = list.data?.slice(0, SHOWN);
  return (
    <Card as="section" aria-labelledby={headingId} className="rounded-xl">
      <Card.Header className="flex flex-row flex-wrap items-start justify-between gap-2">
        <div>
          <Card.Title as="h2" id={headingId} className="text-base font-semibold">Recent activity</Card.Title>
          <Card.Description>Sign-ins and account changes on this server.</Card.Description>
        </div>
        <LinkButton to="/auth/audit" tone="ghost">Open the audit log</LinkButton>
      </Card.Header>
      <Card.Content>
        <ErrorPanel error={list.error} onRetry={list.reload} />
        {list.loading && !list.error && !rows && <SkeletonLines lines={4} label="Loading recent activity" />}
        {rows?.length === 0 && <EmptyState compact title="No activity yet" why="Sign-ins, new keys and administrator changes show here as they happen." />}
        {rows && rows.length > 0 && (
          <ul aria-label="Recent events" className="divide-y divide-border">
            {rows.map((e) => (
              <li key={e.id} className="flex flex-wrap items-baseline justify-between gap-x-3 gap-y-0.5 py-2 text-sm first:pt-0 last:pb-0">
                <span className="flex min-w-0 items-center gap-2">
                  {e.outcome && e.outcome !== "success" && <Badge variant="warning" size="sm">{e.outcome}</Badge>}
                  <span className="font-medium">{EVENT_WORDS[e.event] ?? eventWords({ event: e.event, outcome: "success" })}</span>
                  <span className="truncate text-xs text-muted-foreground">by {actorWords(e.actor, adminEmail, adminId)}</span>
                </span>
                <time dateTime={e.at} title={e.at} className="shrink-0 text-xs text-muted-foreground tabular-nums">{ago(e.at)}</time>
              </li>
            ))}
          </ul>
        )}
      </Card.Content>
    </Card>
  );
}

export default RecentActivity;

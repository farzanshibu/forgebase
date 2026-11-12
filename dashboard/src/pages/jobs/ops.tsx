/**
 * Ops-console pieces shared by the Jobs, Queues and System studios. The generic ones are the shared components
 * (TableCard, StatGrid, Facts, When, IdText, Badge/Dot), re-exported here under the names these pages already use.
 * Only the callout and the code block are local.
 *
 * Usage:
 *   <When iso={job.created_at} />                                  // "5 min ago", exact local and UTC time in the title; "never" when missing
 *   <StatGrid label="Topic metrics" items={[{ label: "Retained messages", value: 3 }]} />
 *   <Section title="Jobs" count={181} actions={<Button>Refresh</Button>}>…</Section>   // a TableCard with a padded body
 *   <CopyId id={job.id} noun="job" />                              // = IdText
 */
import type { ComponentProps, ReactNode } from "react";
import { Facts as BaseFacts } from "../../components/Facts";
import { IdText } from "../../components/IdText";
import { CardBody, TableCard } from "../../components/TableCard";
import { When as BaseWhen } from "../../components/Time";

export { CopyIcon } from "../../components/Button";
export { StatGrid } from "../../components/Stats";
export { Dot } from "../../components/StatusBadge";
export { NUM } from "../../components/TableCard";
export type { StatItem } from "../../components/Stats";
export type { Tone } from "../../components/StatusBadge";

/** A time as "5 min ago"; a missing one reads "never" here (`empty` changes it). */
export function When(p: ComponentProps<typeof BaseWhen>) {
  return <BaseWhen empty="never" {...p} />;
}

/** Classes for the header row of a table inside a Section. */
export const TH = "text-xs uppercase tracking-wide text-muted-foreground";

/** Facts of a resource inside a Section card: a two-column grid without its own border. */
export function Facts(p: ComponentProps<typeof BaseFacts>) {
  return <BaseFacts columns={2} framed={false} {...p} />;
}

/** A TableCard whose content is padded: for forms, code, graphs and tables that are not framed. */
export function Section({ title, count, description, actions, children, className, label }: {
  title: ReactNode;
  count?: number | string;
  description?: ReactNode;
  actions?: ReactNode;
  children: ReactNode;
  className?: string;
  /** Accessible name of the section when the title is not plain words. */
  label?: string;
}) {
  return (
    <TableCard title={title} count={count} description={description} actions={actions} className={className} label={label}>
      <CardBody>{children}</CardBody>
    </TableCard>
  );
}

/** A long id in monospace with a copy button: IdText, named "Copy <noun> id". */
export function CopyId({ id, noun = "", children, className = "" }: { id: string; noun?: string; children?: ReactNode; className?: string }) {
  return (
    <IdText id={id} noun={noun || undefined} className={className}>
      {children}
    </IdText>
  );
}

/** A tinted box for a state that needs attention: a warning, an error, or a note. */
export function Callout({ tone = "info", title, children, role }: { tone?: "info" | "warn" | "bad" | "ok"; title?: ReactNode; children?: ReactNode; role?: "alert" | "status" | "note" }) {
  const cls = {
    info: "border-border bg-muted/40",
    warn: "border-warning/40 bg-warning/10",
    bad: "border-destructive/40 bg-destructive/10",
    ok: "border-success/40 bg-success/10",
  }[tone];
  return (
    <div role={role} className={`rounded-lg border p-3 text-sm ${cls}`}>
      {title && <p className={`font-medium ${tone === "bad" ? "text-destructive" : tone === "warn" ? "text-warning" : tone === "ok" ? "text-success" : ""}`}>{title}</p>}
      {children && <div className={title ? "mt-0.5 text-foreground/80" : "text-foreground/90"}>{children}</div>}
    </div>
  );
}

/** JSON or text in a scrolling block. */
export function Code({ label, children, className = "" }: { label: string; children: ReactNode; className?: string }) {
  return (
    <pre aria-label={label} className={`max-h-96 overflow-auto rounded-lg border border-border bg-muted/40 p-3 font-mono text-xs leading-relaxed text-foreground ${className}`.trim()}>
      {children}
    </pre>
  );
}

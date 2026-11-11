/** A tenant, or "Instance" for a row that has none (FB-142, FB-146: queue settings, jobs, schedules, workflows and workers carry one). */
export default function Tenant({ value }: { value?: string | null }) {
  return value ? <span className="rounded bg-muted px-1.5 py-0.5 text-xs">{value}</span> : <span className="text-muted-foreground">Instance</span>;
}

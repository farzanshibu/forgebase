/**
 * A job's or workflow's state as the shared status Badge (components/StatusBadge): queued blue, running amber
 * (pulsing), succeeded green, failed red, cancelled grey. The words stay the server's; CSS capitalises them.
 */
import type { JobStatus, WorkflowStatus } from "../../api";
import { Badge } from "../../components/StatusBadge";

export default function StatusBadge({ status }: { status: JobStatus }) {
  return <Badge status={status} className="capitalize" title={status === "dead" ? "Failed every attempt; kept as a dead letter" : undefined} />;
}

/** A workflow's state, in the same colours as a job's. */
export function WorkflowStatusBadge({ status }: { status: WorkflowStatus }) {
  return <Badge status={status} className="capitalize" />;
}

import type { JobStatus } from "../../api";
import { Badge, type BadgeVariant } from "panelui-web/badge";

const TONE: Record<JobStatus, BadgeVariant> = {
  queued: "info",
  running: "warning",
  succeeded: "success",
  dead: "destructive",
  cancelled: "secondary",
};

export default function StatusBadge({ status }: { status: JobStatus }) {
  return (
    <Badge variant={TONE[status]} size="sm" dot={status === "running" ? "pulse" : status !== "cancelled"}>
      {status}
    </Badge>
  );
}

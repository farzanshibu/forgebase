import type { JobStatus } from "../../api";

const TONE: Record<JobStatus, string> = {
  queued: "bg-sky-500/20 text-sky-300",
  running: "bg-amber-500/20 text-amber-300",
  succeeded: "bg-emerald-500/20 text-emerald-300",
  dead: "bg-red-500/20 text-red-300",
  cancelled: "bg-zinc-500/20 text-zinc-300",
};

export default function StatusBadge({ status }: { status: JobStatus }) {
  return <span className={`rounded px-1.5 py-0.5 text-xs ${TONE[status]}`}>{status}</span>;
}

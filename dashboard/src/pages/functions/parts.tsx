/**
 * Small presentational pieces the Functions pages share. `Facts` is the shared one (components/Facts), re-exported
 * for the pages that import it from here.
 *
 *   <LogLines lines={run.logs} label="Run logs" />
 *   <RunStatus status="ok" />
 */
import type { InvokeAccess, LogLine } from "../../api";
import { Badge } from "../../components/StatusBadge";
import { ACCESS_LABEL } from "./util";

export { Facts } from "../../components/Facts";
export type { Fact } from "../../components/Facts";

const LEVEL: Record<string, { text: string; row: string }> = {
  error: { text: "text-destructive", row: "bg-destructive/5" },
  warn: { text: "text-warning", row: "bg-warning/5" },
  warning: { text: "text-warning", row: "bg-warning/5" },
  info: { text: "text-info", row: "" },
  debug: { text: "text-muted-foreground", row: "" },
  trace: { text: "text-muted-foreground", row: "" },
};
const levelTone = (level: string) => LEVEL[level.toLowerCase()] ?? { text: "text-muted-foreground", row: "" };

/**
 * Console output of a run, one row per line: the level in its colour in a fixed column, then the message.
 * Rows are not list items, so a list of runs keeps one item per run.
 */
export function LogLines({ lines, label = "Log lines" }: { lines: LogLine[]; label?: string }) {
  return (
    <div role="group" aria-label={label} className="overflow-x-auto rounded-lg border border-border bg-muted/40 py-1 font-mono text-xs leading-5">
      {lines.map((l, i) => {
        const tone = levelTone(l.level);
        return (
          <div key={i} data-level={l.level} className={`grid grid-cols-[3.5rem_minmax(0,1fr)] gap-3 px-3 ${tone.row}`}>
            <span className={`select-none font-semibold uppercase ${tone.text}`}>{l.level}</span>
            <span className="whitespace-pre-wrap break-words text-foreground">{l.message}</span>
          </div>
        );
      })}
    </div>
  );
}

/** An execution's outcome: green for ok, red for anything else (trap, timeout, error). */
export function RunStatus({ status }: { status: string }) {
  return <Badge tone={status === "ok" ? "ok" : "bad"} dot>{status}</Badge>;
}

/** An HTTP status as a badge: green below 400, amber for 4xx, red for 5xx. */
export function HttpStatus({ status }: { status: number }) {
  return <Badge tone={status < 400 ? "ok" : status < 500 ? "warn" : "bad"} size="md" className="font-mono">HTTP {status}</Badge>;
}

/** Who may call the function over HTTP, as a quiet badge (public is amber: anyone on the internet can call it). */
export function AccessBadge({ access }: { access: InvokeAccess }) {
  return <Badge tone={access === "public" ? "warn" : "default"}>{ACCESS_LABEL[access] ?? access}</Badge>;
}

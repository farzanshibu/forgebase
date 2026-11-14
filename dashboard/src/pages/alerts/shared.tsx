import type { AlertSeverity, AlertState } from "../../api";
import { Badge } from "../../components/StatusBadge";
import { Chip } from "../../components/Chips";
import { SEVERITY_TONE, STATE_TONE } from "./util";

export function SeverityBadge({ severity }: { severity: AlertSeverity }) {
  return <Badge tone={SEVERITY_TONE[severity]} dot>{severity}</Badge>;
}

export function StateBadge({ state }: { state: AlertState }) {
  return <Badge tone={STATE_TONE[state]} dot pulse={state === "firing"}>{state}</Badge>;
}

/** `name=value` chips in a row; nothing when empty. */
export function LabelChips({ labels, label = "Labels" }: { labels: Record<string, string>; label?: string }) {
  const items = Object.entries(labels);
  if (items.length === 0) return null;
  return (
    <ul aria-label={label} className="flex flex-wrap gap-1">
      {items.map(([k, v]) => (
        <li key={k}><Chip title={`${k} = ${v}`}>{k}={v}</Chip></li>
      ))}
    </ul>
  );
}

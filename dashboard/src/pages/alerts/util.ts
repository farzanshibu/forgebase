import type { Alert, AlertSeverity, AlertState, LabelMatcher } from "../../api";
import type { Tone } from "../../components/statusTone";

export const SEVERITY_TONE: Record<AlertSeverity, Tone> = { critical: "bad", warning: "warn", info: "info" };
export const STATE_TONE: Record<AlertState, Tone> = { firing: "bad", pending: "warn", resolved: "neutral" };

/** `key=value` lines to an object; the first `=` splits, blank lines are skipped. Returns the line that has no `=` as `error`. */
export function parseKv(text: string): { value: Record<string, string>; error?: string } {
  const value: Record<string, string> = {};
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const i = line.indexOf("=");
    if (i <= 0) return { value, error: `“${line}” needs the form name=value.` };
    value[line.slice(0, i).trim()] = line.slice(i + 1).trim();
  }
  return { value };
}

export const kvText = (o: Record<string, string> | undefined): string => Object.entries(o ?? {}).map(([k, v]) => `${k}=${v}`).join("\n");

/** Accepts `30s`, `5m`, `2h`, `30d`, `1h30m`: what the server's duration fields take. */
export const isDuration = (s: string): boolean => /^(\d+(ms|s|m|h|d|w))+$/.test(s.trim());

/** A number for a table cell; null (no data, NaN) is a dash. */
export function fmtValue(v: number | null | undefined): string {
  if (v === null || v === undefined || !Number.isFinite(v)) return "—";
  if (Math.abs(v) >= 1000) return v.toLocaleString(undefined, { maximumFractionDigits: 0 });
  return String(Math.round(v * 1000) / 1000);
}

/** `0.99958` as `99.958%`. */
export function pct(ratio: number | null | undefined, digits = 2): string {
  if (ratio === null || ratio === undefined || !Number.isFinite(ratio)) return "—";
  const text = (ratio * 100).toFixed(digits);
  return `${text.includes(".") ? text.replace(/\.?0+$/, "") : text}%`;
}

/** The runbook link of an alert, only when it is a web address (an annotation is free text from a rule). */
export function runbookUrl(annotations: Record<string, string>): string | undefined {
  const u = annotations.runbook_url ?? annotations.runbook;
  return u && /^https?:\/\//i.test(u) ? u : undefined;
}

/**
 * The matchers a silence made from an alert starts with: every label but `severity` (which comes from the rule, not
 * the thing that is failing), so it mutes this alert on this series and nothing wider.
 */
export function matchersFromAlert(a: Pick<Alert, "labels">): LabelMatcher[] {
  return Object.entries(a.labels).filter(([k]) => k !== "severity").map(([label, value]) => ({ label, op: "=" as const, value }));
}

/** Whether a sample value passes a rule's comparison: what makes it an alert. NaN (null) never does. */
export function passes(value: number | null, op: string, threshold: number): boolean {
  if (value === null || !Number.isFinite(value)) return false;
  switch (op) {
    case ">": return value > threshold;
    case ">=": return value >= threshold;
    case "<": return value < threshold;
    case "<=": return value <= threshold;
    case "==": return value === threshold;
    case "!=": return value !== threshold;
    default: return false;
  }
}

/** Matcher lines such as `severity=critical`, `topic=~orders.*` (`=`, `!=`, `=~`, `!~`) to matchers. */
export function parseMatcherLines(text: string): { value: LabelMatcher[]; error?: string } {
  const value: LabelMatcher[] = [];
  for (const raw of text.split("\n")) {
    const line = raw.trim();
    if (!line) continue;
    const m = /^([A-Za-z_][A-Za-z0-9_.-]*)\s*(=~|!~|!=|=)\s*(.+)$/.exec(line);
    if (!m) return { value, error: `“${line}” needs the form label=value (or !=, =~, !~).` };
    value.push({ label: m[1]!, op: m[2] as LabelMatcher["op"], value: m[3]!.trim() });
  }
  return { value };
}

export const matcherLines = (ms: LabelMatcher[]): string => ms.map((m) => `${m.label}${m.op}${m.value}`).join("\n");

/** Comma or newline separated words, trimmed, empty ones dropped. */
export const parseList = (text: string): string[] => text.split(/[\n,]/).map((s) => s.trim()).filter(Boolean);

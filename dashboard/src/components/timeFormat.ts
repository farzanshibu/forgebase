/**
 * Time words for the whole dashboard, used by `When` (Time.tsx): relative times that stay fresh, exact times and
 * run times. A plain .ts file so Time.tsx only exports components (fast refresh).
 *
 * Usage:
 *   relative("2026-11-12T09:55:00Z", Date.now())   // "5 min ago", "in 3 h", "just now"; a date beyond 30 days
 *   absolute("2026-11-12T09:55:00Z")               // "Nov 12, 2026, 10:55:00 AM (2026-11-12 09:55:00Z)"
 *   elapsed(95_000)                                // "1 min 35 s"
 *   const now = useNow();                          // re-renders every 30 s from one shared timer
 */
import { useEffect, useState } from "react";

/** A time the API sent: an ISO string, or a number of seconds since 1970 (Unix time). */
export type TimeValue = string | number | Date;

/** The Date for a value, or undefined when it is missing or not a time. */
export function toDate(v: TimeValue | null | undefined): Date | undefined {
  if (v === null || v === undefined || v === "") return undefined;
  const d = v instanceof Date ? v : typeof v === "number" ? new Date(v * 1000) : new Date(v);
  return Number.isNaN(d.getTime()) ? undefined : d;
}

const listeners = new Set<() => void>();
let timer: ReturnType<typeof setInterval> | undefined;
function subscribe(cb: () => void): () => void {
  listeners.add(cb);
  timer ??= setInterval(() => listeners.forEach((l) => l()), 30_000);
  return () => {
    listeners.delete(cb);
    if (listeners.size === 0 && timer) {
      clearInterval(timer);
      timer = undefined;
    }
  };
}

/** The current time, refreshed every 30 s by one shared timer, so relative times stay right on an open page. */
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => subscribe(() => setNow(Date.now())), []);
  return now;
}

/** A day: "Nov 12, 2026" in the reader's locale. */
export function dayText(v: TimeValue): string {
  const d = toDate(v);
  return d ? d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" }) : String(v);
}

/** "just now", "42 s ago", "5 min ago", "in 3 h", "2 d ago"; a date beyond 30 days. */
export function relative(v: TimeValue, now: number): string {
  const d = toDate(v);
  if (!d) return String(v);
  const diff = Math.round((d.getTime() - now) / 1000);
  const s = Math.abs(diff);
  if (s < 5) return "just now";
  let text: string;
  if (s < 60) text = `${s} s`;
  else if (s < 3600) text = `${Math.round(s / 60)} min`;
  else if (s < 86_400) text = `${Math.round(s / 3600)} h`;
  else if (s < 30 * 86_400) text = `${Math.round(s / 86_400)} d`;
  else return dayText(d);
  return diff > 0 ? `in ${text}` : `${text} ago`;
}

/** The exact time in UTC: "2026-11-12 09:55:00Z". */
export function utcText(v: TimeValue): string {
  const d = toDate(v);
  return d ? d.toISOString().replace("T", " ").replace(/\.\d+Z$/, "Z") : String(v);
}

/** The exact time: local, then UTC in brackets. */
export function absolute(v: TimeValue): string {
  const d = toDate(v);
  if (!d) return String(v);
  return `${d.toLocaleString(undefined, { dateStyle: "medium", timeStyle: "medium" })} (${utcText(d)})`;
}

/** 1500 -> "1.5 s", 95000 -> "1 min 35 s": a run time. */
export function elapsed(ms: number): string {
  if (ms < 1000) return `${Math.max(0, Math.round(ms))} ms`;
  const s = ms / 1000;
  if (s < 60) return `${+s.toFixed(1)} s`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} min ${Math.round(s % 60)} s`;
  return `${Math.floor(m / 60)} h ${m % 60} min`;
}

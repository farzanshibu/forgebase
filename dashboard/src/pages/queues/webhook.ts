/** Pure helpers of the webhook tooling screens (ADR-0031): header masking, grace choices, copy-a-command strings. */
import type { RedriveResult } from "../../api";
import { toaster } from "../../toaster";

/** A success toast with the words as they are ("Delivery resumed"); `notify` adds "saved" or "created" itself. */
export const done = (title: string, description?: string) => toaster.add({ title, description, type: "success" });

const SENSITIVE = /^(authorization|proxy-authorization|cookie|set-cookie|x-api-key)$/i;
const SIGNATURE = /signature|secret|token/i;

/**
 * A header value safe to draw. The server already masks the signature (`t=<ts>,v1=<redacted>`); this keeps it masked even if an
 * older server or a proxy let a real one through: every `v1=` part and any credential header shows as `********`.
 */
export function maskHeader(name: string, value: string): string {
  if (SENSITIVE.test(name)) return "********";
  if (SIGNATURE.test(name)) return value.replace(/(v\d+=)([^,\s]+)/g, (_m, k: string, v: string) => (v === "<redacted>" ? `${k}<redacted>` : `${k}********`));
  return value;
}

/** Grace choices for a secret rotation, in milliseconds (the server allows 0 to 7 days and defaults to 24 hours). */
export const GRACE_OPTIONS = [
  { value: "0", label: "No grace: the old secret stops now" },
  { value: String(3_600_000), label: "1 hour" },
  { value: String(86_400_000), label: "24 hours (default)" },
  { value: String(604_800_000), label: "7 days (longest)" },
];

/** `auto-pause` threshold text to the API value: empty turns it off (0); else a whole number 1 to 1000. */
export function parsePauseAfter(text: string): { ok: true; value: number } | { ok: false; error: string } {
  const t = text.trim();
  if (t === "") return { ok: true, value: 0 };
  if (!/^\d+$/.test(t) || Number(t) < 1 || Number(t) > 1000) return { ok: false, error: "Use a whole number from 1 to 1000, or leave it empty to turn auto-pause off." };
  return { ok: true, value: Number(t) };
}

/** Quote one argument for a POSIX shell. */
export const shq = (s: string) => `'${s.replace(/'/g, `'\\''`)}'`;

/** `forgebase queues listen` for a topic, forwarding to a local URL; a filter limits it to one event type. */
export function listenCommand(topic: string, filter?: unknown): string {
  return `forgebase queues listen ${shq(topic)}${filter ? ` --filter ${shq(JSON.stringify(filter))}` : ""} --forward-to http://localhost:3000/webhooks`;
}

/** A curl that publishes `data` to a topic. The token is a placeholder: the dashboard never prints a credential. */
export function publishCurl(origin: string, topic: string, data: unknown): string {
  return [
    `curl -X POST ${shq(`${origin}/api/v1/queues/topics/${encodeURIComponent(topic)}/publish`)}`,
    `  -H 'Authorization: Bearer $FORGEBASE_TOKEN'`,
    `  -H 'Content-Type: application/json'`,
    `  -d ${shq(JSON.stringify({ messages: [{ data }] }))}`,
  ].join(" \\\n");
}

/** A curl that creates a pull subscription on a topic, optionally for one event type. */
export function subscribeCurl(origin: string, topic: string, filter?: unknown): string {
  return [
    `curl -X POST ${shq(`${origin}/api/v1/queues/subscriptions`)}`,
    `  -H 'Authorization: Bearer $FORGEBASE_TOKEN'`,
    `  -H 'Content-Type: application/json'`,
    `  -d ${shq(JSON.stringify({ name: "my-listener", topic, ...(filter ? { filter } : {}) }))}`,
  ].join(" \\\n");
}

/** The words for a redrive's result: how many were delivered again, and why the others were not. */
export function redriveSummary(r: RedriveResult): string {
  const parts = [`Redriven ${r.redriven.length} dead letter${r.redriven.length === 1 ? "" : "s"}.`];
  if (r.already_redriven.length) parts.push(`${r.already_redriven.length} had been redriven before and were left alone.`);
  if (r.not_found.length) parts.push(`${r.not_found.length} could not be found.`);
  if (r.remaining > 0) parts.push(`${r.remaining} more match: redrive again to deliver them.`);
  return parts.join(" ");
}

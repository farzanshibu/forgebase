/**
 * statusTone: one colour per meaning across the dashboard. A status word from the server maps to a Tone; Badge, Dot,
 * Chip and Stat draw it in the theme's state colours. A plain .ts file, so the component files stay fast-refresh safe.
 *
 * Usage:
 *   statusTone("past_due")          // "bad"
 *   statusLive("running")           // true: the badge's dot pulses
 *   <Badge status={job.status} />   // the usual way: tone, dot and words from the status
 *
 *   green  "ok"       succeeded, paid, active, healthy, enabled, verified …
 *   blue   "info"     queued, open, pending, scheduled, draft …
 *   amber  "warn"     running, processing, warning, expiring … (running states pulse)
 *   red    "bad"      failed, past_due, error, down, suspended, revoked, uncollectible …
 *   grey   "neutral"  cancelled, canceled, void, archived, disabled, paused, expired …
 *   "default" is the outline badge for a kind that is not a state ("Custom", "Admin").
 */
export type Tone = "ok" | "info" | "warn" | "bad" | "neutral" | "default";

const WORDS: Record<Exclude<Tone, "default">, string[]> = {
  ok: [
    "succeeded", "success", "ok", "paid", "active", "healthy", "enabled", "verified", "configured", "processed", "sent",
    "delivered", "current", "completed", "complete", "done", "online", "ready", "installed", "accepted", "applied",
    "passed", "granted", "allow", "allowed", "live", "up", "on", "open_signup", "streaming",
  ],
  info: [
    "queued", "open", "pending", "scheduled", "draft", "trialing", "received", "new", "waiting", "enqueued", "created",
    "compensated", "invited",
  ],
  warn: [
    "running", "processing", "warning", "warn", "expiring", "incomplete", "refunded", "partially_refunded", "grace",
    "on_hold", "retrying", "compensating", "degraded", "starting", "stopping", "syncing", "in_progress", "customised",
    "customized", "lease_expired", "cancel_requested", "lagging",
  ],
  bad: [
    "failed", "failure", "past_due", "error", "down", "suspended", "revoked", "uncollectible", "dead", "timeout",
    "timed_out", "rejected", "invalid", "unhealthy", "unreachable", "denied", "deny", "blocked", "trap", "bounced",
  ],
  neutral: [
    "cancelled", "canceled", "void", "voided", "archived", "disabled", "paused", "expired", "replaced", "ignored",
    "retired", "off", "closed", "inactive", "offline", "skipped", "stopped", "unknown", "none", "superseded", "deleted",
  ],
};

const TONE_OF = new Map<string, Tone>();
for (const [tone, words] of Object.entries(WORDS) as [Tone, string[]][]) for (const w of words) TONE_OF.set(w, tone);

const LIVE = new Set(["running", "processing", "compensating", "retrying", "starting", "stopping", "syncing", "in_progress"]);

/** "Past due", "past-due" and "past_due" are one status. */
const key = (status: string) => status.trim().toLowerCase().replace(/[\s-]+/g, "_");

/** The colour of a status word; an unknown word is grey. */
export function statusTone(status: string): Tone {
  return TONE_OF.get(key(status)) ?? "neutral";
}

/** True for a state that is still changing (running, processing…): its dot pulses. */
export function statusLive(status: string): boolean {
  return LIVE.has(key(status));
}

/** A status code as words: "past_due" reads "past due". */
export const statusWords = (status: string) => status.replace(/_/g, " ");

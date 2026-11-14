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

// ---- Creating a push subscription ------------------------------------------------------------------------------------

/** Longest webhook URL the server accepts, in bytes. */
export const MAX_ENDPOINT_BYTES = 2048;

export interface UrlCheck {
  /** Why the URL cannot work; blocks the form. */
  error?: string;
  /** A caution that does not block: the server decides. */
  warning?: string;
}

const PRIVATE_V4: [number, number, string][] = [
  [0x00000000, 8, "the unspecified range 0.0.0.0/8"], [0x0a000000, 8, "the private range 10.0.0.0/8"], [0x64400000, 10, "the shared range 100.64.0.0/10"],
  [0x7f000000, 8, "the loopback range 127.0.0.0/8"], [0xa9fe0000, 16, "the link-local range 169.254.0.0/16"], [0xac100000, 12, "the private range 172.16.0.0/12"],
  [0xc0a80000, 16, "the private range 192.168.0.0/16"], [0xe0000000, 4, "the multicast and reserved ranges"],
];

/** Why a literal IPv4 address is not a public one, or undefined when it is. */
function blockedV4(host: string): string | undefined {
  const m = /^(\d{1,3})\.(\d{1,3})\.(\d{1,3})\.(\d{1,3})$/.exec(host);
  if (!m || m.slice(1).some((p) => Number(p) > 255)) return undefined;
  const n = m.slice(1).reduce((a, p) => a * 256 + Number(p), 0);
  for (const [base, bits, why] of PRIVATE_V4) {
    if (Math.floor(n / 2 ** (32 - bits)) === Math.floor(base / 2 ** (32 - bits))) return why;
  }
  return undefined;
}

/** Why a literal IPv6 address (no brackets) is not a public one, or undefined when it is. */
function blockedV6(host: string): string | undefined {
  const h = host.toLowerCase();
  if (h === "::1") return "the loopback address ::1";
  if (h === "::") return "the unspecified address ::";
  if (/^f[cd]/.test(h)) return "the private range fc00::/7";
  if (/^fe[89ab]/.test(h)) return "the link-local range fe80::/10";
  if (/^ff/.test(h)) return "the multicast range ff00::/8";
  const mapped = /^::ffff:(\d+\.\d+\.\d+\.\d+)$/.exec(h);
  if (mapped) return blockedV4(mapped[1]!);
  // `URL` writes an IPv4-mapped address as two hex groups: ::ffff:a00:1 is 10.0.0.1.
  const hex = /^::ffff:([0-9a-f]{1,4}):([0-9a-f]{1,4})$/.exec(h);
  if (hex) {
    const hi = parseInt(hex[1]!, 16), lo = parseInt(hex[2]!, 16);
    return blockedV4(`${hi >> 8}.${hi & 255}.${lo >> 8}.${lo & 255}`);
  }
  return undefined;
}

/**
 * Checks a webhook URL the way the server does when it creates the subscription: at most 2048 bytes, https, no user name or
 * password, a host, and not a literal loopback, private or reserved address. `http://` is only a warning, because the server
 * accepts it when `[queue] allow_http_push` is on and the dashboard cannot read that setting. Names are not resolved here: the
 * server vets them at delivery. The server's answer stays authoritative.
 */
export function checkWebhookUrl(text: string): UrlCheck {
  const t = text.trim();
  if (t === "") return { error: "Enter the URL ForgeBase should call." };
  if (new TextEncoder().encode(t).length > MAX_ENDPOINT_BYTES) return { error: "The URL is longer than 2048 bytes." };
  let url: URL;
  try {
    url = new URL(t);
  } catch {
    return { error: "That does not look like a URL. Start it with https://." };
  }
  let warning: string | undefined;
  if (url.protocol === "http:") warning = "http:// is refused unless the server runs with [queue] allow_http_push on. Use https:// for anything outside local development.";
  else if (url.protocol !== "https:") return { error: "Webhook URLs must start with https://." };
  if (url.username || url.password) return { error: "Remove the user name and password from the URL: credentials are not allowed. Put a token in the path or verify the signature instead." };
  const host = url.hostname.replace(/^\[|\]$/g, "");
  if (host === "") return { error: "The URL has no host." };
  if (host === "localhost" || host.endsWith(".localhost")) return { error: "localhost cannot be reached from the server's network. Use a public address, or forgebase queues listen to test locally." };
  const bad = host.includes(":") ? blockedV6(host) : blockedV4(host);
  if (bad) return { error: `${host} is in ${bad}. Push endpoints must be public addresses.` };
  return warning ? { warning } : {};
}

/** A function name for a push endpoint: 1 to 48 lower-case letters, digits, dashes or underscores, starting with a letter. */
export const FUNCTION_NAME = /^[a-z][a-z0-9_-]{0,47}$/;

const UNIT_MS: Record<string, number> = { s: 1000, m: 60_000, h: 3_600_000, d: 86_400_000 };
/** Shortest and longest subscription lifetime the server accepts (`ttl_ms`). */
export const MIN_TTL_MS = 60_000;
export const MAX_TTL_MS = 31 * 86_400_000;

/**
 * A lifetime typed by a person: `30m`, `2h`, `7d`, `1h 30m`, `90s`. Empty means no expiry (the subscription lives until deleted).
 * Bounded like the server: 60 seconds to 31 days.
 */
export function parseTtl(text: string): { ok: true; ms?: number } | { ok: false; error: string } {
  const t = text.trim().toLowerCase();
  if (t === "") return { ok: true };
  const parts = t.split(/\s+/);
  let ms = 0;
  for (const p of parts) {
    const m = /^(\d+(?:\.\d+)?)([smhd])$/.exec(p);
    if (!m) return { ok: false, error: "Use a number and a unit: 90s, 30m, 2h, 7d, or 1h 30m." };
    ms += Number(m[1]) * UNIT_MS[m[2]!]!;
  }
  ms = Math.round(ms);
  if (ms < MIN_TTL_MS) return { ok: false, error: "At least 1 minute (60s)." };
  if (ms > MAX_TTL_MS) return { ok: false, error: "At most 31 days (31d)." };
  return { ok: true, ms };
}

/** Seconds typed in a field to milliseconds: empty is absent, otherwise a number above 0. */
export function parseSeconds(text: string, label: string): { ok: true; ms?: number } | { ok: false; error: string } {
  const t = text.trim();
  if (t === "") return { ok: true };
  const n = Number(t);
  if (!(n > 0) || !Number.isFinite(n)) return { ok: false, error: `${label}: use a number above 0.` };
  return { ok: true, ms: Math.round(n * 1000) };
}

/**
 * The signing secret of a webhook subscription that was just created, handed from the New subscription sheet to the subscription's
 * page. It lives in memory only (never in storage, history state or the URL) and is read for one page view: the page copies it into its
 * own state and clears this slot, so a reload, a revisit or the back button cannot show it again.
 */
const pendingSecrets = new Map<string, string>();
export const stashSecret = (sub: string, secret: string) => void pendingSecrets.set(sub, secret);
export const peekSecret = (sub: string) => pendingSecrets.get(sub);
export const clearSecret = (sub: string) => void pendingSecrets.delete(sub);

/** What a receiver runs to check a delivery: the Node SDK, and the CLI that forwards deliveries to localhost while you build. */
export function verifySnippet(): string {
  return [
    `import { constructWebhookEvent } from "@forgebase/sdk/webhooks";`,
    ``,
    `const event = await constructWebhookEvent({`,
    `  secret: process.env.FORGEBASE_WEBHOOK_SECRET!, // the whsec_… value above`,
    `  header: request.headers.get("x-forgebase-signature"),`,
    `  body: await request.text(), // the raw body, never re-serialised JSON`,
    `});`,
    `// throws WebhookVerificationError when the signature does not match: answer 400`,
  ].join("\n");
}

/** A curl that sends the receiver a delivery signed the way the server does, to try the receiver's check by hand. */
export function signedCurl(url: string): string {
  return [
    `body='{"hello":"world"}'; ts=$(date +%s)`,
    `sig=$(printf '%s.%s' "$ts" "$body" | openssl dgst -sha256 -hmac "$FORGEBASE_WEBHOOK_SECRET" -hex | sed 's/^.* //')`,
    `curl -X POST ${shq(url)} \\`,
    `  -H "X-Forgebase-Signature: t=$ts,v1=$sig" -H 'Content-Type: application/json' -d "$body"`,
  ].join("\n");
}

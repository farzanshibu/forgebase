import { WebhookVerificationError } from "./errors";
import type { QueueMessage } from "./queues";

/*
 * Webhook verification for push subscriptions (ADR-0022 §13).
 *
 * The server POSTs `{subscription, delivery_attempt, message}` as JSON with
 *
 *   X-Forgebase-Signature: t=<unix seconds>,v1=<hex HMAC-SHA256(secret, "<t>.<raw body>")>
 *
 * The key is the UTF-8 text of the subscription's secret (`whsec_...`, prefix included). While a secret is
 * being rotated the header may carry several `v1=` values, one per secret that is still valid.
 *
 * Everything here runs on Web Crypto (`globalThis.crypto.subtle`): Node 18+, Deno, Bun, Cloudflare Workers,
 * Vercel Edge and browsers. No Node `crypto` import, no client, no network.
 */

/** Header the server signs push requests with (HTTP header names are case-insensitive). */
export const WEBHOOK_SIGNATURE_HEADER = "X-Forgebase-Signature";

/** Default accepted clock distance between the signed timestamp and `now`, in seconds. */
export const DEFAULT_WEBHOOK_TOLERANCE_SECONDS = 300;

/** The raw request body. Pass the bytes (or the text) exactly as received: never a parsed and re-serialised object. */
export type WebhookBody = string | ArrayBuffer | ArrayBufferView;

/** What a push subscription delivers: the body of the POST. */
export interface WebhookEvent<T = unknown> {
  /** Subscription name (without the tenant prefix). */
  subscription: string;
  /** 1 on the first delivery, then counts up with every retry. */
  delivery_attempt: number;
  message: QueueMessage<T>;
}

export interface VerifyWebhookOptions {
  /**
   * The subscription's signing secret (`whsec_...`, shown once when the subscription is created). An array
   * accepts any of several secrets, e.g. the new and the old one while you roll the secret over.
   */
  secret: string | readonly string[];
  /** Value of the `X-Forgebase-Signature` header (`request.headers.get(...)`, `req.headers[...]`). */
  header: string | null | undefined;
  /** The raw body of the request. */
  body: WebhookBody;
  /**
   * How far the signed timestamp may be from `now`, in seconds, in either direction (default 300). Bounds
   * the window in which a captured request can be replayed. `Infinity` turns the check off (tests only).
   */
  toleranceSeconds?: number;
  /** The current time as a `Date` or milliseconds since the epoch (like `Date.now()`); default: the clock. */
  now?: Date | number;
}

export interface SignWebhookOptions {
  secret: string;
  body: WebhookBody;
  /** Signing time in Unix **seconds** (default: now). */
  timestamp?: number;
}

/** Why a request failed verification; also on `WebhookVerificationError.reason`. */
export type WebhookFailureReason = WebhookVerificationError["reason"];

/** `true` when the signature is valid; the reason is on the error of {@link constructWebhookEvent}. */
export async function verifyWebhookSignature(opts: VerifyWebhookOptions): Promise<boolean> {
  return (await check(opts)) === undefined;
}

/**
 * Verifies the request, then parses the body into the push envelope. Throws {@link WebhookVerificationError}
 * (`reason` says why) when the signature, the timestamp or the payload is not acceptable; answer such a request
 * with a 4xx so the server does not count it as delivered.
 */
export async function constructWebhookEvent<T = unknown>(opts: VerifyWebhookOptions): Promise<WebhookEvent<T>> {
  const failure = await check(opts);
  if (failure) throw failure;
  let parsed: unknown;
  try {
    parsed = JSON.parse(new TextDecoder("utf-8", { fatal: true }).decode(toBytes(opts.body)));
  } catch {
    throw new WebhookVerificationError("invalid_payload", "the webhook body is not valid JSON");
  }
  if (!isEnvelope(parsed)) {
    throw new WebhookVerificationError("invalid_payload", "the webhook body is not a {subscription, delivery_attempt, message} envelope");
  }
  return parsed as WebhookEvent<T>;
}

/**
 * Builds the header value the server would send: `t=<timestamp>,v1=<hex>`. For tests of your own handler, or
 * to drive it from a script; the server signs with the same function.
 */
export async function signWebhook(opts: SignWebhookOptions): Promise<string> {
  const ts = opts.timestamp ?? Math.floor(Date.now() / 1000);
  if (!Number.isSafeInteger(ts) || ts < 0) throw new RangeError("timestamp must be a non-negative integer (Unix seconds)");
  return `t=${ts},v1=${toHex(await mac(opts.secret, ts, toBytes(opts.body)))}`;
}

async function check(opts: VerifyWebhookOptions): Promise<WebhookVerificationError | undefined> {
  const { header } = opts;
  if (header === null || header === undefined || header.trim() === "") {
    return new WebhookVerificationError("missing_header", `the ${WEBHOOK_SIGNATURE_HEADER} header is missing`);
  }
  const parsed = parseHeader(header);
  if (!parsed) return new WebhookVerificationError("malformed_header", `the ${WEBHOOK_SIGNATURE_HEADER} header is malformed`);

  const tolerance = opts.toleranceSeconds ?? DEFAULT_WEBHOOK_TOLERANCE_SECONDS;
  if (!(tolerance >= 0)) throw new RangeError("toleranceSeconds must be zero or more");
  const nowSecs = Math.floor((opts.now === undefined ? Date.now() : opts.now instanceof Date ? opts.now.getTime() : opts.now) / 1000);
  if (Math.abs(nowSecs - parsed.timestamp) > tolerance) {
    return new WebhookVerificationError("timestamp_out_of_tolerance", "the webhook timestamp is outside the tolerance window");
  }

  const body = toBytes(opts.body);
  const secrets = typeof opts.secret === "string" ? [opts.secret] : opts.secret;
  let ok = false;
  // Every secret and every candidate is compared, whatever matched first, so timing says nothing about which one did.
  for (const secret of secrets) {
    const expected = await mac(secret, parsed.timestamp, body);
    for (const candidate of parsed.signatures) ok = constantTimeEqual(expected, candidate) || ok;
  }
  return ok ? undefined : new WebhookVerificationError("signature_mismatch", "no signature matches the payload and secret");
}

interface ParsedHeader {
  timestamp: number;
  signatures: Uint8Array[];
}

/** `t=<secs>,v1=<hex>[,v1=<hex>...]`; unknown keys (future schemes) are ignored; undecodable `v1` values never match. */
function parseHeader(header: string): ParsedHeader | undefined {
  let timestamp: number | undefined;
  const signatures: Uint8Array[] = [];
  let sawV1 = false;
  for (const part of header.split(",")) {
    const eq = part.indexOf("=");
    if (eq < 0) continue;
    const key = part.slice(0, eq).trim();
    const value = part.slice(eq + 1).trim();
    if (key === "t") {
      if (timestamp !== undefined || !/^\d{1,15}$/.test(value)) return undefined;
      timestamp = Number(value);
    } else if (key === "v1") {
      sawV1 = true;
      const bytes = fromHex(value);
      if (bytes) signatures.push(bytes);
    }
  }
  if (timestamp === undefined || !sawV1) return undefined;
  return { timestamp, signatures };
}

function subtle(): SubtleCrypto {
  const s = (globalThis as { crypto?: Crypto }).crypto?.subtle;
  if (!s) throw new Error("@forgebase/sdk webhooks need Web Crypto (globalThis.crypto.subtle): Node 18+, Deno, Bun or an edge runtime");
  return s;
}

/** HMAC-SHA256(secret, "<ts>.<body>"). */
async function mac(secret: string, ts: number, body: Uint8Array): Promise<Uint8Array> {
  const prefix = new TextEncoder().encode(`${ts}.`);
  const message = new Uint8Array(prefix.length + body.length);
  message.set(prefix, 0);
  message.set(body, prefix.length);
  const key = await subtle().importKey("raw", new TextEncoder().encode(secret), { name: "HMAC", hash: "SHA-256" }, false, ["sign"]);
  return new Uint8Array(await subtle().sign("HMAC", key, message));
}

function toBytes(body: WebhookBody): Uint8Array {
  if (typeof body === "string") return new TextEncoder().encode(body);
  if (body instanceof ArrayBuffer) return new Uint8Array(body);
  if (ArrayBuffer.isView(body)) return new Uint8Array(body.buffer, body.byteOffset, body.byteLength);
  throw new TypeError("webhook body must be the raw request body: a string, ArrayBuffer or Uint8Array/Buffer (not a parsed object)");
}

function constantTimeEqual(a: Uint8Array, b: Uint8Array): boolean {
  // A length mismatch is public information (the digest is always 32 bytes); the loop covers the longer side anyway.
  let diff = a.length ^ b.length;
  const n = Math.max(a.length, b.length);
  for (let i = 0; i < n; i++) diff |= (a[i] ?? 0) ^ (b[i] ?? 0);
  return diff === 0;
}

function fromHex(hex: string): Uint8Array | undefined {
  if (hex.length === 0 || hex.length % 2 !== 0 || !/^[0-9a-fA-F]+$/.test(hex)) return undefined;
  const out = new Uint8Array(hex.length / 2);
  for (let i = 0; i < out.length; i++) out[i] = parseInt(hex.slice(2 * i, 2 * i + 2), 16);
  return out;
}

function toHex(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += b.toString(16).padStart(2, "0");
  return s;
}

function isEnvelope(v: unknown): v is WebhookEvent {
  if (typeof v !== "object" || v === null) return false;
  const o = v as Record<string, unknown>;
  return typeof o.subscription === "string" && typeof o.delivery_attempt === "number" && typeof o.message === "object" && o.message !== null;
}

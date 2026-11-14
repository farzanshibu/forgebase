/** Typed client for `/api/v1/queues` (durable topics, subscriptions, snapshots; ADR-0022). */
import { ApiError, request, send } from "./client";
import { authHeaders } from "./token";

const Q = "/api/v1/queues";
const enc = encodeURIComponent;

export interface RecordSource { collection: string; ops: string[] }
export interface QueueTopic {
  name: string; retention_ms: number; retention_bytes?: number | null; max_message_bytes: number; dedup_window_ms: number;
  schema?: unknown; source?: RecordSource | null; labels: Record<string, string>; published_total: number; published_bytes: number;
  created_by: string; created_at: string; updated_at: string;
  /** Tenant the topic belongs to (FB-141); its name is then `<tenant>/<name>`. Absent or null: instance-wide. */
  tenant?: string | null;
  /** What the topic's events are (event catalog, ADR-0031 §6). */
  description?: string | null;
  /** An example of message `data`, checked against the schema. */
  example?: unknown;
}
export interface NewTopic {
  name: string; retention_ms?: number; retention_bytes?: number; max_message_bytes?: number; dedup_window_ms?: number; schema?: unknown; labels?: Record<string, string>;
  description?: string; example?: unknown;
}
/** `PATCH /queues/topics/{topic}`: absent fields keep their value; an empty `description` removes it, `clear_example` removes the example. */
export interface TopicPatch { description?: string; example?: unknown; clear_example?: boolean }
export interface DeliveryAttempt { attempt: number; at: string; consumer?: string | null; outcome?: string | null; error?: string | null }
export interface QueueMessage {
  id: string; topic: string; seq: number; data: unknown; attributes: Record<string, string>; ordering_key?: string | null; dedup_id?: string | null;
  publisher: string; bytes: number; published_at: string; delivery_history?: DeliveryAttempt[] | null;
}
export interface MessagePage { items: QueueMessage[]; next_after?: number | null }
export interface NewMessage { data: unknown; attributes?: Record<string, string>; ordering_key?: string; dedup_id?: string }
export interface Published { message_id: string; seq: number; duplicate: boolean }
export interface PushConfig { kind: "webhook" | "function"; endpoint: string }
export interface QueueSubscription {
  name: string; topic: string; filter?: unknown; ack_deadline_ms: number; max_delivery_attempts?: number | null; backoff_base_ms: number; backoff_max_ms: number;
  dead_letter_topic?: string | null; ordering: boolean; push?: PushConfig | null; labels: Record<string, string>; created_by: string; created_at: string; updated_at: string;
  /** Tenant of the subscription (FB-141). `created_by` is its owner: record messages are delivered as that principal sees them. */
  tenant?: string | null;
  /** End of the grace window of the secret a rotation replaced: until then deliveries carry both signatures (ADR-0031 §4). */
  push_secret_previous_expires_at?: string | null;
  /** Auto-pause (circuit breaker) after this many consecutive failed attempts. */
  pause_after_failures?: number | null;
  /** When push delivery was paused; messages accumulate until it is resumed. */
  paused_at?: string | null;
  /** Why: `auto: …` for the circuit breaker, else the operator's words. */
  paused_reason?: string | null;
  expires_at?: string | null;
  /** Returned only when a webhook subscription is created. */
  push_secret?: string;
}
/** `PATCH /queues/subscriptions/{sub}`: `pause_after_failures: 0` turns auto-pause off. */
export interface SubscriptionPatch { pause_after_failures?: number }
export interface NewSubscription {
  name: string; topic: string; filter?: unknown; ack_deadline_ms?: number; max_delivery_attempts?: number; backoff_base_ms?: number; backoff_max_ms?: number;
  dead_letter_topic?: string; ordering?: boolean; start?: "latest" | "earliest" | { time: string };
  /** Push delivery instead of pull: a webhook URL, or the name of a function. */
  push?: PushConfig;
  /** Push: pause delivery after this many consecutive failed attempts (1 to 1000; default never). */
  pause_after_failures?: number;
  /** Delete the subscription this long after creation (60 s to 31 days; default never). */
  ttl_ms?: number;
}
export interface TopicMetrics {
  topic: string; messages: number; bytes: number; oldest_message_age_ms?: number | null; published_total: number; published_bytes: number;
  published_last_minute: number; publish_rate_per_sec: number; subscriptions: number;
}
export interface SubscriptionMetrics {
  subscription: string; topic: string; backlog_messages: number; backlog_bytes: number; oldest_unacked_age_ms?: number | null; ready: number; delayed: number;
  in_flight: number; delivered_total: number; acked_total: number; nacked_total: number; expired_total: number; dead_lettered_total: number; dead_letters: number;
  acked_last_minute: number; ack_rate_per_sec: number;
  /** Endpoint health of a push subscription (absent for pull). */
  push?: PushHealth | null;
}
/** Endpoint health over the last hour (newest 1000 attempts, test sends excluded) and the circuit-breaker state. */
export interface PushHealth {
  window_ms: number; attempts: number; successes: number; failures: number; success_rate?: number | null;
  latency_p50_ms?: number | null; latency_p95_ms?: number | null; latency_p99_ms?: number | null;
  consecutive_failures: number; last_success_at?: string | null; last_failure_at?: string | null; last_error?: string | null;
  paused: boolean; paused_at?: string | null; paused_reason?: string | null; pause_after_failures?: number | null;
}
export type PushOutcome = "success" | "failure";
export const PUSH_ERROR_CLASSES = ["timeout", "dns", "connect", "tls", "ssrf_refused", "http_status", "redirect", "protocol", "invalid_request", "config", "function"] as const;
export type PushErrorClass = (typeof PUSH_ERROR_CLASSES)[number];
/** One call of a push endpoint: a delivery or a test send (ADR-0031 §1). The signature header is masked by the server. */
export interface PushAttempt {
  id: number; subscription: string; message_id: string; attempt: number; test: boolean; kind: "webhook" | "function"; endpoint: string; at: string; latency_ms: number;
  outcome: PushOutcome; status?: number | null; error_class?: PushErrorClass | null; error?: string | null;
  request_headers: Record<string, string>; response_headers: Record<string, string>;
  response_body?: string | null; response_bytes?: number | null; response_truncated: boolean;
}
export interface PushAttemptPage { items: PushAttempt[]; next_before?: number | null }
export interface AttemptFilter { outcome?: PushOutcome | ""; message_id?: string; test?: boolean | ""; since?: string; until?: string; before?: number; limit?: number }
export interface RedriveRequest { ids?: number[]; all?: boolean; since?: string; filter?: unknown; limit?: number }
export interface RedriveResult { redriven: number[]; already_redriven: number[]; not_found: number[]; remaining: number }
export interface RotatedSecret { subscription: string; push_secret: string; previous_expires_at?: string | null }
export type CatalogExampleSource = "topic" | "schema" | "none";
export interface CatalogEventType { type: string; op: string; description: string; filter: unknown; data_schema: unknown; example: unknown }
export interface CatalogTopic {
  topic: string; tenant?: string | null; description?: string | null; data_schema?: unknown; example: unknown; example_source: CatalogExampleSource;
  source?: RecordSource | null; event_types: CatalogEventType[];
}
export interface DeadLetter {
  id: number; subscription: string; topic: string; message_id: string; data: unknown; attributes: Record<string, string>; ordering_key?: string | null; published_at: string;
  attempts: number; history: DeliveryAttempt[]; reason: string; dead_letter_topic?: string | null; forwarded_message_id?: string | null; at: string;
  /** Set once the record was redriven to the subscription; a record is redriven once. */
  redriven_at?: string | null; redriven_by?: string | null;
}
export interface ReceivedMessage { ack_id: string; delivery_attempt: number; ack_deadline: string; message: QueueMessage }
export interface AckResult { count: number; unknown: string[] }
export interface SeekResult { cursor_seq: number; restored: number }
export interface QueueSnapshot { name: string; subscription: string; topic: string; cursor_seq: number; unacked: number; created_by: string; created_at: string; expires_at: string; tenant?: string | null }

export const QUEUE_OPERATIONS = [
  "get /api/v1/queues/topics", "post /api/v1/queues/topics", "get /api/v1/queues/topics/{topic}", "delete /api/v1/queues/topics/{topic}",
  "post /api/v1/queues/topics/{topic}/publish", "get /api/v1/queues/topics/{topic}/messages", "get /api/v1/queues/topics/{topic}/metrics",
  "get /api/v1/queues/subscriptions", "post /api/v1/queues/subscriptions", "get /api/v1/queues/subscriptions/{sub}", "delete /api/v1/queues/subscriptions/{sub}",
  "post /api/v1/queues/subscriptions/{sub}/pull", "post /api/v1/queues/subscriptions/{sub}/ack", "post /api/v1/queues/subscriptions/{sub}/nack",
  "post /api/v1/queues/subscriptions/{sub}/seek", "get /api/v1/queues/subscriptions/{sub}/metrics", "get /api/v1/queues/subscriptions/{sub}/dead-letters",
  "get /api/v1/queues/subscriptions/{sub}/events", "get /api/v1/queues/snapshots", "post /api/v1/queues/snapshots", "delete /api/v1/queues/snapshots/{snapshot}",
] as const;

const items = async <T>(p: Promise<{ items: T[] }>) => (await p).items;

export const listQueueTopics = (signal?: AbortSignal) => items(request<{ items: QueueTopic[] }>(`${Q}/topics`, { signal }));
export const getQueueTopic = (name: string, signal?: AbortSignal) => request<QueueTopic>(`${Q}/topics/${enc(name)}`, { signal });
/** `tenant` creates the topic in that tenant (header `X-ForgeBase-Tenant`, FB-141); without it the caller's own tenant, or instance-wide for an administrator. */
const tenantHeader = (tenant?: string) => (tenant ? { "X-ForgeBase-Tenant": tenant } : undefined);
export const createQueueTopic = (t: NewTopic, tenant?: string) => send<QueueTopic>("POST", `${Q}/topics`, t, tenantHeader(tenant));
export const deleteQueueTopic = (name: string, force = false) => send<void>("DELETE", `${Q}/topics/${enc(name)}${force ? "?force=true" : ""}`);
export const queueTopicMetrics = (name: string, signal?: AbortSignal) => request<TopicMetrics>(`${Q}/topics/${enc(name)}/metrics`, { signal });
export const queueTopicMessages = (name: string, after = 0, limit = 25, signal?: AbortSignal) =>
  request<MessagePage>(`${Q}/topics/${enc(name)}/messages?after=${after}&limit=${limit}`, { signal });
export const publishQueueMessages = async (name: string, messages: NewMessage[]) =>
  (await send<{ results: Published[] }>("POST", `${Q}/topics/${enc(name)}/publish`, { messages })).results;

export const listQueueSubscriptions = (topic?: string, signal?: AbortSignal) =>
  items(request<{ items: QueueSubscription[] }>(topic ? `${Q}/subscriptions?topic=${enc(topic)}` : `${Q}/subscriptions`, { signal }));
export const getQueueSubscription = (name: string, signal?: AbortSignal) => request<QueueSubscription>(`${Q}/subscriptions/${enc(name)}`, { signal });
export const createQueueSubscription = (s: NewSubscription, tenant?: string) => send<QueueSubscription>("POST", `${Q}/subscriptions`, s, tenantHeader(tenant));
export const deleteQueueSubscription = (name: string) => send<void>("DELETE", `${Q}/subscriptions/${enc(name)}`);
export const queueSubscriptionMetrics = (name: string, signal?: AbortSignal) => request<SubscriptionMetrics>(`${Q}/subscriptions/${enc(name)}/metrics`, { signal });
export const patchQueueTopic = (name: string, patch: TopicPatch) => send<QueueTopic>("PATCH", `${Q}/topics/${enc(name)}`, patch);
export const patchQueueSubscription = (name: string, patch: SubscriptionPatch) => send<QueueSubscription>("PATCH", `${Q}/subscriptions/${enc(name)}`, patch);
export const queueDeadLetters = (name: string, before?: number, limit = 25, signal?: AbortSignal) =>
  items(request<{ items: DeadLetter[] }>(`${Q}/subscriptions/${enc(name)}/dead-letters?limit=${limit}${before ? `&before=${before}` : ""}`, { signal }));
export const pullQueueMessages = async (name: string, max = 10) =>
  (await send<{ messages: ReceivedMessage[] }>("POST", `${Q}/subscriptions/${enc(name)}/pull`, { max_messages: max, consumer: "studio" })).messages;
export const ackQueueMessages = (name: string, ackIds: string[]) => send<AckResult>("POST", `${Q}/subscriptions/${enc(name)}/ack`, { ack_ids: ackIds });
export const nackQueueMessages = (name: string, ackIds: string[]) => send<AckResult>("POST", `${Q}/subscriptions/${enc(name)}/nack`, { ack_ids: ackIds });
export const seekQueueSubscription = (name: string, to: { time: string } | { snapshot: string }) => send<SeekResult>("POST", `${Q}/subscriptions/${enc(name)}/seek`, to);
export const listQueueSnapshots = (signal?: AbortSignal) => items(request<{ items: QueueSnapshot[] }>(`${Q}/snapshots`, { signal }));
export const createQueueSnapshot = (name: string, subscription: string, tenant?: string) => send<QueueSnapshot>("POST", `${Q}/snapshots`, { name, subscription }, tenantHeader(tenant));
export const deleteQueueSnapshot = (name: string) => send<void>("DELETE", `${Q}/snapshots/${enc(name)}`);

// Webhook tooling (ADR-0031): attempt log, test send, redrive, retry now, secret rotation, pause and resume, event catalog.
const qs = (p: Record<string, string | number | boolean | undefined>) => {
  const q = new URLSearchParams();
  for (const [k, v] of Object.entries(p)) if (v !== undefined && v !== "") q.set(k, String(v));
  const s = q.toString();
  return s ? `?${s}` : "";
};
export const listPushAttempts = (name: string, f: AttemptFilter = {}, signal?: AbortSignal) =>
  request<PushAttemptPage>(`${Q}/subscriptions/${enc(name)}/attempts${qs({ ...f, limit: f.limit ?? 50 })}`, { signal });
export const getPushAttempt = (name: string, id: number, signal?: AbortSignal) => request<PushAttempt>(`${Q}/subscriptions/${enc(name)}/attempts/${id}`, { signal });
/** Sends a synthetic signed `forgebase.test` event now and returns the attempt (its `outcome` says whether the endpoint accepted it). 429 `rate_limited`, 409 `not_push_subscription`. */
export const testPushEndpoint = (name: string, data?: unknown) => send<PushAttempt>("POST", `${Q}/subscriptions/${enc(name)}/test`, data === undefined ? {} : { data });
export const redriveDeadLetters = (name: string, req: RedriveRequest) => send<RedriveResult>("POST", `${Q}/subscriptions/${enc(name)}/redrive`, req);
/** Deliveries waiting for a retry backoff become ready now (all, or the given message ids). */
export const retryQueueNow = (name: string, messageIds?: string[]) => send<{ count: number }>("POST", `${Q}/subscriptions/${enc(name)}/retry`, messageIds?.length ? { message_ids: messageIds } : {});
/** Rotates the webhook signing secret: the old one keeps signing for `graceMs` (0 to 7 days, server default 24 h). The new secret is in this answer only. */
export const rotateQueueSecret = (name: string, graceMs?: number) => send<RotatedSecret>("POST", `${Q}/subscriptions/${enc(name)}/secret/rotate`, graceMs === undefined ? {} : { grace_ms: graceMs });
export const expirePreviousQueueSecret = (name: string) => send<QueueSubscription>("POST", `${Q}/subscriptions/${enc(name)}/secret/expire-previous`, {});
export const pauseQueueSubscription = (name: string, reason?: string) => send<QueueSubscription>("POST", `${Q}/subscriptions/${enc(name)}/pause`, reason ? { reason } : {});
export const resumeQueueSubscription = (name: string) => send<QueueSubscription>("POST", `${Q}/subscriptions/${enc(name)}/resume`, {});
export const queueEventCatalog = async (signal?: AbortSignal) => (await request<{ topics: CatalogTopic[] }>(`${Q}/catalog`, { signal })).topics;

/**
 * Streaming pull over server-sent events (`GET .../events`) until `signal` aborts: one `ReceivedMessage` per delivery, acknowledged with `ackQueueMessages`.
 * Resolves when the stream ends; rejects with `ApiError` when the server refuses it.
 */
export async function streamQueueSubscription(name: string, onMessage: (m: ReceivedMessage) => void, signal: AbortSignal, opts: { maxMessages?: number } = {}): Promise<void> {
  const q = opts.maxMessages ? `?max_messages=${opts.maxMessages}` : "";
  let res: Response;
  try {
    res = await fetch(`${Q}/subscriptions/${enc(name)}/events${q}`, { headers: { Accept: "text/event-stream", ...(await authHeaders()) }, signal, credentials: "same-origin" });
  } catch (err) {
    if (err instanceof DOMException && err.name === "AbortError") return;
    throw new ApiError({ status: 0, code: "network_error", message: err instanceof Error ? err.message : "Network request failed" });
  }
  if (!res.ok || !res.body) {
    let code = "http_error", message = res.statusText || `HTTP ${res.status}`;
    try { const b = (await res.json()) as { error?: { code?: string; message?: string } }; if (b.error?.code) { code = b.error.code; message = b.error.message ?? message; } } catch { /* not JSON */ }
    throw new ApiError({ status: res.status, code, message });
  }
  const reader = res.body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  const dispatch = (block: string) => {
    let event = "message", data = "";
    for (const line of block.split("\n")) {
      if (line.startsWith("event:")) event = line.slice(6).trim();
      else if (line.startsWith("data:")) data += (data ? "\n" : "") + line.slice(5).replace(/^ /, "");
    }
    if (event === "error") {
      let code = "stream_error", message = data;
      try { const e = JSON.parse(data) as { code?: string; message?: string }; code = e.code ?? code; message = e.message ?? message; } catch { /* plain text */ }
      throw new ApiError({ status: 0, code, message });
    }
    if (event === "message" && data) {
      try { onMessage(JSON.parse(data) as ReceivedMessage); } catch { /* malformed event */ }
    }
  };
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true }).replace(/\r\n/g, "\n");
      let i: number;
      while ((i = buf.indexOf("\n\n")) >= 0) { dispatch(buf.slice(0, i)); buf = buf.slice(i + 2); }
    }
  } catch (err) {
    if (!(err instanceof DOMException && err.name === "AbortError")) throw err;
  }
}

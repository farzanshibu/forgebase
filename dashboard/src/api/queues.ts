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
}
export interface NewTopic {
  name: string; retention_ms?: number; retention_bytes?: number; max_message_bytes?: number; dedup_window_ms?: number; schema?: unknown; labels?: Record<string, string>;
}
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
}
export interface NewSubscription {
  name: string; topic: string; filter?: unknown; ack_deadline_ms?: number; max_delivery_attempts?: number; backoff_base_ms?: number; backoff_max_ms?: number;
  dead_letter_topic?: string; ordering?: boolean; start?: "latest" | "earliest" | { time: string };
}
export interface TopicMetrics {
  topic: string; messages: number; bytes: number; oldest_message_age_ms?: number | null; published_total: number; published_bytes: number;
  published_last_minute: number; publish_rate_per_sec: number; subscriptions: number;
}
export interface SubscriptionMetrics {
  subscription: string; topic: string; backlog_messages: number; backlog_bytes: number; oldest_unacked_age_ms?: number | null; ready: number; delayed: number;
  in_flight: number; delivered_total: number; acked_total: number; nacked_total: number; expired_total: number; dead_lettered_total: number; dead_letters: number;
  acked_last_minute: number; ack_rate_per_sec: number;
}
export interface DeadLetter {
  id: number; subscription: string; topic: string; message_id: string; data: unknown; attributes: Record<string, string>; ordering_key?: string | null; published_at: string;
  attempts: number; history: DeliveryAttempt[]; reason: string; dead_letter_topic?: string | null; forwarded_message_id?: string | null; at: string;
}
export interface ReceivedMessage { ack_id: string; delivery_attempt: number; ack_deadline: string; message: QueueMessage }
export interface AckResult { count: number; unknown: string[] }
export interface SeekResult { cursor_seq: number; restored: number }
export interface QueueSnapshot { name: string; subscription: string; topic: string; cursor_seq: number; unacked: number; created_by: string; created_at: string; expires_at: string }

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
export const createQueueTopic = (t: NewTopic) => send<QueueTopic>("POST", `${Q}/topics`, t);
export const deleteQueueTopic = (name: string, force = false) => send<void>("DELETE", `${Q}/topics/${enc(name)}${force ? "?force=true" : ""}`);
export const queueTopicMetrics = (name: string, signal?: AbortSignal) => request<TopicMetrics>(`${Q}/topics/${enc(name)}/metrics`, { signal });
export const queueTopicMessages = (name: string, after = 0, limit = 25, signal?: AbortSignal) =>
  request<MessagePage>(`${Q}/topics/${enc(name)}/messages?after=${after}&limit=${limit}`, { signal });
export const publishQueueMessages = async (name: string, messages: NewMessage[]) =>
  (await send<{ results: Published[] }>("POST", `${Q}/topics/${enc(name)}/publish`, { messages })).results;

export const listQueueSubscriptions = (topic?: string, signal?: AbortSignal) =>
  items(request<{ items: QueueSubscription[] }>(topic ? `${Q}/subscriptions?topic=${enc(topic)}` : `${Q}/subscriptions`, { signal }));
export const getQueueSubscription = (name: string, signal?: AbortSignal) => request<QueueSubscription>(`${Q}/subscriptions/${enc(name)}`, { signal });
export const createQueueSubscription = (s: NewSubscription) => send<QueueSubscription>("POST", `${Q}/subscriptions`, s);
export const deleteQueueSubscription = (name: string) => send<void>("DELETE", `${Q}/subscriptions/${enc(name)}`);
export const queueSubscriptionMetrics = (name: string, signal?: AbortSignal) => request<SubscriptionMetrics>(`${Q}/subscriptions/${enc(name)}/metrics`, { signal });
export const queueDeadLetters = (name: string, before?: number, limit = 25, signal?: AbortSignal) =>
  items(request<{ items: DeadLetter[] }>(`${Q}/subscriptions/${enc(name)}/dead-letters?limit=${limit}${before ? `&before=${before}` : ""}`, { signal }));
export const pullQueueMessages = async (name: string, max = 10) =>
  (await send<{ messages: ReceivedMessage[] }>("POST", `${Q}/subscriptions/${enc(name)}/pull`, { max_messages: max, consumer: "studio" })).messages;
export const ackQueueMessages = (name: string, ackIds: string[]) => send<AckResult>("POST", `${Q}/subscriptions/${enc(name)}/ack`, { ack_ids: ackIds });
export const nackQueueMessages = (name: string, ackIds: string[]) => send<AckResult>("POST", `${Q}/subscriptions/${enc(name)}/nack`, { ack_ids: ackIds });
export const seekQueueSubscription = (name: string, to: { time: string } | { snapshot: string }) => send<SeekResult>("POST", `${Q}/subscriptions/${enc(name)}/seek`, to);
export const listQueueSnapshots = (signal?: AbortSignal) => items(request<{ items: QueueSnapshot[] }>(`${Q}/snapshots`, { signal }));
export const createQueueSnapshot = (name: string, subscription: string) => send<QueueSnapshot>("POST", `${Q}/snapshots`, { name, subscription });
export const deleteQueueSnapshot = (name: string) => send<void>("DELETE", `${Q}/snapshots/${enc(name)}`);

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

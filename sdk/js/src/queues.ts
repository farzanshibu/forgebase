import { ApiError, NetworkError, RealtimeError } from "./errors";
import type { Transport } from "./http";

/*
 * Durable topics and queues (`/api/v1/queues`, ADR-0022): `client.queues`.
 * The wire types are written by hand until the OpenAPI fixture carries the queue schemas; they follow the server's JSON exactly (snake_case).
 */

export interface Topic {
  name: string;
  retention_ms: number;
  retention_bytes?: number | null;
  max_message_bytes: number;
  dedup_window_ms: number;
  schema?: unknown;
  source?: { collection: string; ops: string[] } | null;
  labels: Record<string, string>;
  published_total: number;
  published_bytes: number;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface QueueMessage<T = unknown> {
  id: string;
  topic: string;
  seq: number;
  data: T;
  attributes: Record<string, string>;
  ordering_key?: string | null;
  dedup_id?: string | null;
  publisher: string;
  bytes: number;
  published_at: string;
  delivery_history?: { attempt: number; at: string; consumer?: string | null; outcome?: string | null; error?: string | null }[] | null;
}

export interface Published {
  message_id: string;
  seq: number;
  /** The dedup id was seen inside the window: nothing was stored, `message_id` is the first message's. */
  duplicate: boolean;
}

export interface QueueSubscription {
  name: string;
  topic: string;
  filter?: unknown;
  ack_deadline_ms: number;
  max_delivery_attempts?: number | null;
  backoff_base_ms: number;
  backoff_max_ms: number;
  dead_letter_topic?: string | null;
  ordering: boolean;
  push?: { kind: "webhook" | "function"; endpoint: string } | null;
  /** Shown once, when a push subscription is created. */
  push_secret?: string;
  labels: Record<string, string>;
  created_by: string;
  created_at: string;
  updated_at: string;
}

export interface TopicMetrics {
  topic: string;
  messages: number;
  bytes: number;
  oldest_message_age_ms?: number | null;
  published_total: number;
  published_bytes: number;
  published_last_minute: number;
  publish_rate_per_sec: number;
  subscriptions: number;
}

export interface SubscriptionMetrics {
  subscription: string;
  topic: string;
  backlog_messages: number;
  backlog_bytes: number;
  oldest_unacked_age_ms?: number | null;
  ready: number;
  delayed: number;
  in_flight: number;
  delivered_total: number;
  acked_total: number;
  nacked_total: number;
  expired_total: number;
  dead_lettered_total: number;
  dead_letters: number;
  acked_last_minute: number;
  ack_rate_per_sec: number;
}

export interface DeadLetter {
  id: number;
  subscription: string;
  topic: string;
  message_id: string;
  data: unknown;
  attributes: Record<string, string>;
  ordering_key?: string | null;
  published_at: string;
  attempts: number;
  history: { attempt: number; at: string; consumer?: string | null; outcome?: string | null; error?: string | null }[];
  reason: string;
  dead_letter_topic?: string | null;
  forwarded_message_id?: string | null;
  at: string;
}

export interface QueueSnapshot {
  name: string;
  subscription: string;
  topic: string;
  cursor_seq: number;
  unacked: number;
  created_by: string;
  created_at: string;
  expires_at: string;
}

/** One delivery of a message, as the server sends it. */
export interface ReceivedMessage<T = unknown> {
  ack_id: string;
  delivery_attempt: number;
  ack_deadline: string;
  message: QueueMessage<T>;
}

export interface AckResult {
  count: number;
  /** Ack ids the server no longer knows (expired and delivered again, or already settled). */
  unknown: string[];
}

export interface PublishOptions {
  attributes?: Record<string, string>;
  /** Messages with one ordering key reach an ordered subscription in publish order. */
  orderingKey?: string;
  /** A repeat inside the topic's dedup window is accepted once. */
  dedupId?: string;
}

export interface PublishMessage<T = unknown> extends PublishOptions {
  data: T;
}

export interface CreateTopicOptions {
  retentionMs?: number;
  retentionBytes?: number;
  maxMessageBytes?: number;
  dedupWindowMs?: number;
  /** JSON Schema (documented subset) that publishes are validated against. */
  schema?: unknown;
  labels?: Record<string, string>;
}

export interface CreateSubscriptionOptions {
  /** Attribute filter, e.g. `{ kind: "rush" }` or `{ kind: { in: ["a", "b"] } }`. */
  filter?: unknown;
  ackDeadlineMs?: number;
  maxDeliveryAttempts?: number;
  backoffBaseMs?: number;
  backoffMaxMs?: number;
  deadLetterTopic?: string;
  ordering?: boolean;
  push?: { kind: "webhook" | "function"; endpoint: string };
  /** Where a new subscription starts reading: `"latest"` (default), `"earliest"` or a timestamp. */
  start?: "latest" | "earliest" | { time: string };
  labels?: Record<string, string>;
}

export interface PullOptions {
  /** 1-1000, default 10. */
  maxMessages?: number;
  /** Long-poll up to this many milliseconds when nothing is ready. */
  waitMs?: number;
  consumer?: string;
}

/** A delivery handed to a `subscribe` handler. */
export interface Delivery<T = unknown> {
  /** The message data. */
  data: T;
  message: QueueMessage<T>;
  attributes: Record<string, string>;
  /** 1 on the first delivery. */
  attempt: number;
  ackId: string;
  /** Acknowledge now (the handler returning normally does it for you unless `autoAck` is off). */
  ack(): Promise<void>;
  /** Negative acknowledgement: the message is redelivered after the subscription's backoff. */
  nack(): Promise<void>;
  /** Give the handler more time (milliseconds from now, 1 s to 10 min). */
  extend(ackDeadlineMs: number): Promise<void>;
}

export interface SubscribeOptions {
  /** Most unacknowledged messages this stream holds (flow control, default 100). */
  maxMessages?: number;
  /** Most unacknowledged bytes this stream holds (default 10 MiB). */
  maxBytes?: number;
  /** Ack when the handler resolves and nack when it throws (default true). Turn off to settle by hand. */
  autoAck?: boolean;
  /** Reconnect after a dropped stream (default true), with exponential backoff. */
  reconnect?: boolean;
  /** Called with errors that do not end the subscription (a handler that threw, a dropped stream). */
  onError?: (err: unknown) => void;
  signal?: AbortSignal;
}

export interface QueueStream {
  /** Stops the stream; deliveries still held return after their ack deadline. */
  unsubscribe(): void;
  /** Settles when the stream ends: resolves after `unsubscribe`, rejects when the server refuses it for good. */
  done: Promise<void>;
}

const enc = encodeURIComponent;
const Q = "/api/v1/queues";

const compact = <T extends Record<string, unknown>>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

/** Parses a `text/event-stream` body into events; tolerates events split across chunks and CRLF line ends. */
export async function* sseEvents(body: ReadableStream<Uint8Array>): AsyncGenerator<{ event: string; data: string; id?: string }> {
  const reader = body.getReader();
  const dec = new TextDecoder();
  let buf = "";
  try {
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      buf += dec.decode(value, { stream: true }).replace(/\r\n/g, "\n");
      let i: number;
      while ((i = buf.indexOf("\n\n")) >= 0) {
        const block = buf.slice(0, i);
        buf = buf.slice(i + 2);
        let event = "message";
        let data = "";
        let id: string | undefined;
        for (const line of block.split("\n")) {
          if (line.startsWith("event:")) event = line.slice(6).trim();
          else if (line.startsWith("data:")) data += (data ? "\n" : "") + line.slice(5).replace(/^ /, "");
          else if (line.startsWith("id:")) id = line.slice(3).trim();
        }
        if (data || event !== "message") yield { event, data, id };
      }
    }
  } finally {
    reader.releaseLock();
  }
}

/** Topics, subscriptions, publish, pull and streaming pull: `client.queues`. */
export class QueuesApi {
  constructor(
    private readonly t: Transport,
    private readonly sleep: (ms: number) => Promise<void> = (ms) => new Promise((r) => setTimeout(r, ms)),
  ) {}

  // ----------------------------------------------------------------- topics

  async listTopics(): Promise<Topic[]> {
    return (await this.t.request<{ items: Topic[] }>(`${Q}/topics`)).items;
  }

  getTopic(name: string): Promise<Topic> {
    return this.t.request(`${Q}/topics/${enc(name)}`);
  }

  createTopic(name: string, o: CreateTopicOptions = {}): Promise<Topic> {
    const body = compact({
      name,
      retention_ms: o.retentionMs,
      retention_bytes: o.retentionBytes,
      max_message_bytes: o.maxMessageBytes,
      dedup_window_ms: o.dedupWindowMs,
      schema: o.schema,
      labels: o.labels,
    });
    return this.t.request(`${Q}/topics`, { method: "POST", body });
  }

  /** Deletes a topic; `force` deletes its subscriptions too (without it a topic with subscriptions is refused: `topic_has_subscriptions`). */
  async deleteTopic(name: string, o: { force?: boolean } = {}): Promise<void> {
    await this.t.request(`${Q}/topics/${enc(name)}`, { method: "DELETE", query: { force: o.force ? true : undefined } });
  }

  topicMetrics(name: string): Promise<TopicMetrics> {
    return this.t.request(`${Q}/topics/${enc(name)}/metrics`);
  }

  /** Retained messages of a topic, oldest first (a read: nothing is consumed). `nextAfter` continues the listing. */
  async messages(topic: string, o: { after?: number; limit?: number } = {}): Promise<{ items: QueueMessage[]; nextAfter: number | null }> {
    const r = await this.t.request<{ items: QueueMessage[]; next_after?: number | null }>(`${Q}/topics/${enc(topic)}/messages`, { query: { after: o.after, limit: o.limit } });
    return { items: r.items, nextAfter: r.next_after ?? null };
  }

  // ---------------------------------------------------------------- publish

  /** Publishes one message. Resolves with its id and sequence number. */
  async publish<T = unknown>(topic: string, data: T, o: PublishOptions = {}): Promise<Published> {
    const [r] = await this.publishBatch(topic, [{ data, ...o }]);
    if (!r) throw new ApiError({ status: 200, code: "invalid_response", message: "publish returned no result" });
    return r;
  }

  /** Publishes up to 1000 messages in one request; results are in the order sent. */
  async publishBatch(topic: string, messages: PublishMessage[]): Promise<Published[]> {
    const body = {
      messages: messages.map((m) => compact({ data: m.data, attributes: m.attributes, ordering_key: m.orderingKey, dedup_id: m.dedupId })),
    };
    // A dedup id makes a repeat harmless, so those publishes are safe to retry after a network failure.
    const retry = messages.length > 0 && messages.every((m) => m.dedupId !== undefined) ? undefined : false;
    return (await this.t.request<{ results: Published[] }>(`${Q}/topics/${enc(topic)}/publish`, { method: "POST", body, retry })).results;
  }

  // ---------------------------------------------------------- subscriptions

  async listSubscriptions(o: { topic?: string } = {}): Promise<QueueSubscription[]> {
    return (await this.t.request<{ items: QueueSubscription[] }>(`${Q}/subscriptions`, { query: { topic: o.topic } })).items;
  }

  getSubscription(name: string): Promise<QueueSubscription> {
    return this.t.request(`${Q}/subscriptions/${enc(name)}`);
  }

  createSubscription(name: string, topic: string, o: CreateSubscriptionOptions = {}): Promise<QueueSubscription> {
    const body = compact({
      name,
      topic,
      filter: o.filter,
      ack_deadline_ms: o.ackDeadlineMs,
      max_delivery_attempts: o.maxDeliveryAttempts,
      backoff_base_ms: o.backoffBaseMs,
      backoff_max_ms: o.backoffMaxMs,
      dead_letter_topic: o.deadLetterTopic,
      ordering: o.ordering,
      push: o.push,
      start: o.start,
      labels: o.labels,
    });
    return this.t.request(`${Q}/subscriptions`, { method: "POST", body });
  }

  async deleteSubscription(name: string): Promise<void> {
    await this.t.request(`${Q}/subscriptions/${enc(name)}`, { method: "DELETE" });
  }

  subscriptionMetrics(name: string): Promise<SubscriptionMetrics> {
    return this.t.request(`${Q}/subscriptions/${enc(name)}/metrics`);
  }

  /** Messages that used up their delivery attempts, newest first; `before` is a record id from the previous page. */
  async deadLetters(subscription: string, o: { before?: number; limit?: number } = {}): Promise<DeadLetter[]> {
    return (await this.t.request<{ items: DeadLetter[] }>(`${Q}/subscriptions/${enc(subscription)}/dead-letters`, { query: { before: o.before, limit: o.limit } })).items;
  }

  // ------------------------------------------------------------ pull and ack

  /** Takes up to `maxMessages` ready messages (leasing them for the subscription's ack deadline). Acknowledge each with `ack`. */
  async pull<T = unknown>(subscription: string, o: PullOptions = {}): Promise<ReceivedMessage<T>[]> {
    const body = compact({ max_messages: o.maxMessages, wait_ms: o.waitMs, consumer: o.consumer });
    // Pulling leases messages: a blind retry could lease them twice, so it is not retried.
    return (await this.t.request<{ messages: ReceivedMessage<T>[] }>(`${Q}/subscriptions/${enc(subscription)}/pull`, { method: "POST", body, retry: false })).messages;
  }

  ack(subscription: string, ackIds: string[]): Promise<AckResult> {
    return this.t.request(`${Q}/subscriptions/${enc(subscription)}/ack`, { method: "POST", body: { ack_ids: ackIds } });
  }

  nack(subscription: string, ackIds: string[]): Promise<AckResult> {
    return this.t.request(`${Q}/subscriptions/${enc(subscription)}/nack`, { method: "POST", body: { ack_ids: ackIds } });
  }

  /** Changes the ack deadline of held messages (1 s to 10 min from now; `0` is a nack). */
  modifyDeadline(subscription: string, ackIds: string[], ackDeadlineMs: number): Promise<AckResult> {
    return this.t.request(`${Q}/subscriptions/${enc(subscription)}/modify-deadline`, { method: "POST", body: { ack_ids: ackIds, ack_deadline_ms: ackDeadlineMs } });
  }

  // ------------------------------------------------------------------ replay

  /** Replays: moves the cursor back to a time (RFC 3339 or `Date`, inside the topic's retention) so later messages are delivered again, or restores a snapshot's ack state (`restored` counts those). */
  seek(subscription: string, to: { time: string | Date } | { snapshot: string }): Promise<{ cursor_seq: number; restored: number }> {
    const body = "time" in to ? { time: to.time instanceof Date ? to.time.toISOString() : to.time } : { snapshot: to.snapshot };
    return this.t.request(`${Q}/subscriptions/${enc(subscription)}/seek`, { method: "POST", body });
  }

  async listSnapshots(): Promise<QueueSnapshot[]> {
    return (await this.t.request<{ items: QueueSnapshot[] }>(`${Q}/snapshots`)).items;
  }

  createSnapshot(name: string, subscription: string): Promise<QueueSnapshot> {
    return this.t.request(`${Q}/snapshots`, { method: "POST", body: { name, subscription } });
  }

  async deleteSnapshot(name: string): Promise<void> {
    await this.t.request(`${Q}/snapshots/${enc(name)}`, { method: "DELETE" });
  }

  // --------------------------------------------------------- streaming pull

  /**
   * Streaming pull with acknowledgement: `handler` runs for every delivery as the server pushes it (server-sent events,
   * flow-controlled by `maxMessages` / `maxBytes`). When the handler resolves the message is acked; when it throws it is nacked
   * and redelivered after the subscription's backoff (`autoAck: false` leaves both to the handler: `d.ack()` / `d.nack()`).
   * A dropped stream reconnects with backoff; a refusal (missing permission, push subscription, no such subscription) ends it
   * and rejects `done`.
   */
  subscribe<T = unknown>(subscription: string, handler: (d: Delivery<T>) => void | Promise<void>, o: SubscribeOptions = {}): QueueStream {
    const ctrl = new AbortController();
    if (o.signal) {
      if (o.signal.aborted) ctrl.abort();
      else o.signal.addEventListener("abort", () => ctrl.abort(), { once: true });
    }
    const autoAck = o.autoAck ?? true;
    const reconnect = o.reconnect ?? true;
    const report = (e: unknown) => {
      try {
        o.onError?.(e);
      } catch {
        /* a throwing error callback must not break the stream */
      }
    };
    const settle = async (op: "ack" | "nack", id: string) => {
      try {
        if (op === "ack") await this.ack(subscription, [id]);
        else await this.nack(subscription, [id]);
      } catch (e) {
        report(e);
      }
    };
    const deliver = async (r: ReceivedMessage<T>) => {
      let settled = false;
      const once = (op: "ack" | "nack") => async () => {
        if (settled) return;
        settled = true;
        if (op === "ack") await this.ack(subscription, [r.ack_id]);
        else await this.nack(subscription, [r.ack_id]);
      };
      const d: Delivery<T> = {
        data: r.message.data,
        message: r.message,
        attributes: r.message.attributes,
        attempt: r.delivery_attempt,
        ackId: r.ack_id,
        ack: once("ack"),
        nack: once("nack"),
        extend: async (ms) => void (await this.modifyDeadline(subscription, [r.ack_id], ms)),
      };
      try {
        await handler(d);
        if (autoAck && !settled) {
          settled = true;
          await settle("ack", r.ack_id);
        }
      } catch (e) {
        report(e);
        if (autoAck && !settled) {
          settled = true;
          await settle("nack", r.ack_id);
        }
      }
    };

    const run = async () => {
      let delay = 500;
      while (!ctrl.signal.aborted) {
        let opened = false;
        try {
          const res = await this.t.send(`${Q}/subscriptions/${enc(subscription)}/events`, {
            headers: { Accept: "text/event-stream" },
            query: { max_messages: o.maxMessages, max_bytes: o.maxBytes },
            signal: ctrl.signal,
            retry: false,
          });
          if (!res.body) throw new NetworkError("the event stream has no body");
          opened = true;
          const inflight = new Set<Promise<void>>();
          for await (const ev of sseEvents(res.body)) {
            if (ev.event === "error") {
              let code = "stream_error";
              let message = ev.data;
              try {
                const j = JSON.parse(ev.data) as { code?: string; message?: string };
                code = j.code ?? code;
                message = j.message ?? message;
              } catch {
                /* plain text */
              }
              throw new RealtimeError(code, message);
            }
            if (ev.event !== "message" || !ev.data) continue;
            let r: ReceivedMessage<T>;
            try {
              r = JSON.parse(ev.data) as ReceivedMessage<T>;
            } catch {
              continue;
            }
            const p = deliver(r).finally(() => inflight.delete(p));
            inflight.add(p);
          }
          await Promise.allSettled([...inflight]);
          if (!reconnect) return;
        } catch (e) {
          if (ctrl.signal.aborted) return;
          // A refusal is final; a network failure or a server hiccup is worth another try.
          const final = e instanceof RealtimeError || (e instanceof ApiError && e.status >= 400 && e.status < 500 && e.status !== 429 && e.status !== 408);
          if (final || !reconnect) throw e;
          report(e);
        }
        if (opened) delay = 500;
        await this.sleep(delay);
        delay = Math.min(delay * 2, 10_000);
      }
    };
    const done = run().catch((e: unknown) => {
      if (ctrl.signal.aborted) return;
      throw e;
    });
    // `done` may reject with nobody listening yet; the caller still gets the rejection when they await it.
    done.catch(() => undefined);
    return { unsubscribe: () => ctrl.abort(), done };
  }
}

/**
 * In-memory fake of `/api/v1/queues` (ADR-0022) for component tests.
 * The routes are plain handlers: the OpenAPI fixture does not describe the queue API yet (it is regenerated after FIX-4).
 */
import type { AckResult, DeadLetter, MessagePage, QueueMessage, ReceivedMessage, QueueSnapshot, QueueSubscription, QueueTopic } from "../api";
import { envelope, type MockApi } from "./mockApi";

const NOW = "2026-11-09T10:00:00Z";

export interface QueueState {
  topics: QueueTopic[];
  subs: QueueSubscription[];
  messages: Record<string, QueueMessage[]>;
  dead: Record<string, DeadLetter[]>;
  snapshots: QueueSnapshot[];
  /** Resource rules by kind and name (the policy-engine routes the Rules panels use). */
  rules: Record<string, Record<string, Record<string, string | null>>>;
  /** Tenants sent as X-ForgeBase-Tenant on a create, in order. */
  tenants: (string | undefined)[];
  /** Messages the next pull of a subscription returns. */
  ready: Record<string, ReceivedMessage[]>;
  acked: string[];
  nacked: string[];
  seeks: { sub: string; to: unknown }[];
}

export const topic = (name: string, extra: Partial<QueueTopic> = {}): QueueTopic => ({
  name, retention_ms: 7 * 86_400_000, max_message_bytes: 1_048_576, dedup_window_ms: 600_000, labels: {}, published_total: 0, published_bytes: 0,
  created_by: "admin", created_at: NOW, updated_at: NOW, ...extra,
});

export const sub = (name: string, topicName: string, extra: Partial<QueueSubscription> = {}): QueueSubscription => ({
  name, topic: topicName, ack_deadline_ms: 30_000, backoff_base_ms: 1000, backoff_max_ms: 60_000, ordering: false, labels: {}, created_by: "admin", created_at: NOW, updated_at: NOW, ...extra,
});

export const message = (topicName: string, seq: number, data: unknown = { n: seq }, extra: Partial<QueueMessage> = {}): QueueMessage => ({
  id: `msg_${topicName}_${seq}`, topic: topicName, seq, data, attributes: {}, publisher: "admin", bytes: JSON.stringify(data).length, published_at: NOW, ...extra,
});

export const received = (m: QueueMessage, attempt = 1): ReceivedMessage => ({ ack_id: `ack_${m.id}`, delivery_attempt: attempt, ack_deadline: "2026-11-09T10:00:30Z", message: m });

export function installQueues(mock: MockApi): QueueState {
  const st: QueueState = {
    topics: [topic("orders", { published_total: 3, published_bytes: 30 }), topic("audit", { source: { collection: "users", ops: ["create"] } })],
    subs: [sub("order-workers", "orders", { max_delivery_attempts: 3, dead_letter_topic: "orders-dead" }), sub("order-audit", "orders", { ordering: true })],
    messages: { orders: [message("orders", 1), message("orders", 2, { n: 2 }, { attributes: { kind: "rush" }, ordering_key: "o2" }), message("orders", 3)], audit: [] },
    dead: {
      "order-workers": [{
        id: 5, subscription: "order-workers", topic: "orders", message_id: "msg_orders_1", data: { n: 1 }, attributes: {}, published_at: NOW, attempts: 3,
        history: [{ attempt: 1, at: NOW, consumer: "c1", outcome: "nack", error: "boom" }, { attempt: 3, at: NOW, consumer: "c1", outcome: "expired" }],
        reason: "max delivery attempts reached", dead_letter_topic: "orders-dead", at: NOW,
      }],
    },
    snapshots: [{ name: "before-fix", subscription: "order-workers", topic: "orders", cursor_seq: 2, unacked: 1, created_by: "admin", created_at: NOW, expires_at: "2026-11-16T10:00:00Z" }],
    ready: {}, acked: [], nacked: [], seeks: [], rules: { queue_topic: {}, queue_subscription: {} }, tenants: [],
  };
  const Q = "/api/v1/queues";
  const no = (what: string) => envelope(404, `${what}_not_found`, `no such ${what}`);
  const t = (n: string) => st.topics.find((x) => x.name === n);
  const s = (n: string) => st.subs.find((x) => x.name === n);

  mock.on("GET", `${Q}/topics`, () => ({ status: 200, body: { items: st.topics } }));
  /** A tenant caller creates `<tenant>/<name>` (FB-141). */
  const scoped = (name: string, headers: Record<string, string>) => {
    const tenant = headers["X-ForgeBase-Tenant"] ?? headers["x-forgebase-tenant"];
    st.tenants.push(tenant);
    return { tenant, key: tenant ? `${tenant}/${name}` : name };
  };
  mock.on("POST", `${Q}/topics`, ({ body, headers }) => {
    const b = body as { name: string; retention_ms?: number };
    const { tenant, key } = scoped(b.name, headers);
    if (t(key)) return envelope(409, "topic_exists", "topic exists");
    const made = topic(key, { ...(b.retention_ms ? { retention_ms: b.retention_ms } : {}), tenant: tenant ?? null });
    st.topics.push(made);
    st.messages[key] = [];
    return { status: 201, body: made };
  });
  mock.on("GET", `${Q}/topics/{topic}`, ({ params }) => (t(params.topic!) ? { status: 200, body: t(params.topic!) } : no("topic")));
  mock.on("DELETE", `${Q}/topics/{topic}`, ({ params, query }) => {
    if (!t(params.topic!)) return no("topic");
    if (st.subs.some((x) => x.topic === params.topic) && query.get("force") !== "true") return envelope(409, "topic_has_subscriptions", "delete its subscriptions first, or pass force=true");
    st.subs = st.subs.filter((x) => x.topic !== params.topic);
    st.topics = st.topics.filter((x) => x.name !== params.topic);
    return { status: 204 };
  });
  mock.on("POST", `${Q}/topics/{topic}/publish`, ({ params, body }) => {
    const tp = t(params.topic!);
    if (!tp) return no("topic");
    const list = (st.messages[tp.name] ??= []);
    const results = (body as { messages: { data: unknown; attributes?: Record<string, string>; ordering_key?: string; dedup_id?: string }[] }).messages.map((m) => {
      const dup = m.dedup_id ? list.find((x) => x.dedup_id === m.dedup_id) : undefined;
      if (dup) return { message_id: dup.id, seq: dup.seq, duplicate: true };
      const made = message(tp.name, list.length + 1, m.data, { attributes: m.attributes ?? {}, ordering_key: m.ordering_key ?? null, dedup_id: m.dedup_id ?? null });
      list.push(made);
      tp.published_total++;
      return { message_id: made.id, seq: made.seq, duplicate: false };
    });
    return { status: 200, body: { results } };
  });
  mock.on("GET", `${Q}/topics/{topic}/messages`, ({ params, query }) => {
    if (!t(params.topic!)) return no("topic");
    const after = Number(query.get("after") ?? 0), limit = Number(query.get("limit") ?? 50);
    const rest = (st.messages[params.topic!] ?? []).filter((m) => m.seq > after);
    const items = rest.slice(0, limit);
    const body: MessagePage = { items, next_after: rest.length > limit ? items[items.length - 1]!.seq : null };
    return { status: 200, body };
  });
  mock.on("GET", `${Q}/topics/{topic}/metrics`, ({ params }) => {
    const tp = t(params.topic!);
    if (!tp) return no("topic");
    const list = st.messages[tp.name] ?? [];
    return { status: 200, body: { topic: tp.name, messages: list.length, bytes: list.reduce((a, m) => a + m.bytes, 0), oldest_message_age_ms: list.length ? 90_000 : null, published_total: tp.published_total, published_bytes: tp.published_bytes, published_last_minute: 2, publish_rate_per_sec: 0.5, subscriptions: st.subs.filter((x) => x.topic === tp.name).length } };
  });

  mock.on("GET", `${Q}/subscriptions`, ({ query }) => ({ status: 200, body: { items: st.subs.filter((x) => !query.get("topic") || x.topic === query.get("topic")) } }));
  mock.on("POST", `${Q}/subscriptions`, ({ body, headers }) => {
    const b = body as { name: string; topic: string; ack_deadline_ms?: number; max_delivery_attempts?: number; ordering?: boolean };
    const { tenant, key } = scoped(b.name, headers);
    if (s(key)) return envelope(409, "subscription_exists", "subscription exists");
    if (!t(b.topic)) return no("topic");
    const made = sub(key, b.topic, { ack_deadline_ms: b.ack_deadline_ms ?? 30_000, max_delivery_attempts: b.max_delivery_attempts ?? null, ordering: !!b.ordering, tenant: tenant ?? null });
    st.subs.push(made);
    return { status: 201, body: made };
  });
  mock.on("GET", `${Q}/subscriptions/{sub}`, ({ params }) => (s(params.sub!) ? { status: 200, body: s(params.sub!) } : no("subscription")));
  mock.on("DELETE", `${Q}/subscriptions/{sub}`, ({ params }) => {
    if (!s(params.sub!)) return no("subscription");
    st.subs = st.subs.filter((x) => x.name !== params.sub);
    return { status: 204 };
  });
  mock.on("GET", `${Q}/subscriptions/{sub}/metrics`, ({ params }) => {
    const sb = s(params.sub!);
    if (!sb) return no("subscription");
    const dead = st.dead[sb.name]?.length ?? 0;
    return { status: 200, body: { subscription: sb.name, topic: sb.topic, backlog_messages: 7, backlog_bytes: 70, oldest_unacked_age_ms: 120_000, ready: 4, delayed: 1, in_flight: 2, delivered_total: 20, acked_total: 12, nacked_total: 3, expired_total: 1, dead_lettered_total: dead, dead_letters: dead, acked_last_minute: 6, ack_rate_per_sec: 0.1 } };
  });
  mock.on("GET", `${Q}/subscriptions/{sub}/dead-letters`, ({ params }) => (s(params.sub!) ? { status: 200, body: { items: st.dead[params.sub!] ?? [] } } : no("subscription")));
  mock.on("POST", `${Q}/subscriptions/{sub}/pull`, ({ params }) => {
    if (!s(params.sub!)) return no("subscription");
    const out = st.ready[params.sub!] ?? [];
    st.ready[params.sub!] = [];
    return { status: 200, body: { messages: out } };
  });
  const settle = (list: string[]) => ({ params, body }: { params: Record<string, string>; body: unknown }) => {
    if (!s(params.sub!)) return no("subscription");
    const ids = (body as { ack_ids: string[] }).ack_ids;
    list.push(...ids);
    const result: AckResult = { count: ids.length, unknown: [] };
    return { status: 200, body: result };
  };
  mock.on("POST", `${Q}/subscriptions/{sub}/ack`, settle(st.acked));
  mock.on("POST", `${Q}/subscriptions/{sub}/nack`, settle(st.nacked));
  mock.on("POST", `${Q}/subscriptions/{sub}/seek`, ({ params, body }) => {
    if (!s(params.sub!)) return no("subscription");
    const b = body as { time?: string; snapshot?: string };
    if (b.snapshot && !st.snapshots.some((x) => x.name === b.snapshot)) return no("snapshot");
    st.seeks.push({ sub: params.sub!, to: body });
    return { status: 200, body: { cursor_seq: b.snapshot ? 2 : 0, restored: 3 } };
  });
  mock.on("GET", `${Q}/snapshots`, () => ({ status: 200, body: { items: st.snapshots } }));
  mock.on("POST", `${Q}/snapshots`, ({ body }) => {
    const b = body as { name: string; subscription: string };
    if (st.snapshots.some((x) => x.name === b.name)) return envelope(409, "snapshot_exists", "snapshot exists");
    const sb = s(b.subscription);
    if (!sb) return no("subscription");
    const made: QueueSnapshot = { name: b.name, subscription: sb.name, topic: sb.topic, cursor_seq: 3, unacked: 0, created_by: "admin", created_at: NOW, expires_at: "2026-11-16T10:00:00Z" };
    st.snapshots.push(made);
    return { status: 201, body: made };
  });
  mock.on("DELETE", `${Q}/snapshots/{snapshot}`, ({ params }) => {
    if (!st.snapshots.some((x) => x.name === params.snapshot)) return no("snapshot");
    st.snapshots = st.snapshots.filter((x) => x.name !== params.snapshot);
    return { status: 204 };
  });
  // The rules editor: queue rules read @request only, so the kinds list no item fields.
  const R = "/api/v1/policies/resources";
  mock.on("GET", `${R}/{kind}`, ({ params }) => ({ status: 200, body: { kind: params.kind, fields: [], operations: ["list", "view", "create", "update", "delete"], actions: [], items: Object.entries(st.rules[params.kind!] ?? {}).map(([name, rules]) => ({ name, rules })) } }));
  mock.on("PUT", `${R}/{kind}/{name}`, ({ params, body }) => {
    (st.rules[params.kind!] ??= {})[params.name!] = (body as { rules: Record<string, string | null> }).rules;
    return { status: 200, body: { name: params.name, rules: (body as { rules: unknown }).rules } };
  });
  mock.on("DELETE", `${R}/{kind}/{name}`, ({ params }) => {
    delete st.rules[params.kind!]?.[params.name!];
    return { status: 204 };
  });
  return st;
}

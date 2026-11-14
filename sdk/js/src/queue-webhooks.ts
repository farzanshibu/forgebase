import type { Transport } from "./http";
import type { QueueSubscription } from "./queues";
import type { Schemas } from "./types";

/*
 * Webhook delivery tooling of push subscriptions (ADR-0031): the attempt log, test sends, dead-letter redrive, retry now,
 * signing-secret rotation, pause and resume. `client.queues.webhooks("orders-push")`.
 *
 * Reads need `engine:queue_read`, everything else `engine:queue_admin`, on the subscription (its creator holds both). Pull
 * subscriptions answer `409 not_push_subscription` (`ConflictError`) to the calls that need a push target.
 */

export type PushAttempt = Schemas["PushAttempt"];
export type PushAttemptPage = Schemas["PushAttemptPage"];
export type PushOutcome = Schemas["PushOutcome"];
export type RedriveResult = Schemas["RedriveResult"];
export type RetryResult = Schemas["RetryResult"];
export type RotatedSecret = Schemas["RotatedSecret"];

/** Filters of `attempts`. */
export interface AttemptsQuery {
  /** Only `success` or only `failure` attempts. */
  outcome?: PushOutcome;
  /** Only the attempts of one message. */
  messageId?: string;
  /** Only attempts at or after this time (RFC 3339 or `Date`). */
  since?: string | Date;
  /** Only attempts before this time. */
  until?: string | Date;
  /** `true`: test sends only; `false`: real deliveries only. */
  test?: boolean;
  /** Page cursor: the `next_before` of the previous page. */
  before?: number;
  /** Page size. */
  limit?: number;
}

/** Which dead letters to redrive: record ids (1-1000), or `{ all: true }` with optional narrowing. */
export type Redrive =
  | { ids: number[] }
  | {
      /** Every dead letter not redriven yet, oldest first. */
      all: true;
      /** Only records dead-lettered at or after this time. */
      since?: string | Date;
      /** Only records whose message attributes match this filter (subscription filter syntax). */
      filter?: Record<string, unknown>;
      /** Most records to redrive in this call (1-1000, default 1000). */
      limit?: number;
    };

const seg = encodeURIComponent;
const iso = (v: string | Date | undefined): string | undefined => (v instanceof Date ? v.toISOString() : v);

/** Webhook tooling of one push subscription. */
export class QueueWebhooksApi {
  private readonly base: string;

  constructor(
    private readonly t: Transport,
    subscription: string,
  ) {
    this.base = `/api/v1/queues/subscriptions/${seg(subscription)}`;
  }

  /**
   * Push attempts, newest first: deliveries and test sends with status, latency, error class, the request headers sent
   * (signature masked), allowlisted response headers and the response body (cut to the server's limit: `response_truncated`).
   * `next_before` continues the listing.
   */
  attempts(q: AttemptsQuery = {}): Promise<PushAttemptPage> {
    return this.t.request(`${this.base}/attempts`, {
      query: { outcome: q.outcome, message_id: q.messageId, since: iso(q.since), until: iso(q.until), test: q.test, before: q.before, limit: q.limit },
    });
  }

  /** One attempt (`NotFoundError`, `attempt_not_found`). */
  attempt(id: number): Promise<PushAttempt> {
    return this.t.request(`${this.base}/attempts/${id}`);
  }

  /**
   * Sends a synthetic signed event (`"type": "forgebase.test"` in the usual envelope, header `X-Forgebase-Test: 1`) and
   * resolves with the attempt, whatever the endpoint answered. It is not a message: cursors, totals, health and dead letters
   * are untouched. Rate limited per subscription (`RateLimitError`). `data` replaces the default payload.
   */
  test(data?: Record<string, unknown>): Promise<PushAttempt> {
    return this.t.request(`${this.base}/test`, { method: "POST", body: data === undefined ? {} : { data }, retry: false });
  }

  /**
   * Delivers dead letters to the subscription again with a fresh attempt counter. Each record is redriven once; the result
   * lists `redriven`, `already_redriven`, `not_found` and `remaining`.
   */
  redrive(what: Redrive): Promise<RedriveResult> {
    const body =
      "ids" in what
        ? { ids: what.ids }
        : { all: true, since: iso(what.since), filter: what.filter, limit: what.limit };
    return this.t.request(`${this.base}/redrive`, { method: "POST", body: dropUndefined(body) });
  }

  /** Makes deliveries that wait for a retry backoff ready now (all of them, or only `messageIds`). */
  retry(messageIds?: string[]): Promise<RetryResult> {
    return this.t.request(`${this.base}/retry`, { method: "POST", body: messageIds === undefined ? {} : { message_ids: messageIds } });
  }

  /**
   * Rotates the signing secret and resolves with the new `push_secret` (shown once). During the grace window (`graceMs`:
   * default 24 h, at most 7 days, `0` drops the old secret at once) `X-Forgebase-Signature` carries both signatures, new
   * first; receivers accept the request when any `v1` matches (`constructWebhookEvent` with the secret list does).
   * Function subscriptions answer `409 not_webhook_subscription`.
   */
  rotateSecret(o: { graceMs?: number } = {}): Promise<RotatedSecret> {
    return this.t.request(`${this.base}/secret/rotate`, { method: "POST", body: dropUndefined({ grace_ms: o.graceMs }) });
  }

  /** Ends the grace window of a rotation now: only the new secret signs from here on. */
  expirePreviousSecret(): Promise<QueueSubscription> {
    return this.t.request(`${this.base}/secret/expire-previous`, { method: "POST", body: {} });
  }

  /** Pauses delivery. The subscription keeps accumulating messages; none is dead-lettered while paused. */
  pause(o: { reason?: string } = {}): Promise<QueueSubscription> {
    return this.t.request(`${this.base}/pause`, { method: "POST", body: dropUndefined({ reason: o.reason }) });
  }

  /** Resumes delivery: every delayed delivery becomes ready and the backlog is delivered in order. */
  resume(): Promise<QueueSubscription> {
    return this.t.request(`${this.base}/resume`, { method: "POST", body: {} });
  }
}

const dropUndefined = <T extends Record<string, unknown>>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

import { COMMIT_HEADER, CONSISTENCY_HEADER, CommitTokens, READ_AFTER_HEADER, type Consistency } from "./consistency";
import { errorCodeOf, errorFromResponse, InvalidResponseError, isQuotaCode, NetworkError, parseRetryAfter } from "./errors";

export type FetchLike = (input: string, init?: RequestInit) => Promise<Response>;

export interface RetryOptions {
  /** Retries after the first attempt (default 3). `0` disables retrying. */
  maxRetries?: number;
  /** First backoff delay in ms (default 250); doubles each attempt with full jitter. */
  baseDelayMs?: number;
  /** Upper bound for one backoff delay in ms (default 10 000). */
  maxDelayMs?: number;
  /** Longest `Retry-After` the SDK will sleep for, in ms (default 60 000); longer asks fail at once. */
  maxRetryAfterMs?: number;
  /** Statuses retried for every method (default `[429, 503]`). 502 and 504 are retried for idempotent methods only. */
  statuses?: number[];
  /** Test hook. */
  sleep?: (ms: number, signal?: AbortSignal) => Promise<void>;
  /** Test hook: returns [0, 1). */
  random?: () => number;
}

export const DEFAULT_RETRY: Required<Omit<RetryOptions, "sleep" | "random">> = {
  maxRetries: 3,
  baseDelayMs: 250,
  maxDelayMs: 10_000,
  maxRetryAfterMs: 60_000,
  statuses: [429, 503],
};

/** Most conditional-read entries kept per client (least recently used go first). */
const ETAG_CACHE_SIZE = 256;

const IDEMPOTENT = new Set(["GET", "HEAD", "OPTIONS", "PUT", "DELETE"]);

export const defaultSleep = (ms: number, signal?: AbortSignal): Promise<void> =>
  new Promise((resolve, reject) => {
    if (signal?.aborted) return reject(signal.reason);
    const t = setTimeout(() => {
      signal?.removeEventListener("abort", onAbort);
      resolve();
    }, ms);
    const onAbort = () => {
      clearTimeout(t);
      reject(signal?.reason);
    };
    signal?.addEventListener("abort", onAbort, { once: true });
  });

export interface RequestOptions {
  method?: string;
  query?: Record<string, string | number | boolean | undefined | null>;
  body?: unknown;
  /** Raw body (string, Blob, ...) sent as is. */
  rawBody?: BodyInit;
  headers?: Record<string, string>;
  signal?: AbortSignal;
  /** Return the response text instead of JSON. */
  text?: boolean;
  /** Return the raw `Response` (streams, downloads). Errors are still thrown. */
  raw?: boolean;
  /**
   * Resolve with the `Response` instead of throwing for these statuses (`"any"`: every status). Retries still run first, so a
   * 429 or 503 comes back only after the retry budget is spent. Used for functions that answer 404 or 500 on purpose and for
   * `304 Not Modified`.
   */
  acceptStatus?: number[] | "any";
  /**
   * GET only: a conditional read. The client remembers the parsed body and `ETag` of each URL it read this way, sends
   * `If-None-Match` next time and answers a `304` with the remembered value. Servers that ignore `If-None-Match` simply
   * answer `200` and the remembered value is replaced.
   */
  conditional?: boolean;
  /** Skip credentials (sign-in, sign-up, ...). */
  anonymous?: boolean;
  /** Override the retry policy for this call. */
  retry?: RetryOptions | false;
  /** `"strong"` reads this call from the primary; default: the client's `consistency`. */
  consistency?: Consistency;
}

/** What the transport needs from the auth layer. */
export interface Credentials {
  /** Headers for one request (may refresh the access token first). */
  headers(method: string): Promise<Record<string, string>>;
  /** Called after a 401 with the headers that were sent: return `true` when a retry can succeed. */
  onUnauthorized(sent: Record<string, string>): Promise<boolean>;
  /** `credentials` mode for fetch (`include` in cookie mode). */
  readonly fetchCredentials?: RequestCredentials;
}

export class Transport {
  readonly baseUrl: string;
  credentials: Credentials | undefined;
  private readonly fetchImpl: FetchLike;
  private readonly retry: Required<Omit<RetryOptions, "sleep" | "random">>;
  private readonly sleep: (ms: number, signal?: AbortSignal) => Promise<void>;
  private readonly random: () => number;
  private readonly defaultHeaders: Record<string, string>;
  /** Commit tokens seen in responses, sent back as `X-ForgeBase-Read-After`. */
  readonly commitTokens: CommitTokens;
  /** Default consistency of every call. */
  consistency: Consistency;
  /** Conditional-read cache: URL -> `ETag` and the value read with it. */
  private readonly etags = new Map<string, { etag: string; value: unknown }>();

  constructor(opts: {
    baseUrl: string;
    fetch?: FetchLike;
    retry?: RetryOptions;
    headers?: Record<string, string>;
    consistency?: Consistency;
    commitTokens?: CommitTokens;
  }) {
    this.baseUrl = opts.baseUrl.replace(/\/+$/, "");
    if (!opts.fetch && typeof globalThis.fetch !== "function") {
      throw new Error("@forgebase/sdk needs a fetch implementation: pass `fetch` in the client options");
    }
    this.fetchImpl = opts.fetch ?? ((u, i) => globalThis.fetch(u, i));
    this.retry = { ...DEFAULT_RETRY, ...stripFns(opts.retry) };
    this.sleep = opts.retry?.sleep ?? defaultSleep;
    this.random = opts.retry?.random ?? Math.random;
    this.defaultHeaders = opts.headers ?? {};
    this.consistency = opts.consistency ?? "session";
    this.commitTokens = opts.commitTokens ?? new CommitTokens();
  }

  url(path: string, query?: RequestOptions["query"]): string {
    const u = `${this.baseUrl}${path}`;
    if (!query) return u;
    const p = new URLSearchParams();
    for (const [k, v] of Object.entries(query)) if (v !== undefined && v !== null) p.set(k, String(v));
    const qs = p.toString();
    return qs ? `${u}?${qs}` : u;
  }

  /** Delay before retry number `attempt` (1-based): `Retry-After` when sent, else exponential backoff with full jitter. */
  backoff(attempt: number, retryAfterMs: number | undefined, cfg = this.retry): number {
    if (retryAfterMs !== undefined) return retryAfterMs;
    const ceiling = Math.min(cfg.maxDelayMs, cfg.baseDelayMs * 2 ** (attempt - 1));
    return Math.floor(this.random() * ceiling);
  }

  async request<T = unknown>(path: string, opts: RequestOptions = {}): Promise<T> {
    const cacheKey = opts.conditional && !opts.raw && (opts.method ?? "GET").toUpperCase() === "GET" ? this.url(path, opts.query) : undefined;
    const known = cacheKey === undefined ? undefined : this.etags.get(cacheKey);
    let o = opts;
    if (cacheKey !== undefined) {
      const accept = opts.acceptStatus === "any" ? "any" : [...(opts.acceptStatus ?? []), 304];
      o = { ...opts, acceptStatus: accept, headers: known ? { ...opts.headers, "If-None-Match": known.etag } : opts.headers };
    }
    const res = await this.send(path, o);
    if (opts.raw) return res as T;
    if (cacheKey !== undefined && res.status === 304 && known) {
      void res.body?.cancel().catch(() => undefined);
      this.etags.delete(cacheKey);
      this.etags.set(cacheKey, known); // most recently used
      return known.value as T;
    }
    if (res.status === 204) return undefined as T;
    const text = await res.text();
    let value: unknown;
    if (opts.text) value = text;
    else if (!text) value = undefined;
    else {
      try {
        value = JSON.parse(text);
      } catch {
        throw new InvalidResponseError("response was not valid JSON", res.status);
      }
    }
    const etag = res.headers.get("etag");
    if (cacheKey !== undefined && res.ok) {
      this.etags.delete(cacheKey);
      if (etag) {
        this.etags.set(cacheKey, { etag, value });
        if (this.etags.size > ETAG_CACHE_SIZE) this.etags.delete(this.etags.keys().next().value as string);
      }
    }
    return value as T;
  }

  /** Forgets the values remembered by conditional reads. */
  clearConditionalCache(): void {
    this.etags.clear();
  }

  /** Sends with retries; resolves with a 2xx `Response` (or a status listed in `acceptStatus`), throws a typed error otherwise. */
  async send(path: string, opts: RequestOptions = {}): Promise<Response> {
    const method = (opts.method ?? "GET").toUpperCase();
    const cfg = opts.retry === false ? { ...this.retry, maxRetries: 0 } : { ...this.retry, ...stripFns(opts.retry) };
    const url = this.url(path, opts.query);
    let refreshed = false;
    for (let attempt = 0; ; ) {
      const headers: Record<string, string> = { Accept: "application/json", ...this.defaultHeaders };
      let body: BodyInit | undefined;
      if (opts.rawBody !== undefined) body = opts.rawBody;
      else if (opts.body !== undefined) {
        headers["Content-Type"] = "application/json";
        body = JSON.stringify(opts.body);
      }
      if (!opts.anonymous && this.credentials) Object.assign(headers, await this.credentials.headers(method));
      const readAfter = this.commitTokens.header();
      if (readAfter) headers[READ_AFTER_HEADER] = readAfter;
      if ((opts.consistency ?? this.consistency) === "strong") headers[CONSISTENCY_HEADER] = "strong";
      Object.assign(headers, opts.headers);
      const credentialsMode = !opts.anonymous ? this.credentials?.fetchCredentials : undefined;

      let res: Response;
      try {
        res = await this.fetchImpl(url, {
          method,
          headers,
          body,
          signal: opts.signal,
          ...(credentialsMode ? { credentials: credentialsMode } : {}),
        });
      } catch (err) {
        if (opts.signal?.aborted) throw err;
        if (attempt < cfg.maxRetries && IDEMPOTENT.has(method)) {
          attempt++;
          await this.sleep(this.backoff(attempt, undefined, cfg), opts.signal);
          continue;
        }
        throw new NetworkError(err instanceof Error ? err.message : "network request failed", err);
      }
      // A failed request may still have committed (a batch, a partial import).
      this.commitTokens.note(res.headers.get(COMMIT_HEADER));
      if (res.ok) return res;

      if (res.status === 401 && !opts.anonymous && !refreshed && this.credentials && (await this.credentials.onUnauthorized(headers))) {
        refreshed = true;
        void res.body?.cancel().catch(() => undefined);
        continue;
      }

      let retriable =
        cfg.statuses.includes(res.status) || ((res.status === 502 || res.status === 504) && IDEMPOTENT.has(method));
      // A 429 for an exhausted quota (`quota_exceeded`) is not transient: waiting does not help, so it is never retried.
      if (retriable && res.status === 429 && attempt < cfg.maxRetries && isQuotaCode(errorCodeOf(await readBody(res.clone())))) retriable = false;
      if (retriable && attempt < cfg.maxRetries) {
        const ra = parseRetryAfter(res.headers.get("retry-after"));
        if (ra === undefined || ra <= cfg.maxRetryAfterMs) {
          attempt++;
          void res.body?.cancel().catch(() => undefined);
          await this.sleep(this.backoff(attempt, ra, cfg), opts.signal);
          continue;
        }
      }
      if (opts.acceptStatus === "any" || opts.acceptStatus?.includes(res.status)) return res;
      throw errorFromResponse(res.status, await readBody(res), res.headers);
    }
  }
}

async function readBody(res: Response): Promise<unknown> {
  const text = await res.text().catch(() => "");
  if (!text) return undefined;
  try {
    return JSON.parse(text);
  } catch {
    return undefined;
  }
}

function stripFns(r: RetryOptions | undefined): Partial<Required<Omit<RetryOptions, "sleep" | "random">>> {
  if (!r) return {};
  const { sleep: _s, random: _r, ...rest } = r;
  void _s;
  void _r;
  return Object.fromEntries(Object.entries(rest).filter(([, v]) => v !== undefined));
}

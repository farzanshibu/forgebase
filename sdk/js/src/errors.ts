/** Typed errors. Every failure the SDK raises is a `ForgeBaseError`. */

export interface FieldError {
  code: string;
  message: string;
}

export interface ErrorInit {
  status: number;
  code: string;
  message: string;
  requestId?: string;
  details?: Record<string, unknown> | null;
  /** Seconds the server asked the client to wait (`Retry-After`). */
  retryAfter?: number;
  body?: unknown;
}

/** Base class of everything the SDK throws. */
export class ForgeBaseError extends Error {
  /** HTTP status; `0` when no response was received. */
  readonly status: number;
  /** Stable machine-readable code (`validation_failed`, `network_error`, ...). */
  readonly code: string;
  readonly requestId?: string;
  readonly details?: Record<string, unknown> | null;
  readonly retryAfter?: number;
  /** Parsed response body, when there was one. */
  readonly body?: unknown;

  constructor(init: ErrorInit) {
    super(init.message);
    this.name = new.target.name;
    this.status = init.status;
    this.code = init.code;
    this.requestId = init.requestId;
    this.details = init.details;
    this.retryAfter = init.retryAfter;
    this.body = init.body;
  }
}

/** The server answered with an error status (anything but 2xx). */
export class ApiError extends ForgeBaseError {}
/** 400 `validation_failed` and other malformed requests. */
export class ValidationError extends ApiError {
  /** Per-field errors from `details.fields`. */
  get fields(): Record<string, FieldError> {
    const f = (this.details as { fields?: unknown } | null | undefined)?.fields;
    const out: Record<string, FieldError> = {};
    if (typeof f === "object" && f !== null) {
      for (const [k, v] of Object.entries(f as Record<string, unknown>)) {
        if (typeof v === "object" && v !== null) {
          const o = v as { code?: unknown; message?: unknown };
          out[k] = { code: String(o.code ?? "invalid"), message: String(o.message ?? "") };
        }
      }
    }
    return out;
  }
}
/** 401: missing, expired or invalid credentials. */
export class AuthenticationError extends ApiError {}
/** 403: the credentials are fine but the action is not allowed (policy, scope, CSRF, quota-gated feature). */
export class PermissionError extends ApiError {}
/** 404. */
export class NotFoundError extends ApiError {}
/** 409 (unique violation, version mismatch, ...) and 412. */
export class ConflictError extends ApiError {}
/** 429 from rate limiting. Already retried `maxRetries` times when this is thrown. */
export class RateLimitError extends ApiError {}
/** 429 because a quota is exhausted (`details` carries the decision). Never retried: waiting will not help. */
export class QuotaExceededError extends ApiError {}
/** 5xx. 503 is retried before this is thrown. */
export class ServerError extends ApiError {}
/** No HTTP response (DNS, refused connection, timeout, CORS). */
export class NetworkError extends ForgeBaseError {
  constructor(message: string, cause?: unknown) {
    super({ status: 0, code: "network_error", message });
    if (cause !== undefined) (this as { cause?: unknown }).cause = cause;
  }
}
/** The response was 2xx but not what the SDK expected. */
export class InvalidResponseError extends ForgeBaseError {
  constructor(message: string, status = 200) {
    super({ status, code: "invalid_response", message });
  }
}
/** WebSocket / SSE failures. */
export class RealtimeError extends ForgeBaseError {
  constructor(code: string, message: string) {
    super({ status: 0, code, message });
  }
}

const QUOTA_CODES = /quota|limit_exceeded|usage_limit/i;

/** Builds the right error class from a failed response. */
export function errorFromResponse(status: number, body: unknown, headers: Headers): ApiError {
  const env = (body as { error?: Record<string, unknown> } | null | undefined)?.error;
  const code = typeof env?.code === "string" ? env.code : "http_error";
  const message = typeof env?.message === "string" ? env.message : `HTTP ${status}`;
  const ra = parseRetryAfter(headers.get("retry-after"));
  const init: ErrorInit = {
    status,
    code,
    message,
    requestId: typeof env?.request_id === "string" ? env.request_id : (headers.get("x-request-id") ?? undefined),
    details: (env?.details as Record<string, unknown> | null | undefined) ?? undefined,
    retryAfter: ra === undefined ? undefined : ra / 1000,
    body,
  };
  if (status === 429) return QUOTA_CODES.test(code) ? new QuotaExceededError(init) : new RateLimitError(init);
  if (status === 401) return new AuthenticationError(init);
  if (status === 403) return new PermissionError(init);
  if (status === 404) return new NotFoundError(init);
  if (status === 409 || status === 412) return new ConflictError(init);
  if (status === 400 || status === 422) return new ValidationError(init);
  if (status >= 500) return new ServerError(init);
  return new ApiError(init);
}

/** `Retry-After` as milliseconds (delta-seconds or HTTP-date); `undefined` when absent or invalid. */
export function parseRetryAfter(value: string | null, now: number = Date.now()): number | undefined {
  if (value === null) return undefined;
  const v = value.trim();
  if (/^\d+(\.\d+)?$/.test(v)) return Math.round(Number(v) * 1000);
  const at = Date.parse(v);
  return Number.isNaN(at) ? undefined : Math.max(0, at - now);
}

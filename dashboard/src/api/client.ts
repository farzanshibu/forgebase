import { authHeaders, csrfHeaders, notifyUnauthorized } from "./token";
import type { ApiErrorBody, FieldError } from "./types";

/** Error model of PLAN section 5: `{"error":{"code","message","request_id"}}`. */
export class ApiError extends Error {
  readonly status: number;
  readonly code: string;
  readonly requestId?: string;
  /** Parsed response body when the server sent JSON that is not the error model. */
  readonly body?: unknown;

  constructor(init: { status: number; code: string; message: string; requestId?: string; body?: unknown }) {
    super(init.message);
    this.name = "ApiError";
    this.status = init.status;
    this.code = init.code;
    this.requestId = init.requestId;
    this.body = init.body;
  }

  /** Per-field validation errors from `error.details.fields` (400 `validation_failed`). */
  get fields(): Record<string, FieldError> {
    const b = this.body as { error?: { details?: { fields?: unknown } } } | undefined;
    const f = b?.error?.details?.fields;
    if (typeof f !== "object" || f === null) return {};
    const out: Record<string, FieldError> = {};
    for (const [k, v] of Object.entries(f as Record<string, unknown>)) {
      if (typeof v === "object" && v !== null) {
        const o = v as { code?: unknown; message?: unknown };
        out[k] = { code: String(o.code ?? "invalid"), message: String(o.message ?? "") };
      }
    }
    return out;
  }

  /** 401/403: the server wants credentials the dashboard does not have (the session layer handles 401). */
  get isAuth(): boolean {
    return this.status === 401 || this.status === 403;
  }

  /** True when no HTTP response was received (server down, DNS, CORS, etc). */
  get isNetwork(): boolean {
    return this.status === 0;
  }
}

function isErrorBody(v: unknown): v is ApiErrorBody {
  if (typeof v !== "object" || v === null || !("error" in v)) return false;
  const e = (v as { error: unknown }).error;
  return typeof e === "object" && e !== null && typeof (e as { code?: unknown }).code === "string";
}

export interface RequestOptions {
  method?: string;
  body?: unknown;
  signal?: AbortSignal;
  headers?: Record<string, string>;
  /** Send `body` as is (string / Blob) with this content type instead of JSON. */
  rawBody?: BodyInit;
  /** Return the response text instead of parsing JSON (export). */
  text?: boolean;
  /** A 401 is an expected answer (sign-in, whoami): do not tell the session layer it expired. */
  quiet401?: boolean;
}

/** Fetch JSON from the server; throws `ApiError` for every failure mode. */
export async function request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  const method = opts.method ?? "GET";
  const unsafe = !["GET", "HEAD", "OPTIONS"].includes(method.toUpperCase());
  const headers: Record<string, string> = {
    Accept: "application/json",
    ...(await authHeaders()),
    ...(unsafe ? csrfHeaders() : {}),
    ...opts.headers,
  };
  let body: BodyInit | undefined;
  if (opts.rawBody !== undefined) {
    body = opts.rawBody;
  } else if (opts.body !== undefined) {
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(opts.body);
  }

  let res: Response;
  try {
    res = await fetch(path, { method, headers, body, signal: opts.signal, credentials: "same-origin" });
  } catch (err) {
    if (err instanceof DOMException && err.name === "AbortError") throw err;
    throw new ApiError({
      status: 0,
      code: "network_error",
      message: err instanceof Error ? err.message : "Network request failed",
    });
  }

  const text = await res.text();
  if (opts.text && res.ok) return text as T;
  let parsed: unknown;
  if (text) {
    try {
      parsed = JSON.parse(text);
    } catch {
      if (res.ok) {
        throw new ApiError({ status: res.status, code: "invalid_response", message: "Response was not valid JSON" });
      }
    }
  }

  if (!res.ok) {
    if (res.status === 401 && !opts.quiet401) notifyUnauthorized();
    if (isErrorBody(parsed)) {
      const { code, message, request_id } = parsed.error;
      throw new ApiError({ status: res.status, code, message: message ?? res.statusText, requestId: request_id, body: parsed });
    }
    throw new ApiError({
      status: res.status,
      code: "http_error",
      message: res.statusText || `HTTP ${res.status}`,
      body: parsed,
    });
  }
  return parsed as T;
}

export const send = <T>(method: string, path: string, body?: unknown, headers?: Record<string, string>) =>
  request<T>(path, { method, body, headers });
export const get = <T>(path: string, signal?: AbortSignal) => request<T>(path, { signal });

import type { ApiErrorBody } from "./types";

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
}

/** Fetch JSON from the server; throws `ApiError` for every failure mode. */
export async function request<T>(path: string, opts: RequestOptions = {}): Promise<T> {
  const headers: Record<string, string> = { Accept: "application/json", ...opts.headers };
  let body: string | undefined;
  if (opts.body !== undefined) {
    headers["Content-Type"] = "application/json";
    body = JSON.stringify(opts.body);
  }

  let res: Response;
  try {
    res = await fetch(path, { method: opts.method ?? "GET", headers, body, signal: opts.signal });
  } catch (err) {
    if (err instanceof DOMException && err.name === "AbortError") throw err;
    throw new ApiError({
      status: 0,
      code: "network_error",
      message: err instanceof Error ? err.message : "Network request failed",
    });
  }

  const text = await res.text();
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

export const get = <T>(path: string, signal?: AbortSignal) => request<T>(path, { signal });

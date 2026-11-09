import type { FetchLike } from "../src/index";

export interface Call {
  url: string;
  method: string;
  headers: Record<string, string>;
  body?: string;
}

type Reply = Response | ((c: Call) => Response | Promise<Response>) | Error;

export const json = (status: number, body: unknown, headers: Record<string, string> = {}): Response =>
  new Response(status === 204 ? null : JSON.stringify(body), { status, headers: { "content-type": "application/json", ...headers } });

export const errBody = (code: string, message = code, details?: unknown) => ({ error: { code, message, request_id: "req-1", details } });

/** A scripted fetch: replies are consumed in order; the last one repeats. */
export function scripted(...replies: Reply[]): { fetch: FetchLike; calls: Call[] } {
  const calls: Call[] = [];
  let i = 0;
  const fetch: FetchLike = async (url, init = {}) => {
    const call: Call = {
      url,
      method: init.method ?? "GET",
      headers: Object.fromEntries(Object.entries((init.headers ?? {}) as Record<string, string>)),
      body: typeof init.body === "string" ? init.body : undefined,
    };
    calls.push(call);
    const r = replies[Math.min(i++, replies.length - 1)]!;
    if (r instanceof Error) throw r;
    return typeof r === "function" ? r(call) : r.clone();
  };
  return { fetch, calls };
}

export const noSleep = { sleep: () => Promise.resolve(), random: () => 0.5 };

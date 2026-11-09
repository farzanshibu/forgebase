import { errorFromResponse, ForgeBaseError, RealtimeError } from "./errors";
import type { FetchLike } from "./http";
import type { Row } from "./records";

// ---- protocol (ADR-0011) --------------------------------------------------------------------

export interface RecordEvent<T = Row> {
  type: "record.created" | "record.updated" | "record.deleted";
  /** Subscription id. */
  id: string;
  /** Resume cursor: the SDK stores the highest one per subscription. */
  cursor: number;
  collection: string;
  record_id: string;
  tenant_id?: string | null;
  timestamp: string;
  data: T;
}

export interface ChannelMessage<T = unknown> {
  type: "message";
  id: string;
  channel: string;
  from: string;
  data: T;
  timestamp: string;
}

export interface PresenceMember {
  connection_id: string;
  actor: string;
  meta?: unknown;
  joined_at: string;
  instance?: string;
}

export interface PresenceEvent {
  type: "presence.join" | "presence.leave";
  id: string;
  channel: string;
  member: PresenceMember;
}

/** The server could not replay from the cursor: reload your state, then continue from `cursor`. */
export interface ResetEvent {
  type: "reset";
  id: string;
  reason: "cursor_expired" | "cursor_invalid" | "too_far_behind" | (string & {});
  cursor: number;
}

export type RealtimeEvent<T = Row> = RecordEvent<T> | ChannelMessage | PresenceEvent | ResetEvent;
export type RealtimeHandler<T = Row> = (event: RealtimeEvent<T>) => void;

export interface SubscribeOptions {
  /** Client-chosen id (1-64 of `[A-Za-z0-9_.:-]`); generated when omitted. */
  id?: string;
  /** Collection to watch (add `record` for one record, or `filter`). */
  collection?: string;
  record?: string;
  filter?: string;
  params?: Record<string, unknown>;
  /** Custom pub/sub channel (instead of `collection`). */
  channel?: string;
  /** Replay every event after this cursor, then go live. Without it the stream starts at the head. */
  cursor?: number;
}

export interface Subscription {
  readonly id: string;
  /** Highest cursor delivered so far (what a reconnect resumes from). */
  readonly cursor: number | undefined;
  unsubscribe(): Promise<void>;
}

export type RealtimeTransport = "auto" | "websocket" | "sse";
export type RealtimeState = "idle" | "connecting" | "open" | "reconnecting" | "closed";
type Listener = (...args: never[]) => void;

/** Minimal WebSocket surface used by the SDK (browsers, Node 22+, `ws`). */
export interface WebSocketLike {
  readonly readyState: number;
  send(data: string): void;
  close(code?: number, reason?: string): void;
  onopen: ((ev: unknown) => void) | null;
  onmessage: ((ev: { data: unknown }) => void) | null;
  onclose: ((ev: { code: number; reason: string }) => void) | null;
  onerror: ((ev: unknown) => void) | null;
}
export type WebSocketCtor = new (url: string) => WebSocketLike;

export interface TokenSource {
  /** Credential for the realtime connection (access token, or an API key). */
  get(): Promise<{ token?: string; headers: Record<string, string> }>;
  /** Called after the server closed the socket with 4001: get a fresh credential. */
  refresh(): Promise<void>;
  /** Called when the access token rotates while connected. */
  onChange(fn: (token: string | undefined) => void): () => void;
}

export interface RealtimeConfig {
  baseUrl: string;
  tokens: TokenSource;
  fetch?: FetchLike;
  WebSocket?: WebSocketCtor;
  /** `auto` (default): WebSocket, falling back to SSE when it cannot connect. */
  transport?: RealtimeTransport;
  /** Consecutive failed WebSocket connects before falling back to SSE (default 2). */
  fallbackAfter?: number;
  reconnect?: { baseDelayMs?: number; maxDelayMs?: number; maxAttempts?: number };
  /** Test hooks. */
  sleep?: (ms: number) => Promise<void>;
  random?: () => number;
}

interface Sub {
  id: string;
  spec: Record<string, unknown>;
  handler: RealtimeHandler<never>;
  cursor?: number;
  active: boolean;
  /** Set when the server refused the subscription while it was being (re)established. */
  error?: Error;
}

interface Pending {
  resolve(m: Record<string, unknown>): void;
  reject(e: Error): void;
}

const WS_OPEN = 1;

/** Realtime client: `client.realtime`. One connection carries every subscription. */
export class RealtimeClient {
  private readonly subs = new Map<string, Sub>();
  private readonly listeners = new Map<string, Set<Listener>>();
  private readonly pending = new Map<string, Pending>();
  private ws: WebSocketLike | null = null;
  private sseAbort: AbortController | null = null;
  private loop: Promise<void> | null = null;
  private wake: (() => void) | null = null;
  private stopped = false;
  private seq = 0;
  private wsFailures = 0;
  private mode: "websocket" | "sse";
  private _state: RealtimeState = "idle";
  private unwatchToken: (() => void) | null = null;
  private opened: { resolve(): void; reject(e: Error): void }[] = [];
  private readonly sleep: (ms: number) => Promise<void>;
  private readonly random: () => number;

  constructor(private readonly cfg: RealtimeConfig) {
    this.sleep = cfg.sleep ?? ((ms) => new Promise((r) => setTimeout(r, ms)));
    this.random = cfg.random ?? Math.random;
    const wsCtor = this.wsCtor();
    this.mode = cfg.transport === "sse" || (cfg.transport !== "websocket" && !wsCtor) ? "sse" : "websocket";
  }

  get state(): RealtimeState {
    return this._state;
  }

  /** Transport currently in use. */
  get transport(): "websocket" | "sse" {
    return this.mode;
  }

  /** Events: `open`, `close` (code, reason), `reconnecting` (attempt, delayMs), `transport` ("websocket"|"sse"), `error` (Error). */
  on(event: "open", fn: () => void): () => void;
  on(event: "close", fn: (code: number, reason: string) => void): () => void;
  on(event: "reconnecting", fn: (attempt: number, delayMs: number) => void): () => void;
  on(event: "transport", fn: (t: "websocket" | "sse") => void): () => void;
  on(event: "error", fn: (e: Error) => void): () => void;
  on(event: string, fn: (...args: never[]) => void): () => void {
    let set = this.listeners.get(event);
    if (!set) this.listeners.set(event, (set = new Set()));
    set.add(fn);
    return () => set.delete(fn);
  }

  private emit(event: string, ...args: unknown[]): void {
    for (const fn of this.listeners.get(event) ?? []) {
      try {
        (fn as (...a: unknown[]) => void)(...args);
      } catch {
        /* listener errors must not break the connection */
      }
    }
  }

  private wsCtor(): WebSocketCtor | undefined {
    return this.cfg.WebSocket ?? (globalThis as { WebSocket?: WebSocketCtor }).WebSocket;
  }

  // ---- public API -----------------------------------------------------------------------------

  /** Opens the connection (done by the first `subscribe`). Resolves once connected. */
  connect(): Promise<void> {
    this.stopped = false;
    const ready = this._state === "open" ? Promise.resolve() : new Promise<void>((resolve, reject) => this.opened.push({ resolve, reject }));
    if (!this.loop) {
      this.unwatchToken ??= this.cfg.tokens.onChange((token) => {
        if (token && this.mode === "websocket" && this.ws?.readyState === WS_OPEN) this.ws.send(JSON.stringify({ type: "auth", token }));
      });
      this.loop = this.run().finally(() => {
        this.loop = null;
      });
    }
    return ready;
  }

  /** Subscribes to a collection, a record or a channel. Resolves when the server accepted the subscription. */
  async subscribe<T = Row>(opts: SubscribeOptions, handler: RealtimeHandler<T>): Promise<Subscription> {
    const id = opts.id ?? `s${++this.seq}_${Math.floor(this.random() * 1e6).toString(36)}`;
    if (this.subs.has(id)) throw new RealtimeError("duplicate_subscription", `subscription id ${id} is already in use`);
    const { id: _id, cursor, ...rest } = opts;
    void _id;
    const spec = Object.fromEntries(Object.entries(rest).filter(([, v]) => v !== undefined));
    const sub: Sub = { id, spec, handler: handler as RealtimeHandler<never>, cursor, active: false };
    this.subs.set(id, sub);
    try {
      if (this._state === "open" && this.mode === "websocket") {
        await this.activate(sub);
      } else if (this._state === "open") {
        await this.restartSse(sub);
      } else {
        await this.connect();
        if (sub.error) throw sub.error;
        if (!sub.active) await this.until(sub);
      }
    } catch (err) {
      this.subs.delete(id);
      throw err;
    }
    return {
      id,
      get cursor() {
        return sub.cursor;
      },
      unsubscribe: () => this.unsubscribe(id),
    };
  }

  async unsubscribe(id: string): Promise<void> {
    const sub = this.subs.get(id);
    if (!sub) return;
    this.subs.delete(id);
    if (this.mode === "websocket" && this.ws?.readyState === WS_OPEN && sub.active) {
      await this.request({ type: "unsubscribe", id }).catch(() => undefined);
    } else if (this.mode === "sse" && this._state === "open") {
      if (this.subs.size > 0) await this.restartSse().catch(() => undefined);
      else this.sseAbort?.abort();
    }
  }

  /** Publishes to a channel (WebSocket: over the socket; SSE: REST). */
  async publish(channel: string, data: unknown): Promise<void> {
    if (this.mode === "websocket" && this.ws?.readyState === WS_OPEN) {
      await this.request({ type: "publish", channel, data });
      return;
    }
    const { headers } = await this.cfg.tokens.get();
    const res = await this.fetcher()(`${this.cfg.baseUrl}/api/v1/realtime/channels/${encodeURIComponent(channel)}/messages`, {
      method: "POST",
      headers: { "Content-Type": "application/json", Accept: "application/json", ...headers },
      body: JSON.stringify({ data }),
    });
    if (!res.ok) throw errorFromResponse(res.status, await res.json().catch(() => undefined), res.headers);
  }

  /** Joins a channel's presence (WebSocket only; the channel must be subscribed). Resolves with the members. */
  async joinPresence(channel: string, meta?: unknown): Promise<PresenceMember[]> {
    this.needSocket("presence");
    const r = await this.request({ type: "presence.join", channel, meta });
    return (r.members as PresenceMember[] | undefined) ?? [];
  }

  async leavePresence(channel: string): Promise<void> {
    this.needSocket("presence");
    await this.request({ type: "presence.leave", channel });
  }

  /** Members currently present (REST, works on both transports). */
  async presence(channel: string): Promise<PresenceMember[]> {
    const { headers } = await this.cfg.tokens.get();
    const res = await this.fetcher()(`${this.cfg.baseUrl}/api/v1/realtime/channels/${encodeURIComponent(channel)}/presence`, {
      headers: { Accept: "application/json", ...headers },
    });
    const body = (await res.json().catch(() => undefined)) as { members?: PresenceMember[] } | undefined;
    if (!res.ok) throw errorFromResponse(res.status, body, res.headers);
    return body?.members ?? [];
  }

  /** Closes the connection and forgets every subscription. */
  close(): void {
    this.stopped = true;
    this.unwatchToken?.();
    this.unwatchToken = null;
    this.subs.clear();
    this.fail(new RealtimeError("closed", "realtime client closed"));
    this.ws?.close(1000, "client closed");
    this.sseAbort?.abort();
    this.wake?.();
    this._state = "closed";
  }

  // ---- connection loop ------------------------------------------------------------------------

  private fetcher(): FetchLike {
    return this.cfg.fetch ?? ((u, i) => (globalThis.fetch as FetchLike)(u, i));
  }

  private async run(): Promise<void> {
    const rc = { baseDelayMs: 500, maxDelayMs: 15_000, maxAttempts: Infinity, ...this.cfg.reconnect };
    let attempt = 0;
    while (!this.stopped) {
      this._state = attempt === 0 ? "connecting" : "reconnecting";
      let welcomed = false;
      try {
        welcomed = await (this.mode === "websocket" ? this.runSocket() : this.runSse());
      } catch (err) {
        if (this.stopped) break;
        const e = err instanceof Error ? err : new Error(String(err));
        this.emit("error", e);
        // Not worth retrying: bad credentials, forbidden, bad request.
        if (e instanceof ForgeBaseError && [400, 401, 403, 404].includes(e.status) && !(await this.tryRefresh(e))) {
          this.fail(e);
          break;
        }
        if (this.mode === "websocket" && this.shouldFallback(e)) continue;
      }
      if (this.stopped) break;
      if (welcomed) attempt = 0;
      for (const s of this.subs.values()) s.active = false;
      attempt++;
      if (attempt > rc.maxAttempts) {
        this.fail(new RealtimeError("reconnect_failed", `gave up after ${rc.maxAttempts} reconnect attempts`));
        break;
      }
      const delay = welcomed && attempt === 1 ? 0 : Math.floor(this.random() * Math.min(rc.maxDelayMs, rc.baseDelayMs * 2 ** (attempt - 1)));
      this._state = "reconnecting";
      this.emit("reconnecting", attempt, delay);
      await new Promise<void>((resolve) => {
        this.wake = resolve;
        void this.sleep(delay).then(resolve);
      });
      this.wake = null;
    }
    if (this._state !== "closed") this._state = "closed";
  }

  private async tryRefresh(e: ForgeBaseError): Promise<boolean> {
    if (e.status !== 401) return false;
    try {
      await this.cfg.tokens.refresh();
      return true;
    } catch {
      return false;
    }
  }

  private shouldFallback(_e: Error): boolean {
    if (this.cfg.transport === "websocket") return false;
    this.wsFailures++;
    if (this.wsFailures >= (this.cfg.fallbackAfter ?? 2) || !this.wsCtor()) {
      this.mode = "sse";
      this.emit("transport", "sse");
      return true;
    }
    return false;
  }

  /** Resolves with whether the server welcomed us. */
  private runSocket(): Promise<boolean> {
    return new Promise<boolean>((resolve, reject) => {
      const Ctor = this.wsCtor();
      if (!Ctor) return reject(new RealtimeError("no_websocket", "no WebSocket implementation: pass `WebSocket` or use SSE"));
      void this.cfg.tokens
        .get()
        .then(({ token }) => {
          const url = new URL(`${this.cfg.baseUrl}/api/v1/realtime/ws`);
          url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
          if (token) url.searchParams.set("access_token", token);
          const ws = new Ctor(url.toString());
          this.ws = ws;
          let welcomed = false;
          let timer: ReturnType<typeof setInterval> | undefined;
          let last = Date.now();
          const done = (code: number, reason: string, err?: Error) => {
            clearInterval(timer);
            if (this.ws === ws) this.ws = null;
            this.failPending(new RealtimeError("connection_closed", `connection closed (${code})`));
            this.emit("close", code, reason);
            if (err) return reject(err);
            if (code === 4001) void this.cfg.tokens.refresh().catch(() => undefined).finally(() => resolve(welcomed));
            else resolve(welcomed);
          };
          ws.onerror = () => {
            if (!welcomed) done(1006, "connect failed", new RealtimeError("connect_failed", "WebSocket connection failed"));
          };
          ws.onclose = (ev) => done(ev.code, ev.reason);
          ws.onmessage = (ev) => {
            last = Date.now();
            let m: Record<string, unknown>;
            try {
              m = JSON.parse(String(ev.data)) as Record<string, unknown>;
            } catch {
              return;
            }
            if (m.type === "welcome" && !welcomed) {
              welcomed = true;
              this.wsFailures = 0;
              const hb = Number(m.heartbeat_ms) || 25_000;
              timer = setInterval(() => {
                if (Date.now() - last > hb * 2.5) ws.close(4002, "heartbeat timeout");
                else if (ws.readyState === WS_OPEN) ws.send(JSON.stringify({ type: "ping" }));
              }, hb);
              this._state = "open";
              void this.resubscribeAll().then(() => {
                this.emit("open");
                this.settleOpened();
              });
              return;
            }
            this.onMessage(m);
          };
        })
        .catch(reject);
    });
  }

  private async resubscribeAll(): Promise<void> {
    await Promise.all(
      [...this.subs.values()].map((s) =>
        this.activate(s).catch((err: Error) => {
          // A subscription the server now refuses (rule changed while offline) is dropped and reported.
          s.error = err;
          this.subs.delete(s.id);
          this.emit("error", err);
        }),
      ),
    );
  }

  private async activate(sub: Sub): Promise<void> {
    const r = await this.request({ type: "subscribe", id: sub.id, ...sub.spec, ...(sub.cursor === undefined ? {} : { cursor: sub.cursor }) });
    sub.active = true;
    if (typeof r.cursor === "number" && sub.cursor === undefined) sub.cursor = r.cursor;
  }

  private request(msg: Record<string, unknown>): Promise<Record<string, unknown>> {
    const ws = this.ws;
    if (!ws || ws.readyState !== WS_OPEN) return Promise.reject(new RealtimeError("not_connected", "realtime socket is not open"));
    const ref = String(++this.seq);
    return new Promise((resolve, reject) => {
      this.pending.set(ref, { resolve, reject });
      ws.send(JSON.stringify({ ...msg, ref }));
    });
  }

  private onMessage(m: Record<string, unknown>): void {
    const ref = typeof m.ref === "string" ? m.ref : typeof m.ref === "number" ? String(m.ref) : undefined;
    if (ref !== undefined && this.pending.has(ref)) {
      const p = this.pending.get(ref)!;
      this.pending.delete(ref);
      if (m.type === "error") p.reject(new RealtimeError(String(m.code ?? "error"), String(m.message ?? "realtime error")));
      else p.resolve(m);
      return;
    }
    switch (m.type) {
      case "record.created":
      case "record.updated":
      case "record.deleted":
      case "message":
      case "presence.join":
      case "presence.leave":
      case "reset": {
        const sub = typeof m.id === "string" ? this.subs.get(m.id) : undefined;
        if (!sub) return;
        if (typeof m.cursor === "number" && (sub.cursor === undefined || m.cursor > sub.cursor || m.type === "reset")) sub.cursor = m.cursor;
        try {
          sub.handler(m as unknown as RealtimeEvent<never>);
        } catch (err) {
          this.emit("error", err instanceof Error ? err : new Error(String(err)));
        }
        return;
      }
      case "subscribed": {
        // SSE has no refs: the reply carries the subscription id.
        const sub = typeof m.id === "string" ? this.subs.get(m.id) : undefined;
        if (sub) {
          sub.active = true;
          if (typeof m.cursor === "number" && sub.cursor === undefined) sub.cursor = m.cursor;
          this.sseWaiters.get(sub.id)?.();
        }
        return;
      }
      case "error":
        this.emit("error", new RealtimeError(String(m.code ?? "error"), String(m.message ?? "realtime error")));
        if (typeof m.id === "string") this.sseFailed(m.id, new RealtimeError(String(m.code ?? "error"), String(m.message ?? "")));
        return;
      default:
    }
  }

  // ---- SSE ------------------------------------------------------------------------------------

  private sseWaiters = new Map<string, () => void>();
  private sseRejects = new Map<string, (e: Error) => void>();

  private sseFailed(id: string, e: Error): void {
    this.sseRejects.get(id)?.(e);
  }

  private until(sub: Sub): Promise<void> {
    return new Promise<void>((resolve, reject) => {
      this.sseWaiters.set(sub.id, () => {
        this.sseWaiters.delete(sub.id);
        this.sseRejects.delete(sub.id);
        resolve();
      });
      this.sseRejects.set(sub.id, (e) => {
        this.sseWaiters.delete(sub.id);
        this.sseRejects.delete(sub.id);
        reject(e);
      });
    });
  }

  private async restartSse(sub?: Sub): Promise<void> {
    const wait = sub ? this.until(sub) : Promise.resolve();
    this.sseAbort?.abort();
    this.wake?.();
    await wait;
  }

  private async runSse(): Promise<boolean> {
    if (this.subs.size === 0) {
      // Nothing to stream yet: report open and idle until a subscription arrives.
      this._state = "open";
      this.emit("open");
      this.settleOpened();
      await new Promise<void>((resolve) => {
        this.wake = resolve;
      });
      this.wake = null;
      return true;
    }
    const abort = new AbortController();
    this.sseAbort = abort;
    const { headers } = await this.cfg.tokens.get();
    const specs = [...this.subs.values()].map((s) => ({ id: s.id, ...s.spec, ...(s.cursor === undefined ? {} : { cursor: s.cursor }) }));
    const url = new URL(`${this.cfg.baseUrl}/api/v1/realtime/sse`);
    url.searchParams.set("subscriptions", JSON.stringify(specs));
    const res = await this.fetcher()(url.toString(), { headers: { Accept: "text/event-stream", ...headers }, signal: abort.signal }).catch((err) => {
      if (abort.signal.aborted) return null;
      throw err;
    });
    if (!res) return false;
    if (!res.ok) {
      const body = await res.json().catch(() => undefined);
      throw errorFromResponse(res.status, body, res.headers);
    }
    if (!res.body) throw new RealtimeError("sse_unsupported", "this runtime cannot stream response bodies");
    this._state = "open";
    this.emit("open");
    this.settleOpened();
    const reader = res.body.getReader();
    const decoder = new TextDecoder();
    let buf = "";
    let idle: ReturnType<typeof setTimeout> | undefined;
    const arm = () => {
      clearTimeout(idle);
      idle = setTimeout(() => abort.abort(), 75_000);
    };
    arm();
    try {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) break;
        arm();
        buf += decoder.decode(value, { stream: true }).replace(/\r\n/g, "\n");
        let i: number;
        while ((i = buf.indexOf("\n\n")) >= 0) {
          const block = buf.slice(0, i);
          buf = buf.slice(i + 2);
          this.onSseBlock(block);
        }
      }
    } catch (err) {
      if (!abort.signal.aborted) throw err;
    } finally {
      clearTimeout(idle);
    }
    this.emit("close", 1000, "stream ended");
    return true;
  }

  private onSseBlock(block: string): void {
    let event = "message";
    const data: string[] = [];
    for (const line of block.split("\n")) {
      if (line.startsWith(":")) continue;
      const c = line.indexOf(":");
      const field = c < 0 ? line : line.slice(0, c);
      const val = c < 0 ? "" : line.slice(c + 1).replace(/^ /, "");
      if (field === "event") event = val;
      else if (field === "data") data.push(val);
    }
    if (data.length === 0) return;
    let m: Record<string, unknown>;
    try {
      m = JSON.parse(data.join("\n")) as Record<string, unknown>;
    } catch {
      return;
    }
    if (event === "close") {
      this.sseAbort?.abort();
      return;
    }
    if (m.type === undefined) m.type = event;
    this.onMessage(m);
  }

  // ---- helpers --------------------------------------------------------------------------------

  private needSocket(what: string): void {
    if (this.mode !== "websocket") throw new RealtimeError("unsupported_transport", `${what} needs the WebSocket transport (SSE is receive-only)`);
  }

  private settleOpened(): void {
    const w = this.opened;
    this.opened = [];
    for (const o of w) o.resolve();
  }

  private failPending(e: Error): void {
    const p = [...this.pending.values()];
    this.pending.clear();
    for (const x of p) x.reject(e);
  }

  private fail(e: Error): void {
    this.failPending(e);
    const w = this.opened;
    this.opened = [];
    for (const o of w) o.reject(e);
    for (const r of this.sseRejects.values()) r(e);
    this.sseRejects.clear();
    this.sseWaiters.clear();
  }
}

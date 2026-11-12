// Ambient types for the ForgeBase function prelude (ADR-0027).
//
// prelude.ts is the source; prelude.js is generated from it by
// scripts/build-js-prelude.ts (Node's type stripper, which replaces types with
// whitespace so that every line and column stays where it was). Never edit
// prelude.js by hand: edit prelude.ts and rerun the script.
//
// prelude.ts may only carry inline annotations on its existing lines, so the
// shapes it needs live here. The prelude's own classes (URL, Headers, Request,
// Response, ...) are block-scoped inside its IIFE and cannot be named from this
// file; types that involve them are written inline in prelude.ts.
//
// Type-check with:
//   dashboard/node_modules/.bin/tsc -p crates/forgebase-functions/js/tsconfig.json

// ------------------------------------------------------------ engine (engine.c)

/** The native host object engine.c installs as globalThis.__forgebase_native. */
interface ForgeBaseNative {
  /** Runs host op `op` with a JSON argument and returns its JSON result ("" for none); a failure throws an Error whose `hostError` is the JSON {code, message}. */
  call(op: string, arg: string): string;
  /** Writes a log line: 0 debug, 1 info, 2 warn, 3 error. */
  log(level: number, message: string): void;
  utf8encode(text: string): Uint8Array<ArrayBuffer>;
  utf8decode(bytes: ArrayBuffer | ArrayBufferView, fatal: boolean, ignoreBOM: boolean): string;
  b64encode(bytes: ArrayBuffer | ArrayBufferView): string;
  b64decode(base64: string): Uint8Array<ArrayBuffer>;
  /** `n` (at most 65536) bytes from the host CSPRNG. */
  random(n: number): Uint8Array<ArrayBuffer>;
  /** Monotonic clock in fractional milliseconds. */
  now(): number;
}

/** Present until the prelude takes it (and deletes it) on its first line. */
declare var __forgebase_native: ForgeBaseNative | undefined;

/** QuickJS's intrinsic DOMException (JS_AddIntrinsicDOMException in engine.c). */
declare class DOMException extends Error {
  constructor(message?: string, name?: string);
  readonly code: number;
}

/** Installed on globalThis by the prelude itself; declared for AbortSignal.timeout. */
declare function setTimeout(callback: TimerCallback, ms?: number, ...args: unknown[]): number;

// --------------------------------------------------------------- values

/** A typed array or a DataView, seen through what every view has: its buffer window and its constructor. */
type AnyView = TypedArrayView | DataViewView;
interface ViewConstructor {
  readonly name: string;
  new (buffer: ArrayBufferLike, byteOffset?: number, length?: number): AnyView;
}
interface TypedArrayView {
  readonly buffer: ArrayBufferLike;
  readonly byteOffset: number;
  readonly byteLength: number;
  readonly length: number;
  readonly [index: number]: number | bigint;
  readonly constructor: ViewConstructor;
}
interface DataViewView {
  readonly buffer: ArrayBufferLike;
  readonly byteOffset: number;
  readonly byteLength: number;
  /** A DataView has no length. */
  readonly length?: undefined;
  readonly constructor: ViewConstructor;
}

interface ArrayBufferConstructor {
  /** Narrows to AnyView: the lib's ArrayBufferView has neither `length` nor a constructible `constructor`. */
  isView(arg: unknown): arg is AnyView;
}

/** Any value, as console's inspect() takes it apart (any other object is printed through its own keys). */
type InspectValue =
  | undefined
  | null
  | boolean
  | number
  | bigint
  | string
  | symbol
  | Function
  | Error
  | Date
  | AnyView
  | InspectValue[]
  | Map<InspectValue, InspectValue>
  | Set<InspectValue>
  | InspectRecord;
interface InspectRecord {
  readonly [key: string]: InspectValue;
}

/** Any value, as structuredClone() takes it apart (any other object is cloned through its own keys). */
type CloneValue =
  | undefined
  | null
  | boolean
  | number
  | bigint
  | string
  | symbol
  | Function
  | Date
  | RegExp
  | ArrayBuffer
  | DataView
  | AnyView
  | Map<CloneValue, CloneValue>
  | Set<CloneValue>
  | Error
  | CloneValue[]
  | Promise<unknown>
  | WeakMap<WeakKey, unknown>
  | WeakSet<WeakKey>
  | CloneRecord;
interface CloneRecord {
  readonly [key: string]: CloneValue;
}

/** The global error constructors structuredClone() recreates a cloned error with (any other name falls back to Error). */
type ErrorConstructorName = "Error" | "EvalError" | "RangeError" | "ReferenceError" | "SyntaxError" | "TypeError" | "URIError";

/**
 * The `init` of Headers and URLSearchParams when it is neither a string nor an
 * instance: a record, or an iterable of pairs (told apart at run time by
 * Symbol.iterator).
 */
interface RecordOrPairs {
  readonly [key: string]: unknown;
  readonly [Symbol.iterator]?: () => Iterator<Iterable<unknown>>;
}

/** What a body (Request, Response, storage put) is made from: strings, URLSearchParams, Blobs and bytes are recognised, streams (objects with getReader) are rejected, anything else is stringified. */
type BodyValue = string | number | bigint | boolean | symbol | null | undefined | (object & { readonly getReader?: unknown });

// --------------------------------------------------------------- timers

type TimerCallback = (...args: unknown[]) => unknown;
interface Timer {
  at: number;
  fn: TimerCallback;
  args: unknown[];
  /** The interval in milliseconds, 0 for a one-shot timer. */
  repeat: number;
  seq: number;
}

// ------------------------------------------------------------------ URL

/** A parsed URL's components, as URL keeps them. */
interface ParsedURL {
  protocol: string;
  username: string;
  password: string;
  hostname: string;
  port: string;
  pathname: string;
  search: string;
  hash: string;
  /** A non-hierarchical URL (mailto:, data:, ...): pathname is kept verbatim. */
  opaque: boolean;
}

// --------------------------------------------------------------- crypto

/**
 * An algorithm name, or an object with one (and, for importKey, a hash). The
 * string member says that a bare name has no `name` or `hash` property, so
 * code may read `algorithm.hash` without first checking for a string.
 */
type AlgorithmParam = (string & { readonly name?: undefined; readonly hash?: undefined }) | { readonly name?: string; readonly hash?: AlgorithmParam };

interface HmacKeyAlgorithm {
  name: string;
  hash: { name: string };
  length: number;
}

// ------------------------------------------------------- AbortController

interface AbortEvent {
  readonly type: string;
  /** The AbortSignal. */
  readonly target: object;
}
type AbortListener = (event: AbortEvent) => unknown;

// ------------------------------------------------------- host op messages

/** The JSON argument of a host op (see crates/forgebase-functions/src/jsops.rs). */
type HostArgs = Record<string, unknown>;

/** Argument of the "fetch" host op. */
interface HostFetchRequest {
  url: string;
  method: string;
  headers: Record<string, string>;
  body_base64?: string;
}

/** Result of the "fetch" host op. */
interface HostFetchResponse {
  status: number;
  headers?: Record<string, string>;
  body?: string;
  body_base64?: string;
  /** The final URL, after redirects. */
  url?: string;
}

/** Result of the "storage.get" host op. */
interface HostStorageObject {
  body_base64: string;
  info: { readonly key: string; readonly content_type?: string; readonly [field: string]: unknown };
}

/** The caller's identity (auth_info in jsops.rs). */
interface HostAuth {
  readonly type: string;
  readonly id?: string | null;
  readonly collection?: string | null;
  readonly roles?: readonly string[];
  readonly permissions?: readonly string[];
  readonly realm?: string | null;
  readonly org?: string | null;
  readonly tenant?: string | null;
}

/** The JSON input of one execution, as run() receives it (http.rs builds it for HTTP). */
interface HostInvocation {
  readonly trigger: string;
  readonly auth?: HostAuth | null;
  readonly request_id?: string | null;
  readonly function?: { readonly name?: string | null; readonly version?: unknown } | null;
  readonly method?: string;
  readonly url?: string;
  readonly headers?: Readonly<Record<string, string>>;
  readonly body_base64?: string;
  /** Other triggers carry their event's own fields. */
  readonly [field: string]: unknown;
}

/** What run() returns for an HTTP trigger (JSON-encoded). */
interface HostHttpResponse {
  status: number;
  headers: Record<string, string>;
  body_base64?: string;
}

// --------------------------------------------------------- forgebase module

/** Options of ctx.db.from(c).list(). */
interface ListOptions {
  readonly filter?: unknown;
  readonly params?: unknown;
  readonly sort?: unknown;
  readonly page?: unknown;
  readonly perPage?: unknown;
  readonly cursor?: unknown;
}

interface KvSetOptions {
  readonly ttlMs?: number;
  readonly nx?: boolean;
  readonly xx?: boolean;
}

/** ctx.kv: Redis-style commands over the "kv" host op. */
interface KvApi {
  get(key: unknown): Promise<string | null>;
  set(key: unknown, value: unknown, opts?: KvSetOptions): Promise<boolean>;
  del(...keys: unknown[]): Promise<number>;
  incr(key: unknown, by?: number): Promise<number>;
  decr(key: unknown, by?: number): Promise<number>;
  expire(key: unknown, ttlMs: number): Promise<boolean>;
  ttl(key: unknown): Promise<number>;
  exists(key: unknown): Promise<boolean>;
  keys(prefix?: unknown): Promise<string[]>;
  hget(key: unknown, field: unknown): Promise<string | null>;
  hset(key: unknown, field: unknown, value?: unknown): Promise<number>;
  hdel(key: unknown, ...fields: unknown[]): Promise<number>;
  hgetall(key: unknown): Promise<Record<string, string>>;
  command(...cmd: unknown[]): Promise<unknown>;
  /** A namespaced view; only on the default (un-namespaced) API. */
  namespace?: (ns: unknown) => KvApi;
}

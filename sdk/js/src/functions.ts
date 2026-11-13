import { InvalidResponseError } from "./errors";
import type { RequestOptions, Transport } from "./http";
import type { Schemas } from "./types";

/*
 * Server functions (`/api/v1/functions`): `client.functions`.
 * Deploy, inspect, roll back and delete functions, manage their secrets and key-value rules, read execution logs,
 * and invoke HTTP functions. Managing needs `manage_functions` (reading `read_functions`); invoking depends on the
 * function's access mode (`public`, `authenticated` or `permission`).
 */

export type FunctionInfo = Schemas["FunctionInfo"];
export type FunctionVersion = Schemas["VersionInfo"];
export type FunctionDeployed = Schemas["Deployed"];
export type FunctionVersionSource = Schemas["VersionSource"];
export type FunctionExecutionLog = Schemas["ExecutionLog"];
export type FunctionSecretInfo = Schemas["SecretInfo"];
export type FunctionKvRule = Schemas["KvRule"];
export type FunctionAccess = Schemas["InvokeAccess"];
export type FunctionLanguage = Schemas["Language"];
export type FunctionHook = Schemas["HookSpec"];

/** Limits of one execution; every field is optional (server defaults apply). */
export interface FunctionLimits {
  /** Fuel per execution (roughly: WebAssembly instructions, plus a charge per host call). */
  fuel?: number;
  /** Largest linear memory in bytes. */
  memoryBytes?: number;
  /** Wall-clock limit in milliseconds, host calls such as `fetch` included. */
  timeoutMs?: number;
  /** Executions at the same time; more get `429 function_busy`. */
  maxConcurrency?: number;
}

/** What `deploy` takes: a JavaScript / TypeScript `source`, or a `wasm` module. */
export interface FunctionDeployInput {
  /** Function name; the first deploy creates the function, later ones add a version. */
  name: string;
  /** Single-file ES module that calls `serve()` from `"forgebase"` (at most 1 MiB). */
  source?: string;
  /** Language of `source` (server default: `typescript`). */
  language?: FunctionLanguage;
  /** WebAssembly module: raw bytes or an already base64-encoded string. */
  wasm?: Uint8Array | ArrayBuffer | string;
  /** Who may invoke it over HTTP (kept from the previous deploy when omitted; `permission` on create). */
  access?: FunctionAccess;
  /** Make the new version active (server default: `true`). */
  activate?: boolean;
  /** Granted capabilities (`db.read`, `db.write`, `kv`, `log`, `env`, `secrets`, `fetch`, `vector.read`, `storage.write`, `queues.publish`, ... ). */
  capabilities?: string[];
  description?: string;
  /** Plain environment values, readable with the `env` capability. */
  env?: Record<string, string>;
  /** Hosts `fetch` may call (`api.example.com`, `*.example.com`); empty means any public host. */
  fetchHosts?: string[];
  /** Record hooks: run on events of a collection. */
  hooks?: FunctionHook[];
  limits?: FunctionLimits;
}

export interface FunctionLogsOptions {
  /** Newest first; default 50, at most 500. */
  limit?: number;
}

/** Body an `invoke` call can send: JSON-able values are encoded as JSON, the rest is sent as is. */
export type InvokeBody = unknown;

export interface InvokeOptions {
  /** HTTP method (default `POST`); the function sees it in its input. */
  method?: string;
  /** Sub-path after `/invoke/` (`"users/42"`); the function sees it as its request path. */
  path?: string;
  /** Query string parameters passed to the function. */
  query?: Record<string, string | number | boolean | undefined | null>;
  /** Administrators: run this version instead of the active one (a test run). */
  version?: number;
  headers?: Record<string, string>;
  signal?: AbortSignal;
  /** Override the retry policy for this call. */
  retry?: RequestOptions["retry"];
}

const seg = encodeURIComponent;

const wire = <T extends object>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

function toBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

function deployBody(i: FunctionDeployInput): Schemas["DeployRequest"] {
  const wasm = i.wasm === undefined ? undefined : typeof i.wasm === "string" ? i.wasm : toBase64(i.wasm instanceof Uint8Array ? i.wasm : new Uint8Array(i.wasm));
  const l = i.limits;
  return wire({
    name: i.name,
    source: i.source,
    language: i.language,
    wasm,
    access: i.access,
    activate: i.activate,
    capabilities: i.capabilities,
    description: i.description,
    env: i.env,
    fetch_hosts: i.fetchHosts,
    hooks: i.hooks,
    limits: l && wire({ fuel: l.fuel, memory_bytes: l.memoryBytes, timeout_ms: l.timeoutMs, max_concurrency: l.maxConcurrency }),
  });
}

/** Bodies `fetch` takes as is; everything else is JSON. */
function isRawBody(b: unknown): b is BodyInit {
  if (typeof b === "string" || b instanceof ArrayBuffer || ArrayBuffer.isView(b)) return true;
  if (typeof Blob !== "undefined" && b instanceof Blob) return true;
  if (typeof FormData !== "undefined" && b instanceof FormData) return true;
  if (typeof URLSearchParams !== "undefined" && b instanceof URLSearchParams) return true;
  return typeof ReadableStream !== "undefined" && b instanceof ReadableStream;
}

const isJsonType = (ct: string | null) => !!ct && /(^|[/+])json(\s*;|$)/i.test(ct.trim());

/** Secrets of one function (values are sealed at rest and never returned): `client.functions.secrets`. */
export class FunctionSecretsApi {
  constructor(private readonly t: Transport) {}

  /** Names and timestamps of the secrets. */
  async list(fn: string): Promise<FunctionSecretInfo[]> {
    return (await this.t.request<{ items: FunctionSecretInfo[] }>(`/api/v1/functions/${seg(fn)}/secrets`)).items;
  }

  /** Stores (or replaces) a secret; the function reads it with the `secrets` capability. */
  set(fn: string, key: string, value: string): Promise<FunctionSecretInfo> {
    return this.t.request(`/api/v1/functions/${seg(fn)}/secrets/${seg(key)}`, { method: "PUT", body: { value } });
  }

  delete(fn: string, key: string): Promise<void> {
    return this.t.request(`/api/v1/functions/${seg(fn)}/secrets/${seg(key)}`, { method: "DELETE" });
  }
}

/** Key-value namespaces a JavaScript function may reach beyond its private one: `client.functions.kvRules`. */
export class FunctionKvRulesApi {
  constructor(private readonly t: Transport) {}

  /** The rules in force (deny by default). */
  async get(fn: string): Promise<FunctionKvRule[]> {
    return (await this.t.request<{ items: FunctionKvRule[] }>(`/api/v1/functions/${seg(fn)}/kv-rules`)).items;
  }

  /** Replaces the rules (administrators). Returns the rules now in force. */
  async set(fn: string, rules: FunctionKvRule[]): Promise<FunctionKvRule[]> {
    return (await this.t.request<{ items: FunctionKvRule[] }>(`/api/v1/functions/${seg(fn)}/kv-rules`, { method: "PUT", body: { items: rules } })).items;
  }
}

/** Server functions: `client.functions`. */
export class FunctionsApi {
  readonly secrets: FunctionSecretsApi;
  readonly kvRules: FunctionKvRulesApi;

  constructor(private readonly t: Transport) {
    this.secrets = new FunctionSecretsApi(t);
    this.kvRules = new FunctionKvRulesApi(t);
  }

  /** Every function, by name. */
  async list(): Promise<FunctionInfo[]> {
    return (await this.t.request<{ items: FunctionInfo[] }>("/api/v1/functions")).items;
  }

  get(name: string): Promise<FunctionInfo> {
    return this.t.request(`/api/v1/functions/${seg(name)}`);
  }

  /**
   * Deploys a new version (creating the function on the first deploy) and, unless `activate: false`, makes it active.
   * A source that does not compile is a `ValidationError` (`function_compile_failed`): `details` has the first error's
   * `line`, `column` and `message`, and `details.errors` all of them.
   */
  deploy(input: FunctionDeployInput): Promise<FunctionDeployed> {
    return this.t.request("/api/v1/functions", { method: "POST", body: deployBody(input) });
  }

  delete(name: string): Promise<void> {
    return this.t.request(`/api/v1/functions/${seg(name)}`, { method: "DELETE" });
  }

  /** Versions of a function, with which one is active. */
  async versions(name: string): Promise<FunctionVersion[]> {
    return (await this.t.request<{ items: FunctionVersion[] }>(`/api/v1/functions/${seg(name)}/versions`)).items;
  }

  /** Language and source of a JavaScript / TypeScript version as deployed (`NotFoundError` for WebAssembly). */
  versionSource(name: string, version: number): Promise<FunctionVersionSource> {
    return this.t.request(`/api/v1/functions/${seg(name)}/versions/${seg(String(version))}/source`);
  }

  /** Activates `version`, or the newest version before the active one when omitted. */
  rollback(name: string, version?: number): Promise<FunctionInfo> {
    return this.t.request(`/api/v1/functions/${seg(name)}/rollback`, { method: "POST", body: wire({ version }) });
  }

  /** Recent executions, newest first. */
  async logs(name: string, opts: FunctionLogsOptions = {}): Promise<FunctionExecutionLog[]> {
    return (await this.t.request<{ items: FunctionExecutionLog[] }>(`/api/v1/functions/${seg(name)}/logs`, { query: { limit: opts.limit } })).items;
  }

  /** TypeScript declarations of the `"forgebase"` module functions import (for editors and type checking). */
  types(): Promise<string> {
    return this.t.request("/api/v1/functions/types", { text: true });
  }

  /**
   * Invokes an HTTP function and returns what it sends back: parsed JSON for a JSON response, the text otherwise,
   * `undefined` for 204. `input` is sent as JSON, unless it is a string, `Blob`, `ArrayBuffer`, typed array,
   * `FormData`, `URLSearchParams` or stream, which go out as is. A function that answers with a status of 400 or
   * more raises the matching error (`NotFoundError`, `ValidationError`, ...); use `invokeRaw` and the transport's
   * error `body` when you need its exact answer.
   */
  async invoke<T = unknown>(name: string, input?: InvokeBody, opts: InvokeOptions = {}): Promise<T> {
    const res = await this.invokeRaw(name, input, opts);
    if (res.status === 204) return undefined as T;
    const text = await res.text();
    if (!text) return undefined as T;
    if (!isJsonType(res.headers.get("content-type"))) return text as T;
    try {
      return JSON.parse(text) as T;
    } catch {
      throw new InvalidResponseError("function response was not valid JSON", res.status);
    }
  }

  /** Same as `invoke` but resolves with the 2xx `Response` (headers, binary bodies, streams). */
  invokeRaw(name: string, input?: InvokeBody, opts: InvokeOptions = {}): Promise<Response> {
    const sub = (opts.path ?? "").replace(/^\/+/, "");
    const path = `/api/v1/functions/${seg(name)}/invoke${sub ? `/${sub.split("/").map(seg).join("/")}` : ""}`;
    const o: RequestOptions = {
      method: opts.method ?? "POST",
      query: opts.version === undefined ? opts.query : { ...opts.query, version: opts.version },
      headers: opts.headers,
      signal: opts.signal,
      retry: opts.retry,
    };
    if (input !== undefined && input !== null) {
      if (isRawBody(input)) o.rawBody = input;
      else o.body = input;
    }
    return this.t.send(path, o);
  }
}

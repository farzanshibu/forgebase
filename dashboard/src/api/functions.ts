/** Typed client for `/api/v1/functions` (ADR-0017). Hand-written; the OpenAPI fixture does not describe these routes yet. */
import { request, send } from "./client";

const F = "/api/v1/functions";
const enc = encodeURIComponent;

export type InvokeAccess = "public" | "authenticated" | "permission";
export type HookEventName = "before_create" | "after_create" | "before_update" | "after_update" | "before_delete" | "after_delete";

export const FUNCTION_CAPABILITIES = ["db.read", "db.write", "kv", "log", "env", "secrets", "fetch"] as const;
export const HOOK_EVENTS: HookEventName[] = ["before_create", "after_create", "before_update", "after_update", "before_delete", "after_delete"];

export interface FunctionLimits { fuel: number; memory_bytes: number; timeout_ms: number; max_concurrency: number }
export interface HookSpec { collection: string; events: HookEventName[] }
export interface VersionConfig {
  capabilities: string[];
  fetch_hosts: string[];
  limits: FunctionLimits;
  env: Record<string, string>;
  hooks: HookSpec[];
}
export interface FunctionInfo {
  id: string; name: string; description: string; access: InvokeAccess; active_version: number; latest_version: number;
  principal: string; created_at: string; updated_at: string; created_by: string;
}
/** The version's configuration is flattened into the object on the wire. */
export interface VersionInfo extends VersionConfig { version: number; sha256: string; size: number; active: boolean; created_at: string; created_by: string }
export interface Deployed { function: FunctionInfo; version: VersionInfo }
export interface SecretInfo { name: string; created_at: string; updated_at: string }
export interface LogLine { level: string; message: string }
export interface ExecutionLog {
  id: number; function: string; version: number; trigger: string; status: string; duration_ms: number; fuel_used: number;
  error: string | null; logs: LogLine[]; request_id: string | null; created_at: string;
}
export interface DeployRequest {
  name: string;
  /** The WebAssembly module, base64. */
  wasm: string;
  description?: string;
  access?: InvokeAccess;
  capabilities?: string[];
  fetch_hosts?: string[];
  limits?: Partial<FunctionLimits>;
  env?: Record<string, string>;
  hooks?: HookSpec[];
  activate?: boolean;
}

export const FUNCTION_OPERATIONS = [
  "get /api/v1/functions", "post /api/v1/functions",
  "get /api/v1/functions/{name}", "delete /api/v1/functions/{name}",
  "get /api/v1/functions/{name}/versions", "post /api/v1/functions/{name}/rollback",
  "get /api/v1/functions/{name}/logs", "get /api/v1/functions/{name}/secrets",
  "put /api/v1/functions/{name}/secrets/{key}", "delete /api/v1/functions/{name}/secrets/{key}",
] as const;

const f = (name: string) => `${F}/${enc(name)}`;

export const listFunctions = async (signal?: AbortSignal) => (await request<{ items: FunctionInfo[] }>(F, { signal })).items;
export const getFunction = (name: string, signal?: AbortSignal) => request<FunctionInfo>(f(name), { signal });
export const deployFunction = (req: DeployRequest) => send<Deployed>("POST", F, req);
export const deleteFunction = (name: string) => send<void>("DELETE", f(name));
export const listFunctionVersions = async (name: string, signal?: AbortSignal) =>
  (await request<{ items: VersionInfo[] }>(`${f(name)}/versions`, { signal })).items;
/** Activates `version`, or the newest version before the active one when omitted. */
export const rollbackFunction = (name: string, version?: number) =>
  send<FunctionInfo>("POST", `${f(name)}/rollback`, version === undefined ? {} : { version });
export const functionLogs = async (name: string, limit = 50, signal?: AbortSignal) =>
  (await request<{ items: ExecutionLog[] }>(`${f(name)}/logs?limit=${limit}`, { signal })).items;
export const listFunctionSecrets = async (name: string, signal?: AbortSignal) =>
  (await request<{ items: SecretInfo[] }>(`${f(name)}/secrets`, { signal })).items;
export const putFunctionSecret = (name: string, key: string, value: string) =>
  send<SecretInfo>("PUT", `${f(name)}/secrets/${enc(key)}`, { value });
export const deleteFunctionSecret = (name: string, key: string) => send<void>("DELETE", `${f(name)}/secrets/${enc(key)}`);

/** Base64 of a module's bytes (chunked so large modules do not overflow `String.fromCharCode`). */
export function wasmToBase64(bytes: Uint8Array): string {
  let bin = "";
  for (let i = 0; i < bytes.length; i += 0x8000) bin += String.fromCharCode(...bytes.subarray(i, i + 0x8000));
  return btoa(bin);
}

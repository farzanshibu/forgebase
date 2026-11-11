/** PostgreSQL administration for the Database studio (FB-153, FB-155): direct logins and extensions. 400 `*_unavailable` on SQLite. */
import { request, send } from "./client";

const SYS = "/api/v1/system";
const enc = encodeURIComponent;

/** Every `method path` this file calls (the OpenAPI fixture lists them after the next regeneration). */
export const DB_ADMIN_OPERATIONS = [
  "get /api/v1/system/db-logins", "post /api/v1/system/db-logins", "get /api/v1/system/db-logins/{name}",
  "delete /api/v1/system/db-logins/{name}", "post /api/v1/system/db-logins/{name}/rotate",
  "get /api/v1/system/extensions", "post /api/v1/system/extensions", "delete /api/v1/system/extensions/{name}",
] as const;

export interface DbLogin {
  name: string;
  /** The PostgreSQL role, `fbu_<name>`. */
  role: string;
  /** `user:<collection>/<id>` or `service:<id>`. */
  principal: string;
  tenant?: string | null;
  readonly: boolean;
  /** Has the SQL editor's rights (bypasses row-level security). */
  admin: boolean;
  connection_limit: number;
  statement_timeout_ms: number;
  idle_timeout_ms: number;
  valid_until?: string | null;
  /** False: the principal or tenant is gone (or an admin login), so the login sees nothing. */
  has_claims: boolean;
  claims_at?: string | null;
  created_by: string;
  created_at: string;
  rotated_at?: string | null;
}

export interface LoginSpec {
  name: string;
  principal?: string;
  tenant?: string;
  readonly?: boolean;
  admin?: boolean;
  connection_limit?: number;
  statement_timeout_ms?: number;
  valid_until?: string;
}

/** The answer of create and rotate: the only time the password is shown. */
export interface LoginSecret {
  login: DbLogin;
  password: string;
  /** `psql` connection string, without the password. */
  hint: string;
}

export const listDbLogins = async (signal?: AbortSignal) =>
  (await request<{ items: DbLogin[] }>(`${SYS}/db-logins`, { signal })).items;
export const createDbLogin = (spec: LoginSpec) => send<LoginSecret>("POST", `${SYS}/db-logins`, spec);
export const rotateDbLogin = (name: string, validUntil?: string) =>
  send<LoginSecret>("POST", `${SYS}/db-logins/${enc(name)}/rotate`, validUntil ? { valid_until: validUntil } : {});
export const deleteDbLogin = (name: string) => send<void>("DELETE", `${SYS}/db-logins/${enc(name)}`);

export interface ExtensionInfo {
  name: string;
  /** `api` (managed here), `schema` (ForgeBase's migrations) or `feature:<name>` (`[database.postgres.extensions]`). */
  managed_by: string;
  available: boolean;
  default_version?: string | null;
  installed_version?: string | null;
  schema?: string | null;
  comment?: string | null;
}

export const listExtensions = async (signal?: AbortSignal) =>
  (await request<{ items: ExtensionInfo[] }>(`${SYS}/extensions`, { signal })).items;
export const createExtension = (name: string) => send<ExtensionInfo>("POST", `${SYS}/extensions`, { name });
export const dropExtension = (name: string) => send<void>("DELETE", `${SYS}/extensions/${enc(name)}`);

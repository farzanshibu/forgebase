/**
 * The SQL editor's calls. `show_secrets` (FB-143): secret columns are masked unless the request asks; every use is audited.
 * `run_as` (FB-153, PostgreSQL): run as a user or service account, so row and field rules apply; DDL is refused with it
 * (`run_as_ddl`) and each run is audited (`sql.run_as`). `read_from_replica` (FB-169): a read may go to a replica that
 * has replayed your own writes; the server ignores it without replicas, for writes and with `show_secrets`.
 */
import { send } from "../../api/client";
import type { SqlResult } from "../../api";

export interface SqlRunResult extends SqlResult {
  /** True when the server hid secret values in this result. */
  secrets_masked?: boolean | null;
}

export interface RunAs {
  /** `user:<collection>/<id>` or `service:<id>`. */
  principal: string;
  tenant?: string;
}

export interface SqlRunOptions {
  showSecrets?: boolean;
  runAs?: RunAs;
  readFromReplica?: boolean;
}

/** The request body: flags go in only when set, so a plain run sends just `{ sql }`. */
export function sqlBody(sql: string, o: SqlRunOptions = {}): Record<string, unknown> {
  return {
    sql,
    ...(o.showSecrets ? { show_secrets: true } : {}),
    ...(o.runAs ? { run_as: { principal: o.runAs.principal, ...(o.runAs.tenant ? { tenant: o.runAs.tenant } : {}) } } : {}),
    ...(o.readFromReplica ? { read_from_replica: true } : {}),
  };
}

export const runSqlStatement = (sql: string, o: SqlRunOptions | boolean = {}) =>
  send<SqlRunResult>("POST", "/api/v1/sql", sqlBody(sql, typeof o === "boolean" ? { showSecrets: o } : o));

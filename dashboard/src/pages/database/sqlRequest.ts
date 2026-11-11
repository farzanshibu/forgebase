/** The SQL editor's calls. `show_secrets` (FB-143): secret columns are masked unless the request asks; every use is audited. */
import { send } from "../../api/client";
import type { SqlResult } from "../../api";

export interface SqlRunResult extends SqlResult {
  /** True when the server hid secret values in this result. */
  secrets_masked?: boolean | null;
}

export const runSqlStatement = (sql: string, showSecrets = false) =>
  send<SqlRunResult>("POST", "/api/v1/sql", showSecrets ? { sql, show_secrets: true } : { sql });

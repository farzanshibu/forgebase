import type { Transport } from "./http";
import type { Schemas } from "./types";

/*
 * SQL editor (`/api/v1/sql`): `client.sql`. Administrators only by default (the data core's authorizer decides).
 * Parameters are positional (`?` / `?1`); values are strings, numbers, booleans or null.
 */

/** A SQL parameter or cell value as JSON carries it. */
export type SqlValue = string | number | boolean | null | Record<string, unknown> | unknown[];

export type TableInfo = Schemas["TableInfoSchema"];
export type TableColumn = Schemas["ColumnInfoSchema"];
export type QueryPlan = Schemas["QueryPlanSchema"];

/** Result of one statement. */
export interface SqlResult {
  /** `read`, `write`, `schema` or `maintenance`. */
  kind: "read" | "write" | "schema" | "maintenance" | (string & {});
  columns: string[];
  /** One array per row, in `columns` order. At most 10 000 rows are returned (`truncated`). */
  rows: SqlValue[][];
  rows_affected: number;
  /** Rowid of the last insert (SQLite; `null` on PostgreSQL: use `RETURNING`). */
  last_insert_id?: number | null;
  truncated: boolean;
  duration_ms: number;
  /** Secret values in `rows` read `"********"`. Pass `showSecrets` to see them. */
  secrets_masked?: boolean | null;
}

export interface SqlStatement {
  sql: string;
  params?: SqlValue[];
}

export interface SqlRunAs {
  /** `user:<collection>/<id>` or `service:<id>`. */
  principal: string;
  /** The tenant to act in. */
  tenant?: string;
}

export interface SqlOptions {
  /** Return secret columns unmasked and allow statements that name them (audited as `sql.show_secrets`). */
  showSecrets?: boolean;
  /** Read on a replica that has replayed your own writes (ignored without replicas; writes and batches stay on the primary). */
  readFromReplica?: boolean;
  /** Number of the database set a read runs on (`0` or absent: control). */
  set?: number;
  /** PostgreSQL: run as a direct login bound to this principal would (row and field rules apply; DDL refused; audited as `sql.run_as`). */
  runAs?: SqlRunAs;
  signal?: AbortSignal;
}

const enc = encodeURIComponent;
const S = "/api/v1/sql";

const compact = <T extends Record<string, unknown>>(o: T): T => Object.fromEntries(Object.entries(o).filter(([, v]) => v !== undefined)) as T;

const wireOptions = (o: SqlOptions) =>
  compact({
    show_secrets: o.showSecrets,
    read_from_replica: o.readFromReplica,
    set: o.set,
    run_as: o.runAs ? compact({ principal: o.runAs.principal, tenant: o.runAs.tenant }) : undefined,
  });

const statement = (s: string | SqlStatement): SqlStatement => (typeof s === "string" ? { sql: s } : s);

/** Turns a result into one object per row, keyed by column name (the last of two equal names wins). */
export function sqlRows<T = Record<string, SqlValue>>(r: Pick<SqlResult, "columns" | "rows">): T[] {
  return r.rows.map((row) => Object.fromEntries(r.columns.map((c, i) => [c, row[i]])) as T);
}

/** SQL editor: `client.sql`. */
export class SqlApi {
  constructor(private readonly t: Transport) {}

  /** Runs one parameterised statement. Transaction control, `ATTACH`, `VACUUM` and writable pragmas are refused. */
  execute(sql: string, params: SqlValue[] = [], o: SqlOptions = {}): Promise<SqlResult> {
    return this.t.request(S, { method: "POST", body: { sql, params, ...wireOptions(o) }, signal: o.signal });
  }

  /** `execute`, then one object per row keyed by column name. */
  async query<T = Record<string, SqlValue>>(sql: string, params: SqlValue[] = [], o: SqlOptions = {}): Promise<T[]> {
    return sqlRows<T>(await this.execute(sql, params, o));
  }

  /** Runs 1 to 1000 statements in one all-or-nothing transaction; one result per statement. `runAs` applies to the whole batch. */
  async batch(statements: (string | SqlStatement)[], o: Pick<SqlOptions, "showSecrets" | "runAs" | "signal"> = {}): Promise<SqlResult[]> {
    const body = { statements: statements.map((s) => compact({ ...statement(s) })), ...wireOptions(o) };
    return (await this.t.request<{ items: SqlResult[] }>(`${S}/batch`, { method: "POST", body, signal: o.signal })).items;
  }

  /** The query plan (`EXPLAIN QUERY PLAN`) of a statement, without running it. */
  explain(sql: string, params: SqlValue[] = []): Promise<QueryPlan> {
    return this.t.request(`${S}/explain`, { method: "POST", body: { sql, params } });
  }

  /** Tables and views (without ForgeBase system tables) with columns and indexes. */
  async tables(): Promise<TableInfo[]> {
    return (await this.t.request<{ items: TableInfo[] }>(`${S}/tables`)).items;
  }

  /** One table or view. A system table is a `NotFoundError` unless you are an administrator. */
  table(name: string): Promise<TableInfo> {
    return this.t.request(`${S}/tables/${enc(name)}`);
  }
}

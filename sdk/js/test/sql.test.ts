import { describe, expect, it } from "vitest";
import { NotFoundError, PermissionError, Transport } from "../src/index";
import { SqlApi, sqlRows } from "../src/sql";
import { errBody, json, noSleep, scripted } from "./helpers";

const setup = (...r: Parameters<typeof scripted>) => {
  const f = scripted(...r);
  return { f, sql: new SqlApi(new Transport({ baseUrl: "http://x", fetch: f.fetch, retry: { ...noSleep, statuses: [] } })) };
};
const body = (c: { body?: string }) => JSON.parse(c.body!) as unknown;

const result = {
  kind: "read",
  columns: ["id", "name"],
  rows: [[1, "a"], [2, null]],
  rows_affected: 0,
  last_insert_id: null,
  truncated: false,
  duration_ms: 0.4,
};

describe("sql", () => {
  it("executes a statement with params and every option", async () => {
    const { f, sql } = setup(json(200, result));
    const r = await sql.execute("SELECT * FROM t WHERE id > ?", [0], {
      showSecrets: true,
      readFromReplica: true,
      set: 2,
      runAs: { principal: "user:users/u1", tenant: "env1" },
    });
    expect(r).toEqual(result);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/sql" });
    expect(body(f.calls[0]!)).toEqual({
      sql: "SELECT * FROM t WHERE id > ?",
      params: [0],
      show_secrets: true,
      read_from_replica: true,
      set: 2,
      run_as: { principal: "user:users/u1", tenant: "env1" },
    });
  });

  it("sends only what is set", async () => {
    const { f, sql } = setup(json(200, result));
    await sql.execute("SELECT 1");
    expect(body(f.calls[0]!)).toEqual({ sql: "SELECT 1", params: [] });
  });

  it("query() returns one object per row", async () => {
    const { sql } = setup(json(200, result));
    expect(await sql.query<{ id: number; name: string | null }>("SELECT id, name FROM t")).toEqual([
      { id: 1, name: "a" },
      { id: 2, name: null },
    ]);
    expect(sqlRows({ columns: ["x"], rows: [] })).toEqual([]);
  });

  it("runs a batch of strings and statements", async () => {
    const { f, sql } = setup(json(200, { items: [result, result] }));
    const r = await sql.batch(["DELETE FROM t", { sql: "INSERT INTO t(a) VALUES (?)", params: ["x"] }], { showSecrets: false });
    expect(r).toHaveLength(2);
    expect(f.calls[0]!.url).toBe("http://x/api/v1/sql/batch");
    expect(body(f.calls[0]!)).toEqual({ statements: [{ sql: "DELETE FROM t" }, { sql: "INSERT INTO t(a) VALUES (?)", params: ["x"] }], show_secrets: false });
  });

  it("explains without options", async () => {
    const plan = { sql: "SELECT 1", full_scans: ["t"], indexes_used: [], steps: [{ id: 2, parent: 0, detail: "SCAN t" }] };
    const { f, sql } = setup(json(200, plan));
    expect(await sql.explain("SELECT * FROM t WHERE a = ?", ["x"])).toEqual(plan);
    expect(f.calls[0]).toMatchObject({ method: "POST", url: "http://x/api/v1/sql/explain" });
    expect(body(f.calls[0]!)).toEqual({ sql: "SELECT * FROM t WHERE a = ?", params: ["x"] });
  });

  it("lists and describes tables", async () => {
    const table = { name: "posts", kind: "table", columns: [{ name: "id", declared_type: "TEXT", not_null: true, primary_key: true }], indexes: ["ix"] };
    const { f, sql } = setup(json(200, { items: [table] }), json(200, table));
    expect(await sql.tables()).toEqual([table]);
    expect(await sql.table("my table")).toEqual(table);
    expect(f.calls.map((c) => c.url)).toEqual(["http://x/api/v1/sql/tables", "http://x/api/v1/sql/tables/my%20table"]);
  });

  it("maps errors: 403 and 404", async () => {
    const { sql } = setup(json(403, errBody("forbidden", "administrators only")), json(404, errBody("table_not_found", "no such table")));
    await expect(sql.execute("SELECT 1")).rejects.toBeInstanceOf(PermissionError);
    await expect(sql.table("nope")).rejects.toBeInstanceOf(NotFoundError);
  });
});

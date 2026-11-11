/** Database Studio for PostgreSQL (FB-155): backend line, top queries, policies, direct logins, extensions, run-as SQL editor, fuzzy search. */
import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, seeded } from "../../test/helpers";
import { confirmDialog, sheet, toast } from "../../test/dataStudioMock";
import { installPg } from "../../test/pgMock";
import { envelope } from "../../test/mockApi";
import { pick } from "../../test/pick";
import { ApiError } from "../../api";
import type { TableInfo } from "../../api";
import { expandStar, secretRefusal } from "./sqlSecrets";
import { sqlBody } from "./sqlRequest";
import { fuzzyThresholdError } from "./values";
import { widens, isGenerated, accessWords, seesNothing, managedWords } from "./pgHelpers";
import { showSqliteNotice, SQLITE_NOTICE } from "./info";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup(init: Parameters<typeof installPg>[1] = {}) {
  const mock = seeded();
  const pg = installPg(mock, init);
  return { mock, pg };
}

describe("backend line and tabs", () => {
  it("shows backend, engine version, connections, each feature's engine and a link to the topology screen", async () => {
    setup();
    renderStudio("/database");
    const slot = await screen.findByLabelText("Database backend");
    await waitFor(() => expect(slot).toHaveTextContent("PostgreSQL 18.1 · bundled engine · 3 connections · engine 18.1 (ready)"));
    const features = within(screen.getByRole("list", { name: "Database features" }));
    expect(features.getByText("vector: pgvector 0.8.7")).toBeInTheDocument();
    expect(features.getByText("graph: portable")).toBeInTheDocument();
    expect(within(slot).getByRole("link", { name: "Topology" })).toHaveAttribute("href", "/system/topology");
    const tabs = within(screen.getByRole("tablist", { name: "Database sections" }));
    for (const t of ["Policies", "Direct logins", "Extensions"]) expect(tabs.getByRole("tab", { name: t })).toBeInTheDocument();
  });

  it("hides the PostgreSQL tabs on SQLite and says what a direct link needs", async () => {
    setup({ backend: "sqlite" });
    renderStudio("/database/logins");
    expect(await screen.findByRole("region", { name: "Direct logins need PostgreSQL" })).toHaveTextContent("forgebase db convert --to postgres");
    const tabs = within(screen.getByRole("tablist", { name: "Database sections" }));
    expect(tabs.queryByRole("tab", { name: "Policies" })).not.toBeInTheDocument();
    expect(tabs.getByRole("tab", { name: "SQL editor" })).toBeInTheDocument();
    expect(screen.queryByLabelText("Database features")).not.toBeInTheDocument();
  });
});

describe("top queries (pg_stat_statements)", () => {
  it("lists the costliest statements on the Slow queries tab", async () => {
    const { pg } = setup();
    renderStudio("/database/slow");
    const table = await screen.findByRole("table", { name: "Top queries" });
    expect(within(table).getByText("SELECT * FROM orders WHERE id = $1")).toBeInTheDocument();
    expect(within(table).getByText("340.5 ms")).toBeInTheDocument();
    expect(within(table).getByText("2.84 ms")).toBeInTheDocument();
    expect(pg.sql.some((b) => String(b.sql).includes("pg_stat_statements"))).toBe(true);
  });

  it("offers to install the extension when it is missing", async () => {
    const { pg } = setup();
    pg.extensions.find((e) => e.name === "pg_stat_statements")!.installed_version = null;
    renderStudio("/database/slow");
    await userEvent.click(await screen.findByRole("button", { name: "Install pg_stat_statements" }));
    expect(await screen.findByRole("table", { name: "Top queries" })).toBeInTheDocument();
    expect(pg.extensions.find((e) => e.name === "pg_stat_statements")!.installed_version).toBe("1.0");
  });

  it("is not shown on SQLite", async () => {
    setup({ backend: "sqlite" });
    renderStudio("/database/slow");
    await screen.findByText(/Statements slower than/);
    expect(screen.queryByText("Top queries by total time")).not.toBeInTheDocument();
  });
});

describe("policies", () => {
  it("splits generated from hand-written policies and warns about a permissive hand-written one", async () => {
    setup();
    renderStudio("/database/policies");
    const warning = await screen.findByRole("alert");
    expect(warning).toHaveTextContent("A hand-written policy widens access");
    expect(warning).toHaveTextContent("orders_everyone");
    expect(warning).not.toHaveTextContent("orders_tenant_only");
    const written = within(screen.getByRole("table", { name: "Hand-written policies" }));
    expect(written.getByText("orders_everyone")).toBeInTheDocument();
    expect(written.getByText("widens")).toBeInTheDocument();
    expect(written.getByText("orders_tenant_only")).toBeInTheDocument();
    expect(written.getByText("restrictive")).toBeInTheDocument();
    const generated = within(screen.getByRole("table", { name: "Generated policies" }));
    expect(generated.getByText("fb_rule_select")).toBeInTheDocument();
    expect(generated.queryByText("widens")).not.toBeInTheDocument();
    expect(screen.getByText("Hand-written (2)")).toBeInTheDocument();
    expect(screen.getByText("Generated by ForgeBase (1)")).toBeInTheDocument();
  });

  it("says so when every policy is generated", async () => {
    setup({ policies: [["public", "orders", "fb_rule_select", "PERMISSIVE", "SELECT", "{x}", "(a = 1)", ""]] });
    renderStudio("/database/policies");
    expect(await screen.findByRole("region", { name: "No hand-written policies" })).toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("classifies by the fb_ prefix on public tables only", () => {
    const p = (schema: string, name: string, mode = "PERMISSIVE") => ({ schema, table: "t", name, mode, command: "ALL", roles: "", using: "", check: "" });
    expect(isGenerated(p("public", "fb_tenant"))).toBe(true);
    expect(isGenerated(p("api", "fb_tenant"))).toBe(false);
    expect(isGenerated(p("public", "mine"))).toBe(false);
    expect(widens(p("public", "mine"))).toBe(true);
    expect(widens(p("public", "mine", "RESTRICTIVE"))).toBe(false);
    expect(widens(p("public", "fb_x"))).toBe(false);
  });
});

describe("direct logins", () => {
  it("lists logins and marks one with has_claims false as seeing nothing", async () => {
    setup();
    renderStudio("/database/logins");
    const table = await screen.findByRole("table", { name: "Direct logins" });
    const ghost = within(within(table).getByText("fbu_ghost").closest("tr")!);
    expect(ghost.getByText("Sees nothing")).toBeInTheDocument();
    expect(ghost.getByText("user:users/gone")).toBeInTheDocument();
    const ok = within(within(table).getByText("fbu_reporting").closest("tr")!);
    expect(ok.getByText("Active")).toBeInTheDocument();
    expect(ok.getByText("Read only")).toBeInTheDocument();
  });

  it("creates a login, shows the password and the psql hint once, and never again", async () => {
    const { pg } = setup({ logins: [] });
    renderStudio("/database/logins");
    expect(await screen.findByRole("region", { name: "No direct logins yet" })).toBeInTheDocument();
    await userEvent.click(screen.getAllByRole("button", { name: "New direct login" })[0]!);
    const form = await sheet("New direct login");
    await userEvent.type(within(form).getByLabelText(/^Name/), "BAD NAME");
    expect(await within(form).findByText(/Lower-case letters, digits and underscores/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Create login" })).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText(/^Name/));
    await userEvent.type(within(form).getByLabelText(/^Name/), "reporting");
    await userEvent.type(within(form).getByLabelText(/^Acts as/), "service:reporting");
    await userEvent.type(within(form).getByLabelText(/^Tenant/), "acme");
    await userEvent.click(within(form).getByRole("button", { name: "Create login" }));
    expect(pg.created[0]).toMatchObject({ name: "reporting", principal: "service:reporting", tenant: "acme", readonly: true, admin: false });
    expect(await screen.findByTestId("login-password")).toHaveTextContent("s3cret-PASSWORD-once");
    expect(screen.getByTestId("login-hint")).toHaveTextContent("psql host=/run/pg port=5432 dbname=forgebase user=fbu_reporting");
    expect(screen.getByText(/Shown once/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "I have copied it" }));
    await waitFor(() => expect(screen.queryByTestId("login-password")).not.toBeInTheDocument());
    expect(screen.queryByText("s3cret-PASSWORD-once")).not.toBeInTheDocument();
    expect(await screen.findByRole("table", { name: "Direct logins" })).toBeInTheDocument();
  });

  it("shows server errors in the sheet and keeps it open", async () => {
    setup({ logins: [] });
    renderStudio("/database/logins?new=1");
    const form = await sheet("New direct login");
    await userEvent.type(within(form).getByLabelText(/^Name/), "ops");
    await userEvent.type(within(form).getByLabelText(/^Acts as/), "service:nobody");
    await userEvent.click(within(form).getByRole("button", { name: "Create login" }));
    expect(await within(form).findByText("Not found")).toBeInTheDocument();
    expect(screen.queryByTestId("login-password")).not.toBeInTheDocument();
  });

  it("rotates a password (new one shown once) and revokes only after typing the name", async () => {
    const { pg } = setup();
    renderStudio("/database/logins");
    await screen.findByRole("table", { name: "Direct logins" });
    await userEvent.click(screen.getByRole("button", { name: "Rotate reporting" }));
    await confirmDialog(/Rotate the password of fbu_reporting/, "Rotate password");
    expect(await screen.findByTestId("login-password")).toHaveTextContent("rotated-PASSWORD-once");
    expect(pg.rotations).toEqual(["reporting"]);
    await userEvent.click(screen.getByRole("button", { name: "I have copied it" }));
    await waitFor(() => expect(screen.queryByTestId("login-password")).not.toBeInTheDocument());

    await userEvent.click(screen.getByRole("button", { name: "Revoke ghost" }));
    const dialog = await screen.findByRole("alertdialog", { name: /Revoke login fbu_ghost/ });
    expect(within(dialog).getByRole("button", { name: "Revoke login" })).toBeDisabled();
    await userEvent.click(within(dialog).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(pg.deleted).toEqual([]);
    await userEvent.click(screen.getByRole("button", { name: "Revoke ghost" }));
    await confirmDialog(/Revoke login fbu_ghost/, "Revoke login", "ghost");
    await waitFor(() => expect(pg.deleted).toEqual(["login:ghost"]));
    await toast("Login ghost deleted");
  });

  it("words access and the sees-nothing state", () => {
    expect(accessWords({ admin: true, readonly: false })).toMatch(/Administrator/);
    expect(accessWords({ admin: false, readonly: true })).toBe("Read only");
    expect(accessWords({ admin: false, readonly: false })).toBe("Read and write");
    expect(seesNothing({ admin: false, has_claims: false })).toBe(true);
    expect(seesNothing({ admin: true, has_claims: false })).toBe(false);
  });
});

describe("extensions", () => {
  it("shows who manages each one and offers Remove only for the ones managed here", async () => {
    setup();
    renderStudio("/database/extensions");
    const table = await screen.findByRole("table", { name: "Extensions" });
    const row = (n: string) => within(within(table).getByText(n).closest("tr")!);
    expect(row("vector").getByText("Feature: vector")).toBeInTheDocument();
    expect(row("citext").getByText("ForgeBase schema")).toBeInTheDocument();
    expect(row("pg_stat_statements").getByText("You (here)")).toBeInTheDocument();
    expect(row("vector").queryByRole("button")).not.toBeInTheDocument();
    expect(row("citext").queryByRole("button")).not.toBeInTheDocument();
    expect(row("pg_stat_statements").getByRole("button", { name: "Remove pg_stat_statements" })).toBeInTheDocument();
    expect(row("hstore").getByRole("button", { name: "Install hstore" })).toBeInTheDocument();
  });

  it("installs and removes a managed extension", async () => {
    const { pg } = setup();
    renderStudio("/database/extensions");
    await userEvent.click(await screen.findByRole("button", { name: "Install hstore" }));
    expect(await screen.findByRole("button", { name: "Remove hstore" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Remove hstore" }));
    await confirmDialog(/Remove extension hstore/, "Remove extension", "hstore");
    expect(await screen.findByRole("button", { name: "Install hstore" })).toBeInTheDocument();
    expect(pg.deleted).toEqual(["ext:hstore"]);
  });

  it("shows a server refusal in plain words", async () => {
    const { mock } = setup();
    mock.on("POST", "/api/v1/system/extensions", () => envelope(409, "extension_managed", "vector backs the vector feature"));
    mock.custom.unshift(mock.custom.pop()!); // first match wins: put this one ahead of the fake's own route
    renderStudio("/database/extensions");
    await userEvent.click(await screen.findByRole("button", { name: "Install hstore" }));
    expect(await screen.findByText("ForgeBase manages this extension")).toBeInTheDocument();
    expect(managedWords("feature:graph")).toBe("Feature: graph");
  });
});

describe("SQL editor: run as, replica, secrets", () => {
  function usersAndAccounts(mock: ReturnType<typeof seeded>) {
    mock.collections.push({ ...mock.collections[0]!, id: "col_users", name: "users", type: "auth" });
    mock.records.users = [{ id: "u1", email: "ana@example.com", created: "x", updated: "x", _version: 1 }];
    mock.on("GET", "/api/auth/service-accounts", () => ({ status: 200, body: { items: [{ id: "reporting", name: "Reporting", description: "", role: "reader", created_by: "a", created_at: "x", disabled: false }] } }));
  }

  it("sends run_as with the tenant, says which rules apply and remembers the tenant", async () => {
    const { mock, pg } = setup();
    usersAndAccounts(mock);
    renderStudio("/database/sql");
    await screen.findByRole("button", { name: "Run" });
    await pick(await screen.findByLabelText("Run as"), "user:users/u1");
    await userEvent.type(screen.getByLabelText("Tenant"), "acme");
    expect(screen.getByRole("status")).toHaveTextContent(/user:users\/u1 in tenant acme/);
    expect(screen.getByRole("status")).toHaveTextContent(/schema changes are refused/);
    expect(screen.getByRole("status")).toHaveTextContent(/sql\.run_as/);
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    await screen.findByRole("table", { name: "Result" });
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one", run_as: { principal: "user:users/u1", tenant: "acme" } });
    expect(JSON.parse(localStorage.getItem("forgebase.sql.tenants")!)).toEqual(["acme"]);
  });

  it("lists service accounts and runs as one without a tenant", async () => {
    const { mock, pg } = setup();
    usersAndAccounts(mock);
    renderStudio("/database/sql");
    await pick(await screen.findByLabelText("Run as"), "service:reporting");
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    await screen.findByRole("table", { name: "Result" });
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one", run_as: { principal: "service:reporting" } });
  });

  it("shows the run_as_ddl refusal, and 'Switch back to Administrator' drops run_as", async () => {
    const { mock, pg } = setup();
    usersAndAccounts(mock);
    pg.sqlHandler = (b) => (b.run_as ? envelope(400, "run_as_ddl", "run as runs reads and writes only; run DDL without it") : undefined);
    renderStudio("/database/sql");
    await pick(await screen.findByLabelText("Run as"), "service:reporting");
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    expect(await screen.findByText("Run as does not run schema changes")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Switch back to Administrator" }));
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    await screen.findByRole("table", { name: "Result" });
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one" });
  });

  it("sends read_from_replica only when ticked", async () => {
    const { pg } = setup();
    renderStudio("/database/sql");
    await userEvent.click(await screen.findByRole("button", { name: "Run" }));
    await screen.findByRole("table", { name: "Result" });
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one" });
    await userEvent.click(screen.getByRole("checkbox", { name: "Read from replica" }));
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    await waitFor(() => expect(pg.sql).toHaveLength(2));
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one", read_from_replica: true });
  });

  it("hides Run as and Read from replica on SQLite", async () => {
    setup({ backend: "sqlite" });
    renderStudio("/database/sql");
    await screen.findByRole("button", { name: "Run" });
    expect(screen.queryByLabelText("Run as")).not.toBeInTheDocument();
    expect(screen.queryByRole("checkbox", { name: "Read from replica" })).not.toBeInTheDocument();
  });

  it("on a 403 for secret columns offers Show secrets and the column list without them", async () => {
    const { mock, pg } = setup();
    mock.on("GET", "/api/v1/sql/tables", () => ({ status: 200, body: { items: [{ name: "_forgebase_admins", kind: "table", columns: [{ name: "id", declared_type: "text", not_null: true, primary_key: true }, { name: "email", declared_type: "text", not_null: true, primary_key: false }, { name: "password", declared_type: "text", not_null: true, primary_key: false }], indexes: [] }] } }));
    pg.sqlHandler = (b) => (b.show_secrets || !String(b.sql).includes("*") ? undefined : envelope(403, "forbidden", "the statement names the secret column password: set show_secrets to run it (the use is audited)"));
    renderStudio("/database/sql");
    const box = await screen.findByLabelText("SQL");
    await userEvent.clear(box);
    await userEvent.type(box, "SELECT * FROM _forgebase_admins");
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("This statement reads secret columns");
    await userEvent.click(within(alert).getByRole("button", { name: "Run without the secret columns" }));
    await screen.findByRole("table", { name: "Result" });
    expect(pg.sql.at(-1)).toEqual({ sql: 'SELECT "id", "email" FROM "_forgebase_admins"' });
    expect(screen.getByLabelText("SQL")).toHaveValue('SELECT "id", "email" FROM "_forgebase_admins"');

    await userEvent.clear(screen.getByLabelText("SQL"));
    await userEvent.type(screen.getByLabelText("SQL"), "SELECT * FROM _forgebase_admins");
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    await userEvent.click(await screen.findByRole("button", { name: "Run with Show secrets" }));
    await screen.findByRole("table", { name: "Result" });
    expect(pg.sql.at(-1)).toEqual({ sql: "SELECT * FROM _forgebase_admins", show_secrets: true });
  });

  it("explains that Show secrets cannot help when PostgreSQL denies the column", async () => {
    const { pg } = setup();
    pg.sqlHandler = () => envelope(403, "forbidden", "permission denied for table _forgebase_admins (the SQL editor runs as forgebase_sql_admin: no secret columns)");
    renderStudio("/database/sql");
    await userEvent.click(await screen.findByRole("button", { name: "Run" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent(/Show secrets cannot reveal them/);
    expect(within(alert).queryByRole("button", { name: "Run with Show secrets" })).not.toBeInTheDocument();
    expect(alert).toHaveTextContent(/Name the columns you need/);
  });
});

describe("indexes and fuzzy search (FB-166)", () => {
  it("offers the trigram kind when the feature is active and sends it", async () => {
    const { mock } = setup();
    mock.on("GET", "/api/v1/indexes", () => ({ status: 200, body: { items: [{ name: "ix_title_trgm", table: "products", collection: "products", origin: "manual", kind: "btree", unique: false, partial: false, columns: ["title"], definition: { collection: "products", keys: [{ path: "title" }], kind: "trigram" }, stats: {} }] } }));
    mock.on("GET", "/api/v1/indexes/recommendations", () => ({ status: 200, body: { items: [] } }));
    renderStudio("/database/c/products/indexes");
    expect(await screen.findByText("trigram")).toBeInTheDocument();
    await userEvent.type(await screen.findByLabelText(/^Keys/), "title");
    await pick(screen.getByLabelText(/^Kind/), "trigram");
    expect(screen.getByRole("checkbox", { name: "Unique" })).toHaveAttribute("aria-disabled", "true");
    await userEvent.click(screen.getByRole("button", { name: "Create index" }));
    await waitFor(() => expect(mock.bodies["post /api/v1/indexes"]).toBeDefined());
    expect(mock.bodies["post /api/v1/indexes"]![0]).toMatchObject({ collection: "products", kind: "trigram", unique: false });
  });

  it("has no trigram kind when pg_trgm is not active", async () => {
    setup({ features: { trigram: { engine: "portable", state: "portable" } } });
    renderStudio("/database/indexes");
    await screen.findByLabelText(/^Keys/);
    expect(screen.queryByLabelText(/^Kind/)).not.toBeInTheDocument();
  });

  it("sends search with fuzzy and a threshold, and keeps them in the URL", async () => {
    const { mock } = setup();
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    await userEvent.type(screen.getByLabelText("Search"), "pne");
    await userEvent.click(screen.getByRole("switch", { name: "Fuzzy" }));
    await userEvent.type(await screen.findByLabelText("Fuzzy threshold"), "0.5");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(mock.calls.filter((c) => c.endsWith("/records")).length).toBeGreaterThan(1));
    const q = mock.queries.filter((x, i) => mock.calls[i]!.endsWith("/records") && x.has("search")).at(-1)!;
    expect(q.get("search")).toBe("pne");
    expect(q.get("fuzzy")).toBe("true");
    expect(q.get("fuzzy_threshold")).toBe("0.5");
  });

  it("refuses a threshold outside 0 to 1", async () => {
    setup();
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    await userEvent.type(screen.getByLabelText("Search"), "pen");
    await userEvent.click(screen.getByRole("switch", { name: "Fuzzy" }));
    await userEvent.type(await screen.findByLabelText("Fuzzy threshold"), "7");
    expect(screen.getByRole("alert")).toHaveTextContent("A number from 0 to 1");
    expect(screen.getByRole("button", { name: "Apply filter" })).toBeDisabled();
    expect(fuzzyThresholdError("0.3")).toBeUndefined();
    expect(fuzzyThresholdError("")).toBeUndefined();
    expect(fuzzyThresholdError("x")).toBeDefined();
  });

  it("has no Fuzzy switch on SQLite", async () => {
    setup({ backend: "sqlite" });
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    expect(screen.getByLabelText("Search")).toBeInTheDocument();
    expect(screen.queryByRole("switch", { name: "Fuzzy" })).not.toBeInTheDocument();
  });
});

describe("pure helpers", () => {
  const tables: TableInfo[] = [{ name: "users", kind: "table", columns: [{ name: "id", declared_type: "text", not_null: true, primary_key: true }, { name: "email", declared_type: "text", not_null: true, primary_key: false }, { name: "password", declared_type: "text", not_null: true, primary_key: false }, { name: "api_secret", declared_type: "text", not_null: false, primary_key: false }], indexes: [] }];
  it("expandStar drops secret columns and keeps the rest of the statement", () => {
    expect(expandStar("select * from users where id = 1 limit 5;", tables)).toBe('SELECT "id", "email", "api_secret" FROM "users" where id = 1 limit 5');
    expect(expandStar("SELECT * FROM users", tables, ["api_secret"])).toBe('SELECT "id", "email" FROM "users"');
    expect(expandStar('SELECT * FROM "users"', tables)).toBeDefined();
  });
  it("expandStar gives up on anything it cannot rewrite safely", () => {
    expect(expandStar("SELECT id FROM users", tables)).toBeUndefined();
    expect(expandStar("SELECT * FROM nothing", tables)).toBeUndefined();
    expect(expandStar("SELECT * FROM users u JOIN x", [{ ...tables[0]!, name: "users u" }])).toBeUndefined();
    expect(expandStar("SELECT * FROM t", [{ ...tables[0]!, name: "t", columns: [{ name: "id", declared_type: "text", not_null: true, primary_key: true }] }])).toBeUndefined();
  });
  it("secretRefusal reads the two 403 wordings and ignores other errors", () => {
    const e = (status: number, message: string) => new ApiError({ status, code: "forbidden", message });
    expect(secretRefusal(e(403, "the statement names the secret column password: set show_secrets to run it"))).toMatchObject({ canShowSecrets: true, column: "password" });
    expect(secretRefusal(e(403, "permission denied for table x"))).toMatchObject({ canShowSecrets: false });
    expect(secretRefusal(e(403, "operational endpoints are for administrators"))).toBeUndefined();
    expect(secretRefusal(e(400, "set show_secrets"))).toBeUndefined();
    expect(secretRefusal(new Error("x"))).toBeUndefined();
  });
  it("sqlBody sends flags only when set", () => {
    expect(sqlBody("select 1")).toEqual({ sql: "select 1" });
    expect(sqlBody("s", { showSecrets: true, readFromReplica: true, runAs: { principal: "service:a" } })).toEqual({ sql: "s", show_secrets: true, read_from_replica: true, run_as: { principal: "service:a" } });
  });
  it("SQLite notice shows only for SQLite in a build where PostgreSQL is the default", () => {
    expect(showSqliteNotice({ backend: "sqlite", default_backend: "postgres" })).toBe(true);
    expect(showSqliteNotice({ backend: "sqlite", default_backend: "sqlite" })).toBe(false);
    expect(showSqliteNotice({ backend: "postgres", default_backend: "postgres" })).toBe(false);
    expect(showSqliteNotice({ backend: "sqlite" })).toBe(false);
    expect(SQLITE_NOTICE).toContain("forgebase db convert --to postgres");
  });
});

import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import type { CollectionDef } from "../../api";
import { toast } from "../../test/dataStudioMock";
import { renderStudio, seeded } from "../../test/helpers";
import { makeCollection } from "../../test/mockApi";
import { pick } from "../../test/pick";
import RecordForm from "./RecordForm";
import { controlFor, toBody, toDraft } from "./recordDraft";
import { expand, toRules } from "../../components/resourceRulesApi";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const everyType = (): CollectionDef => ({
  ...makeCollection("things", []),
  fields: [
    { name: "title", type: "text", required: true },
    { name: "age", type: "number" },
    { name: "done", type: "bool" },
    { name: "due", type: "date" },
    { name: "mail", type: "email" },
    { name: "site", type: "url" },
    { name: "kind", type: "select", values: ["a", "b"] },
    { name: "meta", type: "json" },
    { name: "tags", type: "array" },
    { name: "secret", type: "password" },
    { name: "owner", type: "relation", collection_id: "c_people" },
    { name: "docs", type: "file", max_select: 3 },
  ],
});

describe("RecordForm", () => {
  it("renders one control per field type and marks required fields", () => {
    const c = everyType();
    render(
      <MemoryRouter>
        <RecordForm collection={c} draft={toDraft(c.fields)} onChange={() => undefined} collections={[]} />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText(/^title/)).toHaveAttribute("type", "text");
    expect(screen.getByLabelText(/^title/)).toHaveAttribute("aria-required", "true");
    expect(screen.getByLabelText(/^age/)).toHaveAttribute("type", "number");
    expect(screen.getByRole("switch", { name: "done" })).toBeInTheDocument();
    expect(screen.getByLabelText(/^due/)).toHaveAttribute("type", "datetime-local");
    expect(screen.getByLabelText(/^mail/)).toHaveAttribute("type", "email");
    expect(screen.getByLabelText(/^site/)).toHaveAttribute("type", "url");
    expect(screen.getByLabelText("kind")).toBeInTheDocument(); // the select's trigger
    expect(screen.getByLabelText(/^meta/).tagName).toBe("TEXTAREA");
    expect(screen.getByLabelText(/^tags/).tagName).toBe("TEXTAREA");
    expect(screen.getByLabelText(/^secret/)).toHaveAttribute("type", "password");
    expect(screen.getByLabelText(/^owner/)).toBeInTheDocument(); // no target records: a plain id box
    expect(screen.getByLabelText(/^docs/)).toBeInTheDocument();
    expect(screen.getAllByText("*").length).toBe(1); // only title is required
  });

  it("shows the error for a field under it", () => {
    const c = everyType();
    render(
      <MemoryRouter>
        <RecordForm collection={c} draft={toDraft(c.fields)} onChange={() => undefined} errors={{ age: "Enter a number." }} />
      </MemoryRouter>,
    );
    expect(screen.getByLabelText(/^age/)).toHaveAttribute("aria-invalid", "true");
    expect(screen.getByText("Enter a number.")).toBeInTheDocument();
  });

  it("maps a draft to the JSON body and reports what cannot be sent", () => {
    const c = everyType();
    const draft = { ...toDraft(c.fields), title: "Hi", age: "4.5", done: true, kind: "a", meta: '{"x":1}', tags: "[1,2]", docs: "a.png, b.png" };
    const ok = toBody(c.fields, draft);
    expect(ok.errors).toEqual({});
    expect(ok.body).toEqual({ title: "Hi", age: 4.5, done: true, kind: "a", meta: { x: 1 }, tags: [1, 2], docs: ["a.png", "b.png"] });
    const bad = toBody(c.fields, { ...draft, title: "", age: "x", meta: "{", tags: "{}" });
    expect(bad.errors).toEqual({ title: "This field is required.", age: "Enter a number.", meta: "Not valid JSON.", tags: "Enter a JSON array, such as [1, 2]." });
  });

  it("picks the control by type", () => {
    expect(controlFor({ name: "a", type: "relation", max_select: 2 })).toBe("list");
    expect(controlFor({ name: "a", type: "select", max_select: 2 })).toBe("selectMany");
    expect(controlFor({ name: "a", type: "geo" })).toBe("json");
  });
});

describe("rules model", () => {
  it("expands the read and write aliases and lets an explicit operation win", () => {
    expect(expand({ read: "", create: '@request.auth.id != ""', delete: null })).toEqual({ list: "", view: "", create: '@request.auth.id != ""', update: undefined, delete: null });
    expect(expand({ write: "" })).toMatchObject({ create: "", update: "", delete: "" });
  });
  it("sends only what is not locked", () => {
    expect(toRules({ list: { mode: "locked", expr: "" }, view: { mode: "everyone", expr: "" }, create: { mode: "expr", expr: " a = 1 " } })).toEqual({ view: "", create: "a = 1" });
  });
});

describe("backend info and SQL secrets", () => {
  it("shows the engine and feature states from /api/system/info in the Database studio", async () => {
    const mock = seeded();
    mock.on("GET", "/api/system/info", () => ({
      status: 200,
      body: {
        name: "forgebase", version: "0.3.0", mode: "dev", data_dir: "/d", started_at: "2026-11-11T00:00:00Z", uptime_secs: 5,
        database: {
          backend: "postgres", mode: "managed", path: "/d/pg", size_bytes: 1, journal_mode: "wal", schema_version: 30, server_version: "18.1", connections: 3,
          engine: { state: "ready", version: "18.1" },
          features: { vector: { engine: "pgvector 0.8.7", state: "active" }, graph: { engine: "portable", state: "portable" }, queue: { engine: "portable", state: "off" } },
        },
      },
    }));
    renderStudio("/database");
    const slot = await screen.findByLabelText("Database backend");
    expect(slot).toHaveTextContent("PostgreSQL 18.1 · bundled engine · 3 connections");
    const features = within(screen.getByRole("list", { name: "Database features" }));
    expect(features.getByText("vector: pgvector 0.8.7")).toBeInTheDocument();
    expect(features.getByText("graph: portable")).toBeInTheDocument();
    expect(features.getByText("queue: off")).toBeInTheDocument();
    expect(slot.getAttribute("data-slot")).toBe("database-backend");
  });

  it("sends show_secrets only when ticked and says when secrets were masked", async () => {
    const mock = seeded();
    mock.on("POST", "/api/v1/sql", ({ body }) => ({
      status: 200,
      body: { kind: "select", columns: ["token"], rows: [[(body as { show_secrets?: boolean }).show_secrets ? "abc" : "***"]], rows_affected: 0, truncated: false, duration_ms: 1, ...((body as { show_secrets?: boolean }).show_secrets ? {} : { secrets_masked: true }) },
    }));
    renderStudio("/database/sql");
    await userEvent.click(await screen.findByRole("button", { name: "Run" }));
    expect(await screen.findByText("***")).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent(/Secret values are masked/);
    expect(mock.bodies["post /api/v1/sql"]![0]).toEqual({ sql: "SELECT 1 AS one" });

    await userEvent.click(screen.getByRole("checkbox", { name: "Show secrets" }));
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    expect(await screen.findByText("abc")).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/sql"]![1]).toEqual({ sql: "SELECT 1 AS one", show_secrets: true });
  });
});

describe("resource rules editor", () => {
  it("shows stored rules, saves changed ones and deletes the set when everything is locked", async () => {
    const mock = seeded();
    const rules: Record<string, Record<string, string | null>> = { orders: { read: '@request.auth.id != ""' } };
    mock.on("GET", "/api/v1/policies/resources/{kind}", () => ({ status: 200, body: { kind: "kv_namespace", fields: [], operations: ["list", "view", "create", "update", "delete"], actions: ["kv_read", "kv_write", "kv_admin"], items: Object.entries(rules).map(([name, r]) => ({ name, rules: r })) } }));
    mock.on("PUT", "/api/v1/policies/resources/{kind}/{name}", ({ params, body }) => { rules[params.name!] = (body as { rules: Record<string, string | null> }).rules; return { status: 200, body: { kind: params.kind, name: params.name, rules: rules[params.name!] } }; });
    mock.on("DELETE", "/api/v1/policies/resources/{kind}/{name}", ({ params }) => { delete rules[params.name!]; return { status: 204 }; });
    renderStudio("/redis/rules?ns=orders");
    expect(await screen.findByLabelText("List (scan or search) expression")).toHaveValue('@request.auth.id != ""');
    expect(screen.getByLabelText("View (read one) expression")).toHaveValue('@request.auth.id != ""');
    expect(screen.getByText(/can only test the caller with/)).toBeInTheDocument();

    await pick(screen.getByLabelText("Create rule"), "everyone");
    await userEvent.click(screen.getByRole("button", { name: "Save rules" }));
    await waitFor(() => expect(rules.orders).toEqual({ list: '@request.auth.id != ""', view: '@request.auth.id != ""', create: "" }));
    await toast("Access rules saved");

    for (const op of ["List (scan or search)", "View (read one)", "Create"]) await pick(screen.getByLabelText(`${op} rule`), "locked");
    await userEvent.click(screen.getByRole("button", { name: "Save rules" }));
    await waitFor(() => expect(rules.orders).toBeUndefined());
  });

  it("will not save an empty expression", async () => {
    const mock = seeded();
    mock.on("GET", "/api/v1/policies/resources/{kind}", () => ({ status: 200, body: { kind: "kv_namespace", fields: [], operations: ["list", "view", "create", "update", "delete"], actions: [], items: [] } }));
    renderStudio("/redis/rules?ns=fresh");
    await screen.findByText(/has no rules yet/);
    await pick(await screen.findByLabelText("Update rule"), "expr");
    await userEvent.clear(screen.getByLabelText("Update expression"));
    expect(screen.getByRole("button", { name: "Save rules" })).toBeDisabled();
    expect(screen.getByText(/Write an expression for/)).toBeInTheDocument();
  });
});

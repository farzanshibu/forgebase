import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installGraph, route } from "../../test/vgMock";
import { confirmDialog, toast } from "../../test/dataStudioMock";
import type { CypherResult, CypherStatus } from "../../api";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const T = { timeout: 5000 };
const READY: CypherStatus = { state: "ready", graph: "fb_t", job_id: null, nodes: 4, edges: 3, done: 7, error: null };

const alice = { id: "alice", label: "Person", properties: { age: 30 } };
const bob = { id: "bob", label: "Person", properties: {} };
const knows = { id: "e1", type: "KNOWS", source: "alice", target: "bob", directed: true, properties: { since: 2020 } };
const RESULT: CypherResult = {
  columns: ["p", "r", "path", "n"],
  rows: [[alice, knows, { nodes: [alice, bob], edges: [knows] }, 3]],
  truncated: false,
};

interface Opts { cypher?: CypherStatus; answer?: (body: { query: string; params?: unknown; limit?: number; timeout_ms?: number }) => ReturnType<typeof envelope> | { status: number; body: unknown } }

function setup(o: Opts = {}) {
  const mock = signedIn(new MockApi().install());
  const st = installGraph(mock);
  if ("cypher" in o ? o.cypher : true) st.cypher = o.cypher ?? { ...READY };
  const G = "/api/v1/graph";
  route(mock, "POST", `${G}/cypher`, ({ body }) => (o.answer ? o.answer(body as never) : { status: 200, body: RESULT }));
  route(mock, "POST", `${G}/cypher/drop`, () => {
    const had = st.cypher?.state !== "none";
    if (st.cypher) st.cypher = { ...st.cypher, state: "none", job_id: null, nodes: 0, edges: 0, done: 0 };
    return { status: 200, body: { dropped: had } };
  });
  return { mock, st };
}

const run = () => userEvent.click(screen.getByRole("button", { name: "Run query" }));
const lastBody = (mock: MockApi) => mock.bodies["post /api/v1/graph/cypher"]?.at(-1) as Record<string, unknown>;

describe("openCypher tab: where it shows", () => {
  it("is a tab only when the stats carry a cypher block", async () => {
    setup();
    renderStudio("/graph");
    expect(await screen.findByRole("tab", { name: "openCypher" }, T)).toHaveAttribute("href", "/graph/cypher");
  });

  it("is hidden, and says why, on a server without Apache AGE", async () => {
    const { mock } = setup({ cypher: undefined });
    renderStudio("/graph");
    await screen.findByText("4 nodes", {}, T);
    expect(screen.queryByRole("tab", { name: "openCypher" })).not.toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("explains it when the page is opened directly without AGE", async () => {
    setup({ cypher: undefined });
    renderStudio("/graph/cypher");
    expect(await screen.findByText(/openCypher is not available on this server/, {}, T)).toBeInTheDocument();
  });
});

describe("openCypher tab: query, parameters and results", () => {
  it("runs the default query with the default limit and timeout, and renders vertex, edge and path cells", async () => {
    const { mock } = setup();
    renderStudio("/graph/cypher");
    await run0();
    const table = await screen.findByRole("table", { name: "Query results" }, T);
    expect(lastBody(mock)).toMatchObject({ query: "MATCH (p:Person) RETURN p", limit: 100, timeout_ms: 10000 });
    expect(within(table).getAllByRole("columnheader").map((h) => h.textContent)).toEqual(["p", "r", "path", "n"]);
    const cells = within(table).getAllByRole("cell");
    expect(cells[0]).toHaveTextContent("(alice:Person){\"age\":30}");
    expect(cells[1]).toHaveTextContent("(alice)-[KNOWS]->(bob){\"since\":2020}");
    expect(cells[2]).toHaveTextContent("(alice:Person)-[KNOWS]->(bob:Person)1 hop");
    expect(cells[3]).toHaveTextContent("3");
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("notes when the result is truncated and when nothing matched", async () => {
    let n = 0;
    setup({ answer: () => ({ status: 200, body: n++ === 0 ? { ...RESULT, truncated: true } : { columns: ["p"], rows: [], truncated: false } }) });
    renderStudio("/graph/cypher");
    await run0();
    expect(await screen.findByRole("note", {}, T)).toHaveTextContent(/Truncated: more rows matched than the limit of 100/);
    await run();
    expect(await screen.findByText("The query ran and matched nothing.", {}, T)).toBeInTheDocument();
    expect(screen.queryByRole("note")).not.toBeInTheDocument();
  });

  it("fills the query and parameters from an example and sends the parameters", async () => {
    const { mock } = setup();
    renderStudio("/graph/cypher");
    await screen.findByRole("group", { name: "Example queries" }, T);
    await userEvent.click(screen.getByRole("button", { name: "Friends of friends" }));
    expect(screen.getByLabelText("Query")).toHaveValue("MATCH (a {id: $start})-[:KNOWS*1..2]->(b) RETURN DISTINCT b");
    expect(screen.getByLabelText(/Parameters/)).toHaveValue('{ "start": "alice" }');
    await run();
    await screen.findByRole("table", { name: "Query results" }, T);
    expect(lastBody(mock)).toMatchObject({ params: { start: "alice" } });
  });

  it("sends the limit and timeout the user typed", async () => {
    const { mock } = setup();
    renderStudio("/graph/cypher");
    await screen.findByLabelText("Query", {}, T);
    fireEvent.change(screen.getByLabelText(/^Limit/), { target: { value: "1000" } });
    fireEvent.change(screen.getByLabelText(/^Timeout/), { target: { value: "1" } });
    await run();
    await screen.findByRole("table", { name: "Query results" }, T);
    expect(lastBody(mock)).toMatchObject({ limit: 1000, timeout_ms: 1 });
  });

  it("checks the bounds and the parameters before asking the server", async () => {
    const { mock } = setup();
    renderStudio("/graph/cypher");
    await screen.findByLabelText("Query", {}, T);
    fireEvent.change(screen.getByLabelText(/^Limit/), { target: { value: "0" } });
    fireEvent.change(screen.getByLabelText(/^Timeout/), { target: { value: "30001" } });
    fireEvent.change(screen.getByLabelText(/Parameters/), { target: { value: "{oops" } });
    await run();
    expect(await screen.findByText("The limit must be a whole number from 1 to 1000.", {}, T)).toBeInTheDocument();
    expect(screen.getByText("The timeout must be a whole number from 1 to 30000.")).toBeInTheDocument();
    expect(screen.getByText(/That is not valid JSON/)).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText(/Parameters/), { target: { value: "[1]" } });
    expect(screen.getByText(/Parameters must be a JSON object/)).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/graph/cypher"]).toBeUndefined();
  });
});

async function run0() {
  await screen.findByLabelText("Query", {}, T);
  await run();
}

describe("openCypher tab: the derived graph's states", () => {
  const cases: [CypherStatus["state"], Partial<CypherStatus>, RegExp][] = [
    ["none", {}, /Not built yet/],
    ["materialising", { nodes: 10, edges: 10, done: 5 }, /Building the graph for openCypher: 5 of 20 items copied/],
    ["ready", {}, /Ready\./],
    ["stale", {}, /Out of date.*rebuilt automatically/],
    ["failed", { error: "disk full" }, /last build failed: disk full/],
  ];
  it.each(cases)("says what %s means in plain words", async (state, extra, words) => {
    setup({ cypher: { ...READY, state, ...extra } });
    renderStudio("/graph/cypher");
    const strip = await screen.findByLabelText("Derived graph status", {}, T);
    expect(strip).toHaveAttribute("data-state", state);
    expect(within(strip).getByRole("status")).toHaveTextContent(words);
    if (state === "materialising") expect(within(strip).getByRole("progressbar")).toHaveAttribute("aria-valuenow", "25");
    else expect(within(strip).queryByRole("progressbar")).not.toBeInTheDocument();
  });

  it("on 409 graph_materialising shows progress, polls the stats and runs the query when ready", async () => {
    let first = true;
    const { st } = setup({
      cypher: { ...READY, state: "none", nodes: 0, edges: 0, done: 0 },
      answer: () => {
        if (st.cypher!.state === "ready") return { status: 200, body: RESULT };
        if (first) {
          first = false;
          st.cypher = { ...READY, state: "materialising", job_id: "job1", nodes: 10, edges: 10, done: 10 };
          setTimeout(() => { st.cypher = { ...READY, state: "ready" }; }, 1500);
        }
        return envelope(409, "graph_materialising", "the graph is being built", { job_id: "job1" });
      },
    });
    renderStudio("/graph/cypher");
    await run0();
    const bar = await screen.findByRole("progressbar", {}, T);
    expect(bar).toHaveAttribute("aria-valuenow", "50");
    expect(screen.getByRole("button", { name: "Waiting for the graph…" })).toBeDisabled();
    expect(await screen.findByRole("table", { name: "Query results" }, T)).toBeInTheDocument();
    expect(screen.queryByRole("progressbar")).not.toBeInTheDocument();
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  }, 15000);

  it("stops waiting and says so when the build fails", async () => {
    const { st } = setup({
      cypher: { ...READY, state: "none", nodes: 0, edges: 0, done: 0 },
      answer: () => {
        st.cypher = { ...READY, state: "failed", error: "out of memory" };
        return envelope(409, "graph_materialising", "the graph is being built");
      },
    });
    renderStudio("/graph/cypher");
    await run0();
    expect(await screen.findByText(/Building the graph failed: out of memory/, {}, T)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Run query" })).toBeEnabled();
  }, 15000);
});

describe("openCypher tab: drop the derived graph", () => {
  it("asks first, explains the rebuild, then drops it", async () => {
    const { mock, st } = setup();
    renderStudio("/graph/cypher");
    const strip = await screen.findByLabelText("Derived graph status", {}, T);
    await userEvent.click(within(strip).getByRole("button", { name: "Drop derived graph" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Drop the derived graph?" }, T);
    expect(dialog).toHaveTextContent(/rebuilt the next time you run a query/);
    expect(dialog).toHaveTextContent(/nodes and edges are not touched/);
    expect(mock.bodies["post /api/v1/graph/cypher/drop"]).toBeUndefined();
    await confirmDialog("Drop the derived graph?", "Drop derived graph");
    await waitFor(() => expect(mock.bodies["post /api/v1/graph/cypher/drop"]).toHaveLength(1), T);
    await toast(/Derived graph deleted/);
    expect(st.cypher?.state).toBe("none");
    await waitFor(() => expect(screen.getByLabelText("Derived graph status")).toHaveAttribute("data-state", "none"), T);
    expect(screen.queryByRole("button", { name: "Drop derived graph" })).not.toBeInTheDocument();
  });

  it("keeps the graph when the dialog is cancelled", async () => {
    const { mock } = setup();
    renderStudio("/graph/cypher");
    await userEvent.click(await screen.findByRole("button", { name: "Drop derived graph" }, T));
    const dialog = await screen.findByRole("alertdialog", {}, T);
    await userEvent.click(within(dialog).getByRole("button", { name: /cancel/i }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(mock.bodies["post /api/v1/graph/cypher/drop"]).toBeUndefined();
  });
});

describe("openCypher tab: every error code, in the server's words with a hint", () => {
  const codes: [number, string, string, RegExp][] = [
    [400, "cypher_unsupported", "no Apache AGE here", /needs PostgreSQL with Apache AGE/],
    [400, "cypher_invalid", "syntax error near RETRN", /final RETURN/],
    [400, "cypher_read_only", "write clauses are not allowed", /Cypher here only reads; write through the REST graph API/],
    [400, "cypher_error", "label Foo does not exist", /Check the labels, properties and parameters/],
    [400, "cypher_timeout", "statement timeout", /raise the timeout \(at most 30 000 ms\)/],
    [400, "invalid_limit", "limit must be 1 to 1000", /whole number from 1 to 1000/],
    [400, "invalid_timeout", "timeout_ms must be 1 to 30000", /from 1 to 30000/],
    [403, "cypher_rules_unsupported", "rules cannot be applied to cypher", /simplify the label rules/],
    [409, "tenant_moving", "tenant is moving", /rebuilt on the new location/],
  ];
  it.each(codes)("%i %s", async (status, code, message, hint) => {
    setup({ answer: () => envelope(status, code, message) });
    renderStudio("/graph/cypher");
    await run0();
    const alert = await screen.findByRole("alert", {}, T);
    expect(alert).toHaveTextContent(`Server message: ${message}`);
    expect(alert).toHaveTextContent(hint);
    expect(alert).toHaveTextContent(`Code ${code}`);
    expect(screen.queryByRole("table", { name: "Query results" })).not.toBeInTheDocument();
  });

  it("names the missing permission on a plain 403", async () => {
    setup({ answer: () => envelope(403, "forbidden", "missing permission engine:graph_cypher") });
    renderStudio("/graph/cypher");
    await run0();
    expect(await screen.findByRole("alert", {}, T)).toHaveTextContent(/lacks the permission engine:graph_cypher/);
  });
});

import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installGraph } from "../../test/vgMock";
import { confirmDialog } from "../../test/dataStudioMock";
import { pick } from "../../test/pick";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  const st = installGraph(mock);
  return { mock, st };
}

describe("Graph Studio overview", () => {
  it("shows the graph with node, edge and edge-type counts", async () => {
    const { mock } = setup();
    renderStudio("/graph");
    expect(await screen.findByText("4 nodes")).toBeInTheDocument();
    expect(screen.getByText("3 edges")).toBeInTheDocument();
    expect(screen.getByText("KNOWS").closest("tr")).toHaveTextContent("2");
    expect(mock.problems).toEqual([]);
  });
});

describe("Graph Studio overview, empty state and rules", () => {
  it("teaches when the graph is empty and loads a sample of people who know each other", async () => {
    const { mock, st } = setup();
    st.nodes.length = 0;
    st.edges.length = 0;
    renderStudio("/graph");
    const region = await screen.findByRole("region", { name: "Your graph is empty" });
    await userEvent.click(within(region).getByRole("button", { name: "Load sample" }));
    await waitFor(() => expect(st.nodes.map((n) => n.id)).toEqual(["alice", "bob", "carol", "dave"]));
    expect(st.edges).toHaveLength(4);
    expect(mock.bodies["post /api/v1/graph/nodes"]?.[0]).toMatchObject({ id: "alice", label: "person" });
    expect(mock.bodies["post /api/v1/graph/edges"]?.[0]).toMatchObject({ type: "knows", source: "alice", target: "bob" });
    expect(mock.problems).toEqual([]);
  });

  it("has Use from code and a header action in the studio header", async () => {
    setup();
    renderStudio("/graph");
    expect(await screen.findByRole("heading", { level: 1, name: "Graph" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Use from code" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "New node" }));
    expect(await screen.findByRole("form", { name: "New node" })).toBeInTheDocument();
  });

  it("edits label and edge-type rules from the Graphs tab", async () => {
    const { mock } = setup();
    const kinds: string[] = [];
    mock.on("GET", "/api/v1/policies/resources/{kind}", ({ params }) => { kinds.push(params.kind!); return { status: 200, body: { kind: params.kind, fields: [{ name: "properties", type: "json" }], operations: ["list", "view", "create", "update", "delete"], actions: [], items: [] } }; });
    renderStudio("/graph");
    await screen.findByText("4 nodes");
    await userEvent.type(screen.getByLabelText(/^Label/), "person");
    await userEvent.click(screen.getByRole("button", { name: "Show rules" }));
    expect(await screen.findByLabelText("List (scan or search) rule")).toBeInTheDocument();
    expect(kinds).toContain("graph_label");
    await pick(screen.getByLabelText("Rules for"), "graph_edge_type");
    expect(screen.queryByLabelText("List (scan or search) rule")).not.toBeInTheDocument();
    await userEvent.type(within(screen.getByRole("region", { name: "Graph access rules" })).getByLabelText(/^Edge type/), "KNOWS");
    await userEvent.click(screen.getByRole("button", { name: "Show rules" }));
    await screen.findByLabelText("List (scan or search) rule");
    expect(kinds).toContain("graph_edge_type");
  });
});

describe("Graph Studio nodes", () => {
  it("filters by label, creates, edits and deletes a node", async () => {
    const { mock, st } = setup();
    renderStudio("/graph/nodes");
    await screen.findByText("alice");
    await userEvent.type(screen.getByLabelText("Label filter"), "Company");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(screen.queryByText("alice")).not.toBeInTheDocument());
    expect(screen.getByText("acme")).toBeInTheDocument();
    await userEvent.clear(screen.getByLabelText("Label filter"));
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await screen.findByText("alice");

    await userEvent.click(screen.getByRole("button", { name: "New node" }));
    let form = await screen.findByRole("form", { name: "New node" });
    await userEvent.type(within(form).getByLabelText(/^Id/), "dave");
    await userEvent.type(within(form).getByLabelText(/^Label/), "Person");
    // the form shows the body it sends, so a wrong field name cannot slip in
    expect(JSON.parse(within(form).getByLabelText("Request body").textContent!)).toEqual({ id: "dave", label: "Person", properties: {} });
    await userEvent.click(within(form).getByRole("button", { name: "Save node" }));
    await screen.findByText("dave");

    await userEvent.click(screen.getByRole("button", { name: "Edit dave" }));
    form = await screen.findByRole("form", { name: "Edit node dave" });
    const props = within(form).getByLabelText(/^Properties/);
    await userEvent.clear(props);
    await userEvent.click(props);
    await userEvent.paste('{"age": 41}');
    await userEvent.click(within(form).getByRole("button", { name: "Save node" }));
    await waitFor(() => expect(st.nodes.find((n) => n.id === "dave")?.properties).toEqual({ age: 41 }));

    await userEvent.click(screen.getByRole("button", { name: "Delete dave" }));
    await confirmDialog("Delete node dave?", "Delete node");
    await waitFor(() => expect(screen.queryByText("dave")).not.toBeInTheDocument());
    expect(mock.problems).toEqual([]);
  });

  it("reports a duplicate id from the API and invalid JSON locally", async () => {
    setup();
    renderStudio("/graph/nodes");
    await screen.findByText("alice");
    await userEvent.click(screen.getByRole("button", { name: "New node" }));
    const form = await screen.findByRole("form", { name: "New node" });
    await userEvent.type(within(form).getByLabelText(/^Id/), "alice");
    await userEvent.click(within(form).getByRole("button", { name: "Save node" }));
    expect(await within(form).findByText(/a node with this id exists/)).toBeInTheDocument();
    const props = within(form).getByLabelText(/^Properties/);
    await userEvent.click(props);
    await userEvent.paste("{oops");
    expect(await within(form).findByText(/Properties is not valid JSON/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Save node" })).toBeDisabled();
  });

  it("pages with next_after", async () => {
    const { mock } = setup();
    renderStudio("/graph/nodes");
    await screen.findByText("alice");
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
    expect(mock.calls.filter((c) => c === "get /api/v1/graph/nodes")).toHaveLength(1);
  });
});

describe("Graph Studio edges", () => {
  it("filters, creates, edits and deletes an edge", async () => {
    const { mock, st } = setup();
    renderStudio("/graph/edges");
    await screen.findByText("e1");
    await userEvent.type(screen.getByLabelText("Type filter"), "WORKS_AT");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(screen.queryByText("e1")).not.toBeInTheDocument());
    expect(screen.getByText("e3")).toBeInTheDocument();
    await userEvent.clear(screen.getByLabelText("Type filter"));
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await screen.findByText("e1");

    await userEvent.click(screen.getByRole("button", { name: "New edge" }));
    const form = await screen.findByRole("form", { name: "New edge" });
    await userEvent.type(within(form).getByLabelText(/^Type/), "LIKES");
    await userEvent.type(within(form).getByLabelText(/^Source node id/), "alice");
    await userEvent.type(within(form).getByLabelText(/^Target node id/), "carol");
    await userEvent.click(within(form).getByRole("button", { name: "Save edge" }));
    await screen.findByText("LIKES");
    expect(mock.bodies["post /api/v1/graph/edges"]?.[0]).toMatchObject({ type: "LIKES", source: "alice", target: "carol", directed: true });

    await userEvent.click(screen.getByRole("button", { name: "Edit e4" }));
    const edit = await screen.findByRole("form", { name: "Edit edge e4" });
    const props = within(edit).getByLabelText(/^Properties/);
    await userEvent.clear(props);
    await userEvent.click(props);
    await userEvent.paste('{"since": 2020}');
    await userEvent.click(within(edit).getByRole("button", { name: "Save edge" }));
    await waitFor(() => expect(st.edges.find((e) => e.id === "e4")?.properties).toEqual({ since: 2020 }));

    await userEvent.click(screen.getByRole("button", { name: "Delete e4" }));
    await confirmDialog("Delete edge e4?", "Delete edge");
    await waitFor(() => expect(screen.queryByText("LIKES")).not.toBeInTheDocument());
    expect(mock.problems).toEqual([]);
  });

  it("shows the API error for an unknown endpoint", async () => {
    setup();
    renderStudio("/graph/edges");
    await screen.findByText("e1");
    await userEvent.click(screen.getByRole("button", { name: "New edge" }));
    const form = await screen.findByRole("form", { name: "New edge" });
    await userEvent.type(within(form).getByLabelText(/^Type/), "X");
    await userEvent.type(within(form).getByLabelText(/^Source node id/), "ghost");
    await userEvent.type(within(form).getByLabelText(/^Target node id/), "alice");
    await userEvent.click(within(form).getByRole("button", { name: "Save edge" }));
    expect(await within(form).findByText(/edge endpoint does not exist/)).toBeInTheDocument();
  });
});

describe("Graph explorer", () => {
  it("loads a node with neighbours, expands, traverses and finds a path", async () => {
    const { mock } = setup();
    renderStudio("/graph/explorer?node=alice");
    expect(await screen.findByRole("button", { name: "Node alice" })).toBeInTheDocument();
    expect(await screen.findByRole("button", { name: "Node bob" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Node carol" })).not.toBeInTheDocument();
    expect(screen.getByTestId("edge-e1")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Node bob" }));
    await userEvent.click(screen.getByRole("button", { name: "Expand neighbours" }));
    expect(await screen.findByRole("button", { name: "Node carol" })).toBeInTheDocument();
    expect(screen.getByTestId("edge-e2")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Node carol" }));
    await pick(screen.getByLabelText("Direction"), "out");
    await userEvent.clear(screen.getByLabelText("Depth"));
    await userEvent.type(screen.getByLabelText("Depth"), "1");
    await userEvent.click(screen.getByRole("button", { name: "Traverse" }));
    expect(await screen.findByRole("button", { name: "Node acme" })).toBeInTheDocument();
    expect(screen.getByRole("status")).toHaveTextContent("Traversal to depth 1");
    expect(mock.bodies["post /api/v1/graph/traverse"]?.[0]).toMatchObject({ start: "carol", direction: "out", max_depth: 1 });

    await userEvent.click(screen.getByRole("button", { name: "Node alice" }));
    await pick(screen.getByLabelText("Direction"), "both");
    await userEvent.type(screen.getByLabelText("Path target id"), "acme");
    await userEvent.click(screen.getByRole("button", { name: "Find path" }));
    await waitFor(() => expect(screen.getByRole("status")).toHaveTextContent("Path of length 3: alice → bob → carol → acme"));
    expect(screen.getByTestId("edge-e1")).toHaveAttribute("data-on-path", "true");
    expect(screen.getByTestId("edge-e3")).toHaveAttribute("data-on-path", "true");
    expect(mock.problems).toEqual([]);
  });

  it("starts empty, loads a node by id and reports a missing one", async () => {
    setup();
    renderStudio("/graph/explorer");
    expect(await screen.findByText("Load a node to begin.")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Start node id"), "ghost");
    await userEvent.click(screen.getByRole("button", { name: "Load node" }));
    expect(await screen.findByText(/no such node/)).toBeInTheDocument();
    await userEvent.clear(screen.getByLabelText("Start node id"));
    await userEvent.type(screen.getByLabelText("Start node id"), "acme");
    await userEvent.click(screen.getByRole("button", { name: "Load node" }));
    expect(await screen.findByRole("button", { name: "Node carol" })).toBeInTheDocument();
    expect(screen.getByLabelText("Node properties")).toBeInTheDocument();
  });

  it("says when no path exists", async () => {
    const { st } = setup();
    st.nodes.push({ id: "zed", label: "Person", properties: {}, created_at: "", updated_at: "" });
    renderStudio("/graph/explorer?node=alice");
    await screen.findByRole("button", { name: "Node bob" });
    await userEvent.type(screen.getByLabelText("Path target id"), "zed");
    await userEvent.click(screen.getByRole("button", { name: "Find path" }));
    expect(await screen.findByText("No path from alice to zed")).toBeInTheDocument();
  });
});

describe("Graph query runner", () => {
  it("runs a traversal and a path query from JSON", async () => {
    const { mock } = setup();
    renderStudio("/graph/query");
    const box = await screen.findByLabelText("Query JSON");
    await userEvent.clear(box);
    await userEvent.click(box);
    await userEvent.paste('{"start":"alice","direction":"out","max_depth":2,"include_start":true}');
    await userEvent.click(screen.getByRole("button", { name: "Run query" }));
    expect(await screen.findByText("3 nodes, 2 edges")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("tab", { name: "Shortest path" }));
    const b2 = screen.getByLabelText("Query JSON");
    await userEvent.clear(b2);
    await userEvent.click(b2);
    await userEvent.paste('{"from":"alice","to":"acme"}');
    await userEvent.click(screen.getByRole("button", { name: "Run query" }));
    expect(await screen.findByText("Path of length 3")).toBeInTheDocument();
    expect(screen.getByText("alice → bob → carol → acme")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("reports invalid JSON without calling the API", async () => {
    const { mock } = setup();
    renderStudio("/graph/query");
    const box = await screen.findByLabelText("Query JSON");
    await userEvent.clear(box);
    await userEvent.click(box);
    await userEvent.paste("{nope");
    await userEvent.click(screen.getByRole("button", { name: "Run query" }));
    expect(await screen.findByText(/Invalid JSON/)).toBeInTheDocument();
    expect(mock.calls.filter((c) => c.includes("/graph/"))).toEqual([]);
  });
});

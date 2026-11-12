import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installVector } from "../../test/vgMock";
import { confirmDialog, toast } from "../../test/dataStudioMock";
import { pick } from "../../test/pick";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  const st = installVector(mock);
  return { mock, st };
}

describe("Vector Studio collections", () => {
  it("lists collections with totals, creates one from the header, opens it and deletes it", async () => {
    const { mock } = setup();
    renderStudio("/vector");
    expect(await screen.findByRole("link", { name: "docs" })).toBeInTheDocument();
    expect(await screen.findByLabelText("Vector totals")).toHaveTextContent(/Collections\s*1/);
    expect(screen.getByText("embedding(3, l2)")).toBeInTheDocument();
    expect(screen.queryByRole("tablist")).not.toBeInTheDocument(); // A-22: no tab row with a single tab

    await userEvent.click(screen.getByRole("button", { name: "New collection" }));
    const form = await screen.findByRole("form", { name: "New vector collection" });
    await userEvent.type(within(form).getByLabelText(/^Name/), "notes");
    await userEvent.clear(within(form).getByLabelText(/^Dimensions/));
    await userEvent.type(within(form).getByLabelText(/^Dimensions/), "4");
    await pick(within(form).getByLabelText("Metric"), "cosine");
    // the form shows the exact body it sends
    expect(JSON.parse(within(form).getByLabelText("Request body").textContent!)).toEqual({ name: "notes", fields: [{ name: "embedding", dimension: 4, metric: "cosine", quantization: "f32" }] });
    await userEvent.click(within(form).getByRole("button", { name: "Create collection" }));
    expect(await screen.findByRole("heading", { level: 1, name: "notes" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/vector/collections"]?.[0]).toMatchObject({ name: "notes", fields: [{ name: "embedding", dimension: 4, metric: "cosine", quantization: "f32" }] });
    expect(mock.bodies["post /api/v1/vector/collections"]?.[0]).not.toHaveProperty("dimension");

    await userEvent.click(screen.getAllByRole("link", { name: "Vector" })[0]!);
    await userEvent.click(await screen.findByRole("button", { name: "Delete notes" }));
    await confirmDialog("Delete vector collection notes?", "Delete collection", "notes");
    await waitFor(() => expect(screen.queryByRole("link", { name: "notes" })).not.toBeInTheDocument());
    expect(mock.problems).toEqual([]);
  });

  it("teaches in the empty state and loads a three-dimension sample", async () => {
    const { mock, st } = setup();
    st.collections.length = 0;
    renderStudio("/vector");
    const region = await screen.findByRole("region", { name: "No vector collections yet" });
    await userEvent.click(within(region).getByRole("button", { name: "Load sample" }));
    expect(await screen.findByRole("link", { name: "sample_notes" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/vector/collections"]?.[0]).toMatchObject({ name: "sample_notes", fields: [{ name: "embedding", dimension: 3, metric: "cosine" }] });
    expect(st.items.sample_notes).toHaveLength(6);
    expect(mock.problems).toEqual([]);
  });

  it("links an optional source collection", async () => {
    const { mock } = setup();
    mock.on("GET", "/api/v1/collections", () => ({ status: 200, body: { items: [{ id: "c1", name: "articles", type: "base", schema_mode: "strict", fields: [], version: 1 }] } }));
    renderStudio("/vector?new=1");
    const form = await screen.findByRole("form", { name: "New vector collection" });
    await userEvent.type(within(form).getByLabelText(/^Name/), "linked");
    await pick(within(form).getByLabelText("Source collection"), "articles");
    await userEvent.click(within(form).getByRole("button", { name: "Create collection" }));
    await waitFor(() => expect(mock.bodies["post /api/v1/vector/collections"]?.[0]).toMatchObject({ name: "linked", source_collection: "articles" }));
  });

  it("shows the API error envelope when the name is taken", async () => {
    const { mock } = setup();
    renderStudio("/vector");
    await screen.findByRole("link", { name: "docs" });
    await userEvent.click(screen.getByRole("button", { name: "New collection" }));
    const form = await screen.findByRole("form", { name: "New vector collection" });
    await userEvent.type(within(form).getByLabelText(/^Name/), "docs");
    await userEvent.click(within(form).getByRole("button", { name: "Create collection" }));
    expect(await within(form).findByText(/collection exists/)).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("offers Use from code and rules on a collection", async () => {
    const { mock } = setup();
    mock.on("GET", "/api/v1/policies/resources/{kind}", () => ({ status: 200, body: { kind: "vector_collection", fields: [{ name: "id", type: "text" }, { name: "metadata", type: "json" }], operations: ["list", "view", "create", "update", "delete"], actions: [], items: [] } }));
    renderStudio("/vector/c/docs/rules");
    expect(await screen.findByText(/metadata\.<key>/)).toBeInTheDocument();
    expect(await screen.findByLabelText("List (scan or search) rule")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Use from code" }));
    expect(await screen.findByText(/\/api\/v1\/vector\/collections\/docs\/search/)).toBeInTheDocument();
  });
});

describe("Vector Studio records", () => {
  it("browses, inspects, adds and deletes records", async () => {
    const { mock, st } = setup();
    renderStudio("/vector/c/docs");
    expect(await screen.findByText("3 records")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "b" }));
    expect(await screen.findByLabelText("Item JSON")).toHaveTextContent('"green pear"');
    await userEvent.keyboard("{Escape}");

    await userEvent.click(screen.getByRole("button", { name: "Add record" }));
    const form = await screen.findByRole("form", { name: "Add record" });
    await userEvent.type(within(form).getByLabelText(/^Id/), "d");
    await userEvent.type(within(form).getByLabelText(/^Vector/), "0, 0, 1");
    await userEvent.type(within(form).getByLabelText(/^Metadata/), '{{"tag":"z"}');
    await userEvent.click(within(form).getByRole("button", { name: "Save record" }));
    await toast("Record d created");
    await screen.findByText("4 records");
    expect(mock.bodies["post /api/v1/vector/collections/{name}/upsert"]?.[0]).toEqual({ items: [{ id: "d", vectors: { embedding: [0, 0, 1] }, metadata: { tag: "z" } }] });

    await userEvent.click(screen.getByRole("button", { name: "Delete a" }));
    await confirmDialog("Delete record a?", "Delete record");
    await screen.findByText("3 records");
    expect(st.items.docs!.map((i) => i.id)).toEqual(["b", "c", "d"]);
    expect(mock.problems).toEqual([]);
  });

  it("rejects a malformed or wrong-sized vector before calling the API", async () => {
    const { mock } = setup();
    renderStudio("/vector/c/docs");
    await screen.findByText("3 records");
    await userEvent.click(screen.getByRole("button", { name: "Add record" }));
    const form = await screen.findByRole("form", { name: "Add record" });
    await userEvent.type(within(form).getByLabelText(/^Vector/), "1, x");
    await userEvent.click(within(form).getByRole("button", { name: "Save record" }));
    expect(await within(form).findByText("A vector is a list of finite numbers")).toBeInTheDocument();
    await userEvent.clear(within(form).getByLabelText(/^Vector/));
    await userEvent.type(within(form).getByLabelText(/^Vector/), "1, 2");
    expect(await within(form).findByText("This field takes 3 numbers; you entered 2.")).toBeInTheDocument();
    expect(mock.calls.filter((c) => c.includes("/upsert"))).toEqual([]);
  });

  it("shows an empty state with the primary action when a collection has no records", async () => {
    const { st } = setup();
    st.items.docs = [];
    renderStudio("/vector/c/docs");
    const region = await screen.findByRole("region", { name: "No records yet" });
    await userEvent.click(within(region).getByRole("button", { name: "Add record" }));
    expect(await screen.findByRole("form", { name: "Add record" })).toBeInTheDocument();
  });
});

describe("Vector Studio search", () => {
  it("runs a filtered similarity search and shows strategy and distances", async () => {
    const { mock } = setup();
    renderStudio("/vector/c/docs/search");
    await screen.findByRole("form", { name: "Search" });
    await userEvent.type(screen.getByLabelText("Query vector"), "1, 0, 0");
    await userEvent.type(screen.getByLabelText("Metadata filter"), "tag = x");
    await userEvent.click(screen.getByRole("button", { name: "Run search" }));
    expect(await screen.findByText("2 hits")).toBeInTheDocument();
    expect(screen.getByLabelText("Search summary")).toHaveTextContent("strategy filtered_ann");
    const rows = screen.getAllByRole("row").slice(1);
    expect(within(rows[0]!).getByText("a")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("c")).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/vector/collections/{name}/search"]?.[0]).toMatchObject({ field: "embedding", vector: [1, 0, 0], filter: "tag = x", filter_mode: "auto", k: 10 });
    expect(mock.problems).toEqual([]);
  });

  it("surfaces a bad filter as an API error", async () => {
    setup();
    renderStudio("/vector/c/docs/search");
    await screen.findByRole("form", { name: "Search" });
    await userEvent.type(screen.getByLabelText("Query vector"), "1,0,0");
    await userEvent.type(screen.getByLabelText("Metadata filter"), "tag ??");
    await userEvent.click(screen.getByRole("button", { name: "Run search" }));
    expect(await screen.findByText(/cannot parse filter/)).toBeInTheDocument();
  });

  it("runs a hybrid search and shows the score breakdown per hit", async () => {
    const { mock } = setup();
    renderStudio("/vector/c/docs/search");
    await screen.findByRole("form", { name: "Search" });
    await userEvent.click(screen.getByRole("tab", { name: "Hybrid" }));
    await userEvent.type(screen.getByLabelText("Query vector"), "1, 0, 0");
    await userEvent.type(screen.getByLabelText("Query text"), "red");
    await userEvent.click(screen.getByRole("button", { name: "Run search" }));
    expect(await screen.findByText("3 hybrid hits")).toBeInTheDocument();
    for (const h of ["Fused score", "Vector rank", "Vector distance", "Text rank", "Text score"]) expect(screen.getByRole("columnheader", { name: h })).toBeInTheDocument();
    expect(screen.getByLabelText("Search summary")).toHaveTextContent("3 vector candidates");
    expect(mock.bodies["post /api/v1/vector/collections/{name}/hybrid"]?.[0]).toMatchObject({ vector: [1, 0, 0], text: "red", text_operator: "or", weights: { vector: 1, text: 1 } });
    expect(mock.problems).toEqual([]);
  });

  it("needs a vector or text for hybrid search", async () => {
    setup();
    renderStudio("/vector/c/docs/search");
    await screen.findByRole("form", { name: "Search" });
    await userEvent.click(screen.getByRole("tab", { name: "Hybrid" }));
    await userEvent.click(screen.getByRole("button", { name: "Run search" }));
    expect(await screen.findByText("Hybrid search needs a vector, text, or both")).toBeInTheDocument();
  });
});

describe("Vector Studio index", () => {
  it("shows index status, rebuilds and measures recall", async () => {
    const { mock, st } = setup();
    renderStudio("/vector/c/docs/index");
    expect(await screen.findByTestId("state-embedding")).toHaveTextContent("ready");
    expect(screen.getByText("3 / 3")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Rebuild embedding" }));
    await waitFor(() => expect(st.rebuilds).toBe(1));
    expect(await screen.findByText("manual, 7 ms")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Measure recall" }));
    expect(await screen.findByRole("status")).toHaveTextContent(/recall 0\.97\d* over 20 samples/);
    expect(mock.problems).toEqual([]);
  });
});

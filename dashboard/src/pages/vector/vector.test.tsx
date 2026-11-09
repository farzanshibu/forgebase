import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installVector } from "../../test/vgMock";

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
  it("lists collections with totals, creates one and deletes it", async () => {
    const { mock } = setup();
    renderStudio("/vector");
    expect(await screen.findByRole("link", { name: "docs" })).toBeInTheDocument();
    expect(await screen.findByLabelText("Vector totals")).toHaveTextContent("1 collections");
    expect(screen.getByText("embedding(3, l2)")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "New collection" }));
    const form = screen.getByRole("form", { name: "New vector collection" });
    await userEvent.type(within(form).getByLabelText("Name"), "notes");
    await userEvent.clear(within(form).getByLabelText("Dimension"));
    await userEvent.type(within(form).getByLabelText("Dimension"), "4");
    await userEvent.selectOptions(within(form).getByLabelText("Metric"), "cosine");
    await userEvent.click(within(form).getByRole("button", { name: "Create collection" }));
    expect(await screen.findByRole("link", { name: "notes" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/vector/collections"]?.[0]).toMatchObject({ name: "notes", fields: [{ name: "embedding", dimension: 4, metric: "cosine", quantization: "f32" }] });

    await userEvent.click(screen.getByRole("button", { name: "Delete notes" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete notes" }));
    await waitFor(() => expect(screen.queryByRole("link", { name: "notes" })).not.toBeInTheDocument());
    expect(mock.problems).toEqual([]);
  });

  it("shows the API error envelope when the name is taken", async () => {
    const { mock } = setup();
    renderStudio("/vector");
    await screen.findByRole("link", { name: "docs" });
    await userEvent.click(screen.getByRole("button", { name: "New collection" }));
    const form = screen.getByRole("form", { name: "New vector collection" });
    await userEvent.type(within(form).getByLabelText("Name"), "docs");
    await userEvent.click(within(form).getByRole("button", { name: "Create collection" }));
    expect(await screen.findByText(/collection exists/)).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });
});

describe("Vector Studio records", () => {
  it("browses, inspects, upserts and deletes records", async () => {
    const { mock, st } = setup();
    renderStudio("/vector/c/docs");
    expect(await screen.findByText("3 records")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "b" }));
    expect(await screen.findByLabelText("Item JSON")).toHaveTextContent('"green pear"');

    const form = screen.getByRole("form", { name: "Upsert record" });
    await userEvent.type(within(form).getByLabelText(/^Id/), "d");
    await userEvent.type(within(form).getByLabelText("Vector"), "0, 0, 1");
    await userEvent.type(within(form).getByLabelText(/^Metadata/), '{{"tag":"z"}');
    await userEvent.click(within(form).getByRole("button", { name: "Upsert" }));
    expect(await screen.findByText("Saved d (1 created, 0 updated)")).toBeInTheDocument();
    await screen.findByText("4 records");
    expect(mock.bodies["post /api/v1/vector/collections/{name}/upsert"]?.[0]).toEqual({ items: [{ id: "d", vectors: { embedding: [0, 0, 1] }, metadata: { tag: "z" } }] });

    await userEvent.click(screen.getByRole("button", { name: "Delete a" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete a" }));
    await screen.findByText("3 records");
    expect(st.items.docs!.map((i) => i.id)).toEqual(["b", "c", "d"]);
    expect(mock.problems).toEqual([]);
  });

  it("rejects a malformed vector before calling the API", async () => {
    const { mock } = setup();
    renderStudio("/vector/c/docs");
    await screen.findByText("3 records");
    const form = screen.getByRole("form", { name: "Upsert record" });
    await userEvent.type(within(form).getByLabelText("Vector"), "1, x");
    await userEvent.click(within(form).getByRole("button", { name: "Upsert" }));
    expect(await screen.findByText("A vector is a list of finite numbers")).toBeInTheDocument();
    expect(mock.calls.filter((c) => c.includes("/upsert"))).toEqual([]);
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

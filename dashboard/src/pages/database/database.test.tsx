import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, seeded } from "../../test/helpers";
import { pick } from "../../test/pick";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

describe("auth states", () => {
  it("returns to sign-in with an expiry notice when the API answers 401 mid-session", async () => {
    const mock = seeded({ requireAuth: true });
    renderStudio("/database/sql");
    const run = await screen.findByRole("button", { name: "Run" });
    mock.cookieSession = false;
    await userEvent.click(run);
    expect(await screen.findByText("Your session expired. Sign in again.")).toBeInTheDocument();
    expect(screen.getByRole("form", { name: "Sign in" })).toBeInTheDocument();
  });
});

describe("collection browser and grid", () => {
  it("lists collections and opens one", async () => {
    const mock = seeded();
    renderStudio("/database");
    await userEvent.click(await screen.findByRole("link", { name: "products" }));
    expect(await screen.findByText("Pen")).toBeInTheDocument();
    expect(screen.getByText("Notebook")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("filters, sorts and paginates through the API query string", async () => {
    const mock = seeded();
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    await userEvent.type(screen.getByLabelText("Filter"), "title = Pen");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(screen.queryByText("Notebook")).not.toBeInTheDocument());
    expect(screen.getByText(/1 records/)).toBeInTheDocument();
    await userEvent.clear(screen.getByLabelText("Filter"));
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await screen.findByText("Notebook");
    await userEvent.click(screen.getByRole("button", { name: /^price/ }));
    await userEvent.click(screen.getByRole("button", { name: /^price/ }));
    await waitFor(() => {
      const rows = screen.getAllByRole("row").slice(1);
      expect(within(rows[0]!).getByText("Pen")).toBeInTheDocument(); // price desc by string compare: "3" > "12"
    });
    expect(screen.getByRole("button", { name: "Previous" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
    expect(mock.problems).toEqual([]);
  });

  it("edits a cell inline with If-Match and shows per-field errors from the envelope", async () => {
    const mock = seeded();
    renderStudio("/database/c/products");
    const cell = await screen.findByText("Pen");
    await userEvent.dblClick(cell);
    const input = screen.getByLabelText("Edit title");
    await userEvent.clear(input);
    await userEvent.type(input, "Marker{Enter}");
    expect(await screen.findByText("Marker")).toBeInTheDocument();
    expect(mock.records.products![0]).toMatchObject({ title: "Marker", _version: 2 });

    // A bad number is caught before the request is sent.
    await userEvent.dblClick(screen.getByText("12"));
    const price = screen.getByLabelText("Edit price");
    await userEvent.clear(price);
    await userEvent.type(price, "abc{Enter}");
    expect(await screen.findByText(/price: Not a number/)).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("deletes a record only after confirmation and supports bulk delete", async () => {
    const mock = seeded();
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    const row = screen.getByText("Pen").closest("tr")!;
    await userEvent.click(within(row).getByRole("button", { name: "Delete" }));
    expect(mock.records.products).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: /Confirm delete/ }));
    await waitFor(() => expect(screen.queryByText("Pen")).not.toBeInTheDocument());

    await userEvent.click(screen.getByLabelText("Select all"));
    await userEvent.click(screen.getByRole("button", { name: "Delete selected" }));
    await userEvent.click(screen.getByRole("button", { name: /Confirm delete selected/ }));
    expect(await screen.findByText(/No records/)).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("bulk-sets a field on the selected rows", async () => {
    const mock = seeded();
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    await userEvent.click(screen.getByLabelText("Select all"));
    await pick(screen.getByLabelText("Bulk field"), "price");
    await userEvent.type(screen.getByLabelText("Bulk value"), "7");
    await userEvent.click(screen.getByRole("button", { name: "Apply to selected" }));
    await waitFor(() => expect(mock.records.products!.map((r) => r.price)).toEqual([7, 7]));
  });
});

describe("JSON editor", () => {
  it("creates a record and surfaces validation errors per field", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/r/new");
    const box = await screen.findByLabelText("Record JSON");
    await userEvent.clear(box);
    await userEvent.click(box);
    await userEvent.paste('{"price": 5}');
    await userEvent.click(screen.getByRole("button", { name: "Create record" }));
    expect(await screen.findByText(/validation_failed/)).toBeInTheDocument();
    expect(screen.getByText(/title is required/)).toBeInTheDocument();

    await userEvent.clear(box);
    await userEvent.click(box);
    await userEvent.paste('{"title": "Stapler", "price": 5}');
    await userEvent.click(screen.getByRole("button", { name: "Create record" }));
    await waitFor(() => expect(mock.records.products).toHaveLength(3));
    expect(await screen.findByText(/Record rec101/)).toBeInTheDocument();
  });

  it("rejects invalid JSON locally and saves edits with the loaded version", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/r/p1");
    const box = await screen.findByLabelText("Record JSON");
    await userEvent.type(box, "oops");
    await userEvent.click(screen.getByRole("button", { name: "Save record" }));
    expect(await screen.findByText(/Invalid JSON/)).toBeInTheDocument();

    await userEvent.clear(box);
    await userEvent.click(box);
    await userEvent.paste('{"id":"p1","_version":1,"title":"Pencil","price":2}');
    await userEvent.click(screen.getByRole("button", { name: "Save record" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
    const sent = mock.bodies["put /api/v1/collections/{collection}/records/{id}"]![0];
    expect(sent).toEqual({ title: "Pencil", price: 2 }); // read-only keys stripped
    expect(mock.problems).toEqual([]);
  });

  it("reports a version conflict from a stale If-Match", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/r/p1");
    await screen.findByLabelText("Record JSON");
    mock.records.products![0]!._version = 9; // changed by someone else
    await userEvent.click(screen.getByRole("button", { name: "Save record" }));
    expect(await screen.findByText(/version_conflict/)).toBeInTheDocument();
  });
});

describe("schema designer", () => {
  it("creates a collection with typed fields", async () => {
    const mock = seeded();
    renderStudio("/database/new");
    await userEvent.type(await screen.findByLabelText("Name"), "orders");
    await userEvent.click(screen.getByRole("button", { name: "Add field" }));
    await userEvent.type(screen.getByLabelText("Field 1 name"), "total");
    await pick(screen.getByLabelText("Field 1 type"), "number");
    await userEvent.click(screen.getByLabelText("Field 1 required"));
    await userEvent.click(screen.getByRole("button", { name: "Create collection" }));
    await waitFor(() => expect(mock.collections.map((c) => c.name)).toContain("orders"));
    const sent = mock.bodies["post /api/v1/collections"]![0] as { fields: Record<string, unknown>[] };
    expect(sent.fields[0]).toMatchObject({ name: "total", type: "number", required: true });
    expect(mock.problems).toEqual([]);
  });

  it("shows per-field validation errors and bad options JSON", async () => {
    seeded();
    renderStudio("/database/new");
    await screen.findByLabelText("Name");
    await userEvent.click(screen.getByRole("button", { name: "Create collection" }));
    expect(await screen.findByText(/name is required/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Add field" }));
    await userEvent.type(screen.getByLabelText("Field 1 options"), "{{nope");
    await userEvent.click(screen.getByRole("button", { name: "Create collection" }));
    expect(await screen.findByText(/not valid JSON/)).toBeInTheDocument();
  });

  it("edits an existing schema, keeping system fields locked", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/schema");
    await userEvent.click(await screen.findByRole("button", { name: "Add field" }));
    const nameInputs = screen.getAllByLabelText(/Field \d+ name/);
    expect(nameInputs[0]).toBeDisabled(); // system field `id`
    await userEvent.type(nameInputs.at(-1)!, "sku");
    await userEvent.click(screen.getByRole("button", { name: "Save schema" }));
    expect(await screen.findByText("Schema saved.")).toBeInTheDocument();
    expect(mock.collections[0]!.fields.map((f) => f.name)).toContain("sku");
  });
});

describe("index manager", () => {
  it("creates, rebuilds, shows stats and drops an index", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/indexes");
    await screen.findByText("No indexes.");
    await userEvent.type(screen.getByLabelText(/^Keys/), "price, -title");
    await userEvent.click(screen.getByRole("button", { name: "Create index" }));
    expect(await screen.findByText("idx_products_price_title")).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/indexes"]![0]).toMatchObject({ collection: "products", keys: [{ path: "price" }, { path: "title", desc: true }] });
    await userEvent.click(screen.getByRole("button", { name: "Stats" }));
    expect(await screen.findByText("8.00 KiB")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Rebuild" }));
    await waitFor(() => expect(mock.calls).toContain("post /api/v1/indexes/idx_products_price_title/rebuild"));
    await userEvent.click(screen.getByRole("button", { name: "Drop" }));
    await userEvent.click(screen.getByRole("button", { name: /Confirm drop/ }));
    await screen.findByText("No indexes.");
    expect(mock.problems).toEqual([]);
  });

  it("applies a recommendation", async () => {
    const mock = seeded();
    renderStudio("/database/indexes");
    await userEvent.click(await screen.findByRole("button", { name: "Create" }));
    await waitFor(() => expect(mock.indexes).toHaveLength(1));
  });
});

describe("SQL editor and slow queries", () => {
  it("runs a query, keeps history, and explains", async () => {
    const mock = seeded();
    renderStudio("/database/sql");
    await userEvent.click(await screen.findByRole("button", { name: "Run" }));
    const table = await screen.findByRole("table", { name: "Result" });
    expect(within(table).getByText("one")).toBeInTheDocument();
    expect(JSON.parse(localStorage.getItem("forgebase.sql.history")!)).toEqual(["SELECT 1 AS one"]);
    await userEvent.click(screen.getByRole("button", { name: "Explain" }));
    expect(await screen.findByText("SCAN products")).toBeInTheDocument();
    expect(screen.getByText(/Full scans: products/)).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("shows SQL errors from the envelope", async () => {
    seeded();
    renderStudio("/database/sql");
    const box = await screen.findByLabelText("SQL");
    await userEvent.clear(box);
    await userEvent.type(box, "SELEC nope");
    await userEvent.click(screen.getByRole("button", { name: "Run" }));
    expect(await screen.findByText(/sql_error/)).toBeInTheDocument();
  });

  it("lists slow queries, sets the threshold and clears the log", async () => {
    const mock = seeded();
    renderStudio("/database/slow");
    expect(await screen.findByText(/SELECT \* FROM t WHERE x = \?/)).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Threshold ms"), "50");
    await userEvent.click(screen.getByRole("button", { name: "Set threshold" }));
    expect(await screen.findByText("50 ms")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Clear log" }));
    await userEvent.click(screen.getByRole("button", { name: /Confirm clear log/ }));
    expect(await screen.findByText("No slow queries recorded.")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });
});

describe("import / export", () => {
  it("exports and imports", async () => {
    const mock = seeded();
    const click = vi.fn();
    URL.createObjectURL = vi.fn(() => "blob:x");
    URL.revokeObjectURL = vi.fn();
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(click);
    renderStudio("/database/c/products/transfer");
    await userEvent.click(await screen.findByRole("button", { name: "Export" }));
    await waitFor(() => expect(click).toHaveBeenCalled());

    const file = new File([JSON.stringify([{ title: "Ruler", price: 1 }])], "r.json", { type: "application/json" });
    await userEvent.upload(screen.getByLabelText("Import file"), file);
    await userEvent.click(screen.getByRole("button", { name: "Import" }));
    expect(await screen.findByText(/Imported 1 records \(json\)/)).toBeInTheDocument();
    expect(mock.records.products).toHaveLength(3);
    expect(mock.problems).toEqual([]);
  });
});

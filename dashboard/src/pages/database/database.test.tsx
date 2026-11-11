import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, seeded } from "../../test/helpers";
import { confirmDialog } from "../../test/dataStudioMock";
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

  it("filters, sorts and pages through the API query string and keeps them in the URL", async () => {
    const mock = seeded();
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    await userEvent.type(screen.getByLabelText("Filter"), "title = Pen");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(screen.queryByText("Notebook")).not.toBeInTheDocument());
    expect(screen.getByText("1–1 of 1")).toBeInTheDocument();
    expect(mock.calls.filter((c) => c.endsWith("/records")).length).toBeGreaterThan(1);
    await userEvent.click(screen.getByRole("button", { name: "Clear filter" }));
    await screen.findByText("Notebook");
    expect(screen.getByLabelText("Filter")).toHaveValue("");
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

  it("starts from the filter, sort and page in the URL and shows each column's type", async () => {
    const mock = seeded();
    renderStudio('/database/c/products?filter=title%20~%20%22Note%22&sort=-price&page=1');
    expect(await screen.findByText("Notebook")).toBeInTheDocument();
    expect(screen.queryByText("Pen")).not.toBeInTheDocument();
    expect(screen.getByLabelText("Filter")).toHaveValue('title ~ "Note"');
    const q = mock.calls.filter((c) => c.endsWith("/records"));
    expect(q.length).toBeGreaterThan(0);
    const head = screen.getByRole("columnheader", { name: /^price/ });
    expect(head).toHaveAttribute("aria-sort", "descending");
    expect(within(head).getByText("number")).toBeInTheDocument();
    expect(screen.getByText("Filter help")).toBeInTheDocument();
  });

  it("opens a record in the form from a row click and saves it with the loaded version", async () => {
    const mock = seeded();
    renderStudio("/database/c/products");
    await userEvent.click(await screen.findByText("Pen"));
    const form = await screen.findByRole("form", { name: "Edit record p1" });
    const title = within(form).getByLabelText(/^title/);
    expect(title).toHaveValue("Pen");
    await userEvent.clear(title);
    await userEvent.type(title, "Pencil");
    await userEvent.click(within(form).getByRole("button", { name: "Save record" }));
    await waitFor(() => expect(mock.records.products![0]).toMatchObject({ title: "Pencil", price: 3, _version: 2 }));
    expect(mock.bodies["put /api/v1/collections/{collection}/records/{id}"]![0]).toMatchObject({ title: "Pencil", price: 3 });
    expect(mock.headers.some((h) => h["If-Match"] === '"1"')).toBe(true);
    expect(mock.problems).toEqual([]);
  });

  it("deletes a record only after confirmation and supports bulk delete", async () => {
    const mock = seeded();
    renderStudio("/database/c/products");
    await screen.findByText("Pen");
    await userEvent.click(screen.getByRole("button", { name: "Delete p1" }));
    expect(mock.records.products).toHaveLength(2);
    await confirmDialog("Delete record p1?", "Delete record");
    await waitFor(() => expect(screen.queryByText("Pen")).not.toBeInTheDocument());

    await userEvent.click(screen.getByLabelText("Select all"));
    await userEvent.click(screen.getByRole("button", { name: "Delete selected" }));
    await confirmDialog("Delete 1 records?", "Delete selected");
    expect(await screen.findByRole("region", { name: "No records yet" })).toBeInTheDocument();
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

describe("record form", () => {
  it("adds a record through typed fields and maps server field errors under their fields", async () => {
    const mock = seeded();
    renderStudio("/database/c/products?record=new");
    const form = await screen.findByRole("form", { name: "New record" });
    await userEvent.type(within(form).getByLabelText(/^price/), "5");
    await userEvent.click(within(form).getByRole("button", { name: "Create record" }));
    // the form catches the missing required field before any request is sent
    expect(await within(form).findByText("This field is required.")).toBeInTheDocument();
    expect(mock.records.products).toHaveLength(2);

    await userEvent.type(within(form).getByLabelText(/^title/), "Stapler");
    await userEvent.click(within(form).getByRole("button", { name: "Create record" }));
    await waitFor(() => expect(mock.records.products).toHaveLength(3));
    expect(mock.bodies["post /api/v1/collections/{collection}/records"]![0]).toEqual({ title: "Stapler", price: 5 });
    expect(await screen.findByText("Record created")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("shows the server's field error under the field", async () => {
    const mock = seeded();
    mock.on("POST", "/api/v1/collections/{collection}/records", () => ({
      status: 400,
      body: { error: { code: "validation_failed", message: "invalid record", request_id: "r9", details: { fields: { title: { code: "too_short", message: "title must have at least 3 characters" } } } } },
    }));
    renderStudio("/database/c/products?record=new");
    const form = await screen.findByRole("form", { name: "New record" });
    await userEvent.type(within(form).getByLabelText(/^title/), "ab");
    await userEvent.click(within(form).getByRole("button", { name: "Create record" }));
    expect(await within(form).findByText("title must have at least 3 characters")).toBeInTheDocument();
    expect(within(form).getByLabelText(/^title/)).toHaveAttribute("aria-invalid", "true");
  });

  it("edits as JSON: bad JSON is refused locally, good JSON strips read-only keys", async () => {
    const mock = seeded();
    renderStudio("/database/c/products?record=p1");
    const form = await screen.findByRole("form", { name: "Edit record p1" });
    await userEvent.click(within(form).getByRole("switch", { name: "Edit as JSON" }));
    const box = await within(form).findByLabelText("Record JSON");
    expect(JSON.parse((box as HTMLTextAreaElement).value)).toMatchObject({ title: "Pen", price: 3 });
    await userEvent.click(box);
    await userEvent.clear(box);
    await userEvent.type(box, "oops");
    await userEvent.click(within(form).getByRole("button", { name: "Save record" }));
    expect((await within(form).findAllByText(/Invalid JSON/)).length).toBeGreaterThan(0);

    await userEvent.clear(box);
    await userEvent.click(box);
    await userEvent.paste('{"id":"p1","_version":1,"title":"Pencil","price":2}');
    await userEvent.click(within(form).getByRole("button", { name: "Save record" }));
    await waitFor(() => expect(mock.records.products![0]).toMatchObject({ title: "Pencil", price: 2 }));
    expect(mock.bodies["put /api/v1/collections/{collection}/records/{id}"]![0]).toEqual({ title: "Pencil", price: 2 }); // read-only keys stripped
    expect(mock.problems).toEqual([]);
  });

  it("reports a version conflict from a stale If-Match", async () => {
    const mock = seeded();
    renderStudio("/database/c/products?record=p1");
    const form = await screen.findByRole("form", { name: "Edit record p1" });
    await within(form).findByLabelText(/^title/);
    mock.records.products![0]!._version = 9; // changed by someone else
    await userEvent.click(within(form).getByRole("button", { name: "Save record" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(/version/i);
  });

  it("old record links open the form on the grid", async () => {
    seeded();
    renderStudio("/database/c/products/r/p2");
    expect(await screen.findByRole("form", { name: "Edit record p2" })).toBeInTheDocument();
    expect(screen.getByText("Notebook", { selector: "td" })).toBeInTheDocument();
  });
});

describe("schema designer", () => {
  it("creates a collection with typed fields and typed options", async () => {
    const mock = seeded();
    renderStudio("/database/new");
    await userEvent.type(await screen.findByLabelText(/^Name/), "orders");
    await userEvent.click(screen.getByRole("button", { name: "Add field" }));
    await userEvent.type(screen.getByLabelText("Field 1 name"), "total");
    await pick(screen.getByLabelText("Field 1 type"), "number");
    await userEvent.click(screen.getByLabelText("Field 1 required"));
    await userEvent.type(screen.getByLabelText("Field 1 min"), "0");
    await userEvent.click(screen.getByRole("checkbox", { name: "Whole numbers only" }));
    await userEvent.click(screen.getByRole("button", { name: "Create collection" }));
    await waitFor(() => expect(mock.collections.map((c) => c.name)).toContain("orders"));
    const sent = mock.bodies["post /api/v1/collections"]![0] as { fields: Record<string, unknown>[] };
    expect(sent.fields[0]).toMatchObject({ name: "total", type: "number", required: true, min: 0, integer: true });
    expect(mock.problems).toEqual([]);
  });

  it("explains type and schema mode with help tips", async () => {
    seeded();
    renderStudio("/database/new");
    await screen.findByLabelText(/^Name/);
    expect(screen.getByRole("button", { name: "What is Base collection?" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "What is Auth collection?" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "What is View collection?" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "What is Schema mode?" })).toBeInTheDocument();
  });

  it("validates names as you type and refuses a bad option value", async () => {
    const mock = seeded();
    renderStudio("/database/new");
    const name = await screen.findByLabelText(/^Name/);
    await userEvent.type(name, "1bad");
    expect(await screen.findByText("Use letters, digits and underscores, starting with a letter.")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Create collection" })).toBeDisabled;
    await userEvent.clear(name);
    await userEvent.type(name, "good");
    await userEvent.click(screen.getByRole("button", { name: "Add field" }));
    await userEvent.type(screen.getByLabelText("Field 1 name"), "title");
    await userEvent.type(screen.getByLabelText("Field 1 min length"), "abc");
    await userEvent.click(screen.getByRole("button", { name: "Create collection" }));
    expect(await screen.findByText(/must be a whole number/)).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/collections"]).toBeUndefined();
  });

  it("edits an existing schema, keeping system fields locked", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/schema");
    await userEvent.click(await screen.findByRole("button", { name: "Add field" }));
    const nameInputs = screen.getAllByLabelText(/Field \d+ name/);
    expect(nameInputs[0]).toBeDisabled(); // system field `id`
    await userEvent.type(nameInputs.at(-1)!, "sku");
    await userEvent.click(screen.getByRole("button", { name: "Save schema" }));
    expect(await screen.findByText("Schema saved")).toBeInTheDocument();
    expect(mock.collections[0]!.fields.map((f) => f.name)).toContain("sku");
  });

  it("deletes a collection only after its name is typed", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/schema");
    await userEvent.click(await screen.findByRole("button", { name: "Delete collection" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete collection products?" });
    const confirm = within(dialog).getByRole("button", { name: "Delete collection" });
    expect(confirm).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText(/to confirm/), "products");
    await userEvent.click(confirm);
    await waitFor(() => expect(mock.collections).toHaveLength(0));
    expect(await screen.findByRole("region", { name: "No collections yet" })).toBeInTheDocument();
  });
});

describe("index manager", () => {
  it("creates, rebuilds, shows stats and drops an index", async () => {
    const mock = seeded();
    renderStudio("/database/c/products/indexes");
    await screen.findByRole("region", { name: "No indexes yet" });
    await userEvent.type(screen.getByLabelText(/^Keys/), "price, -title");
    await userEvent.click(screen.getByRole("button", { name: "Create index" }));
    expect(await screen.findByText("idx_products_price_title")).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/indexes"]![0]).toMatchObject({ collection: "products", keys: [{ path: "price" }, { path: "title", desc: true }] });
    await userEvent.click(screen.getByRole("button", { name: "Stats" }));
    expect(await screen.findByText("8.00 KiB")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Rebuild" }));
    await waitFor(() => expect(mock.calls).toContain("post /api/v1/indexes/idx_products_price_title/rebuild"));
    await userEvent.click(screen.getByRole("button", { name: "Drop idx_products_price_title" }));
    await confirmDialog("Drop index idx_products_price_title?", "Drop index");
    await screen.findByRole("region", { name: "No indexes yet" });
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
    await confirmDialog("Clear the slow-query log?", "Clear log");
    expect(await screen.findByRole("region", { name: "No slow queries recorded" })).toBeInTheDocument();
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

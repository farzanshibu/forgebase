/** End-to-end smoke: drives the Database Studio main flows through the real router against the OpenAPI-checked mock. */
import { afterEach, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio } from "./test/helpers";
import { MockApi } from "./test/mockApi";

afterEach(() => vi.unstubAllGlobals());

it("create collection -> add record -> filter -> edit cell -> index -> SQL -> slow log", async () => {
  const mock = new MockApi().install();
  renderStudio("/database");
  const u = userEvent.setup();

  // Empty server, then design a collection.
  expect(await screen.findByText(/No collections yet/)).toBeInTheDocument();
  await u.click(screen.getByRole("link", { name: "New collection" }));
  await u.type(await screen.findByLabelText("Name"), "notes");
  await u.click(screen.getByRole("button", { name: "Add field" }));
  await u.type(screen.getByLabelText("Field 1 name"), "body");
  await u.click(screen.getByLabelText("Field 1 required"));
  await u.click(screen.getByRole("button", { name: "Create collection" }));
  expect(await screen.findByRole("heading", { name: "notes" })).toBeInTheDocument();

  // Add two records through the JSON editor.
  for (const body of ["alpha", "beta"]) {
    await u.click(await screen.findByRole("link", { name: "Add record" }));
    const box = await screen.findByLabelText("Record JSON");
    await u.clear(box);
    await u.click(box);
    await u.paste(JSON.stringify({ body }));
    await u.click(screen.getByRole("button", { name: "Create record" }));
    await screen.findByText(/^Record rec/);
    await u.click(screen.getByRole("link", { name: "Back to data" }));
  }
  expect(await screen.findByText("alpha")).toBeInTheDocument();
  expect(screen.getByText("beta")).toBeInTheDocument();

  // Filter, then edit a cell.
  await u.type(screen.getByLabelText("Filter"), "body = beta");
  await u.click(screen.getByRole("button", { name: "Apply filter" }));
  await waitFor(() => expect(screen.queryByText("alpha")).not.toBeInTheDocument());
  await u.dblClick(screen.getByText("beta"));
  const input = screen.getByLabelText("Edit body");
  await u.clear(input);
  await u.type(input, "gamma{Enter}");
  // The edited row no longer matches the active filter; clearing it shows the new value.
  expect(await screen.findByText(/No records match this filter/)).toBeInTheDocument();
  await u.clear(screen.getByLabelText("Filter"));
  await u.click(screen.getByRole("button", { name: "Apply filter" }));
  expect(await screen.findByText("gamma")).toBeInTheDocument();

  // Index manager.
  await u.click(within(screen.getByRole("navigation", { name: "Collection sections" })).getByRole("link", { name: "Indexes" }));
  await u.type(await screen.findByLabelText(/^Keys/), "body");
  await u.click(screen.getByRole("button", { name: "Create index" }));
  expect(await screen.findByText("idx_notes_body")).toBeInTheDocument();

  // SQL editor + slow queries.
  await u.click(screen.getByRole("link", { name: "SQL editor" }));
  await u.click(await screen.findByRole("button", { name: "Run" }));
  expect(await screen.findByRole("table", { name: "Result" })).toBeInTheDocument();
  await u.click(screen.getByRole("link", { name: "Slow queries" }));
  expect(await screen.findByText(/250\.5 ms/)).toBeInTheDocument();

  expect(mock.problems).toEqual([]);
});

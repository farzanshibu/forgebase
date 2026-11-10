import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, useLocation } from "react-router-dom";
import { setTokenSource } from "../api";
import { STUDIOS, studio, visibleStudios } from "../studios";
import { makeCollection, MockApi } from "../test/mockApi";
import { CommandPalette } from "./CommandPalette";
import { resetPaletteCache } from "./paletteData";

function Where() {
  const l = useLocation();
  return <output aria-label="Path">{l.pathname + l.search}</output>;
}

/** A studio's own row, not one of its "Studio › Tab" rows. */
const studioOption = (label: string) => new RegExp(`^${label}(?! ›)`);

function setup(studios = STUDIOS) {
  setTokenSource(() => null);
  const mock = new MockApi({ collections: [makeCollection("notes", [{ name: "title", type: "text" }])] }).install();
  // Other resource lists answer empty; a failing source is skipped.
  mock.on("GET", "/api/v1/storage/buckets", () => ({ status: 200, body: { items: [{ name: "avatars", visibility: "private", versioning: false, created_at: "2026-11-01T00:00:00Z" }] } }));
  mock.on("GET", "/api/v1/functions", () => ({ status: 403, body: { error: { code: "forbidden", message: "missing permission functions:read", request_id: "r" } } }));
  mock.on("GET", "/api/v1/queues/topics", () => ({ status: 200, body: { items: [] } }));
  mock.on("GET", "/api/v1/vector/collections", () => ({ status: 200, body: { items: [] } }));
  render(
    <MemoryRouter initialEntries={["/"]}>
      <CommandPalette studios={studios} />
      <Where />
    </MemoryRouter>,
  );
  return mock;
}

beforeEach(() => resetPaletteCache());
afterEach(() => vi.unstubAllGlobals());

describe("CommandPalette", () => {
  it("opens on Ctrl+K and on Cmd+K, as a dialog with a search combobox, and Escape closes it", async () => {
    setup();
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const dialog = await screen.findByRole("dialog", { name: "Command palette" });
    expect(await screen.findByRole("combobox", { name: /search/i })).toHaveFocus();
    await userEvent.keyboard("{Escape}");
    await waitFor(() => expect(dialog).not.toBeInTheDocument());
    fireEvent.keyDown(window, { key: "K", metaKey: true });
    expect(await screen.findByRole("dialog", { name: "Command palette" })).toBeInTheDocument();
  });

  it("opens from the Search button and lists every studio and the create actions", async () => {
    setup();
    await userEvent.click(screen.getByRole("button", { name: /Search/ }));
    await screen.findByRole("dialog", { name: "Command palette" });
    for (const s of STUDIOS) expect(screen.getByRole("option", { name: studioOption(s.label) })).toBeInTheDocument();
    for (const a of ["New collection", "New bucket", "New function", "Create API key", "New topic"]) expect(screen.getByRole("option", { name: new RegExp(a) })).toBeInTheDocument();
  });

  it("filters as you type and Enter goes to the highlighted result", async () => {
    setup();
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const input = await screen.findByRole("combobox", { name: /search/i });
    await userEvent.type(input, "Key-val");
    await waitFor(() => expect(screen.queryByRole("option", { name: studioOption("Database") })).not.toBeInTheDocument());
    expect(screen.getByRole("option", { name: studioOption("Key-value") })).toBeInTheDocument();
    await userEvent.keyboard("{Enter}");
    await waitFor(() => expect(screen.getByLabelText("Path")).toHaveTextContent("/redis"));
    await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  });

  it("finds resources fetched on open, skipping a source that refuses", async () => {
    const mock = setup();
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    const input = await screen.findByRole("combobox", { name: /search/i });
    expect(await screen.findByRole("option", { name: /avatars/ })).toBeInTheDocument();
    await userEvent.type(input, "notes");
    const opt = await screen.findByRole("option", { name: /notes/ });
    await userEvent.click(opt);
    await waitFor(() => expect(screen.getByLabelText("Path")).toHaveTextContent("/database/c/notes"));
    expect(mock.problems).toEqual([]);
  });

  it("hides studios whose feature is off, with their create actions and resources", async () => {
    const studios = visibleStudios([{ id: "storage", name: "Storage", summary: "", always_on: false, can_disable: true, enabled: false, studio_path: "/storage", settings: [] }]);
    expect(studios).not.toContain(studio("storage"));
    setup(studios);
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    await screen.findByRole("dialog", { name: "Command palette" });
    expect(screen.queryByRole("option", { name: studioOption("Storage") })).not.toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /New bucket/ })).not.toBeInTheDocument();
    expect(await screen.findByRole("option", { name: /notes/ })).toBeInTheDocument();
    expect(screen.queryByRole("option", { name: /avatars/ })).not.toBeInTheDocument();
  });
});

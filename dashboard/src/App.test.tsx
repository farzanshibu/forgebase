import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import App from "./App";
import { GROUPS, STUDIOS } from "./studios";
import { signedIn } from "./test/helpers";
import { MockApi } from "./test/mockApi";

afterEach(() => vi.unstubAllGlobals());

describe("App shell", () => {
  it("lists every studio in the sidebar", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter><App /></MemoryRouter>);
    const nav = await screen.findByRole("navigation", { name: "Studios" });
    for (const s of STUDIOS) expect(await within(nav).findByRole("link", { name: s.label })).toBeInTheDocument();
  });

  it("opens on the live overview, not a placeholder", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter initialEntries={["/"]}><App /></MemoryRouter>);
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
    expect(screen.queryByText(/Arrives in phase/)).not.toBeInTheDocument();
  });

  it("renders a lazily loaded studio route once its chunk arrives", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter initialEntries={["/system"]}><App /></MemoryRouter>);
    // The lazy route renders once its chunk has loaded.
    expect(await screen.findByRole("tablist", { name: "System sections" })).toBeInTheDocument();
  });

  it("groups the nav: Overview first, then Build, Users and access, Operate, with one name per studio", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter><App /></MemoryRouter>);
    const nav = await screen.findByRole("navigation", { name: "Studios" });
    const text = nav.textContent ?? "";
    const at = (w: string) => text.indexOf(w);
    expect(at("Overview")).toBeGreaterThanOrEqual(0);
    expect(at("Overview")).toBeLessThan(at("Build"));
    expect(at("Build")).toBeLessThan(at("Users and access"));
    expect(at("Users and access")).toBeLessThan(at("Operate"));
    expect(text).not.toMatch(/Studio\b/);
    for (const g of GROUPS) for (const s of STUDIOS.filter((x) => x.group === g.id)) expect(at(s.label)).toBeGreaterThan(at(g.label));
    expect(within(nav).getByRole("link", { name: "Key-value" })).toHaveAttribute("href", "/redis");
  });

  it("hides a studio whose feature is switched off from the nav and the palette", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("GET", "/api/system/features", () => ({ status: 200, body: { features: [{ id: "vector", name: "Vector", summary: "", always_on: false, can_disable: true, enabled: false, studio_path: "/vector", settings: [] }], restart_pending: false } }));
    render(<MemoryRouter><App /></MemoryRouter>);
    const nav = await screen.findByRole("navigation", { name: "Studios" });
    await waitFor(() => expect(within(nav).queryByRole("link", { name: "Vector" })).not.toBeInTheDocument());
    expect(within(nav).getByRole("link", { name: "Graph" })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "k", ctrlKey: true });
    await screen.findByRole("dialog", { name: "Command palette" });
    expect(screen.queryByRole("option", { name: /^Vector(?! ›)/ })).not.toBeInTheDocument();
  });

  it("puts breadcrumbs and a Search button in the top bar, and Ctrl+K jumps to a studio", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter initialEntries={["/system/backups"]}><App /></MemoryRouter>);
    const crumbs = await screen.findByRole("navigation", { name: "Breadcrumb" });
    expect(within(crumbs).getByRole("link", { name: "System" })).toHaveAttribute("href", "/system");
    expect(crumbs).toHaveTextContent("Backups");
    expect(screen.getByRole("button", { name: /Search/ })).toBeInTheDocument();
    fireEvent.keyDown(window, { key: "k", metaKey: true });
    await userEvent.type(await screen.findByRole("combobox", { name: /search/i }), "Queues");
    await userEvent.keyboard("{Enter}");
    expect(await screen.findByRole("tablist", { name: "Queues sections" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("navigation", { name: "Breadcrumb" })).toHaveTextContent("Queues"));
  });

  it("answers an unknown address with a page that says how to find things", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter initialEntries={["/nowhere"]}><App /></MemoryRouter>);
    const heading = await screen.findByRole("heading", { name: "Page not found" });
    expect(within(heading.parentElement!).getByRole("link", { name: "Overview" })).toHaveAttribute("href", "/");
  });

  it("shows the sign-in form when nobody is signed in", async () => {
    signedIn(new MockApi().install(), false);
    render(<MemoryRouter><App /></MemoryRouter>);
    expect(await screen.findByRole("form", { name: "Sign in" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Studios" })).not.toBeInTheDocument();
  });
});

import { afterEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import App from "./App";
import { STUDIOS } from "./studios";
import { signedIn } from "./test/helpers";
import { MockApi } from "./test/mockApi";

afterEach(() => vi.unstubAllGlobals());

describe("App shell", () => {
  it("lists every studio in the sidebar", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter><App /></MemoryRouter>);
    for (const s of STUDIOS) expect(await screen.findByRole("link", { name: new RegExp(s.label) })).toBeInTheDocument();
  });

  it("opens on the live overview, not a placeholder", async () => {
    signedIn(new MockApi().install());
    render(<MemoryRouter initialEntries={["/"]}><App /></MemoryRouter>);
    expect(await screen.findByRole("heading", { name: "Dashboard" })).toBeInTheDocument();
    expect(screen.queryByText(/Arrives in phase/)).not.toBeInTheDocument();
  });

  it("builds every studio: none is left as a placeholder", () => {
    for (const s of STUDIOS) expect(s.phase).toBeNull();
  });

  it("shows the sign-in form when nobody is signed in", async () => {
    signedIn(new MockApi().install(), false);
    render(<MemoryRouter><App /></MemoryRouter>);
    expect(await screen.findByRole("form", { name: "Sign in" })).toBeInTheDocument();
    expect(screen.queryByRole("navigation", { name: "Studios" })).not.toBeInTheDocument();
  });
});

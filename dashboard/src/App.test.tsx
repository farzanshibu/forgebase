import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import App from "./App";
import { STUDIOS } from "./studios";

describe("App shell", () => {
  it("lists every studio in the sidebar", () => {
    render(<MemoryRouter><App /></MemoryRouter>);
    for (const s of STUDIOS) expect(screen.getByRole("link", { name: new RegExp(s.label) })).toBeInTheDocument();
  });

  it("shows an honest arrives-in-phase panel for unbuilt studios", () => {
    render(<MemoryRouter initialEntries={["/database"]}><App /></MemoryRouter>);
    expect(screen.getByText("Arrives in phase 2")).toBeInTheDocument();
  });
});

import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes, useLocation } from "react-router-dom";
import { describe, expect, it } from "vitest";
import { RouteTabs } from "./RouteTabs";

const TABS = [
  { to: "/x", label: "One", end: true },
  { to: "/x/two", label: "Two" },
  { to: "/x/three?ns=a", label: "Three" },
];

function Where() {
  const l = useLocation();
  return <p data-testid="where">{l.pathname + l.search}</p>;
}

function setup(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <RouteTabs label="Sections" tabs={TABS} />
      <Routes><Route path="*" element={<Where />} /></Routes>
    </MemoryRouter>,
  );
}

describe("RouteTabs", () => {
  it("marks the tab for the current location, nested paths included", () => {
    setup("/x/two/deeper");
    expect(screen.getByRole("tablist", { name: "Sections" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Two" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "One" })).toHaveAttribute("aria-selected", "false");
  });

  it("end tabs match only their exact path", () => {
    setup("/x/other");
    expect(screen.getByRole("tab", { name: "One" })).toHaveAttribute("aria-selected", "false");
  });

  it("navigates on choice and keeps the query string of the target", async () => {
    const u = userEvent.setup();
    setup("/x");
    expect(screen.getByRole("tab", { name: "One" })).toHaveAttribute("aria-selected", "true");
    await u.click(screen.getByRole("tab", { name: "Three" }));
    expect(screen.getByTestId("where")).toHaveTextContent("/x/three?ns=a");
    expect(screen.getByRole("tab", { name: "Three" })).toHaveAttribute("aria-selected", "true");
  });
});

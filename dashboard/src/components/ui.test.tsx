import { describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, useLocation, useNavigate } from "react-router-dom";
import { ConfirmButton } from "./ui";
import { RouteTabs, Sel, activeTab } from "./controls";
import { pick } from "../test/pick";

const TABS = [
  { to: "/x", label: "One", end: true },
  { to: "/x/two", label: "Two" },
  { to: "/x/two/deep", label: "Deeper" },
];

function Where() {
  const { pathname } = useLocation();
  const nav = useNavigate();
  return (
    <>
      <output aria-label="Path">{pathname}</output>
      <button onClick={() => nav(-1)}>Back</button>
    </>
  );
}

describe("activeTab", () => {
  it("is exact for an end tab and the longest prefix otherwise", () => {
    expect(activeTab(TABS, "/x")).toBe("/x");
    expect(activeTab(TABS, "/x/other")).toBeNull();
    expect(activeTab(TABS, "/x/two/9")).toBe("/x/two");
    expect(activeTab(TABS, "/x/two/deep/1")).toBe("/x/two/deep");
    expect(activeTab([{ to: "/o/a b/members", label: "m" }], "/o/a%20b/members")).toBe("/o/a b/members");
  });
});

describe("RouteTabs", () => {
  it("takes the active tab from the location, navigates on choosing, and follows the back button", async () => {
    render(
      <MemoryRouter initialEntries={["/x/two/9"]}>
        <RouteTabs label="Sections" tabs={TABS} />
        <Where />
      </MemoryRouter>,
    );
    expect(screen.getByRole("tab", { name: "Two" })).toHaveAttribute("aria-selected", "true");
    await userEvent.click(screen.getByRole("tab", { name: "One" }));
    expect(screen.getByLabelText("Path")).toHaveTextContent("/x");
    await waitFor(() => expect(screen.getByRole("tab", { name: "One" })).toHaveAttribute("aria-selected", "true"));
    expect(screen.getByRole("tab", { name: "One" })).toHaveAttribute("href", "/x");
    await userEvent.click(screen.getByRole("button", { name: "Back" }));
    await waitFor(() => expect(screen.getByRole("tab", { name: "Two" })).toHaveAttribute("aria-selected", "true"));
  });
});

describe("ConfirmButton", () => {
  it("opens an alert dialog; cancel does nothing and the action confirms once", async () => {
    const onConfirm = vi.fn();
    render(<ConfirmButton label="Delete thing" onConfirm={onConfirm} />);
    await userEvent.click(screen.getByRole("button", { name: "Delete thing" }));
    expect(await screen.findByRole("alertdialog")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
    expect(onConfirm).not.toHaveBeenCalled();
    await userEvent.click(screen.getByRole("button", { name: "Delete thing" }));
    await userEvent.click(await screen.findByRole("button", { name: "Confirm delete thing" }));
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });

  it("stays disabled when asked", () => {
    render(<ConfirmButton label="Delete thing" onConfirm={() => {}} disabled />);
    expect(screen.getByRole("button", { name: "Delete thing" })).toBeDisabled();
  });
});

describe("Sel", () => {
  function Harness({ onChange }: { onChange: (v: string) => void }) {
    const [v, setV] = useState("");
    return <Sel aria-label="Kind" value={v} placeholder="choose…" onChange={(n) => { setV(n); onChange(n); }} options={["a", { value: "b", label: "Bee" }]} />;
  }
  it("shows the placeholder, then the chosen option's label, and reports its value", async () => {
    const onChange = vi.fn();
    render(<Harness onChange={onChange} />);
    const trigger = screen.getByRole("combobox", { name: "Kind" });
    expect(trigger).toHaveTextContent("choose…");
    await pick(trigger, "b");
    expect(onChange).toHaveBeenCalledWith("b");
    expect(screen.getByRole("combobox", { name: "Kind" })).toHaveTextContent("Bee");
  });
});

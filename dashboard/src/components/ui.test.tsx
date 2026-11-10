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

  it("ignores a query string on a tab's target when matching", () => {
    const tabs = [{ to: "/x", label: "One", end: true }, { to: "/x/three?ns=a", label: "Three" }];
    expect(activeTab(tabs, "/x/three")).toBe("/x/three?ns=a");
    expect(activeTab(tabs, "/x/three/deeper")).toBe("/x/three?ns=a");
    expect(activeTab(tabs, "/x/other")).toBeNull();
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

describe("RouteTabs links", () => {
  it("keeps the query string of the target in the link and navigates to it", async () => {
    function Search() {
      const l = useLocation();
      return <output aria-label="Where">{l.pathname + l.search}</output>;
    }
    render(
      <MemoryRouter initialEntries={["/x"]}>
        <RouteTabs label="Sections" tabs={[{ to: "/x", label: "One", end: true }, { to: "/x/three?ns=a", label: <b>Three</b> }]} />
        <Search />
      </MemoryRouter>,
    );
    expect(screen.getByRole("tablist", { name: "Sections" })).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "One" })).toHaveAttribute("aria-selected", "true");
    expect(screen.getByRole("tab", { name: "Three" })).toHaveAttribute("href", "/x/three?ns=a");
    await userEvent.click(screen.getByRole("tab", { name: "Three" }));
    expect(screen.getByLabelText("Where")).toHaveTextContent("/x/three?ns=a");
    await waitFor(() => expect(screen.getByRole("tab", { name: "Three" })).toHaveAttribute("aria-selected", "true"));
  });

  it("end tabs match only their exact path", () => {
    render(
      <MemoryRouter initialEntries={["/x/other"]}>
        <RouteTabs label="Sections" tabs={TABS} />
      </MemoryRouter>,
    );
    expect(screen.getByRole("tab", { name: "One" })).toHaveAttribute("aria-selected", "false");
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

describe("Sel with numbers", () => {
  it("reports a numeric option as a number and shows its label", async () => {
    const onChange = vi.fn();
    function Harness() {
      const [v, setV] = useState(60);
      return <Sel aria-label="Expires in" value={v} onChange={(n) => { setV(n); onChange(n); }} options={[{ value: 60, label: "1 minute" }, { value: 300, label: "5 minutes" }]} />;
    }
    render(<Harness />);
    const trigger = screen.getByRole("combobox", { name: "Expires in" });
    expect(trigger).toHaveTextContent("1 minute");
    await pick(trigger, "5 minutes");
    expect(onChange).toHaveBeenCalledWith(300);
    expect(screen.getByRole("combobox", { name: "Expires in" })).toHaveTextContent("5 minutes");
  });
});

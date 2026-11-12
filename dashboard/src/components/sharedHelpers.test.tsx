import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { toaster } from "../toaster";
import { CopyButton } from "./Button";
import { CreateSheet } from "./CreateSheet";
import { Facts } from "./Facts";
import { IdText } from "./IdText";
import { StatGrid, Stat } from "./Stats";
import { Badge } from "./StatusBadge";
import { statusLive, statusTone } from "./statusTone";
import { TableCard } from "./TableCard";
import { When } from "./Time";
import { ConfirmButton } from "./ui";

describe("statusTone", () => {
  it("gives one colour per meaning across studios", () => {
    for (const s of ["succeeded", "paid", "active", "healthy", "enabled", "verified"]) expect(statusTone(s)).toBe("ok");
    for (const s of ["queued", "open", "pending", "scheduled", "draft"]) expect(statusTone(s)).toBe("info");
    for (const s of ["running", "processing", "warning", "expiring"]) expect(statusTone(s)).toBe("warn");
    for (const s of ["failed", "past_due", "error", "down", "suspended", "revoked", "uncollectible", "dead"]) expect(statusTone(s)).toBe("bad");
    for (const s of ["cancelled", "canceled", "void", "archived", "disabled", "paused", "expired"]) expect(statusTone(s)).toBe("neutral");
  });

  it("reads case, spaces and dashes alike, and is grey for a word it does not know", () => {
    expect(statusTone("Past due")).toBe("bad");
    expect(statusTone("past-due")).toBe("bad");
    expect(statusTone("SUCCEEDED")).toBe("ok");
    expect(statusTone("frobnicated")).toBe("neutral");
    expect(statusLive("running")).toBe(true);
    expect(statusLive("succeeded")).toBe(false);
  });

  it("draws a status badge with the status's words and a dot", () => {
    const { container } = render(<Badge status="past_due" />);
    expect(screen.getByText("past due")).toBeInTheDocument();
    expect(container.querySelector('[data-variant="destructive"]')).not.toBeNull();
    expect(container.querySelector('[data-slot="badge-dot"]')).not.toBeNull();
  });
});

describe("Facts", () => {
  it("takes tuples and objects, and skips falsy entries", () => {
    const off = false as boolean;
    render(
      <Facts
        label="Collection facts"
        items={[["Type", "base"], { label: "Id", value: "c_123", mono: true, title: "c_123" }, off && ["Hidden", "no"], null, undefined, ["Fields", 6]]}
      />,
    );
    const dl = screen.getByLabelText("Collection facts");
    expect(dl.tagName).toBe("DL");
    expect(within(dl).getAllByRole("term").map((t) => t.textContent)).toEqual(["Type", "Id", "Fields"]);
    expect(within(dl).getAllByRole("definition").map((d) => d.textContent)).toEqual(["base", "c_123", "6"]);
    expect(within(dl).getByText("c_123")).toHaveAttribute("title", "c_123");
    expect(dl).not.toHaveTextContent("Hidden");
  });
});

describe("When", () => {
  it("shows a relative time with the exact local and UTC time on hover", () => {
    vi.useFakeTimers({ toFake: ["Date"] });
    vi.setSystemTime(new Date("2026-11-12T10:00:00Z"));
    try {
      render(<When value="2026-11-12T09:55:00Z" />);
      const t = screen.getByText("5 min ago");
      expect(t.tagName).toBe("TIME");
      expect(t).toHaveAttribute("datetime", "2026-11-12T09:55:00Z");
      expect(t.getAttribute("title")).toContain("(2026-11-12 09:55:00Z)");
    } finally {
      vi.useRealTimers();
    }
  });

  it("shows the fallback for a missing time, a day with `day`, and Unix seconds as a time", () => {
    const { rerender } = render(<When value={null} fallback="Never" />);
    expect(screen.getByText("Never")).toBeInTheDocument();
    rerender(<When iso={undefined} />);
    expect(screen.getByText("—")).toBeInTheDocument();
    rerender(<When value="2026-11-12T09:55:00Z" day />);
    expect(screen.getByText(/2026/)).toHaveAttribute("title", expect.stringContaining("2026-11-12 09:55:00Z"));
    rerender(<When value={1_794_477_300} />);
    expect(document.querySelector("time")).toHaveAttribute("title", expect.stringContaining("2026-11-12 09:55:00Z"));
  });
});

describe("TableCard, StatGrid and IdText", () => {
  it("is a region named by its title, with the count beside it", () => {
    render(
      <TableCard title="Invoices" count={3} description="Newest first." actions={<button>Refresh</button>}>
        <p>body</p>
      </TableCard>,
    );
    const region = screen.getByRole("region", { name: "Invoices" });
    expect(within(region).getByRole("heading", { name: /^Invoices\s*3$/ })).toBeInTheDocument();
    expect(within(region).getByRole("button", { name: "Refresh" })).toBeInTheDocument();
  });

  it("draws stats from items or children the same way", () => {
    const onClick = vi.fn();
    render(
      <>
        <StatGrid label="From items" items={[{ label: "Failed", value: 2, tone: "bad" }]} />
        <StatGrid label="From children">
          <Stat label="Failed" value={2} tone="bad" onClick={onClick} />
        </StatGrid>
      </>,
    );
    expect(screen.getByLabelText("From items")).toHaveTextContent("Failed2");
    expect(screen.getByLabelText("From children")).toHaveTextContent("Failed2");
    screen.getByRole("button", { name: "Failed: 2" }).click();
    expect(onClick).toHaveBeenCalled();
  });

  it("cuts an id to fit and names its copy button", async () => {
    const write = vi.fn(async () => undefined);
    Object.defineProperty(navigator, "clipboard", { value: { writeText: write }, configurable: true });
    render(<IdText id="job_0123456789" noun="job" />);
    expect(screen.getByText("job_0123456789")).toHaveAttribute("title", "job_0123456789");
    const copy = screen.getByRole("button", { name: "Copy job id" });
    expect(copy).toHaveAttribute("title", "Copy job id");
    await userEvent.click(copy);
    expect(write).toHaveBeenCalledWith("job_0123456789");
    // The name stays the label; the tick and a polite "Copied" say it worked.
    expect(await screen.findByText("Copied")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Copy job id" })).toBeInTheDocument();
  });

  it("keeps the text CopyButton's words", () => {
    render(<CopyButton text="x" label="Copy request id" />);
    expect(screen.getByRole("button", { name: "Copy request id" })).toBeInTheDocument();
  });
});

describe("ConfirmButton short and CreateSheet toast", () => {
  it("shows the short words and keeps the full label as the name", async () => {
    const onConfirm = vi.fn();
    render(<ConfirmButton label="Archive price pro_month" short="Archive" onConfirm={onConfirm} />);
    const trigger = screen.getByRole("button", { name: "Archive price pro_month" });
    expect(trigger).toHaveTextContent("Archive");
    await userEvent.click(trigger);
    const dialog = await screen.findByRole("alertdialog");
    expect(dialog).toHaveTextContent("Archive price pro_month?");
    await userEvent.click(within(dialog).getByRole("button", { name: "Confirm archive price pro_month" }));
    expect(onConfirm).toHaveBeenCalled();
  });

  it("toasts saved for an edit sheet", async () => {
    const add = vi.spyOn(toaster, "add");
    const onOpenChange = vi.fn();
    render(
      <MemoryRouter>
        <CreateSheet open onOpenChange={onOpenChange} title="Edit schema" submitLabel="Save" toast="saved" onSubmit={async () => ({ what: "Schema" })}>
          <p>fields</p>
        </CreateSheet>
      </MemoryRouter>,
    );
    await userEvent.click(await screen.findByRole("button", { name: "Save" }));
    await waitFor(() => expect(add).toHaveBeenCalledWith(expect.objectContaining({ title: "Schema saved" })));
    expect(add).not.toHaveBeenCalledWith(expect.objectContaining({ title: "Schema created" }));
  });
});

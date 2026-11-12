import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { StatGrid, When } from "./ops";
import { elapsed, relative } from "./time";

const NOW = Date.parse("2026-11-12T10:00:00Z");

describe("ops time helpers", () => {
  it("says how long ago or how soon, in plain units", () => {
    expect(relative("2026-11-12T10:00:02Z", NOW)).toBe("just now");
    expect(relative("2026-11-12T09:59:18Z", NOW)).toBe("42 s ago");
    expect(relative("2026-11-12T09:55:00Z", NOW)).toBe("5 min ago");
    expect(relative("2026-11-12T13:00:00Z", NOW)).toBe("in 3 h");
    expect(relative("2026-11-10T10:00:00Z", NOW)).toBe("2 d ago");
    expect(relative("not a time", NOW)).toBe("not a time");
  });

  it("writes run times", () => {
    expect([elapsed(400), elapsed(1500), elapsed(95_000), elapsed(7_380_000)]).toEqual(["400 ms", "1.5 s", "1 min 35 s", "2 h 3 min"]);
  });

  it("shows a time with the exact time on hover, and words for a missing one", () => {
    const { rerender } = render(<When iso="2026-11-12T09:55:00Z" />);
    const t = screen.getByText(/ago|in |just now|\d{4}/);
    expect(t.tagName).toBe("TIME");
    expect(t).toHaveAttribute("datetime", "2026-11-12T09:55:00Z");
    expect(t.getAttribute("title")).toContain("2026-11-12 09:55:00Z");
    rerender(<When iso={null} empty="not yet" />);
    expect(screen.getByText("not yet")).toBeInTheDocument();
  });

  it("labels each figure of a stat strip", () => {
    render(<StatGrid label="Topic metrics" items={[{ label: "Retained messages", value: 3 }, { label: "Rate", value: "0.50/s", tone: "warn" }]} />);
    expect(screen.getByLabelText("Topic metrics")).toHaveTextContent("Retained messages3Rate0.50/s");
  });
});

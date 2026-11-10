import { describe, expect, it, vi } from "vitest";
import { render, renderHook, screen, act, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useCursorList } from "../hooks";
import { Pager } from "./Pager";

describe("Pager (pages)", () => {
  it("shows the range and total, and Previous and Next stop at the ends", async () => {
    const onPage = vi.fn();
    const { rerender } = render(<Pager page={1} perPage={25} total={60} onPage={onPage} label="Records pages" />);
    const nav = screen.getByRole("navigation", { name: "Records pages" });
    expect(nav).toHaveTextContent("1–25 of 60");
    expect(screen.getByRole("button", { name: "Previous" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "Next" }));
    expect(onPage).toHaveBeenCalledWith(2);
    rerender(<Pager page={3} perPage={25} total={60} onPage={onPage} label="Records pages" />);
    expect(nav).toHaveTextContent("51–60 of 60");
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
    await userEvent.click(screen.getByRole("button", { name: "Previous" }));
    expect(onPage).toHaveBeenLastCalledWith(2);
  });

  it("reads 'Page n' when the total is unknown and follows hasNext", () => {
    render(<Pager page={2} perPage={10} hasNext={false} onPage={() => {}} />);
    expect(screen.getByRole("navigation", { name: "Pages" })).toHaveTextContent("Page 2");
    expect(screen.getByRole("button", { name: "Next" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Previous" })).toBeEnabled();
  });

  it("says 0 of 0 for an empty table", () => {
    render(<Pager page={1} perPage={10} total={0} onPage={() => {}} />);
    expect(screen.getByRole("navigation")).toHaveTextContent("0–0 of 0");
  });
});

describe("Pager (load more)", () => {
  it("shows how many are shown and loads more until the end", async () => {
    const onMore = vi.fn();
    const { rerender } = render(<Pager mode="more" shown={20} hasMore onMore={onMore} noun="objects" />);
    expect(screen.getByRole("navigation", { name: "More results" })).toHaveTextContent("20 objects shown");
    await userEvent.click(screen.getByRole("button", { name: "Load more" }));
    expect(onMore).toHaveBeenCalled();
    rerender(<Pager mode="more" shown={25} hasMore={false} onMore={onMore} noun="objects" />);
    expect(screen.getByRole("navigation")).toHaveTextContent("25 objects shown, that is all");
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });
});

describe("useCursorList", () => {
  it("appends pages until the cursor runs out, and reload starts over", async () => {
    const pages: Record<string, { items: number[]; next?: string | null }> = { first: { items: [1, 2], next: "b" }, b: { items: [3], next: null } };
    const fetchPage = vi.fn(async (cursor: string | undefined) => pages[cursor ?? "first"]!);
    const { result } = renderHook(() => useCursorList(fetchPage, "nums"));
    await waitFor(() => expect(result.current.items).toEqual([1, 2]));
    expect(result.current.hasMore).toBe(true);
    act(() => result.current.loadMore());
    await waitFor(() => expect(result.current.items).toEqual([1, 2, 3]));
    expect(result.current.hasMore).toBe(false);
    act(() => result.current.reload());
    await waitFor(() => expect(result.current.items).toEqual([1, 2]));
  });
});

import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { API_KEY_PLACEHOLDER } from "../snippets";
import { UseFromCode, UseFromCodeButton } from "./UseFromCode";

describe("UseFromCode", () => {
  it("has JavaScript, Rust and curl tabs, each with the origin, the name and only the key placeholder", async () => {
    render(<UseFromCode kind="collection" name="notes" />);
    expect(screen.getByRole("region", { name: "Use from code" })).toBeInTheDocument();
    for (const lang of ["JavaScript", "Rust", "curl"]) {
      await userEvent.click(screen.getByRole("tab", { name: lang }));
      const code = await screen.findByLabelText(`${lang} snippet`);
      expect(code).toHaveTextContent(location.origin);
      expect(code).toHaveTextContent("notes");
      expect(code).toHaveTextContent(API_KEY_PLACEHOLDER);
    }
  });

  it("copies the snippet of the open tab", async () => {
    const writeText = vi.fn<(text: string) => Promise<void>>(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    render(<UseFromCode kind="bucket" name="avatars" />);
    await userEvent.click(screen.getByRole("tab", { name: "curl" }));
    await userEvent.click(await screen.findByRole("button", { name: "Copy curl snippet" }));
    expect(writeText).toHaveBeenCalledWith(expect.stringContaining("/api/v1/storage/buckets/avatars/objects"));
    expect(writeText.mock.calls[0]?.[0]).toContain(API_KEY_PLACEHOLDER);
  });

  it("opens in a side sheet from the header button", async () => {
    render(<UseFromCodeButton kind="topic" name="orders" />);
    await userEvent.click(screen.getByRole("button", { name: "Use from code" }));
    const dialog = await screen.findByRole("dialog");
    await waitFor(() => expect(dialog).toHaveTextContent("Snippets for orders"));
    expect(await screen.findByLabelText("JavaScript snippet")).toHaveTextContent('fb.queues.publish("orders"');
  });
});

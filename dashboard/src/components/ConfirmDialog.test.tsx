import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { ApiError } from "../api";
import { Button } from "./Button";
import { ConfirmDialog } from "./ConfirmDialog";
import { ConfirmButton } from "./ui";

function Harness({ onConfirm }: { onConfirm: () => void | Promise<unknown> }) {
  return (
    <ConfirmDialog
      trigger={<Button tone="danger">Delete bucket</Button>}
      title="Delete bucket avatars?"
      consequence="Every file in it is deleted."
      confirmLabel="Delete bucket"
      confirmText="avatars"
      onConfirm={onConfirm}
    />
  );
}

describe("ConfirmDialog", () => {
  it("keeps the confirm button disabled until the name is typed exactly", async () => {
    const onConfirm = vi.fn();
    render(<Harness onConfirm={onConfirm} />);
    await userEvent.click(screen.getByRole("button", { name: "Delete bucket" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete bucket avatars?" });
    expect(dialog).toHaveTextContent("Every file in it is deleted.");
    const confirm = screen.getAllByRole("button", { name: "Delete bucket" }).find((b) => dialog.contains(b))!;
    const input = screen.getByLabelText(/Type avatars to confirm/);
    expect(confirm).toBeDisabled();
    await userEvent.type(input, "avatar");
    expect(confirm).toBeDisabled();
    await userEvent.type(input, "S");
    expect(confirm).toBeDisabled();
    await userEvent.clear(input);
    await userEvent.type(input, "avatars");
    expect(confirm).toBeEnabled();
    await userEvent.click(confirm);
    expect(onConfirm).toHaveBeenCalledTimes(1);
    await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
  });

  it("stays open and shows the error when the action fails", async () => {
    const onConfirm = vi.fn(async () => {
      throw new ApiError({ status: 409, code: "bucket_not_empty", message: "bucket not empty", requestId: "r1" });
    });
    render(<Harness onConfirm={onConfirm} />);
    await userEvent.click(screen.getByRole("button", { name: "Delete bucket" }));
    await userEvent.type(await screen.findByLabelText(/Type avatars to confirm/), "avatars{Enter}");
    expect(await screen.findByText("The bucket still holds files")).toBeInTheDocument();
    expect(screen.getByRole("alertdialog")).toBeInTheDocument();
  });

  it("ConfirmButton keeps its one-line API and passes confirmText through", async () => {
    const onConfirm = vi.fn();
    render(<ConfirmButton label="Delete topic orders" confirmText="orders" consequence="Its messages are lost." onConfirm={onConfirm} />);
    await userEvent.click(screen.getByRole("button", { name: "Delete topic orders" }));
    const confirm = await screen.findByRole("button", { name: "Confirm delete topic orders" });
    expect(confirm).toBeDisabled();
    await userEvent.type(screen.getByLabelText(/Type orders to confirm/), "orders");
    await userEvent.click(confirm);
    expect(onConfirm).toHaveBeenCalledTimes(1);
  });
});

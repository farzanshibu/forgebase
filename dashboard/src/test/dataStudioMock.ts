/** Test helpers shared by the data studios' tests (FB-100): answering a ConfirmDialog the way a person does. */
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

/**
 * Answer the open ConfirmDialog (`alertdialog`) named `title`: type `typed` when it asks for the name, then press the
 * confirm button called `confirm`, and wait for the dialog to close.
 */
export async function confirmDialog(title: string | RegExp, confirm: string | RegExp, typed?: string): Promise<void> {
  const dialog = await screen.findByRole("alertdialog", { name: title });
  if (typed !== undefined) await userEvent.type(within(dialog).getByLabelText(/to confirm/), typed);
  await userEvent.click(within(dialog).getByRole("button", { name: confirm }));
  await waitFor(() => expect(screen.queryByRole("alertdialog")).not.toBeInTheDocument());
}

/** The side sheet (`form` named by its title) that CreateSheet opens. */
export const sheet = (title: string | RegExp) => screen.findByRole("form", { name: title });

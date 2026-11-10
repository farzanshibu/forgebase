import { screen, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";

/** Open a PanelUI select (found by its label, or passed in) and choose the option with this exact label. */
export async function pick(user: UserEvent, select: string | HTMLElement, option: string) {
  const trigger = typeof select === "string" ? await screen.findByLabelText(select) : select;
  await user.click(trigger);
  const list = await screen.findByRole("listbox");
  await user.click(within(list).getByRole("option", { name: option }));
}

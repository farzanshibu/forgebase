import { screen, within } from "@testing-library/react";
import type { UserEvent } from "@testing-library/user-event";

/** Open a PanelUI select (found by its label, or passed in) and choose the option whose name matches. */
export async function pick(user: Pick<UserEvent, "click">, select: string | HTMLElement, option: string | RegExp) {
  const trigger = typeof select === "string" ? await screen.findByLabelText(select) : select;
  await user.click(trigger);
  const list = await screen.findByRole("listbox");
  await user.click(await within(list).findByRole("option", { name: option }));
}

import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

/**
 * Choose an option of a PanelUI select the way a person does: open the trigger, then pick the option.
 * `option` matches the option's value (the way `selectOptions` did) or, failing that, its visible label.
 */
export async function pick(trigger: HTMLElement, option: string): Promise<void> {
  await userEvent.click(trigger);
  const listbox = await screen.findByRole("listbox");
  const options = within(listbox).getAllByRole("option");
  const hit = options.find((o) => o.getAttribute("data-value") === option) ?? options.find((o) => o.textContent?.trim() === option);
  if (!hit) throw new Error(`No option "${option}" in [${options.map((o) => `${o.getAttribute("data-value")}=${o.textContent?.trim()}`).join(", ")}]`);
  await userEvent.click(hit);
}

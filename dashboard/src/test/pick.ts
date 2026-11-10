import { screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

/**
 * Choose an option of a PanelUI select the way a person does: open the trigger, then pick the option.
 * `option` matches the option's value (the way `selectOptions` did) or, failing that, its visible label
 * (a string for an exact label, a RegExp for a pattern).
 */
export async function pick(trigger: HTMLElement, option: string | RegExp): Promise<void> {
  await userEvent.click(trigger);
  const listbox = await screen.findByRole("listbox");
  const options = within(listbox).getAllByRole("option");
  const label = (o: HTMLElement) => o.textContent?.trim() ?? "";
  const hit =
    options.find((o) => typeof option === "string" && o.getAttribute("data-value") === option) ??
    options.find((o) => (typeof option === "string" ? label(o) === option : option.test(label(o))));
  if (!hit) throw new Error(`No option ${String(option)} in [${options.map((o) => `${o.getAttribute("data-value")}=${label(o)}`).join(", ")}]`);
  await userEvent.click(hit);
}

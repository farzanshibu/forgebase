import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { PasswordInput, PasswordStrength } from "./PasswordInput";
import { passwordScore } from "./passwordScore";

describe("passwordScore", () => {
  it("is 0 when empty, 1 under the minimum, and grows with length and character mix", () => {
    expect(passwordScore("", 10)).toBe(0);
    expect(passwordScore("short", 10)).toBe(1);
    expect(passwordScore("abcdefghij", 10)).toBe(2);
    expect(passwordScore("abcdefghijklmnop", 10)).toBe(3);
    expect(passwordScore("forge ahead daily 2026", 10)).toBe(4);
  });
});

describe("PasswordInput", () => {
  it("shows and hides the password with a labelled toggle", async () => {
    render(
      <>
        <label htmlFor="pw">Password</label>
        <PasswordInput id="pw" defaultValue="secret-words" />
      </>,
    );
    const input = screen.getByLabelText("Password");
    expect(input).toHaveAttribute("type", "password");
    await userEvent.click(screen.getByRole("button", { name: "Show password" }));
    expect(input).toHaveAttribute("type", "text");
    expect(screen.getByRole("button", { name: "Hide password" })).toHaveAttribute("aria-pressed", "true");
  });

  it("says nothing about strength until the user types", () => {
    const { rerender } = render(<PasswordStrength value="" min={10} />);
    expect(screen.queryByText(/Too short|Fair|Good|Strong/)).not.toBeInTheDocument();
    rerender(<PasswordStrength value="abc" min={10} />);
    expect(screen.getByText("Too short")).toBeInTheDocument();
  });
});

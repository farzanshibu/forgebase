import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { useState } from "react";
import { validate } from "../validate";
import { FormField } from "./FormField";
import { HelpTip } from "./HelpTip";
import { GLOSSARY } from "../glossary";

function NameField() {
  const [v, setV] = useState("");
  const [touched, setTouched] = useState(false);
  return (
    <FormField label="Name" help="Letters, digits and underscores." required error={touched ? validate.collectionName(v) : undefined}>
      {(p) => <input {...p} value={v} onChange={(e) => { setV(e.target.value); setTouched(true); }} />}
    </FormField>
  );
}

describe("FormField", () => {
  it("labels the control and points aria-describedby at the error and the help", async () => {
    render(<NameField />);
    const input = screen.getByLabelText(/Name/);
    expect(input).toHaveAttribute("aria-required", "true");
    expect(input).not.toHaveAttribute("aria-invalid");
    expect(input).toHaveAccessibleDescription("Letters, digits and underscores.");
    await userEvent.type(input, "1x");
    const error = screen.getByText("Use letters, digits and underscores, starting with a letter.");
    expect(input).toHaveAttribute("aria-invalid", "true");
    expect(input.getAttribute("aria-describedby")!.split(" ")).toContain(error.id);
    expect(input).toHaveAccessibleDescription(/starting with a letter/);
    await userEvent.clear(input);
    await userEvent.type(input, "notes");
    expect(input).not.toHaveAttribute("aria-invalid");
  });

  it("adds a HelpTip for a glossary term", () => {
    render(<FormField label="TTL" term="ttl">{(p) => <input {...p} />}</FormField>);
    expect(screen.getByRole("button", { name: "What is TTL?" })).toHaveAccessibleDescription(GLOSSARY.ttl.text);
  });
});

describe("HelpTip", () => {
  it("is a keyboard-reachable button described by the glossary text", async () => {
    render(<HelpTip term="rls">Row-level security</HelpTip>);
    const btn = screen.getByRole("button", { name: "What is RLS?" });
    expect(btn).toHaveAccessibleDescription(GLOSSARY.rls.text);
    await userEvent.tab();
    expect(btn).toHaveFocus();
    expect(screen.getByText("Row-level security")).toBeInTheDocument();
  });
});

describe("validate.ts", () => {
  it("mirrors the server's name rules", () => {
    expect(validate.collectionName("notes_2")).toBeUndefined();
    expect(validate.collectionName("")).toMatch(/Enter a name/);
    expect(validate.collectionName("2notes")).toMatch(/starting with a letter/);
    expect(validate.collectionName("_forgebase_x")).toMatch(/reserved/);
    expect(validate.collectionName("a".repeat(64))).toMatch(/63/);
    expect(validate.bucketName("my-uploads.v2")).toBeUndefined();
    expect(validate.bucketName("Uploads")).toMatch(/lower-case/);
    expect(validate.functionName("hello-fn")).toBeUndefined();
    expect(validate.functionName("a".repeat(49))).toMatch(/48/);
    expect(validate.topicName("orders:eu-1")).toBeUndefined();
    expect(validate.namespaceName("cache")).toBeUndefined();
    expect(validate.json("{")).toMatch(/not valid JSON/);
    expect(validate.integer(1, 10)("11")).toMatch(/10 or less/);
    expect(validate.all(validate.required("a title"), validate.json)("")).toBe("Enter a title.");
  });
});

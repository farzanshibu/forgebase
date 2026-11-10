import { describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { ApiError } from "../api";
import { explain, fieldErrors, missingPermission } from "../errors";
import { ErrorPanel } from "./ErrorPanel";
import { ErrorView } from "./ui";

const api = (status: number, code: string, message: string, details?: unknown) =>
  new ApiError({ status, code, message, requestId: "req-42", body: { error: { code, message, request_id: "req-42", ...(details ? { details } : {}) } } });

describe("errors.ts", () => {
  it("maps a known code to plain words and falls back to the server message", () => {
    expect(explain(api(400, "invalid_name", 'invalid name ""'))).toMatchObject({ title: "That name is not allowed", next: expect.stringMatching(/letters, digits and underscores/) });
    expect(explain(api(400, "weird_thing", "the widget is sideways")).title).toBe("the widget is sideways");
    expect(explain(new Error("boom")).title).toBe("boom");
  });

  it("words 401 and 403 differently, and 403 names the permission without saying sign in", () => {
    const s = explain(api(401, "unauthenticated", "authentication required"));
    const f = explain(api(403, "forbidden", "missing permission collections:write"));
    expect(s.title).toBe("Your session ended");
    expect(f.title).toBe("You do not have permission to do this");
    expect(f.next).toContain("collections:write");
    expect(`${f.title} ${f.next}`).not.toMatch(/sign in|sign-in/i);
    expect(missingPermission(api(403, "forbidden", "missing permission records:read"))).toBe("records:read");
  });

  it("reads per-field errors for FormField", () => {
    const e = api(400, "validation_failed", "validation failed", { fields: { title: { code: "required", message: "" }, price: { code: "min", message: "must be at least 0" } } });
    expect(fieldErrors(e)).toEqual({ title: "This field is required", price: "must be at least 0" });
    expect(fieldErrors(new Error("x"))).toEqual({});
  });
});

describe("ErrorPanel", () => {
  it("shows the plain title, the next step and the request id; Details hides the raw JSON until opened", async () => {
    render(<ErrorPanel error={api(400, "invalid_name", 'invalid name "": use letters')} />);
    const alert = screen.getByRole("alert");
    expect(alert).toHaveTextContent("That name is not allowed");
    expect(alert).toHaveTextContent(/What to do:/);
    expect(alert).toHaveTextContent("Request id req-42");
    expect(alert).toHaveTextContent("Code invalid_name");
    expect(alert).toHaveTextContent('Server message: invalid name "": use letters');
    expect(screen.queryByText(/"request_id": "req-42"/)).not.toBeInTheDocument();
    await userEvent.click(screen.getByText("Details"));
    expect(await screen.findByText(/"request_id": "req-42"/)).toBeVisible();
  });

  it("copies the request id", async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: { writeText } });
    render(<ErrorPanel error={api(500, "internal", "internal server error")} />);
    await userEvent.click(screen.getByRole("button", { name: "Copy request id" }));
    expect(writeText).toHaveBeenCalledWith("req-42");
    await waitFor(() => expect(screen.getByRole("button", { name: "Copied" })).toBeInTheDocument());
  });

  it("words 401 and 403 differently on screen", () => {
    const { unmount } = render(<ErrorPanel error={api(401, "unauthenticated", "authentication required")} />);
    expect(screen.getByRole("alert")).toHaveTextContent("Your session ended");
    unmount();
    render(<ErrorPanel error={api(403, "forbidden", "missing permission storage:write")} />);
    expect(screen.getByRole("alert")).toHaveTextContent("You do not have permission to do this");
    expect(screen.getByRole("alert")).toHaveTextContent("storage:write");
    expect(screen.getByRole("alert")).not.toHaveTextContent(/sign.in/i);
  });

  it("lists field errors unless the form shows them, and offers a retry", async () => {
    const e = api(400, "validation_failed", "validation failed", { fields: { title: { code: "required", message: "is required" } } });
    const retry = vi.fn();
    const { rerender } = render(<ErrorPanel error={e} onRetry={retry} />);
    expect(screen.getByRole("list", { name: "Fields to fix" })).toHaveTextContent("title: is required");
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(retry).toHaveBeenCalled();
    rerender(<ErrorPanel error={e} fields="hide" />);
    expect(screen.queryByRole("list", { name: "Fields to fix" })).not.toBeInTheDocument();
  });

  it("shows a local error as its message alone", () => {
    render(<ErrorPanel error={new Error("A vector is a list of finite numbers")} />);
    expect(screen.getByRole("alert")).toHaveTextContent(/^A vector is a list of finite numbers$/);
  });

  it("renders nothing without an error, and ErrorView is the same component", () => {
    const { container } = render(<ErrorPanel error={undefined} />);
    expect(container).toBeEmptyDOMElement();
    expect(ErrorView).toBe(ErrorPanel);
  });

  it("points a switched-off feature at Features & settings", () => {
    render(
      <MemoryRouter>
        <ErrorPanel error={api(404, "feature_disabled", "Storage is switched off")} />
      </MemoryRouter>,
    );
    expect(screen.getByRole("region", { name: "Feature disabled" })).toHaveTextContent("Storage is switched off");
    expect(screen.getByRole("link", { name: /Features & settings/ })).toHaveAttribute("href", "/system/features");
  });
});

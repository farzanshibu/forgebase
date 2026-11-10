import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installFunctions } from "../../test/fnMock";
import { pick } from "../../test/panelSelect";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installFunctions(mock) };
}

const wasm = () => new File([new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0])], "mod.wasm", { type: "application/wasm" });

describe("Functions Studio list and deploy", () => {
  it("lists functions, deploys a new one from a .wasm upload and deletes it", async () => {
    const { mock, st } = setup();
    renderStudio("/functions");
    expect(await screen.findByRole("link", { name: "hello" })).toBeInTheDocument();
    expect(within(screen.getByRole("row", { name: /hello/ })).getAllByText("v2")).toHaveLength(2);

    await userEvent.click(screen.getByRole("button", { name: "Deploy function" }));
    const form = screen.getByRole("form", { name: "Deploy function" });
    await userEvent.type(within(form).getByLabelText("Name"), "greeter");
    await userEvent.upload(within(form).getByLabelText("WebAssembly module"), wasm());
    await userEvent.click(within(form).getByLabelText("log"));
    await userEvent.type(within(form).getByLabelText("Timeout ms"), "2500");
    await userEvent.type(within(form).getByLabelText(/^Environment/), "MODE=prod");
    await userEvent.type(within(form).getByLabelText(/^Record hooks/), "orders: after_create, after_update");
    await userEvent.click(within(form).getByRole("button", { name: "Deploy" }));

    // Lands on the new function's page.
    expect(await screen.findByText("greeter", { selector: "h2" })).toBeInTheDocument();
    const body = mock.bodies["post /api/v1/functions"]![0] as Record<string, unknown>;
    expect(body).toMatchObject({
      name: "greeter", wasm: "AGFzbQEAAAA=", access: "permission", capabilities: ["log"], limits: { timeout_ms: 2500 },
      env: { MODE: "prod" }, hooks: [{ collection: "orders", events: ["after_create", "after_update"] }], activate: true,
    });
    expect(st.functions.map((f) => f.name)).toEqual(["hello", "greeter"]);
    expect(mock.problems).toEqual([]);
  });

  it("asks for a file, rejects a bad hook line before calling the API, and shows the server's error", async () => {
    const { mock } = setup();
    renderStudio("/functions");
    await screen.findByRole("link", { name: "hello" });
    await userEvent.click(screen.getByRole("button", { name: "Deploy function" }));
    const form = screen.getByRole("form", { name: "Deploy function" });
    await userEvent.type(within(form).getByLabelText("Name"), "greeter");
    await userEvent.click(within(form).getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("Choose a .wasm file to upload")).toBeInTheDocument();

    await userEvent.upload(within(form).getByLabelText("WebAssembly module"), wasm());
    await userEvent.type(within(form).getByLabelText(/^Record hooks/), "orders: sometimes");
    await userEvent.click(within(form).getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText(/unknown event sometimes/)).toBeInTheDocument();
    expect(mock.calls.filter((c) => c === "post /api/v1/functions")).toHaveLength(0);
  });

  it("deletes a function after confirmation", async () => {
    const { mock } = setup();
    renderStudio("/functions");
    await screen.findByRole("link", { name: "hello" });
    await userEvent.click(screen.getByRole("button", { name: "Delete hello" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete hello" }));
    await waitFor(() => expect(screen.queryByRole("link", { name: "hello" })).not.toBeInTheDocument());
    expect(screen.getByText("No functions yet.")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });
});

describe("Functions Studio versions", () => {
  it("shows history, activates an older version and deploys a new one prefilled from the active config", async () => {
    const { mock, st } = setup();
    renderStudio("/functions/f/hello");
    expect(await screen.findByText(/active v2 of 2/)).toBeInTheDocument();
    const rows = await screen.findAllByRole("row");
    expect(within(rows[1]!).getByText("active")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Activate v1" }));
    expect(await screen.findByText("Active version is now v1")).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/functions/{name}/rollback"]![0]).toEqual({ version: 1 });
    expect(st.functions[0]!.active_version).toBe(1);

    await userEvent.click(screen.getByRole("button", { name: "Roll back to previous" }));
    // Nothing older than v1.
    expect(await screen.findByText(/no earlier version/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Deploy new version" }));
    const form = screen.getByRole("form", { name: "Deploy new version" });
    expect(within(form).getByLabelText("Name")).toHaveAttribute("readonly");
    expect(within(form).getByLabelText(/^Environment/)).toHaveValue("MODE=x");
    await userEvent.upload(within(form).getByLabelText("WebAssembly module"), wasm());
    await userEvent.click(within(form).getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("Deployed v3 (active)")).toBeInTheDocument();
    expect(st.versions.hello![0]!.version).toBe(3);
    expect(mock.problems).toEqual([]);
  });
});

describe("Functions Studio logs", () => {
  it("shows executions with log lines and errors, filters by status and pages older ones", async () => {
    const { mock, st } = setup();
    st.logs.hello = Array.from({ length: 60 }, (_, i) => ({ id: 60 - i, function: "hello", version: 2, trigger: "http", status: i === 0 ? "trap" : "ok", duration_ms: 1, fuel_used: 1, error: i === 0 ? "boom" : null, logs: i === 1 ? [{ level: "info", message: "hi there" }] : [], request_id: null, created_at: "2026-11-09T10:00:00Z" }));
    renderStudio("/functions/f/hello/logs");
    const list = await screen.findByRole("list", { name: "Executions" });
    expect(await within(list).findAllByRole("listitem")).toHaveLength(50);
    expect(within(list).getByText("boom")).toBeInTheDocument();
    expect(within(list).getByText("[info] hi there")).toBeInTheDocument();

    await pick(userEvent, screen.getByLabelText("Status"), "trap");
    expect(within(list).getAllByRole("listitem")).toHaveLength(1);
    await pick(userEvent, screen.getByLabelText("Status"), "all");

    await userEvent.click(screen.getByRole("button", { name: "Load older" }));
    await waitFor(() => expect(within(screen.getByRole("list", { name: "Executions" })).getAllByRole("listitem")).toHaveLength(60));
    expect(mock.calls).toContain("get /api/v1/functions/hello/logs");
    expect(screen.queryByRole("button", { name: "Load older" })).not.toBeInTheDocument();
  });

  it("polls while Live is on", async () => {
    const { mock } = setup();
    renderStudio("/functions/f/hello/logs");
    await within(await screen.findByRole("list", { name: "Executions" })).findAllByRole("listitem");
    const before = mock.calls.filter((c) => c.endsWith("/logs")).length;
    await userEvent.click(screen.getByRole("button", { name: "Refresh" }));
    await waitFor(() => expect(mock.calls.filter((c) => c.endsWith("/logs")).length).toBeGreaterThan(before));
  });
});

describe("Functions Studio secrets", () => {
  it("lists names only, sets a secret without echoing the value, and deletes one", async () => {
    const { mock, st } = setup();
    renderStudio("/functions/f/hello/secrets");
    expect(await screen.findByText("API_KEY")).toBeInTheDocument();

    const form = screen.getByRole("form", { name: "Set secret" });
    expect(within(form).getByLabelText("Value")).toHaveAttribute("type", "password");
    await userEvent.type(within(form).getByLabelText("Name"), "DB_PASS");
    await userEvent.type(within(form).getByLabelText("Value"), "hunter2");
    await userEvent.click(within(form).getByRole("button", { name: "Save secret" }));
    expect(await screen.findByText("DB_PASS")).toBeInTheDocument();
    expect(st.secretValues["hello/DB_PASS"]).toBe("hunter2");
    // The value is cleared and never rendered.
    expect(within(form).getByLabelText("Value")).toHaveValue("");
    expect(document.body.textContent).not.toContain("hunter2");

    await userEvent.click(screen.getByRole("button", { name: "Delete API_KEY" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete api_key" }));
    await waitFor(() => expect(screen.queryByText("API_KEY")).not.toBeInTheDocument());
    expect(mock.problems).toEqual([]);
  });
});

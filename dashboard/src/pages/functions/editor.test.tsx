import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { addJsFunction, installFunctions, SYNTAX_ERROR } from "../../test/fnMock";
import { monacoState, setEditorCode } from "../../test/monacoMock";
import { pick } from "../../test/pick";
import { TEMPLATES } from "./templates";
import { FALLBACK_FORGEBASE_DTS } from "./fallbackTypes";

vi.mock("./monacoEnv", async () => (await import("../../test/monacoMock")).monacoEnvMock());

beforeEach(() => monacoState.reset());
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  const st = installFunctions(mock);
  addJsFunction(st);
  return { mock, st };
}

const editor = (name: string) => screen.findByRole("textbox", { name: `${name} source` });
const wasm = () => new File([new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0])], "mod.wasm", { type: "application/wasm" });

async function openNew(name: string, template?: string, language?: string) {
  renderStudio("/functions/new");
  const form = await screen.findByRole("form", { name: "New function" });
  await userEvent.type(within(form).getByLabelText("Name"), name);
  if (template) await pick(within(form).getByLabelText("Template"), template);
  if (language) await pick(within(form).getByLabelText("Language"), language);
  await userEvent.click(within(form).getByRole("button", { name: "Open editor" }));
  return form;
}

describe("New function", () => {
  it.each(TEMPLATES)("starts from the $label template in TypeScript, deploys it and opens the function", async (t) => {
    const { mock, st } = setup();
    await openNew("fresh", t.id);
    const ta = await editor("fresh");
    expect(ta).toHaveValue(t.source.typescript);
    expect(t.source.typescript).toContain('from "forgebase"');
    for (const c of t.capabilities) expect(screen.getByRole("switch", { name: c })).toBeChecked();
    expect(screen.getByRole("switch", { name: "service_role" })).not.toBeChecked();

    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("fresh", { selector: "h2" })).toBeInTheDocument();
    const body = mock.bodies["post /api/v1/functions"]![0] as Record<string, unknown>;
    expect(body).toMatchObject({ name: "fresh", language: "typescript", source: t.source.typescript, capabilities: t.capabilities, access: "permission", activate: true });
    expect(body.wasm).toBeUndefined();
    if (t.hooks.length) expect(body.hooks).toEqual(t.hooks);
    expect(st.versions.fresh![0]).toMatchObject({ runtime: "js", language: "typescript" });
    expect(mock.problems).toEqual([]);
  });

  it("offers JavaScript, which strips the type annotations", async () => {
    const { mock } = setup();
    await openNew("plain", "http", "javascript");
    const ta = await editor("plain");
    expect(ta).toHaveValue(TEMPLATES[0]!.source.javascript);
    expect((ta as HTMLTextAreaElement).value).not.toContain(": Request");
    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    await screen.findByText("plain", { selector: "h2" });
    expect(mock.bodies["post /api/v1/functions"]![0]).toMatchObject({ language: "javascript" });
  });

  it("marks service_role as admin-only and sends it when switched on; the server's refusal is shown", async () => {
    const { mock, st } = setup();
    st.admin = false;
    await openNew("power");
    await editor("power");
    expect(screen.getByText("admin only")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("switch", { name: "service_role" }));
    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText(/service_role needs an administrator/)).toBeInTheDocument();
    expect((mock.bodies["post /api/v1/functions"]![0] as { capabilities: string[] }).capabilities).toContain("service_role");
    expect(st.functions.map((f) => f.name)).not.toContain("power");
  });

  it("rejects a bad name before opening the editor", async () => {
    setup();
    renderStudio("/functions/new");
    const form = await screen.findByRole("form", { name: "New function" });
    await userEvent.type(within(form).getByLabelText("Name"), "Bad Name");
    await userEvent.click(within(form).getByRole("button", { name: "Open editor" }));
    expect(await screen.findByText(/Name: lower-case letters/)).toBeInTheDocument();
    expect(screen.queryByRole("textbox", { name: /source$/ })).not.toBeInTheDocument();
  });

  it("still uploads a WebAssembly module", async () => {
    const { mock, st } = setup();
    renderStudio("/functions/new");
    const form = await screen.findByRole("form", { name: "New function" });
    await pick(within(form).getByLabelText("Language"), "wasm");
    const up = await screen.findByRole("form", { name: "Deploy function" });
    await userEvent.type(within(up).getByLabelText("Name"), "modfn");
    await userEvent.upload(within(up).getByLabelText("WebAssembly module"), wasm());
    await userEvent.click(within(up).getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("modfn", { selector: "h2" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/functions"]![0]).toMatchObject({ name: "modfn", wasm: "AGFzbQEAAAA=" });
    expect(st.versions.modfn![0]).toMatchObject({ runtime: "wasm" });
  });

  it("starts from New function on the list", async () => {
    setup();
    renderStudio("/functions");
    await userEvent.click(await screen.findByRole("button", { name: "New function" }));
    expect(await screen.findByRole("form", { name: "New function" })).toBeInTheDocument();
  });
});

describe("Editor", () => {
  it("gives the editor the server's forgebase types, or a bundled copy when the call fails", async () => {
    const { mock, st } = setup();
    await openNew("typed");
    await editor("typed");
    await waitFor(() => expect(monacoState.extraLib).toBe(st.types));
    expect(screen.getByText(/Types: from the server/)).toBeInTheDocument();
    expect(mock.calls).toContain("get /api/v1/functions/types");
  });

  it("falls back to the bundled types", async () => {
    const { st } = setup();
    st.types = null;
    await openNew("typed");
    await editor("typed");
    await waitFor(() => expect(monacoState.extraLib).toBe(FALLBACK_FORGEBASE_DTS));
    expect(FALLBACK_FORGEBASE_DTS).toContain("export function serve");
    expect(screen.getByText(/Types: bundled copy/)).toBeInTheDocument();
  });

  it("follows the light and dark theme", async () => {
    setup();
    localStorage.setItem("forgebase.admin.theme", "dark");
    await openNew("dusk");
    await editor("dusk");
    await waitFor(() => expect(monacoState.theme).toBe("vs-dark"));
  });

  it("maps a compile error to its line and column and keeps the editor open", async () => {
    const { mock, st } = setup();
    await openNew("broken");
    const ta = await editor("broken");
    act(() => setEditorCode(ta, `const a = 1;\nconst b = ${SYNTAX_ERROR};\n`));
    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    const list = await screen.findByRole("alert", { name: "Compile errors" });
    expect(within(list).getByText("Line 2, column 7")).toBeInTheDocument();
    expect(monacoState.markers).toEqual([expect.objectContaining({ startLineNumber: 2, startColumn: 7, severity: 8, message: expect.stringContaining("unexpected token") })]);
    expect(ta).toBeInTheDocument();
    expect(st.functions.map((f) => f.name)).not.toContain("broken");

    // Fixing the code and deploying again clears the markers.
    act(() => setEditorCode(ta, "serve(() => new Response('ok'));\n"));
    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("broken", { selector: "h2" })).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("saves a draft with Ctrl+S and restores it after a reload; Discard goes back to the template", async () => {
    setup();
    const first = await openNew("drafty");
    void first;
    const ta = await editor("drafty");
    act(() => setEditorCode(ta, "// my work in progress\n"));
    await userEvent.click(ta);
    await userEvent.keyboard("{Control>}s{/Control}");
    expect(await screen.findByText("Draft saved in this browser")).toBeInTheDocument();
    expect(JSON.parse(localStorage.getItem("forgebase.fn-draft.new:drafty")!).source).toBe("// my work in progress\n");

    // "Reload": a fresh render of the same screen.
    document.body.innerHTML = "";
    const again = await openNew("drafty");
    void again;
    const ta2 = await editor("drafty");
    expect(ta2).toHaveValue("// my work in progress\n");
    expect(screen.getByText(/Restored your unsaved draft/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Discard draft" }));
    await waitFor(() => expect(ta2).toHaveValue(TEMPLATES[0]!.source.typescript));
    expect(localStorage.getItem("forgebase.fn-draft.new:drafty")).toBeNull();
  });

  it("Cmd+S also saves, and a failing storage is reported instead of throwing", async () => {
    setup();
    await openNew("nostore");
    const ta = await editor("nostore");
    vi.spyOn(Storage.prototype, "setItem").mockImplementation(() => { throw new Error("quota"); });
    await userEvent.click(ta);
    await userEvent.keyboard("{Meta>}s{/Meta}");
    expect(await screen.findByText(/Could not save the draft/)).toBeInTheDocument();
  });

  it("edits an existing function: loads the active source and deploys a new version keeping its configuration", async () => {
    const { mock, st } = setup();
    renderStudio("/functions/f/echo/code");
    const ta = await editor("echo");
    expect(ta).toHaveValue('serve(async () => new Response("two"));\n');
    expect(screen.getByRole("switch", { name: "kv" })).toBeChecked();
    act(() => setEditorCode(ta, 'serve(async () => new Response("three"));\n'));
    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("Deployed v3 (active)")).toBeInTheDocument();
    const body = mock.bodies["post /api/v1/functions"]![0] as Record<string, unknown>;
    expect(body).toMatchObject({ name: "echo", language: "typescript", capabilities: ["log", "kv"], env: { MODE: "x" }, limits: { timeout_ms: 5000 } });
    expect(st.sources.echo![3]!.source).toContain("three");
  });

  it("explains that a WebAssembly function has no source to edit", async () => {
    setup();
    renderStudio("/functions/f/hello/code");
    expect(await screen.findByText(/WebAssembly module, which has no source to edit/)).toBeInTheDocument();
  });
});

describe("Test panel", () => {
  it("sends the request and shows status, headers, body and the execution's logs", async () => {
    const { mock, st } = setup();
    renderStudio("/functions/f/echo/test");
    const form = await screen.findByRole("form", { name: "Test run" });
    await pick(within(form).getByLabelText("Method"), "PUT");
    await userEvent.type(within(form).getByLabelText("Path"), "/items");
    await userEvent.type(within(form).getByLabelText("Query"), "name=Ada&x=1");
    await userEvent.type(within(form).getByLabelText(/^Headers/), "X-Test: yes");
    await userEvent.click(within(form).getByLabelText("JSON body"));
    await userEvent.paste('{"n": 1}');
    await userEvent.click(within(form).getByRole("button", { name: "Send test request" }));

    const result = await screen.findByRole("group", { name: "Test result" });
    expect(within(result).getByText("HTTP 200")).toBeInTheDocument();
    expect(within(result).getByLabelText("Response headers")).toHaveTextContent("content-type: application/json");
    expect(within(result).getByLabelText("Response body")).toHaveTextContent('"hello": "world"');
    expect(await within(result).findByText("[info] echo ran")).toBeInTheDocument();
    const call = st.invokes[0]!;
    expect(call).toMatchObject({ method: "PUT", path: "/api/v1/functions/echo/invoke", body: { n: 1 } });
    expect(new URLSearchParams(call.query).get("name")).toBe("Ada");
    expect(call.headers).toMatchObject({ "X-Test": "yes", "Content-Type": "application/json" });
    expect(new URLSearchParams(call.query).has("version")).toBe(false);
    expect(mock.problems).toEqual([]);
  });

  it("targets a chosen version, and shows non-200 answers as they are", async () => {
    const { st } = setup();
    st.invokeReply = { status: 502, headers: { "x-request-id": "none" }, body: { error: { code: "function_trap", message: "boom" } } };
    renderStudio("/functions/f/echo/test");
    const form = await screen.findByRole("form", { name: "Test run" });
    await pick(within(form).getByLabelText("Version"), "1");
    await userEvent.click(within(form).getByRole("button", { name: "Send test request" }));
    expect(await screen.findByText("HTTP 502")).toBeInTheDocument();
    expect(new URLSearchParams(st.invokes[0]!.query).get("version")).toBe("1");
    expect(screen.getByLabelText("Response body")).toHaveTextContent("function_trap");
    expect(await screen.findByText("No log lines for this run.")).toBeInTheDocument();
  });

  it("refuses a body that is not JSON without calling the server", async () => {
    const { st } = setup();
    renderStudio("/functions/f/echo/test");
    const form = await screen.findByRole("form", { name: "Test run" });
    await userEvent.click(within(form).getByLabelText("JSON body"));
    await userEvent.paste("{oops");
    await userEvent.click(within(form).getByRole("button", { name: "Send test request" }));
    expect(await screen.findByText("The body is not valid JSON")).toBeInTheDocument();
    expect(st.invokes).toHaveLength(0);
  });
});

describe("Versions", () => {
  it("lists runtime and language, opens a JS version read-only and diffs it against the current one", async () => {
    setup();
    renderStudio("/functions/f/echo");
    expect(await screen.findByText(/active v2 of 2/)).toBeInTheDocument();
    const rows = await screen.findAllByRole("row");
    expect(within(rows[1]!).getByText("js")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("typescript")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "View source v1" }));
    const ta = await screen.findByRole("textbox", { name: "v1 source" });
    expect(ta).toHaveValue('serve(async () => new Response("one"));\n');
    expect(ta).toHaveAttribute("readonly");

    await userEvent.click(screen.getByRole("button", { name: /^Diff against current/ }));
    const diff = await screen.findByTestId("diff");
    expect(diff.querySelector('[data-side="original"]')).toHaveTextContent('Response("two")');
    expect(diff.querySelector('[data-side="modified"]')).toHaveTextContent('Response("one")');

    // The existing rollback keeps working on a JS function.
    await userEvent.click(screen.getByRole("button", { name: "Activate v1" }));
    expect(await screen.findByText("Active version is now v1")).toBeInTheDocument();
  });

  it("shows WebAssembly versions as wasm with no source button", async () => {
    setup();
    renderStudio("/functions/f/hello");
    expect(await screen.findByText(/active v2 of 2/)).toBeInTheDocument();
    const rows = await screen.findAllByRole("row");
    expect(within(rows[1]!).getByText("wasm")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /View source/ })).not.toBeInTheDocument();
  });
});

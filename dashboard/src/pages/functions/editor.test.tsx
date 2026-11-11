import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, makeCollection } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { addJsFunction, installFunctions, SYNTAX_ERROR } from "../../test/fnMock";
import { monacoState, setEditorCode } from "../../test/monacoMock";
import { pick } from "../../test/pick";
import { TEMPLATES, hooksFor, template } from "./templates";
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

const newEditor = () => screen.findByRole("textbox", { name: "New function source" });
const editor = (name: string) => screen.findByRole("textbox", { name: `${name} source` });
const wasm = () => new File([new Uint8Array([0, 0x61, 0x73, 0x6d, 1, 0, 0, 0])], "mod.wasm", { type: "application/wasm" });

/** Opens New function (one page), picks a template and a language, and names the function. */
async function openNew(name: string, template?: string, language?: string) {
  renderStudio("/functions/new");
  const form = await screen.findByRole("form", { name: "Function settings" });
  const nameBox = within(form).getByLabelText(/^Name/);
  await userEvent.clear(nameBox);
  await userEvent.type(nameBox, name);
  if (template) await userEvent.click(screen.getByRole("button", { name: new RegExp(`^${template}`) }));
  if (language) await pick(within(form).getByLabelText("Language"), language);
  return form;
}

describe("New function", () => {
  it.each(TEMPLATES)("starts from the $label template in TypeScript, deploys it and opens the Test tab", async (t) => {
    const { mock, st } = setup();
    const form = await openNew("fresh", t.label);
    if (t.needsCollection) await userEvent.type(within(form).getByLabelText("Collection"), "orders");
    const collection = t.needsCollection ? "orders" : "";
    const source = t.source(collection || "your_collection");
    const ta = await newEditor();
    await waitFor(() => expect(ta).toHaveValue(source.typescript));
    expect(source.typescript).toContain('from "forgebase"');
    for (const c of t.capabilities) expect(screen.getByRole("switch", { name: c })).toBeChecked();
    expect(screen.getByRole("switch", { name: "service_role" })).not.toBeChecked();

    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    // A deploy toasts and opens the function's Test tab.
    expect(await screen.findByRole("form", { name: "Test run" })).toBeInTheDocument();
    expect(await screen.findByText("fresh", { selector: "h1" })).toBeInTheDocument();
    const body = mock.bodies["post /api/v1/functions"]![0] as Record<string, unknown>;
    expect(body).toMatchObject({ name: "fresh", language: "typescript", source: source.typescript, capabilities: t.capabilities, access: "permission", activate: true });
    expect(body.wasm).toBeUndefined();
    expect(body.hooks).toEqual(hooksFor(t, collection));
    expect(st.versions.fresh![0]).toMatchObject({ runtime: "js", language: "typescript" });
    expect(mock.problems).toEqual([]);
  });

  it("lists a db.read template, with a collection picker over the real collections", () => {
    const read = TEMPLATES.find((t) => t.label === "Read a collection")!;
    expect(read.capabilities).toContain("db.read");
    expect(read.needsCollection).toBe(true);
    expect(read.source("orders").typescript).toContain('ctx.db.from("orders")');
    // No template assumes a collection name: the webhook and the hook take the chosen one.
    for (const t of TEMPLATES.filter((x) => x.needsCollection)) expect(t.source("books").typescript).not.toContain("orders");
    expect(TEMPLATES.find((t) => t.id === "hook")!.source("books").typescript).toContain("books");
  });

  it("picks the newest collection by default and generates the code for the one chosen", async () => {
    const mock = signedIn(new MockApi({ collections: [makeCollection("alpha", []), makeCollection("zeta", [])] }).install());
    addJsFunction(installFunctions(mock));
    renderStudio("/functions/new");
    const ta = await newEditor();
    expect((ta as HTMLTextAreaElement).value).toContain('from("zeta")');
    expect(screen.getByLabelText("Collection")).toHaveTextContent("zeta");
    await pick(screen.getByLabelText("Collection"), "alpha");
    await waitFor(() => expect((screen.getByRole("textbox", { name: "New function source" }) as HTMLTextAreaElement).value).toContain('from("alpha")'));
  });

  it("offers JavaScript, which strips the type annotations", async () => {
    const { mock } = setup();
    await openNew("plain", "HTTP API", "JavaScript");
    const ta = await newEditor();
    await waitFor(() => expect(ta).toHaveValue(template("http").source("").javascript));
    expect((ta as HTMLTextAreaElement).value).not.toContain(": Request");
    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    await screen.findByRole("form", { name: "Test run" });
    expect(mock.bodies["post /api/v1/functions"]![0]).toMatchObject({ language: "javascript" });
  });

  it("marks service_role as admin-only and sends it when switched on; the server's refusal is shown", async () => {
    const { mock, st } = setup();
    st.admin = false;
    await openNew("power");
    await newEditor();
    expect(screen.getByText("admin only")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("switch", { name: "service_role" }));
    await userEvent.click(screen.getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("You do not have permission to do this")).toBeInTheDocument();
    expect((mock.bodies["post /api/v1/functions"]![0] as { capabilities: string[] }).capabilities).toContain("service_role");
    expect(st.functions.map((f) => f.name)).not.toContain("power");
  });

  it("checks the name as you type and does not offer Deploy for a bad one", async () => {
    setup();
    await openNew("Bad Name");
    expect(await screen.findByText(/Use lower-case letters, digits, dashes or underscores/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Deploy" })).toBeDisabled();
  });

  it("suggests a valid name from the template and the collection", async () => {
    setup();
    renderStudio("/functions/new");
    const form = await screen.findByRole("form", { name: "Function settings" });
    await waitFor(() => expect(within(form).getByLabelText(/^Name/)).toHaveValue("read"));
    await userEvent.type(within(form).getByLabelText("Collection"), "Order Items");
    await waitFor(() => expect(within(form).getByLabelText(/^Name/)).toHaveValue("read-order-items"));
  });

  it("still uploads a WebAssembly module, from Upload .wasm on the list", async () => {
    const { mock, st } = setup();
    renderStudio("/functions");
    await userEvent.click(await screen.findByRole("button", { name: "Upload .wasm" }));
    const up = await screen.findByRole("form", { name: "Deploy function" });
    await userEvent.type(within(up).getByLabelText("Name"), "modfn");
    await userEvent.upload(within(up).getByLabelText("WebAssembly module"), wasm());
    await userEvent.click(within(up).getByRole("button", { name: "Deploy" }));
    expect(await screen.findByText("modfn", { selector: "h1" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/functions"]![0]).toMatchObject({ name: "modfn", wasm: "AGFzbQEAAAA=" });
    expect(st.versions.modfn![0]).toMatchObject({ runtime: "wasm" });
  });

  it("starts from New function on the list", async () => {
    setup();
    renderStudio("/functions");
    await userEvent.click(await screen.findByRole("button", { name: "New function" }));
    expect(await screen.findByRole("form", { name: "Function settings" })).toBeInTheDocument();
  });
});

describe("Editor", () => {
  it("gives the editor the server's forgebase types, or a bundled copy when the call fails", async () => {
    const { mock, st } = setup();
    await openNew("typed");
    await newEditor();
    await waitFor(() => expect(monacoState.extraLib).toBe(st.types));
    expect(screen.getByText(/Types: from the server/)).toBeInTheDocument();
    expect(mock.calls).toContain("get /api/v1/functions/types");
  });

  it("falls back to the bundled types", async () => {
    const { st } = setup();
    st.types = null;
    await openNew("typed");
    await newEditor();
    await waitFor(() => expect(monacoState.extraLib).toBe(FALLBACK_FORGEBASE_DTS));
    expect(FALLBACK_FORGEBASE_DTS).toContain("export function serve");
    expect(screen.getByText(/Types: bundled copy/)).toBeInTheDocument();
  });

  it("follows the light and dark theme", async () => {
    setup();
    localStorage.setItem("forgebase.admin.theme", "dark");
    await openNew("dusk");
    await newEditor();
    await waitFor(() => expect(monacoState.theme).toBe("vs-dark"));
  });

  it("maps a compile error to its line and column and keeps the editor open", async () => {
    const { mock, st } = setup();
    await openNew("broken");
    const ta = await newEditor();
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
    expect(await screen.findByRole("form", { name: "Test run" })).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("saves a draft with Ctrl+S and restores it after a reload; Discard goes back to the template", async () => {
    setup();
    const first = await openNew("drafty");
    void first;
    const ta = await newEditor();
    act(() => setEditorCode(ta, "// my work in progress\n"));
    await userEvent.click(ta);
    await userEvent.keyboard("{Control>}s{/Control}");
    expect(await screen.findByText("Draft saved in this browser")).toBeInTheDocument();
    expect(JSON.parse(localStorage.getItem("forgebase.fn-draft.new:read:typescript:")!).source).toBe("// my work in progress\n");

    // "Reload": a fresh render of the same screen.
    cleanup();
    const again = await openNew("drafty");
    void again;
    const ta2 = await newEditor();
    expect(ta2).toHaveValue("// my work in progress\n");
    expect(screen.getByText(/Restored your unsaved draft/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Discard draft" }));
    await waitFor(() => expect(ta2).toHaveValue(TEMPLATES[0]!.source("your_collection").typescript));
    expect(localStorage.getItem("forgebase.fn-draft.new:read:typescript:")).toBeNull();
  });

  it("Cmd+S also saves, and a failing storage is reported instead of throwing", async () => {
    setup();
    await openNew("nostore");
    const ta = await newEditor();
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
    expect(await screen.findByRole("form", { name: "Test run" })).toBeInTheDocument();
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
    await userEvent.click(within(form).getByRole("button", { name: "Run test" }));

    const result = await screen.findByRole("group", { name: "Test result" });
    expect(within(result).getByText("HTTP 200")).toBeInTheDocument();
    expect(within(result).getByLabelText("Response headers")).toHaveTextContent("content-type: application/json");
    expect(within(result).getByLabelText("Response body")).toHaveTextContent('"hello": "world"');
    expect(await within(result).findByText("[info] echo ran")).toBeInTheDocument();
    const call = st.invokes[0]!;
    expect(call).toMatchObject({ method: "PUT", path: "/api/v1/functions/echo/invoke/items", body: { n: 1 } });
    expect(new URLSearchParams(call.query).get("name")).toBe("Ada");
    expect(call.headers).toMatchObject({ "X-Test": "yes", "Content-Type": "application/json" });
    expect(new URLSearchParams(call.query).has("version")).toBe(false);
    expect(mock.problems).toEqual([]);
  });

  it("sends an x-request-id, shows it with a copy button and a link to that run's logs", async () => {
    const { st } = setup();
    renderStudio("/functions/f/echo/test");
    const form = await screen.findByRole("form", { name: "Test run" });
    await userEvent.click(within(form).getByRole("button", { name: "Run test" }));
    const result = await screen.findByRole("group", { name: "Test result" });
    expect(st.invokes[0]!.headers["x-request-id"]).toMatch(/^[0-9a-f-]{36}$/);
    expect(within(result).getByLabelText("Request id")).toHaveTextContent("req-echo");
    expect(within(result).getByRole("button", { name: "Copy request id" })).toBeInTheDocument();
    expect(within(result).getByRole("link", { name: "Find in logs" })).toHaveAttribute("href", "/functions/f/echo/logs?request=req-echo");
    // Only this run's log lines are shown.
    expect(await within(result).findByText("[info] echo ran")).toBeInTheDocument();
  });

  it("Find in logs search: a request id from an error lists the function run that carried it", async () => {
    const { mock } = setup();
    renderStudio("/functions/logs?request=req-echo");
    const list = await screen.findByRole("list", { name: "Matching executions" });
    expect(await within(list).findByRole("link", { name: "echo" })).toHaveAttribute("href", "/functions/f/echo/logs?request=req-echo");
    expect(mock.problems).toEqual([]);
  });

  it("Find in logs search: says so when no run has the id", async () => {
    setup();
    renderStudio("/functions/logs?request=nope-123");
    expect(await screen.findByRole("region", { name: "No function run has this request id" })).toBeInTheDocument();
  });

  it("shows a failing run as an error with its message, what to do and the request id", async () => {
    const { st } = setup();
    st.invokeReply = { status: 500, headers: { "x-request-id": "req-echo" }, body: { error: { code: "function_error", message: "ReferenceError: nope is not defined" } } };
    renderStudio("/functions/f/echo/test");
    const form = await screen.findByRole("form", { name: "Test run" });
    await userEvent.click(within(form).getByRole("button", { name: "Run test" }));
    const result = await screen.findByRole("group", { name: "Test result" });
    const alert = within(result).getByRole("alert");
    expect(alert).toHaveTextContent("ReferenceError: nope is not defined");
    expect(alert).toHaveTextContent("What to do");
    expect(alert).toHaveTextContent("Request id req-echo");
  });

  it("targets a chosen version, and shows non-200 answers as they are", async () => {
    const { st } = setup();
    st.invokeReply = { status: 502, headers: { "x-request-id": "none" }, body: { error: { code: "function_trap", message: "boom" } } };
    renderStudio("/functions/f/echo/test");
    const form = await screen.findByRole("form", { name: "Test run" });
    await pick(within(form).getByLabelText("Version"), "1");
    await userEvent.click(within(form).getByRole("button", { name: "Run test" }));
    expect(await screen.findByText("HTTP 502")).toBeInTheDocument();
    expect(screen.getByRole("alert")).toHaveTextContent("boom");
    expect(new URLSearchParams(st.invokes[0]!.query).get("version")).toBe("1");
    expect(screen.getByLabelText("Response body")).toHaveTextContent("function_trap");
    expect(await screen.findByText(/No log lines for this run/)).toBeInTheDocument();
  });

  it("refuses a body that is not JSON without calling the server", async () => {
    const { st } = setup();
    renderStudio("/functions/f/echo/test");
    const form = await screen.findByRole("form", { name: "Test run" });
    await userEvent.click(within(form).getByLabelText("JSON body"));
    await userEvent.paste("{oops");
    await userEvent.click(within(form).getByRole("button", { name: "Run test" }));
    expect(await screen.findByText("The body is not valid JSON")).toBeInTheDocument();
    expect(st.invokes).toHaveLength(0);
  });
});

describe("Versions", () => {
  it("lists runtime and language, opens a JS version read-only and diffs it against the current one", async () => {
    setup();
    renderStudio("/functions/f/echo");
    expect(await screen.findByText(/Active v2 of 2/)).toBeInTheDocument();
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
    expect(await screen.findByText(/Active v2 of 2/)).toBeInTheDocument();
    const rows = await screen.findAllByRole("row");
    expect(within(rows[1]!).getByText("wasm")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /View source/ })).not.toBeInTheDocument();
  });
});

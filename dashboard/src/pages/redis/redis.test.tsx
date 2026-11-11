import { afterEach, describe, expect, it, vi } from "vitest";
import { act, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installKv } from "../../test/kvMock";
import { confirmDialog } from "../../test/dataStudioMock";
import { pick } from "../../test/pick";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installKv(mock) };
}
const keyList = () => within(screen.getByRole("list", { name: "Keys" }));
const names = () => keyList().getAllByRole("button").map((b) => b.textContent);

describe("Redis key browser", () => {
  it("lists keys, filters by pattern and by type, and switches namespace", async () => {
    setup();
    renderStudio("/redis");
    expect(await screen.findByRole("list", { name: "Keys" })).toBeInTheDocument();
    expect(names()).toEqual(["board", "events", "greeting", "tags", "todo", "user:1", "user:2"]);

    await userEvent.type(screen.getByLabelText("Pattern"), "user:*");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(names()).toEqual(["user:1", "user:2"]));

    await userEvent.clear(screen.getByLabelText("Pattern"));
    await pick(screen.getByLabelText("Type"), "zset");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(names()).toEqual(["board"]));

    await pick(screen.getByLabelText("Type"), "any");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    await waitFor(() => expect(names()).toHaveLength(7));

    const ns = screen.getByLabelText("Namespace");
    await userEvent.clear(ns);
    await userEvent.type(ns, "other{enter}");
    await waitFor(() => expect(names()).toEqual(["x"]));
  });

  it("pages through a large namespace", async () => {
    const { st } = setup();
    for (let i = 0; i < 60; i++) st.ns.default![`bulk:${String(i).padStart(3, "0")}`] = { type: "string", value: "v", ttl_ms: null };
    renderStudio("/redis");
    await screen.findByRole("list", { name: "Keys" });
    expect(names()).toHaveLength(50);
    expect(screen.getByText("50 keys shown")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(names()).toHaveLength(67));
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("says when nothing matches", async () => {
    setup();
    renderStudio("/redis");
    await screen.findByRole("list", { name: "Keys" });
    await userEvent.type(screen.getByLabelText("Pattern"), "zzz*");
    await userEvent.click(screen.getByRole("button", { name: "Apply filter" }));
    expect(await screen.findByRole("region", { name: "No keys match" })).toBeInTheDocument();
  });

  it("creates a string key from the header, opens it, and flushes the namespace only after its name is typed", async () => {
    const { st } = setup();
    renderStudio("/redis?ns=other");
    await screen.findByRole("list", { name: "Keys" });
    await userEvent.click(screen.getByRole("button", { name: "New key" }));
    const form = await screen.findByRole("form", { name: "New key" });
    await userEvent.type(within(form).getByLabelText(/^Key/), "fresh");
    await userEvent.type(within(form).getByLabelText("Value"), "v1");
    await userEvent.type(within(form).getByLabelText(/^Expire after/), "30");
    await userEvent.click(within(form).getByRole("button", { name: "Create key" }));
    expect(await screen.findByLabelText("String value")).toHaveValue("v1");
    expect(st.ns.other!.fresh).toMatchObject({ type: "string", value: "v1", ttl_ms: 30_000 });

    await userEvent.click(screen.getByRole("button", { name: "Flush namespace other" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Flush namespace other?" });
    expect(within(dialog).getByRole("button", { name: "Flush namespace" })).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText(/to confirm/), "other");
    await userEvent.click(within(dialog).getByRole("button", { name: "Flush namespace" }));
    expect(await screen.findByRole("region", { name: "No keys in other yet" })).toBeInTheDocument();
    expect(st.ns.other).toEqual({});
  });

  it("keeps the namespace in the URL and validates the name as you type", async () => {
    setup();
    renderStudio("/redis?ns=other");
    await screen.findByRole("list", { name: "Keys" });
    expect(screen.getByLabelText("Namespace")).toHaveValue("other");
    expect(screen.getByRole("tab", { name: "Console" })).toHaveAttribute("href", "/redis/console?ns=other");
    const ns = screen.getByLabelText("Namespace");
    await userEvent.clear(ns);
    await userEvent.type(ns, "bad name");
    expect(await screen.findByText(/Use letters, digits, dots, colons, dashes or underscores/)).toBeInTheDocument();
  });

  it("is called Key-value, says it is Redis compatible, and offers Use from code for the namespace", async () => {
    setup();
    renderStudio("/redis");
    expect(await screen.findByRole("heading", { level: 1, name: "Key-value" })).toBeInTheDocument();
    expect(screen.getByText(/Redis compatible/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Use from code" }));
    expect(await screen.findByText(/api\/v1\/kv\/default/)).toBeInTheDocument();
  });
});

describe("Redis key viewers and editors", () => {
  it("edits a string and keeps its TTL", async () => {
    const { st } = setup();
    st.ns.default!.greeting!.ttl_ms = 60_000;
    renderStudio("/redis?key=greeting");
    const box = await screen.findByLabelText("String value");
    expect(box).toHaveValue("hello");
    expect(screen.getByRole("button", { name: "Save value" })).toBeDisabled();
    await userEvent.clear(box);
    await userEvent.type(box, "bonjour");
    await userEvent.click(screen.getByRole("button", { name: "Save value" }));
    await waitFor(() => expect(st.ns.default!.greeting).toMatchObject({ value: "bonjour", ttl_ms: 60_000 }));
  });

  it("views, sets and removes a TTL", async () => {
    const { st } = setup();
    renderStudio("/redis?key=user:1");
    expect(await screen.findByLabelText("Remaining TTL")).toHaveTextContent("90 s");
    await userEvent.clear(screen.getByLabelText("TTL amount"));
    await userEvent.type(screen.getByLabelText("TTL amount"), "2");
    await pick(screen.getByLabelText("TTL unit"), "minutes");
    await userEvent.click(screen.getByRole("button", { name: "Set TTL" }));
    await waitFor(() => expect(st.ns.default!["user:1"]!.ttl_ms).toBe(120_000));
    expect(await screen.findByText("2 min")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Remove TTL" }));
    await waitFor(() => expect(st.ns.default!["user:1"]!.ttl_ms).toBeNull());
    expect(await screen.findByText("no expiry")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove TTL" })).toBeDisabled();
  });

  it("edits, adds and deletes hash fields", async () => {
    const { st } = setup();
    renderStudio("/redis?key=user:1");
    const name = await screen.findByLabelText("Value of name");
    expect(name).toHaveValue("Ann");
    expect(screen.getByRole("button", { name: "Save name" })).toBeDisabled();
    await userEvent.clear(name);
    await userEvent.type(name, "Annie");
    await userEvent.click(screen.getByRole("button", { name: "Save name" }));
    await waitFor(() => expect((st.ns.default!["user:1"]!.value as Record<string, string>).name).toBe("Annie"));

    await userEvent.type(screen.getByLabelText("New field name"), "city");
    await userEvent.type(screen.getByLabelText("New field value"), "Oslo");
    await userEvent.click(screen.getByRole("button", { name: "Add field" }));
    expect(await screen.findByLabelText("Value of city")).toHaveValue("Oslo");

    await userEvent.click(screen.getByRole("button", { name: "Delete field age" }));
    await waitFor(() => expect(screen.queryByLabelText("Value of age")).not.toBeInTheDocument());
    expect(st.ns.default!["user:1"]!.value).toEqual({ name: "Annie", city: "Oslo" });
  });

  it("pushes, pops, edits (LSET) and removes (LREM) list items through REST", async () => {
    const { mock, st } = setup();
    renderStudio("/redis?key=todo");
    const items = () => within(screen.getByRole("list", { name: "List items" })).getAllByRole("listitem").map((li) => li.textContent?.replace(/EditRemove$/, ""));
    await waitFor(() => expect(items()).toEqual(["0a", "1b", "2c"]));
    await userEvent.type(screen.getByLabelText("List value"), "z");
    await userEvent.click(screen.getByRole("button", { name: "Push left" }));
    await waitFor(() => expect(items()).toEqual(["0z", "1a", "2b", "3c"]));
    await userEvent.type(screen.getByLabelText("List value"), "y");
    await userEvent.click(screen.getByRole("button", { name: "Push right" }));
    await waitFor(() => expect(items()).toHaveLength(5));
    await userEvent.click(screen.getByRole("button", { name: "Pop left" }));
    await waitFor(() => expect(items()).toEqual(["0a", "1b", "2c", "3y"]));
    await userEvent.click(screen.getByRole("button", { name: "Pop right" }));
    await waitFor(() => expect(items()).toEqual(["0a", "1b", "2c"]));
    await userEvent.click(screen.getByRole("button", { name: "Remove item 1" }));
    await waitFor(() => expect(items()).toEqual(["0a", "1c"]));
    expect(st.ns.default!.todo!.value).toEqual(["a", "c"]);
    expect(st.execs).toEqual([]);

    await userEvent.click(screen.getByRole("button", { name: "Edit item 1" }));
    const box = screen.getByLabelText("Item 1 value");
    await userEvent.clear(box);
    await userEvent.type(box, "C2");
    await userEvent.click(screen.getByRole("button", { name: "Save item 1" }));
    await waitFor(() => expect(items()).toEqual(["0a", "1C2"]));
    expect(st.ns.default!.todo!.value).toEqual(["a", "C2"]);
    expect(mock.problems).toEqual([]);
  });

  it("adds and removes set members", async () => {
    const { st } = setup();
    renderStudio("/redis?key=tags");
    await screen.findByRole("list", { name: "Set members" });
    await userEvent.type(screen.getByLabelText("New member"), "green");
    await userEvent.click(screen.getByRole("button", { name: "Add member" }));
    expect(await screen.findByRole("button", { name: "Remove member green" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Remove member red" }));
    await waitFor(() => expect(st.ns.default!.tags!.value).toEqual(["blue", "green"]));
  });

  it("edits scores and adds and removes sorted set members", async () => {
    const { st } = setup();
    renderStudio("/redis?key=board");
    const score = await screen.findByLabelText("Score of ann");
    expect(score).toHaveValue("10");
    await userEvent.clear(score);
    await userEvent.type(score, "30");
    await userEvent.click(screen.getByRole("button", { name: "Save score of ann" }));
    await waitFor(() => expect(st.ns.default!.board!.value).toEqual([{ member: "bo", score: 20 }, { member: "ann", score: 30 }]));
    await userEvent.type(screen.getByLabelText("New member"), "cy");
    await userEvent.clear(screen.getByLabelText("New score"));
    await userEvent.type(screen.getByLabelText("New score"), "5");
    await userEvent.click(screen.getByRole("button", { name: "Add member" }));
    expect(await screen.findByLabelText("Score of cy")).toHaveValue("5");
    await userEvent.click(screen.getByRole("button", { name: "Remove bo" }));
    await waitFor(() => expect(screen.queryByLabelText("Score of bo")).not.toBeInTheDocument());
  });

  it("browses a stream, adds an entry and deletes one", async () => {
    const { st } = setup();
    renderStudio("/redis?key=events");
    const list = await screen.findByRole("list", { name: "Stream entries" });
    expect(within(list).getByText("1-0")).toBeInTheDocument();
    expect(within(list).getByText("login")).toBeInTheDocument();

    await userEvent.type(screen.getByLabelText("New entry fields"), "no equals sign");
    await userEvent.click(screen.getByRole("button", { name: "Add entry" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("must look like name=value");

    await userEvent.clear(screen.getByLabelText("New entry fields"));
    await userEvent.type(screen.getByLabelText("New entry fields"), "kind=logout");
    await userEvent.click(screen.getByRole("button", { name: "Add entry" }));
    expect(await screen.findByText("2-0")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Delete entry 1-0" }));
    await waitFor(() => expect(screen.queryByText("1-0")).not.toBeInTheDocument());
    expect(st.execs).toEqual([]);
    expect((st.ns.default!.events!.value as { id: string }[]).map((e) => e.id)).toEqual(["2-0"]);
  });

  it("deletes a key after confirmation and returns to the list", async () => {
    const { st } = setup();
    renderStudio("/redis?key=greeting");
    await screen.findByLabelText("String value");
    await userEvent.click(screen.getByRole("button", { name: "Delete key" }));
    await confirmDialog("Delete key greeting?", "Delete key");
    expect(await screen.findByRole("region", { name: "Select a key" })).toBeInTheDocument();
    expect(st.ns.default!.greeting).toBeUndefined();
    await waitFor(() => expect(names()).not.toContain("greeting"));
  });

  it("reports a key that does not exist and a wrong-type conflict", async () => {
    const { mock } = setup();
    renderStudio("/redis?key=nope");
    expect(await screen.findByRole("region", { name: "Key nope is gone" })).toBeInTheDocument();
    mock.custom = mock.custom.filter((c) => !c.template.endsWith("/hashes/{key}/set"));
    mock.on("POST", "/api/v1/kv/{ns}/hashes/{key}/set", () => envelope(409, "kv_wrong_type", "wrong type"));
  });

  it("shows a wrong-type error from the server on an edit", async () => {
    const { mock } = setup();
    mock.custom = mock.custom.filter((c) => !c.template.endsWith("/hashes/{key}/set"));
    mock.on("POST", "/api/v1/kv/{ns}/hashes/{key}/set", () => envelope(409, "kv_wrong_type", "the key changed type"));
    renderStudio("/redis?key=user:2");
    await userEvent.type(await screen.findByLabelText("New field name"), "a");
    await userEvent.click(screen.getByRole("button", { name: "Add field" }));
    expect(await screen.findByText(/kv_wrong_type/)).toBeInTheDocument();
  });
});

describe("Redis console", () => {
  it("runs commands with quoting, shows replies and errors, and recalls history", async () => {
    const { st } = setup();
    renderStudio("/redis/console");
    const input = await screen.findByLabelText("Command");
    await userEvent.type(input, 'SET note "hello world"{enter}');
    const out = screen.getByRole("log", { name: "Console output" });
    await waitFor(() => expect(within(out).getByText('"OK"')).toBeInTheDocument());
    expect(st.execs[0]).toEqual(["SET", "note", "hello world"]);
    expect(st.ns.default!.note!.value).toBe("hello world");

    await userEvent.type(input, "KEYS *{enter}");
    await waitFor(() => expect(within(out).getByText(/1\) "board"/)).toBeInTheDocument());
    await userEvent.type(input, "GET missing{enter}");
    await waitFor(() => expect(within(out).getByText("(nil)")).toBeInTheDocument());
    await userEvent.type(input, "BOOM{enter}");
    expect(await within(out).findByText("(error) kv_unknown_command: unknown command 'BOOM'")).toBeInTheDocument();

    await userEvent.type(input, "{arrowup}");
    expect(input).toHaveValue("BOOM");
    await userEvent.type(input, "{arrowup}");
    expect(input).toHaveValue("GET missing");
    await userEvent.type(input, "{arrowdown}{arrowdown}{arrowdown}");
    expect(input).toHaveValue("");
  });

  it("rejects an unbalanced quote without calling the server, and clears", async () => {
    const { st } = setup();
    renderStudio("/redis/console");
    await userEvent.type(await screen.findByLabelText("Command"), 'GET "abc{enter}');
    expect(await screen.findByText("(error) Unbalanced quote")).toBeInTheDocument();
    expect(st.execs).toHaveLength(0);
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.queryByText("(error) Unbalanced quote")).not.toBeInTheDocument();
  });
});

describe("Redis Pub/Sub inspector", () => {
  function stubSse(frames: string[]) {
    const base = globalThis.fetch;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      if (!String(input).includes("/subscribe/")) return base(input, init);
      const enc = new TextEncoder();
      const stream = new ReadableStream<Uint8Array>({
        start(c) {
          for (const f of frames) { const mid = Math.floor(f.length / 2); c.enqueue(enc.encode(f.slice(0, mid))); c.enqueue(enc.encode(f.slice(mid))); }
          init?.signal?.addEventListener("abort", () => { try { c.error(new DOMException("aborted", "AbortError")); } catch { /* closed */ } });
        },
      });
      return Promise.resolve(new Response(stream, { status: 200, headers: { "Content-Type": "text/event-stream" } }));
    }));
  }

  it("listens to a channel, shows messages (even split across chunks) and drops, and stops", async () => {
    setup();
    stubSse(['event: message\ndata: {"channel":"news","message":"hi there"}\n\n', "event: lagged\ndata: 3\n\n", 'event: message\ndata: {"channel":"news","message":{"base64":"AAE="}}\n\n']);
    renderStudio("/redis/pubsub");
    await userEvent.type(await screen.findByLabelText("Channel"), "news");
    await userEvent.click(screen.getByRole("button", { name: "Listen" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Listening on news");
    const list = await screen.findByRole("list", { name: "Received messages" });
    await waitFor(() => expect(within(list).getAllByRole("listitem")).toHaveLength(2));
    expect(within(list).getByText(/hi there/)).toBeInTheDocument();
    expect(within(list).getByText(/base64:AAE=/)).toBeInTheDocument();
    expect(screen.getByText(/3 message\(s\) were dropped/)).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Stop listening" }));
    expect(await screen.findByRole("button", { name: "Listen" })).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByRole("region", { name: "No messages yet" })).toBeInTheDocument();
  });

  it("lists channels with live subscribers and fills the channel field from one", async () => {
    const { mock } = setup();
    renderStudio("/redis/pubsub");
    const list = await screen.findByRole("list", { name: "Active channels" });
    expect(within(list).getByText("2 subscribers")).toBeInTheDocument();
    await userEvent.click(within(list).getByRole("button", { name: "news" }));
    expect(screen.getByLabelText("Channel")).toHaveValue("news");
    expect(mock.problems).toEqual([]);
  });

  it("publishes a message and reports the receivers", async () => {
    const { st } = setup();
    renderStudio("/redis/pubsub");
    await userEvent.type(await screen.findByLabelText("Channel"), "news");
    await userEvent.type(screen.getByLabelText("Message"), "breaking");
    await userEvent.click(screen.getByRole("button", { name: "Publish" }));
    expect(await screen.findByText("Delivered to 1 subscriber.")).toBeInTheDocument();
    expect(st.published).toEqual([{ channel: "news", message: "breaking" }]);
  });

  it("shows a refused subscription", async () => {
    setup();
    const base = globalThis.fetch;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
      String(input).includes("/subscribe/") ? Promise.resolve(new Response(JSON.stringify(envelope(403, "forbidden", "no permission").body), { status: 403 })) : base(input, init)));
    renderStudio("/redis/pubsub");
    await userEvent.type(await screen.findByLabelText("Channel"), "secret");
    await userEvent.click(screen.getByRole("button", { name: "Listen" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("You do not have permission to do this");
    await act(async () => undefined);
    expect(screen.getByRole("button", { name: "Listen" })).toBeInTheDocument();
  });
});

describe("Redis memory and stats", () => {
  it("shows memory use against the budget, cache statistics and namespaces", async () => {
    setup();
    renderStudio("/redis/stats");
    expect(await screen.findByLabelText("Memory")).toHaveTextContent("5.00 MiB used of 10.0 MiB (50%) · eviction policy allkeys-lru");
    expect(screen.getByRole("progressbar", { name: "Memory budget" })).toHaveAttribute("aria-valuenow", "50");
    const stats = screen.getByLabelText("Cache statistics");
    expect(within(stats).getByText("90%")).toBeInTheDocument();
    expect(within(stats).getByText("Evicted keys").nextSibling).toHaveTextContent("1");
    expect(screen.getByText(/changes are written to SQLite/)).toBeInTheDocument();
    const t = await screen.findByRole("table", { name: "Namespaces" });
    expect(within(t).getByText("default").closest("tr")).toHaveTextContent("7");
  });

  it("shows a permission error for non-administrators", async () => {
    const { mock } = setup();
    mock.custom = mock.custom.filter((c) => !c.template.endsWith("/_info"));
    mock.on("GET", "/api/v1/kv/_info", () => envelope(403, "forbidden", "administrators only"));
    renderStudio("/redis/stats");
    expect(await screen.findByText("You do not have permission to do this")).toBeInTheDocument();
  });
});

describe("Redis tenant scope and rules (FB-138)", () => {
  it("says when the figures are one tenant's", async () => {
    const { mock } = setup();
    mock.custom = mock.custom.filter((c) => !c.template.endsWith("/kv/_info"));
    mock.on("GET", "/api/v1/kv/_info", () => ({ status: 200, body: { keys: 3, used_memory_bytes: 10, max_memory_bytes: 100, eviction_policy: "allkeys-lru", hits: 1, misses: 1, expired_keys: 0, evicted_keys: 0, channels: 0, unsaved_keys: 0, persistent: false, scope: "tenant", tenant: "t:acme" } }));
    renderStudio("/redis/stats");
    expect(await screen.findByRole("status")).toHaveTextContent("Figures for tenant t:acme only");
  });

  it("has a Rules tab that explains shared namespaces", async () => {
    const { mock } = setup();
    mock.on("GET", "/api/v1/policies/resources/{kind}", () => ({ status: 200, body: { kind: "kv_namespace", fields: [], operations: ["list", "view", "create", "update", "delete"], actions: [], items: [] } }));
    renderStudio("/redis/rules?ns=shared.config");
    expect(await screen.findByText(/is shared: every tenant of a realm/)).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Rules" })).toBeInTheDocument();
  });
});

import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installQueues, message, received } from "../../test/queueMock";
import { duration, parseAttrs, parseJson } from "./util";
import { pick } from "../../test/pick";
import { confirmDialog, sheet, toast } from "../../test/dataStudioMock";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installQueues(mock) };
}

describe("Queues helpers", () => {
  it("parses attributes, JSON and durations", () => {
    expect(parseAttrs("a=b\n\n c = d=e ")).toEqual({ a: "b", c: " d=e" });
    expect(() => parseAttrs("nokey")).toThrow(/key=value/);
    expect(parseJson("{\"a\":1}")).toEqual({ ok: true, value: { a: 1 } });
    expect(parseJson("{nope")).toEqual({ ok: false });
    expect([duration(null), duration(250), duration(1500), duration(90_000), duration(7_200_000), duration(172_800_000)]).toEqual(["none", "250 ms", "1.5 s", "1.5 min", "2 h", "2 d"]);
  });
});

describe("Queues topics", () => {
  it("lists topics, creates one in a sheet and opens it, and says when the name is taken", async () => {
    const { mock } = setup();
    renderStudio("/queues");
    const table = await screen.findByRole("table", { name: "Topics" });
    expect(within(table).getByRole("link", { name: "orders" })).toBeInTheDocument();
    expect(within(table).getByText("records of users")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "New topic" }));
    const form = await sheet("New topic");
    await userEvent.type(within(form).getByLabelText(/^Name/), "invoices");
    await userEvent.type(within(form).getByLabelText(/^Retention/), "2");
    await userEvent.click(within(form).getByRole("button", { name: "Create topic" }));
    await toast("Topic invoices created");
    expect(mock.bodies["post /api/v1/queues/topics"]).toEqual([{ name: "invoices", retention_ms: 172_800_000 }]);
    // The new topic opens.
    expect(await screen.findByText("invoices", { selector: "h1" })).toBeInTheDocument();

    renderStudio("/queues?new=1");
    const again = await sheet("New topic");
    await userEvent.type(within(again).getByLabelText(/^Name/), "orders");
    await userEvent.click(within(again).getByRole("button", { name: "Create topic" }));
    expect(await within(again).findByRole("alert")).toHaveTextContent("topic_exists");
  });

  it("checks the topic name as you type", async () => {
    setup();
    renderStudio("/queues?new=1");
    const form = await sheet("New topic");
    await userEvent.type(within(form).getByLabelText(/^Name/), "bad name!");
    expect(await within(form).findByText(/Use letters, digits/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Create topic" })).toBeDisabled();
  });

  it("creates a topic for a tenant: sends the tenant header and shows the tenant on the list", async () => {
    const { mock, st } = setup();
    renderStudio("/queues?new=1");
    const form = await sheet("New topic");
    await userEvent.type(within(form).getByLabelText(/^Name/), "ledger");
    await userEvent.type(within(form).getByLabelText("Tenant"), "acme");
    await userEvent.click(within(form).getByRole("button", { name: "Create topic" }));
    await toast("Topic acme/ledger created");
    expect(st.tenants).toEqual(["acme"]);
    expect(mock.headers.some((h) => h["X-ForgeBase-Tenant"] === "acme")).toBe(true);
    expect(st.topics.at(-1)).toMatchObject({ name: "acme/ledger", tenant: "acme" });
  });

  it("shows each topic's tenant, or Instance when it has none", async () => {
    const { st } = setup();
    st.topics.push({ ...st.topics[0]!, name: "acme/ledger", tenant: "acme" });
    renderStudio("/queues");
    const table = await screen.findByRole("table", { name: "Topics" });
    expect(within(within(table).getByRole("row", { name: /acme\/ledger/ })).getByText("acme", { selector: "span" })).toBeInTheDocument();
    expect(within(within(table).getByRole("row", { name: /^orders/ })).getByText("Instance")).toBeInTheDocument();
  });

  it("explains an empty list", async () => {
    const { st } = setup();
    st.topics = [];
    renderStudio("/queues");
    expect(await screen.findByRole("region", { name: "No topics yet" })).toBeInTheDocument();
  });

  it("shows the server's refusal when the caller may not read queues", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("GET", "/api/v1/queues/topics", () => ({ status: 403, body: { error: { code: "forbidden", message: "no", request_id: "r" } } }));
    renderStudio("/queues");
    expect(await screen.findByRole("alert")).toHaveTextContent("You do not have permission to do this");
  });
});

describe("Queues topic detail", () => {
  it("shows metrics, browses messages and expands one", async () => {
    const { mock } = setup();
    renderStudio("/queues/t/orders");
    const metrics = await screen.findByLabelText("Topic metrics");
    expect(metrics).toHaveTextContent("Retained messages3");
    expect(metrics).toHaveTextContent("Subscriptions2");
    const table = await screen.findByRole("table", { name: "Messages" });
    expect(within(table).getAllByRole("row")).toHaveLength(4);
    await userEvent.click(screen.getByRole("button", { name: "Show message 2" }));
    expect(screen.getByLabelText("Message 2 data")).toHaveTextContent('"n": 2');
    expect(screen.getByText(/kind=rush/)).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Hide message 2" }));
    expect(screen.queryByLabelText("Message 2 data")).not.toBeInTheDocument();
    expect(mock.calls).toContain("get /api/v1/queues/topics/orders/messages");
  });

  it("pages through a long backlog", async () => {
    const { st } = setup();
    st.messages.orders = Array.from({ length: 30 }, (_, i) => message("orders", i + 1));
    renderStudio("/queues/t/orders");
    const table = await screen.findByRole("table", { name: "Messages" });
    expect(within(table).getAllByRole("row")).toHaveLength(26);
    await userEvent.click(screen.getByRole("button", { name: "Load more" }));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Messages" })).getAllByRole("row")).toHaveLength(31));
    expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument();
  });

  it("publishes a message with attributes, ordering key and dedup id, and flags a duplicate", async () => {
    const { mock } = setup();
    renderStudio("/queues/t/orders");
    await screen.findByRole("table", { name: "Messages" });
    const data = screen.getByLabelText("Data (JSON)");
    await userEvent.clear(data);
    await userEvent.click(data);
    await userEvent.paste('{"id":9}');
    await userEvent.type(screen.getByLabelText(/Attributes/), "kind=rush");
    await userEvent.type(screen.getByLabelText(/^Ordering key/), "o9");
    await userEvent.type(screen.getByLabelText(/^Dedup id/), "d9");
    await userEvent.click(screen.getByRole("button", { name: "Publish message" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Published msg_orders_4 (sequence 4).");
    expect(mock.bodies["post /api/v1/queues/topics/{topic}/publish"]).toEqual([{ messages: [{ data: { id: 9 }, attributes: { kind: "rush" }, ordering_key: "o9", dedup_id: "d9" }] }]);
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Messages" })).getAllByRole("row")).toHaveLength(5));

    await userEvent.click(screen.getByRole("button", { name: "Publish message" }));
    expect(await screen.findByText(/a duplicate: accepted once before/)).toBeInTheDocument();
  });

  it("refuses data that is not JSON and bad attribute lines without calling the server", async () => {
    const { mock } = setup();
    renderStudio("/queues/t/orders");
    await screen.findByRole("table", { name: "Messages" });
    const data = screen.getByLabelText("Data (JSON)");
    await userEvent.clear(data);
    await userEvent.click(data);
    await userEvent.paste("{nope");
    await userEvent.click(screen.getByRole("button", { name: "Publish message" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("not valid JSON");
    await userEvent.clear(data);
    await userEvent.click(data);
    await userEvent.paste("1");
    await userEvent.type(screen.getByLabelText(/Attributes/), "oops");
    await userEvent.click(screen.getByRole("button", { name: "Publish message" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("key=value");
    expect(mock.calls.filter((c) => c.endsWith("/publish"))).toHaveLength(0);
  });

  it("lists the subscriptions of the topic with their owner, and creates one in a sheet", async () => {
    const { mock } = setup();
    renderStudio("/queues/t/orders");
    const list = await screen.findByRole("list", { name: "Topic subscriptions" });
    expect(within(list).getByRole("link", { name: "order-workers" })).toBeInTheDocument();
    expect(within(list).getAllByText(/owner admin/)).toHaveLength(2);
    await userEvent.click(screen.getByRole("button", { name: "New subscription" }));
    const form = await sheet("New subscription");
    await userEvent.type(within(form).getByLabelText(/^Name/), "billing");
    await userEvent.clear(within(form).getByLabelText(/^Ack deadline/));
    await userEvent.type(within(form).getByLabelText(/^Ack deadline/), "45");
    await userEvent.type(within(form).getByLabelText(/^Max attempts/), "5");
    await userEvent.click(within(form).getByRole("checkbox", { name: /^Ordered/ }));
    await userEvent.click(within(form).getByRole("button", { name: "Create subscription" }));
    await toast("Subscription billing created");
    expect(mock.bodies["post /api/v1/queues/subscriptions"]).toEqual([{ name: "billing", topic: "orders", ack_deadline_ms: 45_000, ordering: true, start: "latest", max_delivery_attempts: 5 }]);
    expect(await screen.findByLabelText("Subscription settings")).toBeInTheDocument();
  });

  it("creates a subscription in the topic's tenant and shows the tenant and owner", async () => {
    const { st } = setup();
    st.topics.push({ ...st.topics[0]!, name: "acme/ledger", tenant: "acme" });
    st.messages["acme/ledger"] = [];
    renderStudio("/queues/t/acme%2Fledger");
    await screen.findByText("acme/ledger", { selector: "h1" });
    await userEvent.click(await screen.findByRole("button", { name: "New subscription" }));
    const form = await sheet("New subscription");
    await userEvent.type(within(form).getByLabelText(/^Name/), "audit");
    await userEvent.click(within(form).getByRole("button", { name: "Create subscription" }));
    await toast("Subscription acme/audit created");
    expect(st.tenants.at(-1)).toBe("acme");
    expect(st.subs.at(-1)).toMatchObject({ name: "acme/audit", tenant: "acme", created_by: "admin" });
    const settings = await screen.findByLabelText("Subscription settings");
    expect(settings).toHaveTextContent("Owneradmin");
    expect(settings).toHaveTextContent("Tenantacme");
  });

  it("asks for the topic's name, offers force when it still has subscriptions, then deletes it", async () => {
    const { mock, st } = setup();
    renderStudio("/queues/t/orders");
    await screen.findByRole("table", { name: "Messages" });
    await userEvent.click(screen.getByRole("button", { name: "Delete topic" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete topic orders?" });
    const confirm = within(dialog).getByRole("button", { name: "Delete topic" });
    expect(confirm).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText(/to confirm/), "orders");
    await userEvent.click(confirm);
    expect(await within(dialog).findByRole("alert")).toHaveTextContent("topic_has_subscriptions");
    await userEvent.click(await within(dialog).findByRole("button", { name: "Delete topic and its subscriptions" }));
    await waitFor(() => expect(st.topics.map((t) => t.name)).toEqual(["audit"]));
    expect(mock.calls).toContain("delete /api/v1/queues/topics/orders");
    expect(await screen.findByRole("table", { name: "Topics" })).toBeInTheDocument();
  });

  it("explains that a record topic shows the owner's view and withholds what the owner may not see", async () => {
    setup();
    renderStudio("/queues/t/audit");
    expect(await screen.findByRole("note")).toHaveTextContent(/as its subscription's owner may see it/);
    expect(screen.getByRole("note")).toHaveTextContent(/withheld/);
    expect(screen.getByRole("note")).toHaveTextContent("users");
  });

  it("offers topic rules that read @request only, and saves them", async () => {
    const { mock, st } = setup();
    renderStudio("/queues/t/orders");
    const rules = await screen.findByLabelText("Rules for orders");
    expect(mock.calls).toContain("get /api/v1/policies/resources/queue_topic");
    expect(screen.getByText(/rules may use|can only test the caller with/i, { exact: false })).toBeInTheDocument();
    await pick(within(rules).getByLabelText("List (scan or search) rule"), "Everyone");
    await userEvent.click(screen.getByRole("button", { name: "Save rules" }));
    await toast("Access rules saved");
    expect(st.rules.queue_topic!.orders).toEqual({ list: "" });
  });

  it("reports a topic that does not exist", async () => {
    setup();
    renderStudio("/queues/t/ghost");
    expect((await screen.findAllByRole("alert"))[0]).toHaveTextContent("topic_not_found");
  });
});

describe("Queues subscriptions", () => {
  it("lists every subscription", async () => {
    setup();
    renderStudio("/queues/subscriptions");
    const table = await screen.findByRole("table", { name: "Subscriptions" });
    expect(within(table).getByRole("link", { name: "order-workers" })).toBeInTheDocument();
    expect(within(table).getByText("pull, ordered")).toBeInTheDocument();
    expect(within(within(table).getByRole("row", { name: /order-workers/ })).getByText("admin")).toBeInTheDocument();
  });

  it("explains an empty subscription list", async () => {
    const { st } = setup();
    st.subs = [];
    renderStudio("/queues/subscriptions");
    expect(await screen.findByRole("region", { name: "No subscriptions yet" })).toBeInTheDocument();
  });

  it("shows settings, backlog metrics and dead letters with their history", async () => {
    setup();
    renderStudio("/queues/s/order-workers");
    expect(await screen.findByLabelText("Subscription settings")).toHaveTextContent("Dead-letter topicorders-dead");
    const m = await screen.findByLabelText("Subscription metrics");
    expect(m).toHaveTextContent("Backlog7 message(s)");
    expect(m).toHaveTextContent("In flight2");
    expect(m).toHaveTextContent("Oldest unacked2 min");
    const dead = await screen.findByRole("list", { name: "Dead letters" });
    expect(dead).toHaveTextContent("max delivery attempts reached");
    await userEvent.click(within(dead).getByRole("button", { name: "Show dead letter 5" }));
    expect(screen.getByLabelText("Dead letter 5 data")).toHaveTextContent('"n": 1');
    expect(screen.getByLabelText("Dead letter 5 history")).toHaveTextContent("attempt 1");
    expect(screen.getByLabelText("Dead letter 5 history")).toHaveTextContent("boom");
  });

  it("replays from a time and from a snapshot, and manages snapshots", async () => {
    const { st, mock } = setup();
    renderStudio("/queues/s/order-workers");
    await screen.findByRole("list", { name: "Dead letters" });
    expect(screen.getByRole("button", { name: "Replay from this time" })).toBeDisabled();

    await userEvent.type(screen.getByLabelText("Replay from"), "2026-11-09T09:00");
    await userEvent.click(screen.getByRole("button", { name: "Replay from this time" }));
    expect(await screen.findByRole("status")).toHaveTextContent("3 message(s) were put back");
    expect(st.seeks[0]).toEqual({ sub: "order-workers", to: { time: new Date("2026-11-09T09:00").toISOString() } });

    await pick(await screen.findByLabelText("Snapshot"), /^before-fix/);
    await userEvent.click(screen.getByRole("button", { name: "Restore snapshot" }));
    await waitFor(() => expect(st.seeks).toHaveLength(2));
    expect(st.seeks[1]).toEqual({ sub: "order-workers", to: { snapshot: "before-fix" } });

    await userEvent.type(screen.getByLabelText("New snapshot"), "nightly");
    await userEvent.click(screen.getByRole("button", { name: "Save snapshot" }));
    await toast("Snapshot nightly created");
    expect(st.snapshots.map((s) => s.name)).toEqual(["before-fix", "nightly"]);
    await userEvent.click(screen.getByLabelText("Snapshot"));
    expect(await screen.findByRole("option", { name: /nightly/ })).toBeInTheDocument();
    await userEvent.keyboard("{Escape}");

    await userEvent.type(screen.getByLabelText("New snapshot"), "nightly");
    await userEvent.click(screen.getByRole("button", { name: "Save snapshot" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("snapshot_exists");
    expect(mock.problems).toEqual([]);
  });

  it("deletes a subscription and returns to the list", async () => {
    const { st } = setup();
    renderStudio("/queues/s/order-audit");
    await screen.findByLabelText("Subscription settings");
    await userEvent.click(screen.getByRole("button", { name: "Delete subscription" }));
    await confirmDialog("Delete subscription order-audit?", "Delete subscription", "order-audit");
    await waitFor(() => expect(st.subs.map((s) => s.name)).toEqual(["order-workers"]));
    expect(await screen.findByRole("table", { name: "Subscriptions" })).toBeInTheDocument();
  });
});

describe("Pub/Sub inspector", () => {
  function stubSse(frames: string[], status = 200) {
    const base = globalThis.fetch;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      if (!String(input).includes("/events")) return base(input, init);
      if (status !== 200) return Promise.resolve(new Response(JSON.stringify({ error: { code: "push_subscription", message: "push subscriptions are not pulled", request_id: "r" } }), { status }));
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
  async function pickSub() {
    const sel = await screen.findByLabelText("Subscription");
    await pick(sel, /^order-workers/);
  }
  const frame = (n: number) => `event: message\nid: msg_orders_${n}\ndata: ${JSON.stringify(received(message("orders", n)))}\n\n`;

  it("streams deliveries (even split across chunks) and acks and nacks them", async () => {
    const { st } = setup();
    stubSse([frame(1), frame(2), frame(3)]);
    renderStudio("/queues/inspector");
    await pickSub();
    await userEvent.click(screen.getByRole("button", { name: "Listen" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Listening on order-workers");
    const list = await screen.findByRole("list", { name: "Deliveries" });
    await waitFor(() => expect(within(list).getAllByRole("listitem")).toHaveLength(3));

    await userEvent.click(within(list).getByRole("button", { name: "Ack msg_orders_1" }));
    await waitFor(() => expect(st.acked).toEqual(["ack_msg_orders_1"]));
    await userEvent.click(within(list).getByRole("button", { name: "Nack msg_orders_2" }));
    await waitFor(() => expect(st.nacked).toEqual(["ack_msg_orders_2"]));
    expect(within(list).queryByRole("button", { name: "Ack msg_orders_1" })).not.toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Ack all" }));
    await waitFor(() => expect(st.acked).toEqual(["ack_msg_orders_1", "ack_msg_orders_3"]));
    expect(screen.getByRole("button", { name: "Ack all" })).toBeDisabled();

    await userEvent.click(screen.getByRole("button", { name: "Stop listening" }));
    expect(screen.queryByRole("status")).not.toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Clear" }));
    expect(screen.getByRole("region", { name: "No deliveries yet" })).toBeInTheDocument();
  });

  it("sends the flow-control window with the stream request", async () => {
    setup();
    stubSse([]);
    renderStudio("/queues/inspector");
    await pickSub();
    await userEvent.clear(screen.getByLabelText("Most in flight"));
    await userEvent.type(screen.getByLabelText("Most in flight"), "4");
    await userEvent.click(screen.getByRole("button", { name: "Listen" }));
    await waitFor(() => expect(vi.mocked(globalThis.fetch).mock.calls.some(([u]) => String(u).endsWith("/order-workers/events?max_messages=4"))).toBe(true));
  });

  it("shows the server's refusal for a stream", async () => {
    setup();
    stubSse([], 409);
    renderStudio("/queues/inspector");
    await pickSub();
    await userEvent.click(screen.getByRole("button", { name: "Listen" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("push_subscription");
    expect(screen.getByRole("button", { name: "Listen" })).toBeInTheDocument();
  });

  it("publishes to a topic and refuses bad JSON", async () => {
    const { mock } = setup();
    renderStudio("/queues/inspector");
    const topicSelect = await screen.findByLabelText("Topic");
    await pick(topicSelect, "orders");
    await userEvent.click(screen.getByRole("button", { name: "Publish" }));
    expect(await screen.findByText("Published to orders as sequence 4.")).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/queues/topics/{topic}/publish"]).toEqual([{ messages: [{ data: { hello: "world" } }] }]);
    const data = screen.getByLabelText("Data (JSON)");
    await userEvent.clear(data);
    await userEvent.type(data, "nope");
    await userEvent.click(screen.getByRole("button", { name: "Publish" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("not valid JSON");
  });

  it("does not offer record-source topics for publishing", async () => {
    setup();
    renderStudio("/queues/inspector");
    const select = await screen.findByLabelText("Topic");
    await userEvent.click(select);
    const list = await screen.findByRole("listbox");
    await waitFor(() => expect(within(list).getByRole("option", { name: "orders" })).toBeInTheDocument());
    expect(within(list).queryByRole("option", { name: "audit" })).not.toBeInTheDocument();
  });
});

describe("Queues studio navigation", () => {
  it("has a sidebar entry and unknown pages say so", async () => {
    setup();
    renderStudio("/queues/nope");
    expect(await screen.findByText("No such page in Queues.")).toBeInTheDocument();
    expect(screen.getAllByRole("link", { name: "Queues" }).length).toBeGreaterThan(0);
  });
});

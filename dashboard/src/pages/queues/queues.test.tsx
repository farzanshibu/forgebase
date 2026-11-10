import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installQueues, message, received } from "../../test/queueMock";
import { duration, parseAttrs, parseJson } from "./util";
import { pick } from "../../test/pick";

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
  it("lists topics and creates one, and says when the name is taken", async () => {
    const { mock } = setup();
    renderStudio("/queues");
    const table = await screen.findByRole("table", { name: "Topics" });
    expect(within(table).getByRole("link", { name: "orders" })).toBeInTheDocument();
    expect(within(table).getByText("records of users")).toBeInTheDocument();

    await userEvent.type(screen.getByLabelText("Name"), "invoices");
    await userEvent.type(screen.getByLabelText(/Retention/), "2");
    await userEvent.click(screen.getByRole("button", { name: "Create topic" }));
    expect(await within(await screen.findByRole("table", { name: "Topics" })).findByRole("link", { name: "invoices" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/queues/topics"]).toEqual([{ name: "invoices", retention_ms: 172_800_000 }]);

    await userEvent.type(screen.getByLabelText("Name"), "orders");
    await userEvent.click(screen.getByRole("button", { name: "Create topic" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("topic_exists");
  });

  it("explains an empty list", async () => {
    const { st } = setup();
    st.topics = [];
    renderStudio("/queues");
    expect(await screen.findByText(/No topics yet/)).toBeInTheDocument();
  });

  it("shows the server's refusal when the caller may not read queues", async () => {
    const mock = signedIn(new MockApi().install());
    mock.on("GET", "/api/v1/queues/topics", () => ({ status: 403, body: { error: { code: "forbidden", message: "no", request_id: "r" } } }));
    renderStudio("/queues");
    expect(await screen.findByRole("alert")).toHaveTextContent("Sign-in required");
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
    await userEvent.type(screen.getByLabelText("Ordering key"), "o9");
    await userEvent.type(screen.getByLabelText("Dedup id"), "d9");
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

  it("lists and creates subscriptions of the topic", async () => {
    const { mock } = setup();
    renderStudio("/queues/t/orders");
    const list = await screen.findByRole("list", { name: "Topic subscriptions" });
    expect(within(list).getByRole("link", { name: "order-workers" })).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Subscription name"), "billing");
    await userEvent.clear(screen.getByLabelText("Ack deadline (s)"));
    await userEvent.type(screen.getByLabelText("Ack deadline (s)"), "45");
    await userEvent.type(screen.getByLabelText("Max attempts"), "5");
    await userEvent.click(screen.getByLabelText("Ordered"));
    await userEvent.click(screen.getByRole("button", { name: "Create subscription" }));
    expect(await within(await screen.findByRole("list", { name: "Topic subscriptions" })).findByRole("link", { name: "billing" })).toBeInTheDocument();
    expect(mock.bodies["post /api/v1/queues/subscriptions"]).toEqual([{ name: "billing", topic: "orders", ack_deadline_ms: 45_000, ordering: true, start: "latest", max_delivery_attempts: 5 }]);
  });

  it("asks for force when a topic still has subscriptions, then deletes it", async () => {
    const { mock, st } = setup();
    renderStudio("/queues/t/orders");
    await screen.findByRole("table", { name: "Messages" });
    await userEvent.click(screen.getByRole("button", { name: "Delete topic" }));
    await userEvent.click(await screen.findByRole("button", { name: /confirm|yes/i }));
    expect(await screen.findByRole("alert")).toHaveTextContent("topic_has_subscriptions");
    await userEvent.click(screen.getByRole("button", { name: "Delete topic and its subscriptions" }));
    await userEvent.click(await screen.findByRole("button", { name: /confirm|yes/i }));
    await waitFor(() => expect(st.topics.map((t) => t.name)).toEqual(["audit"]));
    expect(mock.calls).toContain("delete /api/v1/queues/topics/orders");
    expect(await screen.findByRole("table", { name: "Topics" })).toBeInTheDocument();
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
    expect(await screen.findByText("Snapshot nightly saved.")).toBeInTheDocument();
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
    await userEvent.click(await screen.findByRole("button", { name: /confirm|yes/i }));
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
    expect(screen.getByText("No deliveries yet.")).toBeInTheDocument();
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
    expect(screen.getAllByRole("link", { name: /Queues Studio/ }).length).toBeGreaterThan(0);
  });
});

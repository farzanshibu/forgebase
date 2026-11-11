import { afterEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { Toast } from "panelui-web/toast";
import { GLOSSARY } from "../glossary";
import { STUDIOS, PLATFORM_ONLY, visibleStudios } from "../studios";
import { notify, toaster } from "../toaster";
import { MockApi } from "../test/mockApi";
import { setTokenSource } from "../api";
import { Check, Toggle } from "./controls";
import { PageHeader } from "./PageHeader";
import { ResourceRules } from "./ResourceRules";

afterEach(() => vi.unstubAllGlobals());

describe("PageHeader title HelpTip", () => {
  it("shows the studio's glossary term as a ? button after the title", () => {
    render(<MemoryRouter><PageHeader studio="functions" /></MemoryRouter>);
    expect(screen.getByRole("heading", { level: 1, name: "Functions" })).toBeInTheDocument();
    const tip = screen.getByRole("button", { name: "What is Edge function?" });
    expect(tip).toBeInTheDocument();
  });

  it("takes a term of its own, and null hides the studio's", () => {
    const { unmount } = render(<MemoryRouter><PageHeader title="orders" term="topic" docs={null} /></MemoryRouter>);
    expect(screen.getByRole("button", { name: "What is Topic?" })).toBeInTheDocument();
    unmount();
    render(<MemoryRouter><PageHeader studio="functions" term={null} /></MemoryRouter>);
    expect(screen.queryByRole("button", { name: /What is/ })).not.toBeInTheDocument();
  });

  it("every studio term exists in the glossary", () => {
    for (const s of STUDIOS) if (s.term) expect(GLOSSARY[s.term], s.path).toBeDefined();
    expect(STUDIOS.filter((s) => s.term).map((s) => s.path)).toEqual(expect.arrayContaining(["functions", "queues", "jobs", "vector", "graph", "identity"]));
  });
});

describe("glossary", () => {
  it("defines tenant, consent, role set and reserved kind", () => {
    for (const t of ["tenant", "consent", "role_set", "reserved_kind"] as const) {
      expect(GLOSSARY[t].short.length).toBeGreaterThan(0);
      expect(GLOSSARY[t].text.length).toBeGreaterThan(20);
    }
  });
});

describe("realm administrators see no platform-only studios", () => {
  it("hides Billing, System and Features and settings from the nav list, not from a platform administrator", () => {
    const realm = visibleStudios(undefined, { platform: false }).map((s) => s.path);
    for (const p of PLATFORM_ONLY) expect(realm).not.toContain(p);
    expect(realm).toContain("identity");
    const platform = visibleStudios(undefined).map((s) => s.path);
    for (const p of PLATFORM_ONLY) expect(platform).toContain(p);
  });
});

describe("Check and Toggle aria-label", () => {
  it("passes aria-label through even with a visible label", () => {
    render(<><Check checked={false} onChange={() => {}} label="Everything" aria-label="Select every row" /><Toggle checked={false} onChange={() => {}} label="Live" aria-label="Live updates" /></>);
    expect(screen.getByRole("checkbox", { name: "Select every row" })).toBeInTheDocument();
    expect(screen.getByRole("switch", { name: "Live updates" })).toBeInTheDocument();
  });

  it("without an aria-label the visible label is the name", () => {
    render(<Check checked onChange={() => {}} label="Required" />);
    expect(screen.getByRole("checkbox", { name: "Required" })).toBeChecked();
  });
});

describe("one toast per action", () => {
  it("an identical toast added twice in a row draws one card", async () => {
    render(<Toast.Provider toastManager={toaster}><p>app</p></Toast.Provider>);
    act(() => { notify.saved("Dedupe test"); notify.saved("Dedupe test"); });
    await waitFor(() => expect(document.querySelectorAll('[data-slot="toast"]')).toHaveLength(1));
    // A different toast is still shown.
    act(() => { notify.deleted("Other thing"); });
    await waitFor(() => expect(document.querySelectorAll('[data-slot="toast"]')).toHaveLength(2));
  });
});

describe("ResourceRules in components/", () => {
  it("edits the queue kinds (topic and subscription) like any other kind", async () => {
    setTokenSource(() => null);
    const mock = new MockApi().install();
    const seen: string[] = [];
    mock.on("GET", "/api/v1/policies/resources/{kind}", ({ params }) => {
      seen.push(params.kind!);
      return { status: 200, body: { kind: params.kind, fields: [], operations: ["view", "create"], actions: [], items: [] } };
    });
    mock.on("PUT", "/api/v1/policies/resources/{kind}/{name}", ({ params, body }) => ({ status: 200, body: { kind: params.kind, name: params.name, rules: (body as { rules: unknown }).rules } }));
    render(<MemoryRouter><ResourceRules kind="queue_topic" name="orders" noun="topic" /></MemoryRouter>);
    await screen.findByText(/has no rules yet/);
    expect(seen).toEqual(["queue_topic"]);
  });
});

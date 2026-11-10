import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter } from "react-router-dom";
import { ApiError, type CollectionDef } from "../../api";
import { makeCollection } from "../../test/mockApi";
import { fn, key } from "../../test/overviewMock";
import { API_KEY_PLACEHOLDER } from "../../snippets";
import { ConnectApp } from "./ConnectApp";
import { GettingStarted } from "./GettingStarted";
import { ago, eventWords, firstQuerySnippet, gettingStartedSteps, PLACEHOLDER_COLLECTION } from "./model";
import { liveKeys, newestCollection, type ProjectState } from "./state";

const users: CollectionDef = { ...makeCollection("users", []), type: "auth" };
const system: CollectionDef = { ...makeCollection("_jobs", []), system: true };
const notes: CollectionDef = { ...makeCollection("notes", []), created: "2026-11-08T00:00:00Z" };
const posts: CollectionDef = { ...makeCollection("posts", []), created: "2026-11-09T00:00:00Z" };

/** A fresh server: only the built-in users collection, one empty bucket, no key, no function. */
const fresh = (): ProjectState => ({
  collections: { state: "ok", value: [users, system] },
  keys: { state: "ok", value: [] },
  storage: { state: "ok", value: { buckets: 1, objects: 0, versions: 0, logical_bytes: 0, stored_bytes: 0, open_uploads: 0 } },
  functions: { state: "ok", value: [] },
});

const finished = (): ProjectState => ({
  collections: { state: "ok", value: [users, notes, posts] },
  keys: { state: "ok", value: [key()] },
  storage: { state: "ok", value: { buckets: 1, objects: 3, versions: 3, logical_bytes: 30, stored_bytes: 30, open_uploads: 0 } },
  functions: { state: "ok", value: [fn("hello")] },
});

const ui = (node: React.ReactNode) => render(<MemoryRouter>{node}</MemoryRouter>);

beforeEach(() => {
  try {
    localStorage.clear();
  } catch {
    /* no storage in this environment */
  }
});

describe("gettingStartedSteps", () => {
  it("on a fresh server only the administrator step is done, and each other step links to where to do it", () => {
    const steps = gettingStartedSteps(fresh(), { adminEmail: "root@example.com" });
    expect(steps.map((s) => [s.id, s.done])).toEqual([["admin", true], ["collection", false], ["key", false], ["file", false], ["function", false]]);
    expect(steps[0]!.detail).toContain("root@example.com");
    expect(Object.fromEntries(steps.slice(1).map((s) => [s.id, s.to]))).toEqual({ collection: "/database/new", key: "/auth/keys?new=1", file: "/storage", function: "/functions/new" });
  });

  it("asks for a bucket first when there is none", () => {
    const p = fresh();
    p.storage = { state: "ok", value: { buckets: 0, objects: 0, versions: 0, logical_bytes: 0, stored_bytes: 0, open_uploads: 0 } };
    const file = gettingStartedSteps(p).find((s) => s.id === "file")!;
    expect(file).toMatchObject({ done: false, to: "/storage?new=1", action: "New bucket" });
  });

  it("marks every step done after the journey and links to the newest collection and function", () => {
    const steps = gettingStartedSteps(finished());
    expect(steps.every((s) => s.done)).toBe(true);
    expect(steps.find((s) => s.id === "collection")).toMatchObject({ to: "/database/c/posts", action: "Open posts" });
    expect(steps.find((s) => s.id === "collection")!.detail).toMatch(/^2 collections\. The newest is posts\./);
    expect(steps.find((s) => s.id === "function")).toMatchObject({ to: "/functions/f/hello" });
    expect(steps.find((s) => s.id === "file")!.detail).toBe("3 files in 1 bucket.");
  });

  it("does not count revoked or expired keys, or functions with no live version", () => {
    const p = finished();
    p.keys = { state: "ok", value: [key({ revoked: true }), key({ id: "k2", expires_at: "2020-01-01T00:00:00Z" })] };
    p.functions = { state: "ok", value: [fn("draft", { active_version: 0 })] };
    const steps = gettingStartedSteps(p);
    expect(steps.find((s) => s.id === "key")!.done).toBe(false);
    expect(steps.find((s) => s.id === "function")!.done).toBe(false);
    expect(liveKeys([key({ expires_at: "2999-01-01T00:00:00Z" })])).toHaveLength(1);
  });

  it("hides a step whose feature is switched off, and says when a step could not be checked", () => {
    const p = fresh();
    p.functions = { state: "off" };
    p.keys = { state: "error", error: new ApiError({ status: 500, code: "internal", message: "boom", requestId: "req-1" }) };
    const steps = gettingStartedSteps(p, { disabled: (path) => path === "storage" });
    expect(steps.map((s) => s.id)).toEqual(["admin", "collection", "key"]);
    const k = steps.find((s) => s.id === "key")!;
    expect(k.done).toBeUndefined();
    expect(k.detail).toBe("Could not check: Something went wrong on the server.");
    expect(k.to).toBe("/auth/keys?new=1");
  });

  it("picks the newest user collection, ignoring system and built-in ones", () => {
    expect(newestCollection([users, system])).toBeUndefined();
    expect(newestCollection([users, notes, posts, system])?.name).toBe("posts");
  });
});

describe("GettingStarted", () => {
  afterEach(() => {
    try {
      localStorage.clear();
    } catch {
      /* ignore */
    }
  });

  it("shows step 1 done and four one-click links on a fresh server", () => {
    ui(<GettingStarted project={fresh()} adminEmail="root@example.com" />);
    const region = screen.getByRole("region", { name: "Getting started" });
    expect(within(region).getByText(/1 of 5 done/)).toBeInTheDocument();
    expect(within(region).getByRole("progressbar", { name: "Getting started progress" })).toHaveAttribute("aria-valuenow", "1");
    const links = within(region).getAllByRole("link");
    expect(links.map((a) => [a.textContent, a.getAttribute("href")])).toEqual([
      ["New collection", "/database/new"],
      ["Create API key", "/auth/keys?new=1"],
      ["Upload a file", "/storage"],
      ["New function", "/functions/new"],
    ]);
    const items = within(region).getAllByRole("listitem");
    expect(items[0]).toHaveTextContent(/^Done: 1\. Create the administrator/);
    expect(items[1]).toHaveTextContent(/^To do: 2\. Create a collection/);
  });

  it("remembers collapsing per browser", async () => {
    const u = userEvent.setup();
    const { unmount } = ui(<GettingStarted project={fresh()} />);
    const toggle = screen.getByRole("button", { name: "Hide steps" });
    expect(toggle).toHaveAttribute("aria-expanded", "true");
    await u.click(toggle);
    expect(screen.getByRole("button", { name: "Show steps" })).toHaveAttribute("aria-expanded", "false");
    expect(screen.queryByRole("list", { name: "Steps" })).not.toBeInTheDocument();
    unmount();

    ui(<GettingStarted project={fresh()} />);
    expect(screen.getByRole("button", { name: "Show steps" })).toBeInTheDocument();
    await u.click(screen.getByRole("button", { name: "Show steps" }));
    expect(screen.getByRole("list", { name: "Steps" })).toBeInTheDocument();
  });

  it("folds into one line once every step is done, with Show again", async () => {
    const u = userEvent.setup();
    ui(<GettingStarted project={finished()} />);
    const region = screen.getByRole("region", { name: "Getting started" });
    expect(region).toHaveTextContent("All 5 steps are done.");
    expect(within(region).queryByRole("list")).not.toBeInTheDocument();
    await u.click(within(region).getByRole("button", { name: "Show again" }));
    expect(within(screen.getByRole("region", { name: "Getting started" })).getAllByRole("listitem")).toHaveLength(5);
  });

  it("shows a skeleton while the project state loads", () => {
    ui(<GettingStarted project={undefined} />);
    expect(screen.getByRole("status", { name: "Loading Getting started" })).toBeInTheDocument();
  });
});

describe("ConnectApp", () => {
  it("shows the server URL with a copy button and links to the Create API key dialog", () => {
    ui(<ConnectApp collection="posts" />);
    const region = screen.getByRole("region", { name: "Connect your app" });
    expect(within(region).getByText(location.origin)).toBeInTheDocument();
    expect(within(region).getByRole("button", { name: "Copy server URL" })).toBeInTheDocument();
    expect(within(region).getByRole("link", { name: "Create an API key" })).toHaveAttribute("href", "/auth/keys?new=1");
  });

  it("fills the snippets with the origin and the newest collection, in all three languages, with a placeholder key", async () => {
    const u = userEvent.setup();
    ui(<ConnectApp collection="notes" />);
    const panel = screen.getByRole("region", { name: "First query" });
    for (const tab of ["JavaScript", "Rust", "curl"]) {
      await u.click(within(panel).getByRole("tab", { name: tab }));
      const code = within(panel).getByLabelText(`${tab} snippet`);
      expect(code).toHaveTextContent(location.origin);
      expect(code).toHaveTextContent("notes");
      if (tab !== "curl") expect(code).toHaveTextContent(API_KEY_PLACEHOLDER);
    }
  });

  it("uses a placeholder collection and says so when there is none", () => {
    ui(<ConnectApp />);
    expect(screen.getByLabelText("JavaScript snippet")).toHaveTextContent(`fb.collection("${PLACEHOLDER_COLLECTION}")`);
    expect(screen.getByText(/You have no collection yet/)).toBeInTheDocument();
  });

  it("puts the install command at the top of each SDK snippet and never a real key", () => {
    const s = firstQuerySnippet("http://example.test", "notes");
    expect(s.js.split("\n")[0]).toBe("// Install: npm install @forgebase/sdk");
    expect(s.rust.split("\n")[0]).toBe("// Install: cargo add forgebase-sdk tokio serde_json");
    for (const text of [s.js, s.rust, s.curl]) {
      expect(text).toContain("http://example.test");
      expect(text).toContain(API_KEY_PLACEHOLDER);
      expect(text).not.toMatch(/fb_[a-z0-9]{16,}/i);
    }
  });
});

describe("RecentActivity helpers", () => {
  it("words events plainly and keeps unknown ones readable", () => {
    expect(eventWords({ event: "admin.created", outcome: "success" })).toBe("Administrator created");
    expect(eventWords({ event: "login.admin", outcome: "failure" })).toBe("Administrator sign-in (failure)");
    expect(eventWords({ event: "webhook.secret_rotated", outcome: "success" })).toBe("Webhook secret rotated");
  });

  it("says how long ago", () => {
    const now = Date.parse("2026-11-07T10:00:00Z");
    expect(ago("2026-11-07T09:59:30Z", now)).toBe("just now");
    expect(ago("2026-11-07T09:55:00Z", now)).toBe("5 min ago");
    expect(ago("2026-11-07T07:00:00Z", now)).toBe("3 h ago");
  });
});

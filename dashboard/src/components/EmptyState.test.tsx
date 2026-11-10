import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { resetServerVersion } from "../hooks";
import { docsHref } from "../studios";
import { Button } from "./Button";
import { EmptyState } from "./EmptyState";
import { PageHeader } from "./PageHeader";
import { SkeletonCards, SkeletonTable } from "./Skeleton";

function stubVersion(version: string | null) {
  vi.stubGlobal(
    "fetch",
    vi.fn(async () => (version ? new Response(JSON.stringify({ name: "forgebase", version, mode: "single", data_dir: "/d", started_at: "", uptime_secs: 1 }), { status: 200 }) : new Response("{}", { status: 500 }))),
  );
}

beforeEach(() => resetServerVersion());
afterEach(() => vi.unstubAllGlobals());

describe("EmptyState", () => {
  it("says what the thing is and offers its action, a sample and a docs link", () => {
    stubVersion(null);
    render(
      <EmptyState
        title="No buckets yet"
        why="Buckets hold files your app uploads, like avatars or exports."
        action={<Button tone="primary">New bucket</Button>}
        sample={<Button>Try a sample</Button>}
        docs="docs/adr/0018-object-storage.md"
      />,
    );
    const region = screen.getByRole("region", { name: "No buckets yet" });
    expect(region).toHaveTextContent("Buckets hold files your app uploads");
    expect(screen.getByRole("heading", { name: "No buckets yet" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New bucket" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Try a sample" })).toBeInTheDocument();
    const docs = screen.getByRole("link", { name: /Docs/ });
    expect(docs).toHaveAttribute("href", "https://github.com/farzanshibu/forgebase/blob/main/docs/adr/0018-object-storage.md");
    expect(docs).toHaveAttribute("target", "_blank");
  });
});

describe("PageHeader", () => {
  it("takes the title, the sentence and the docs from studios.ts, at the running version's tag", async () => {
    stubVersion("0.3.0");
    render(<PageHeader studio="queues" primary={<Button tone="primary">New topic</Button>} />);
    expect(screen.getByRole("heading", { level: 1, name: "Queues" })).toBeInTheDocument();
    expect(screen.getByText(/Durable topics and subscriptions/)).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "New topic" })).toBeInTheDocument();
    await waitFor(() => expect(screen.getByRole("link", { name: /Docs/ })).toHaveAttribute("href", "https://github.com/farzanshibu/forgebase/blob/v0.3.0/docs/adr/0022-queues-pubsub.md"));
  });

  it("lets props win and hides the docs link with docs={null}", () => {
    stubVersion(null);
    render(<PageHeader studio="database" title="posts" description="Collection" docs={null} />);
    expect(screen.getByRole("heading", { level: 1, name: "posts" })).toBeInTheDocument();
    expect(screen.queryByRole("link", { name: /Docs/ })).not.toBeInTheDocument();
  });

  it("links docs to main for a dev build", () => {
    expect(docsHref("docs/api.md", "0.3.0-dev")).toBe("https://github.com/farzanshibu/forgebase/blob/main/docs/api.md");
    expect(docsHref("/docs/api.md#6-errors", "1.2.3")).toBe("https://github.com/farzanshibu/forgebase/blob/v1.2.3/docs/api.md#6-errors");
  });
});

describe("Skeleton", () => {
  it("is one labelled status per group", () => {
    render(
      <>
        <SkeletonTable rows={2} columns={3} label="Loading buckets" />
        <SkeletonCards count={2} label="Loading stats" />
      </>,
    );
    expect(screen.getByRole("status", { name: "Loading buckets" })).toBeInTheDocument();
    expect(screen.getByRole("status", { name: "Loading stats" })).toBeInTheDocument();
    expect(screen.getAllByRole("status")).toHaveLength(2);
  });
});

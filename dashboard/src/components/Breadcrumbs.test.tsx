import { describe, expect, it } from "vitest";
import { render, screen } from "@testing-library/react";
import { MemoryRouter } from "react-router-dom";
import { Breadcrumbs, CrumbsProvider } from "./Breadcrumbs";
import { crumbsFor, useCrumbs } from "./crumbs";

describe("crumbsFor", () => {
  it("builds the trail from the route and studios.ts; the last crumb is not a link", () => {
    expect(crumbsFor("/")).toEqual([{ label: "Overview" }]);
    expect(crumbsFor("/queues")).toEqual([{ label: "Queues" }]);
    expect(crumbsFor("/queues/subscriptions")).toEqual([{ label: "Queues", to: "/queues" }, { label: "Subscriptions" }]);
    expect(crumbsFor("/queues/t/orders")).toEqual([{ label: "Queues", to: "/queues" }, { label: "Topics", to: "/queues" }, { label: "orders" }]);
    expect(crumbsFor("/database/c/posts/schema")).toEqual([
      { label: "Database", to: "/database" },
      { label: "Collections", to: "/database" },
      { label: "posts", to: "/database/c/posts" },
      { label: "Schema" },
    ]);
    expect(crumbsFor("/functions/f/hello/logs")).toEqual([{ label: "Functions", to: "/functions" }, { label: "hello", to: "/functions/f/hello" }, { label: "Logs" }]);
    expect(crumbsFor("/storage/b/my%20files")).toEqual([{ label: "Storage", to: "/storage" }, { label: "Buckets", to: "/storage" }, { label: "my files" }]);
    expect(crumbsFor("/system/features")).toEqual([{ label: "System", to: "/system" }, { label: "Features and settings" }]);
    expect(crumbsFor("/system/backups")).toEqual([{ label: "System", to: "/system" }, { label: "Backups" }]);
    expect(crumbsFor("/nowhere")).toEqual([{ label: "Page not found" }]);
  });
});

function Detail() {
  useCrumbs([{ label: "Database", to: "/database" }, { label: "posts", to: "/database/c/posts" }, { label: "Pen" }]);
  return null;
}

describe("Breadcrumbs", () => {
  it("renders a breadcrumb landmark with links and the current page", () => {
    render(
      <MemoryRouter initialEntries={["/queues/t/orders"]}>
        <Breadcrumbs />
      </MemoryRouter>,
    );
    const nav = screen.getByRole("navigation", { name: "Breadcrumb" });
    expect(screen.getByRole("link", { name: "Queues" })).toHaveAttribute("href", "/queues");
    expect(screen.getByRole("link", { name: "Topics" })).toHaveAttribute("href", "/queues");
    expect(nav).toHaveTextContent("orders");
    expect(screen.queryByRole("link", { name: "orders" })).not.toBeInTheDocument();
  });

  it("lets a page override the trail", async () => {
    render(
      <MemoryRouter initialEntries={["/database/c/posts/r/p1"]}>
        <CrumbsProvider>
          <Breadcrumbs />
          <Detail />
        </CrumbsProvider>
      </MemoryRouter>,
    );
    expect(await screen.findByText("Pen")).toBeInTheDocument();
    expect(screen.getByRole("link", { name: "posts" })).toHaveAttribute("href", "/database/c/posts");
  });
});

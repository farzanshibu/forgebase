import { afterEach, describe, expect, it } from "vitest";
import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, signedIn } from "../test/helpers";
import { MockApi, makeCollection } from "../test/mockApi";
import { installOverview } from "../test/overviewMock";
import { notify } from "../toaster";

const GUIDE = /^Getting started: \d of 5 done/;

function server() {
  const mock = signedIn(new MockApi({ collections: [makeCollection("posts", [{ name: "title", type: "text" }])] }).install());
  const st = installOverview(mock, { objects: 0 });
  return { mock, st };
}

afterEach(() => localStorage.clear());

describe("JourneyGuide", () => {
  it("shows the progress and the next step on a studio page, and opens it from the menu", async () => {
    server();
    renderStudio("/storage");
    const guide = await screen.findByRole("button", { name: GUIDE });
    // Admin and a collection are done; no key, file or function yet.
    expect(guide).toHaveAccessibleName("Getting started: 2 of 5 done. Next: create an API key");
    await userEvent.click(guide);
    await userEvent.click(await screen.findByRole("menuitem", { name: /3\. Create an API key/ }));
    // The key step opens Authentication with its create sheet (?new=1), so the page behind it is aria-hidden.
    expect(await screen.findByRole("heading", { level: 1, name: "Authentication", hidden: true })).toBeInTheDocument();
  });

  it("is not on Overview, which shows the full checklist", async () => {
    server();
    renderStudio("/");
    await screen.findByRole("region", { name: "Getting started" });
    expect(screen.queryByRole("button", { name: GUIDE })).not.toBeInTheDocument();
  });

  it("moves on when a create toast fires", async () => {
    const { st } = server();
    renderStudio("/storage");
    await screen.findByRole("button", { name: /2 of 5 done/ });
    st.keys = [{ id: "k1", kind: "api_key", name: "app", owner_type: "admin", owner_id: "adm1", collection: null, scopes: ["collections:read"], created_at: "2026-11-01T00:00:00Z", expires_at: null, revoked_at: null, last_used_at: null } as never];
    act(() => void notify.created("API key app"));
    await waitFor(() => expect(screen.getByRole("button", { name: GUIDE })).toHaveAccessibleName(/3 of 5 done/));
  });

  it("stays hidden after Hide this guide", async () => {
    server();
    renderStudio("/storage");
    await userEvent.click(await screen.findByRole("button", { name: GUIDE }));
    await userEvent.click(await screen.findByRole("menuitem", { name: "Hide this guide" }));
    await waitFor(() => expect(screen.queryByRole("button", { name: GUIDE })).not.toBeInTheDocument());
    expect(localStorage.getItem("forgebase.admin.guide.hidden")).toBe("1");
  });
});

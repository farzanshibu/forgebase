/** End-to-end smoke for the Database topology screen (FB-176): one signed-in journey, by clicking through the studios. */
import { afterEach, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, seeded } from "./test/helpers";
import { installPg } from "./test/pgMock";
import { installTopology, node, simpleView, bodiesOf } from "./test/topologyMock";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const T = { timeout: 5000 };

it("Database studio -> View topology -> one server -> add a local replica -> see it streaming -> promote check -> remove", async () => {
  const mock = seeded();
  installPg(mock);
  const view = simpleView();
  view.nodes = view.nodes.slice(0, 1); // one server: the empty state
  const st = installTopology(mock, {
    view,
    // The clone finishes at once: the new replica is there, streaming, when the screen asks again.
    afterWrite: (key, body, s) => {
      if (key === "POST /nodes") s.view.nodes.push(node("r1", "control", { name: (body as { name: string }).name, state: "streaming" }));
    },
  });
  renderStudio("/database");
  const u = userEvent.setup();

  // The Database studio links to the new screen.
  await u.click(await screen.findByRole("link", { name: "View topology" }, T));
  expect(await screen.findByRole("heading", { level: 1, name: /Database topology/ }, T)).toBeInTheDocument();
  expect(screen.getByRole("tab", { name: "Database topology", selected: true })).toBeInTheDocument();

  // One server: the page teaches what a replica and a shard are, and changes nothing until you add one.
  const empty = await screen.findByRole("region", { name: "One server, and that is fine" }, T);
  expect(empty).toHaveTextContent("Nothing changes until you add one.");

  // Add a local replica.
  await u.click(within(empty).getByRole("button", { name: "Add replica" }));
  const form = await screen.findByRole("dialog", { name: "Add a replica" }, T);
  await u.type(within(form).getByLabelText(/^Name/), "standby");
  await u.click(within(form).getByRole("button", { name: "Add replica" }));
  await waitFor(() => expect(bodiesOf(st, "POST /nodes")).toHaveLength(1), T);

  // It shows up streaming, and the empty state is gone.
  const row = (await screen.findByText("standby", undefined, T)).closest("tr")!;
  expect(within(row).getByText("Streaming")).toBeInTheDocument();
  await waitFor(() => expect(screen.queryByRole("region", { name: "One server, and that is fine" })).not.toBeInTheDocument(), T);

  // Promote: the check runs and the typed name is asked for; cancel leaves everything as it was.
  await u.click(screen.getByRole("button", { name: "Actions for standby" }));
  await u.click(await screen.findByRole("menuitem", { name: /Promote/ }, T));
  const promote = await screen.findByRole("dialog", { name: "Promote standby to primary?" }, T);
  expect(await within(promote).findByText(/The primary is reachable/, undefined, T)).toBeInTheDocument();
  await u.click(within(promote).getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).not.toBeInTheDocument());
  expect(bodiesOf(st, "POST /nodes/{id}/promote")).toEqual([]);

  // Remove it with its name.
  await u.click(screen.getByRole("button", { name: "Actions for standby" }));
  await u.click(await screen.findByRole("menuitem", { name: /Remove replica/ }, T));
  const remove = await screen.findByRole("dialog", { name: "Remove replica standby?" }, T);
  await u.type(within(remove).getByLabelText(/to confirm/), "standby");
  await u.click(within(remove).getByRole("button", { name: "Remove replica" }));
  await waitFor(() => expect(screen.queryByText("standby")).not.toBeInTheDocument(), T);
  expect(mock.problems).toEqual([]);
});

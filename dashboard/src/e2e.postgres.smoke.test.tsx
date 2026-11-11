/** End-to-end smoke for the PostgreSQL Database studio (FB-155): one signed-in journey through every new screen, by clicking the tabs. */
import { afterEach, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, seeded } from "./test/helpers";
import { installPg } from "./test/pgMock";
import { confirmDialog } from "./test/dataStudioMock";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

it("backend line -> top queries -> policies -> direct login (password once) -> extensions -> SQL read from replica", async () => {
  const mock = seeded();
  const pg = installPg(mock, { logins: [] });
  renderStudio("/database");
  const u = userEvent.setup();

  // The backend line names the engine; the three PostgreSQL tabs are there.
  expect(await screen.findByLabelText("Database backend")).toHaveTextContent("PostgreSQL 18.1");
  const tabs = await screen.findByRole("tablist", { name: "Database sections" });
  await waitFor(() => expect(within(tabs).getByRole("tab", { name: "Policies" })).toBeInTheDocument());

  // Slow queries tab carries the top queries.
  await u.click(within(tabs).getByRole("tab", { name: "Slow queries" }));
  expect(await screen.findByRole("table", { name: "Top queries" })).toBeInTheDocument();

  // Policies: the widening warning.
  await u.click(within(tabs).getByRole("tab", { name: "Policies" }));
  expect(await screen.findByRole("alert")).toHaveTextContent("widens access");

  // Direct logins: create one, see the password once, then revoke it.
  await u.click(within(tabs).getByRole("tab", { name: "Direct logins" }));
  expect(await screen.findByRole("region", { name: "No direct logins yet" })).toBeInTheDocument();
  await u.click(screen.getAllByRole("button", { name: "New direct login" })[0]!);
  const form = await screen.findByRole("form", { name: "New direct login" });
  await u.type(within(form).getByLabelText(/^Name/), "bi");
  await u.type(within(form).getByLabelText(/^Acts as/), "service:reporting");
  await u.click(within(form).getByRole("button", { name: "Create login" }));
  expect(await screen.findByTestId("login-password")).toHaveTextContent("s3cret-PASSWORD-once");
  await u.click(await screen.findByRole("button", { name: "I have copied it" }));
  await waitFor(() => expect(screen.queryByTestId("login-password")).not.toBeInTheDocument());
  await u.click(await screen.findByRole("button", { name: "Revoke bi" }));
  await confirmDialog(/Revoke login fbu_bi/, "Revoke login", "bi");
  await waitFor(() => expect(pg.deleted).toEqual(["login:bi"]));

  // Extensions: managed ones have no Remove.
  await u.click(within(tabs).getByRole("tab", { name: "Extensions" }));
  const table = await screen.findByRole("table", { name: "Extensions" });
  expect(within(within(table).getByText("vector").closest("tr")!).queryByRole("button")).not.toBeInTheDocument();

  // SQL editor: tick Read from replica and run.
  await u.click(within(tabs).getByRole("tab", { name: "SQL editor" }));
  await u.click(await screen.findByRole("checkbox", { name: "Read from replica" }));
  await u.click(await screen.findByRole("button", { name: "Run" }));
  await screen.findByRole("table", { name: "Result" });
  expect(pg.sql.at(-1)).toEqual({ sql: "SELECT 1 AS one", read_from_replica: true });
  expect(mock.problems).toEqual([]);
});

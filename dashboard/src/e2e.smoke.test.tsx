/** End-to-end smoke: drives the Database Studio main flows through the real router against the OpenAPI-checked mock. */
import { afterEach, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { pick } from "./test/pick";
import { renderStudio, signedIn } from "./test/helpers";
import { MockApi } from "./test/mockApi";
import { installIdentity } from "./test/idMock";

afterEach(() => vi.unstubAllGlobals());

it("create collection -> add record -> filter -> edit cell -> index -> SQL -> slow log", async () => {
  const mock = signedIn(new MockApi().install());
  renderStudio("/database");
  const u = userEvent.setup();

  // Empty server, then design a collection.
  expect(await screen.findByRole("region", { name: "No collections yet" })).toBeInTheDocument();
  await u.click(screen.getAllByRole("button", { name: "New collection" })[0]!);
  await u.type(await screen.findByLabelText(/^Name/), "notes");
  await u.click(await screen.findByRole("button", { name: "Add field" }));
  await u.type(await screen.findByLabelText("Field 1 name"), "body");
  await u.click(await screen.findByLabelText("Field 1 required"));
  await u.click(await screen.findByRole("button", { name: "Create collection" }));
  expect(await screen.findByRole("heading", { name: "notes" })).toBeInTheDocument();

  // Add two records through the typed form (FB-100).
  for (const body of ["alpha", "beta"]) {
    await u.click((await screen.findAllByRole("button", { name: "Add record" }))[0]!);
    const form = await screen.findByRole("form", { name: "New record" });
    await u.type(within(form).getByLabelText(/^body/), body);
    await u.click(within(form).getByRole("button", { name: "Create record" }));
    await waitFor(() => expect(screen.queryByRole("form", { name: "New record" })).not.toBeInTheDocument());
  }
  expect(await screen.findByText("alpha")).toBeInTheDocument();
  expect(screen.getByText("beta")).toBeInTheDocument();

  // Filter, then edit the record in its form.
  await u.type(await screen.findByLabelText("Filter"), "body = beta");
  await u.click(await screen.findByRole("button", { name: "Apply filter" }));
  await waitFor(() => expect(screen.queryByText("alpha")).not.toBeInTheDocument());
  await u.click(await screen.findByText("beta"));
  const edit = await screen.findByRole("form", { name: /^Edit record/ });
  const input = within(edit).getByLabelText(/^body/);
  await u.clear(input);
  await u.type(input, "gamma");
  await u.click(within(edit).getByRole("button", { name: "Save record" }));
  // The edited row no longer matches the active filter; clearing it shows the new value.
  // The edit sheet closes (its exit transition releases the page's inert/scroll lock) before the page is usable again.
  await waitFor(() => expect(screen.queryByRole("form", { name: /^Edit record/ })).not.toBeInTheDocument());
  expect(await screen.findByText(/No records match this filter/)).toBeInTheDocument();
  await u.click(screen.getAllByRole("button", { name: "Clear filter" })[0]!);
  expect(await screen.findByText("gamma")).toBeInTheDocument();
  await waitFor(() => expect(document.body.style.overflow).not.toBe("hidden"));

  // Index manager.
  await u.click(within(await screen.findByRole("tablist", { name: "Collection sections" })).getByRole("tab", { name: "Indexes" }));
  await u.type(await screen.findByLabelText(/^Keys/), "body");
  await u.click(await screen.findByRole("button", { name: "Create index" }));
  expect(await screen.findByText("idx_notes_body")).toBeInTheDocument();

  // SQL editor + slow queries.
  // A collection has its own header; the studio's tabs are one click up (the breadcrumb or the nav rail).
  await u.click(screen.getAllByRole("link", { name: "Database" })[0]!);
  await u.click(await screen.findByRole("tab", { name: "SQL editor" }));
  await u.click(await screen.findByRole("button", { name: "Run" }));
  expect(await screen.findByRole("table", { name: "Result" })).toBeInTheDocument();
  await u.click(await screen.findByRole("tab", { name: "Slow queries" }));
  expect(await screen.findByText(/250\.5 ms/)).toBeInTheDocument();

  expect(mock.problems).toEqual([]);
});

it("Identity Studio: realm -> public client app -> branding preview -> publish agreement", async () => {
  const mock = signedIn(new MockApi().install());
  const st = installIdentity(mock);
  renderStudio("/identity");
  const u = userEvent.setup();
  expect(await screen.findByRole("cell", { name: "acme" })).toBeInTheDocument();

  await u.click(await screen.findByRole("tab", { name: "Client apps" }));
  await screen.findByText("app_web");
  await u.type(await screen.findByLabelText("Name"), "Mobile");
  await pick(screen.getByLabelText("Type"), "public");
  await u.click(await screen.findByRole("button", { name: "Create client app" }));
  await waitFor(() => expect(st.clients.map((c) => c.name)).toContain("Mobile"));

  await u.click(await screen.findByRole("tab", { name: "Branding" }));
  await u.type(await screen.findByLabelText("Sign-in title"), "Hello");
  expect(screen.getByLabelText("Sign-in preview")).toHaveTextContent("Hello");

  await u.click(await screen.findByRole("tab", { name: "Legal agreements" }));
  const form = await screen.findByRole("form", { name: "Publish agreement" });
  await u.type(within(form).getByLabelText("Kind"), "privacy");
  await u.type(within(form).getByLabelText("Title"), "Privacy");
  await u.type(within(form).getByLabelText("Body"), "We keep little.");
  await u.click(within(form).getByRole("button", { name: "Publish version" }));
  await waitFor(() => expect(st.agreements.map((a) => a.kind)).toContain("privacy"));
  expect(mock.problems).toEqual([]);
});

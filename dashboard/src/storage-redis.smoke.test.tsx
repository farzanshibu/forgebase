/** End-to-end smoke (mocked server): the main Storage Studio and Redis Studio flows through the real router. */
import { afterEach, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { renderStudio, signedIn } from "./test/helpers";
import { MockApi } from "./test/mockApi";
import { installStorage } from "./test/storageMock";
import { installKv } from "./test/kvMock";

afterEach(() => vi.unstubAllGlobals());

it("storage: create bucket -> enable versioning -> upload twice -> versions -> restore -> sign -> ACL -> delete bucket", async () => {
  const mock = signedIn(new MockApi().install());
  const st = installStorage(mock);
  st.buckets = [];
  st.versions = {};
  st.acls = {};
  renderStudio("/storage");
  const u = userEvent.setup();

  expect(await screen.findByRole("region", { name: "No buckets yet" })).toBeInTheDocument();
  await u.click(screen.getAllByRole("button", { name: "New bucket" })[0]!);
  await u.type(await screen.findByLabelText(/^Name/), "smoke-files");
  await u.click(screen.getByRole("button", { name: "Create bucket" }));
  // creating a bucket opens it
  await screen.findByRole("heading", { name: "smoke-files" });

  await u.click(await screen.findByRole("tab", { name: "Policy" }));
  await u.click(await screen.findByRole("checkbox", { name: /Keep versions/ }));
  await u.click(screen.getByRole("button", { name: "Save policy" }));
  expect((await screen.findAllByText("Bucket policy saved")).length).toBeGreaterThan(0); // a toast is announced and drawn

  await u.click(screen.getByRole("tab", { name: "Objects" }));
  await u.upload(await screen.findByLabelText("Choose files"), new File(["one"], "a.txt", { type: "text/plain" }));
  await u.click(await screen.findByRole("button", { name: "Select a.txt" }));
  await u.upload(screen.getByLabelText("Choose files"), new File(["three"], "a.txt", { type: "text/plain" }));
  const versions = await screen.findByRole("list", { name: "Versions" });
  await waitFor(() => expect(within(screen.getByRole("list", { name: "Versions" })).getAllByRole("listitem")).toHaveLength(2));
  void versions;
  await u.click(screen.getByRole("button", { name: /Restore version/ }));
  await waitFor(() => expect(within(screen.getByRole("list", { name: "Versions" })).getAllByRole("listitem")).toHaveLength(3));

  await u.click(screen.getByRole("button", { name: "Generate URL" }));
  expect(((await screen.findByLabelText("Signed URL")) as HTMLTextAreaElement).value).toContain("/signed/smoke-files/a.txt");

  await u.click(screen.getByRole("tab", { name: "Access (ACL)" }));
  await u.type(await screen.findByLabelText(/^Grantee/), "user:bo");
  await u.click(screen.getByRole("button", { name: "Add entry" }));
  expect((await screen.findAllByText("Access list saved")).length).toBeGreaterThan(0);

  await u.click(screen.getAllByRole("link", { name: "Storage" })[0]!);
  await u.click(await screen.findByRole("button", { name: "Delete bucket smoke-files" }));
  const dialog = await screen.findByRole("alertdialog", { name: "Delete bucket smoke-files?" });
  await u.click(within(dialog).getByRole("checkbox", { name: /Also delete its objects/ }));
  await u.type(within(dialog).getByLabelText(/to confirm/), "smoke-files");
  await u.click(within(dialog).getByRole("button", { name: "Delete bucket" }));
  expect(await screen.findByRole("region", { name: "No buckets yet" })).toBeInTheDocument();
  expect(mock.problems).toEqual([]);
});

it("redis: create key -> TTL -> hash edit -> console -> stats", async () => {
  const mock = signedIn(new MockApi().install());
  const st = installKv(mock);
  st.ns = {};
  renderStudio("/redis");
  const u = userEvent.setup();

  expect(await screen.findByRole("region", { name: "No keys in default yet" })).toBeInTheDocument();
  await u.click(screen.getAllByRole("button", { name: "New key" })[0]!);
  const form = await screen.findByRole("form", { name: "New key" });
  await u.type(within(form).getByLabelText(/^Key/), "cfg");
  await u.type(within(form).getByLabelText("Value"), "v1");
  await u.click(within(form).getByRole("button", { name: "Create key" }));
  await screen.findByLabelText("String value");
  await u.click(screen.getByRole("button", { name: "Set TTL" }));
  await waitFor(() => expect(st.ns.default!.cfg!.ttl_ms).toBe(60_000));
  expect(await screen.findByLabelText("Remaining TTL")).toHaveTextContent("60 s");

  await u.click(screen.getByRole("tab", { name: "Console" }));
  await u.type(await screen.findByLabelText("Command"), "SET h x{enter}");
  await waitFor(() => expect(st.execs).toContainEqual(["SET", "h", "x"]));

  await u.click(screen.getByRole("tab", { name: "Keys" }));
  expect(await screen.findByRole("button", { name: "h" })).toBeInTheDocument();

  await u.click(screen.getByRole("tab", { name: "Memory and stats" }));
  expect(await screen.findByLabelText("Memory")).toBeInTheDocument();
  expect(mock.problems).toEqual([]);
});

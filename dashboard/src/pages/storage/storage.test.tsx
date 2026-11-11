import { afterEach, describe, expect, it, vi } from "vitest";
import { fireEvent, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { bucket, installStorage } from "../../test/storageMock";
import { storageQuotaFor, uploadObject } from "../../api";
import { confirmDialog, toast } from "../../test/dataStudioMock";
import { pick } from "../../test/pick";

afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installStorage(mock) };
}
const rows = (name: string) => within(screen.getByRole("table", { name })).getAllByRole("row");

describe("Storage buckets", () => {
  it("lists buckets with their scope and instance totals, and asks for no quota for an administrator", async () => {
    const { mock, st } = setup();
    st.buckets.push(bucket("acme/uploads", { tenant: "acme" }));
    renderStudio("/storage");
    expect(await screen.findByRole("table", { name: "Buckets" })).toBeInTheDocument();
    expect(rows("Buckets")).toHaveLength(4);
    expect(screen.getByRole("link", { name: "media" })).toBeInTheDocument();
    expect(within(rows("Buckets")[3]!).getByText("acme")).toBeInTheDocument();
    expect(within(rows("Buckets")[1]!).getByText("Instance")).toBeInTheDocument();
    expect(await screen.findByLabelText("Storage totals")).toHaveTextContent("3 buckets · 4 objects · 5 versions");
    expect(await screen.findByText(/No quota applies to administrators/)).toBeInTheDocument();
    expect(screen.queryByRole("progressbar", { name: "Storage quota" })).not.toBeInTheDocument();
    // A-21: an administrator has no entitlements subject, so the dashboard never asks.
    expect(mock.calls.filter((c) => c.includes("/entitlements"))).toEqual([]);
  });

  it("shows a tenant-scoped total when the server reports one", async () => {
    const { mock } = setup();
    mock.custom = mock.custom.filter((c) => !c.template.endsWith("/storage/stats"));
    mock.on("GET", "/api/v1/storage/stats", () => ({ status: 200, body: { buckets: 1, objects: 2, versions: 2, logical_bytes: 10, stored_bytes: 10, open_uploads: 0, scope: "tenant", tenant: "acme" } }));
    renderStudio("/storage");
    expect(await screen.findByLabelText("Storage totals")).toHaveTextContent("Tenant acme only1 buckets");
  });

  it("shows per-bucket usage from the usage endpoint", async () => {
    const { mock } = setup();
    renderStudio("/storage");
    expect(await screen.findByLabelText("Usage of media")).toHaveTextContent("2.01 KiB · 3 objects");
    expect(await screen.findByLabelText("Usage of docs")).toHaveTextContent("300 B · 1 object");
    expect(mock.problems).toEqual([]);
  });

  it("creates a bucket from the header in a sheet and reports a refused name", async () => {
    const { st } = setup();
    renderStudio("/storage");
    await screen.findByRole("table", { name: "Buckets" });
    await userEvent.click(screen.getByRole("button", { name: "New bucket" }));
    const form = await screen.findByRole("form", { name: "New bucket" });
    await userEvent.type(within(form).getByLabelText(/^Name/), "Bad Name");
    expect(await within(form).findByText(/Use lower-case letters/)).toBeInTheDocument();
    expect(within(form).getByRole("button", { name: "Create bucket" })).toBeDisabled();
    await userEvent.clear(within(form).getByLabelText(/^Name/));
    await userEvent.type(within(form).getByLabelText(/^Name/), "ab");
    await userEvent.click(within(form).getByRole("button", { name: "Create bucket" }));
    expect(await within(form).findByRole("alert")).toBeInTheDocument();

    await userEvent.clear(within(form).getByLabelText(/^Name/));
    await userEvent.type(within(form).getByLabelText(/^Name/), "reports");
    await pick(within(form).getByLabelText("Visibility"), "public");
    await userEvent.click(within(form).getByRole("checkbox", { name: "Versioning" }));
    await userEvent.type(within(form).getByLabelText("Max object size"), "10 MiB");
    await userEvent.click(within(form).getByRole("button", { name: "Create bucket" }));
    await waitFor(() => expect(st.buckets.find((b) => b.name === "reports")).toMatchObject({ visibility: "public", versioning: true, max_object_bytes: 10 * 1024 * 1024 }));
    await toast("Bucket reports created");
  });

  it("opens the new-bucket sheet from ?new=1 (the palette's action) and sends the tenant header", async () => {
    const { mock } = setup();
    renderStudio("/storage?new=1");
    const form = await screen.findByRole("form", { name: "New bucket" });
    await userEvent.type(within(form).getByLabelText(/^Name/), "avatars");
    await userEvent.type(within(form).getByLabelText("Tenant"), "acme");
    await userEvent.click(within(form).getByRole("button", { name: "Create bucket" }));
    await waitFor(() => expect(mock.headers.some((h) => h["X-ForgeBase-Tenant"] === "acme")).toBe(true));
  });

  it("deletes a bucket only after its name is typed; a full one only with \"Also delete its objects\" in that bucket's dialog", async () => {
    const { st } = setup();
    renderStudio("/storage");
    await screen.findByRole("table", { name: "Buckets" });
    await userEvent.click(screen.getByRole("button", { name: "Delete bucket docs" }));
    const dialog = await screen.findByRole("alertdialog", { name: "Delete bucket docs?" });
    const confirm = within(dialog).getByRole("button", { name: "Delete bucket" });
    expect(confirm).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText(/to confirm/), "doc");
    expect(confirm).toBeDisabled();
    await userEvent.type(within(dialog).getByLabelText(/to confirm/), "s");
    await userEvent.click(confirm);
    expect(await within(dialog).findByText("The bucket still holds files")).toBeInTheDocument();
    expect(st.buckets).toHaveLength(2);

    await userEvent.click(within(dialog).getByRole("checkbox", { name: /Also delete its objects/ }));
    await userEvent.click(within(dialog).getByRole("button", { name: "Delete bucket" }));
    await waitFor(() => expect(st.buckets.map((b) => b.name)).toEqual(["media"]));
    // the option lives in each bucket's own dialog: media's starts unticked
    await userEvent.click(screen.getByRole("button", { name: "Delete bucket media" }));
    expect(within(await screen.findByRole("alertdialog", { name: "Delete bucket media?" })).getByRole("checkbox", { name: /Also delete its objects/ })).not.toBeChecked();
  });

  it("shows an empty state with the primary action when there are no buckets", async () => {
    const { st } = setup();
    st.buckets.length = 0;
    renderStudio("/storage");
    const region = await screen.findByRole("region", { name: "No buckets yet" });
    await userEvent.click(within(region).getByRole("button", { name: "New bucket" }));
    expect(await screen.findByRole("form", { name: "New bucket" })).toBeInTheDocument();
  });

  it("rotates the signing key only after confirmation", async () => {
    const { mock } = setup();
    mock.on("POST", "/api/v1/storage/maintenance/rotate-signing-key", () => ({ status: 204 }));
    renderStudio("/storage");
    await userEvent.click(await screen.findByRole("button", { name: "Rotate signing key" }));
    expect(mock.calls).not.toContain("post /api/v1/storage/maintenance/rotate-signing-key");
    await confirmDialog("Rotate the signing key?", "Rotate key");
    await waitFor(() => expect(mock.calls).toContain("post /api/v1/storage/maintenance/rotate-signing-key"));
  });

  it("runs the reconciler and shows its report", async () => {
    const { mock } = setup();
    renderStudio("/storage");
    await userEvent.click(await screen.findByRole("button", { name: "Run reconcile" }));
    expect(await screen.findByLabelText("Reconcile report")).toHaveTextContent("1 uploads aborted, 2 staging files deleted, 3 orphan blobs deleted, 4 unreferenced blobs deleted");
    expect(mock.calls).toContain("post /api/v1/storage/maintenance/reconcile");
  });

  it("explains when instance totals are not permitted and shows no quota", async () => {
    const { mock } = setup();
    mock.custom = mock.custom.filter((c) => !c.template.endsWith("/storage/stats"));
    mock.on("GET", "/api/v1/storage/stats", () => envelope(403, "forbidden", "storage_admin required"));
    renderStudio("/storage");
    expect(await screen.findByText(/need the storage_admin permission/)).toBeInTheDocument();
    expect(await screen.findByRole("table", { name: "Buckets" })).toBeInTheDocument();
  });
});

describe("Storage object browser", () => {
  it("browses folders, shows breadcrumbs and loads more", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media");
    expect(await screen.findByRole("table", { name: "Objects" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Open folder photos" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Select readme.txt" })).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "Open folder photos" }));
    expect(await screen.findByRole("button", { name: "Select a.png" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Select b.png" })).toBeInTheDocument();
    expect(within(screen.getByRole("navigation", { name: "Folder path" })).getByText("photos")).toBeInTheDocument();
    await userEvent.click(within(screen.getByRole("navigation", { name: "Folder path" })).getByRole("button", { name: "media" }));
    expect(await screen.findByRole("button", { name: "Select readme.txt" })).toBeInTheDocument();

    for (let i = 0; i < 120; i++) st.versions[`media/bulk/f${String(i).padStart(3, "0")}.txt`] = [{ ...st.versions["media/readme.txt"]![0]!, key: `bulk/f${String(i).padStart(3, "0")}.txt` }];
    renderStudio("/storage/b/media?prefix=bulk/");
    const more = await screen.findByRole("button", { name: "Load more" });
    await userEvent.click(more);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Load more" })).not.toBeInTheDocument());
  });

  it("reads object details from the metadata endpoint, not from a listing", async () => {
    const { mock } = setup();
    const seen: string[] = [];
    const base = globalThis.fetch;
    vi.stubGlobal("fetch", vi.fn((input: RequestInfo | URL, init?: RequestInit) => { seen.push(String(input)); return base(input, init); }));
    renderStudio("/storage/b/media?prefix=photos/&key=photos/a.png");
    const details = await screen.findByLabelText("Object details");
    expect(await within(details).findByText("2.00 KiB")).toBeInTheDocument();
    expect(seen.some((u) => u.includes("/buckets/media/meta/photos/a.png"))).toBe(true);
    expect(mock.problems).toEqual([]);
  });

  it("shows details, version history, restores a version and signs a URL for the object", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media?prefix=photos/&key=photos/a.png");
    const details = await screen.findByLabelText("Object details");
    expect(await within(details).findByText("2.00 KiB")).toBeInTheDocument();
    const versions = await within(details).findByRole("list", { name: "Versions" });
    expect(within(versions).getAllByRole("listitem")).toHaveLength(2);
    expect(within(versions).getByText("latest")).toBeInTheDocument();

    await userEvent.click(within(details).getByRole("button", { name: "Restore version v1" }));
    await toast("Version restored as the current version");
    await waitFor(() => expect(within(screen.getByRole("list", { name: "Versions" })).getAllByRole("listitem")).toHaveLength(3));
    expect(st.versions["media/photos/a.png"]![0]!.size).toBe(1024);

    await pick(within(details).getByLabelText("Expires in"), "7 days");
    await userEvent.click(within(details).getByRole("button", { name: "Generate URL" }));
    expect(((await within(details).findByLabelText("Signed URL")) as HTMLTextAreaElement).value).toContain("expires=604800");
    expect(st.signed[0]).toMatchObject({ key: "photos/a.png", method: "GET", expires_in: 604800 });
  });

  it("copies and moves an object", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media?key=readme.txt");
    const details = await screen.findByLabelText("Object details");
    const dest = within(details).getByLabelText("Destination key");
    await userEvent.clear(dest);
    await userEvent.type(dest, "copy/readme.txt");
    await userEvent.click(within(details).getByRole("button", { name: "Copy" }));
    await toast("Copied to copy/readme.txt");
    expect(st.versions["media/copy/readme.txt"]).toBeDefined();
    expect(st.versions["media/readme.txt"]).toBeDefined();

    await userEvent.clear(dest);
    await userEvent.type(dest, "moved.txt");
    await userEvent.click(within(details).getByRole("button", { name: "Move" }));
    await waitFor(() => expect(screen.queryByLabelText("Object details")).not.toBeInTheDocument());
    expect(st.versions["media/readme.txt"]).toBeUndefined();
    expect(st.versions["media/moved.txt"]).toBeDefined();
  });

  it("deletes an object: a delete marker when versioned, gone when not", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media?key=readme.txt");
    const details = await screen.findByLabelText("Object details");
    await userEvent.click(await within(details).findByRole("button", { name: "Delete object" }));
    await confirmDialog("Delete readme.txt?", "Delete object");
    expect(await within(details).findByText("delete marker")).toBeInTheDocument();
    expect(st.versions["media/readme.txt"]![0]!.delete_marker).toBe(true);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Select readme.txt" })).not.toBeInTheDocument());
  });

  it("deletes an object in an unversioned bucket and closes the panel", async () => {
    const { st } = setup();
    renderStudio("/storage/b/docs?key=guide.md");
    const details = await screen.findByLabelText("Object details");
    await userEvent.click(await within(details).findByRole("button", { name: "Delete object" }));
    await confirmDialog("Delete guide.md?", "Delete object");
    await waitFor(() => expect(screen.queryByLabelText("Object details")).not.toBeInTheDocument());
    expect(st.versions["docs/guide.md"]).toBeUndefined();
    expect(await screen.findByRole("region", { name: "This folder is empty" })).toBeInTheDocument();
  });

  it("downloads through the authenticated API", async () => {
    const { mock } = setup();
    const create = vi.fn(() => "blob:x");
    vi.stubGlobal("URL", Object.assign(URL, { createObjectURL: create, revokeObjectURL: vi.fn() }));
    const click = vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(() => undefined);
    renderStudio("/storage/b/media?key=readme.txt");
    await userEvent.click(await screen.findByRole("button", { name: "Download" }));
    await waitFor(() => expect(create).toHaveBeenCalled());
    expect(click).toHaveBeenCalled();
    expect(mock.calls).toContain("get /api/v1/storage/buckets/media/objects/readme.txt");
  });

  it("uploads a small file into the current folder with one PUT", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media?prefix=photos/");
    await screen.findByRole("button", { name: "Select a.png" });
    await userEvent.upload(screen.getByLabelText("Choose files"), new File(["hello"], "n.txt", { type: "text/plain" }));
    expect(await screen.findByRole("button", { name: "Select n.txt" })).toBeInTheDocument();
    expect(st.puts).toEqual([{ key: "photos/n.txt", size: 5, type: "text/plain" }]);
    expect(within(screen.getByRole("list", { name: "Uploads" })).getByText("done")).toBeInTheDocument();
  });

  it("uploads a large file in resumable chunks with progress", async () => {
    const { st } = setup();
    renderStudio("/storage/b/docs");
    await screen.findByRole("button", { name: "Select guide.md" });
    const big = new File([new Uint8Array(9 * 1024 * 1024)], "big.bin", { type: "application/octet-stream" });
    await userEvent.upload(screen.getByLabelText("Choose files"), big);
    await waitFor(() => expect(st.chunks).toHaveLength(3), { timeout: 5000 });
    expect(st.chunks.map((c) => c.size)).toEqual([4 * 1024 * 1024, 4 * 1024 * 1024, 1024 * 1024]);
    expect(st.puts).toHaveLength(0);
    const bar = await screen.findByLabelText("Upload big.bin");
    expect(bar).toHaveAttribute("value", String(9 * 1024 * 1024));
    expect(await screen.findByRole("button", { name: "Select big.bin" })).toBeInTheDocument();
    expect(Object.keys(st.uploads)).toHaveLength(0);
  });

  it("shows a refused upload (too large) in the transfer list", async () => {
    const { st } = setup();
    st.buckets.find((b) => b.name === "docs")!.max_object_bytes = 3;
    renderStudio("/storage/b/docs");
    await screen.findByRole("button", { name: "Select guide.md" });
    await userEvent.upload(screen.getByLabelText("Choose files"), new File(["toolong"], "x.txt"));
    expect(await screen.findByText(/object_too_large/)).toBeInTheDocument();
    expect(within(screen.getByRole("list", { name: "Uploads" })).getByText("failed")).toBeInTheDocument();
  });
});

describe("uploadObject", () => {
  it("continues from the server's offset after offset_mismatch and aborts the session on failure", async () => {
    const { st } = setup();
    st.forceOffset = 20;
    const seen: number[] = [];
    const info = await uploadObject("docs", "r.bin", new Blob([new Uint8Array(50)]), { threshold: 10, chunkSize: 20, onProgress: (s) => seen.push(s) });
    expect(info.size).toBe(50);
    expect(st.chunks.map((c) => c.offset)).toEqual([20, 40]);
    expect(seen[seen.length - 1]).toBe(50);

    const ctrl = new AbortController();
    await expect(uploadObject("docs", "z.bin", new Blob([new Uint8Array(50)]), { threshold: 10, chunkSize: 20, signal: ctrl.signal, onProgress: (sent) => sent >= 20 && ctrl.abort() })).rejects.toThrow();
    expect(Object.keys(st.uploads)).toHaveLength(0);
  });
});

describe("Storage policy, ACL and signed URLs", () => {
  it("edits the bucket policy", async () => {
    const { st } = setup();
    renderStudio("/storage/b/docs/settings");
    await pick(await screen.findByLabelText("Visibility"), "public");
    await userEvent.click(screen.getByRole("checkbox", { name: /Keep versions/ }));
    await userEvent.type(screen.getByLabelText("Max object size"), "10 MiB");
    await userEvent.type(screen.getByLabelText(/Allowed content types/), "image/png{enter}text/plain");
    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    await toast("Bucket policy saved");
    expect(st.buckets.find((b) => b.name === "docs")).toMatchObject({ visibility: "public", versioning: true, max_object_bytes: 10485760, allowed_content_types: ["image/png", "text/plain"] });
  });

  it("shows how much space the bucket uses on its policy page", async () => {
    const { mock } = setup();
    renderStudio("/storage/b/media/settings");
    expect(await screen.findByLabelText("Bucket space")).toHaveTextContent("Uses 2.01 KiB in 3 objects; 3.01 KiB across 4 versions.");
    expect(mock.problems).toEqual([]);
  });

  it("rejects an unreadable size without calling the server", async () => {
    const { mock } = setup();
    renderStudio("/storage/b/docs/settings");
    await userEvent.type(await screen.findByLabelText("Max object size"), "lots");
    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    expect(await screen.findByText(/Enter a size like/)).toBeInTheDocument();
    expect(mock.calls.some((c) => c.startsWith("patch"))).toBe(false);
  });

  it("lists, adds and removes prefix ACL entries", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media/access");
    const t = await screen.findByRole("table", { name: "ACL entries" });
    expect(within(t).getByText("role:editor")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Prefix"), "docs/");
    await userEvent.type(screen.getByLabelText(/^Grantee/), "user:bo");
    await pick(screen.getByLabelText("Permission"), "admin");
    await userEvent.click(screen.getByRole("button", { name: "Add entry" }));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "ACL entries" })).getAllByRole("row")).toHaveLength(3));
    expect(st.acls.media!.entries[1]).toEqual({ prefix: "docs/", grantee: "user:bo", permission: "admin" });

    await userEvent.click(screen.getByRole("button", { name: "Remove role:editor on photos/" }));
    await waitFor(() => expect(st.acls.media!.entries).toHaveLength(1));
    await toast("Access list saved");
  });

  it("says when the ACL is empty", async () => {
    setup();
    renderStudio("/storage/b/docs/access");
    expect(await screen.findByRole("region", { name: "No entries yet" })).toBeInTheDocument();
  });

  it("generates an upload (PUT) signed URL with the chosen expiry", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media/share");
    await userEvent.type(await screen.findByLabelText(/^Key/), "in/new.txt");
    await pick(screen.getByLabelText("Method"), "PUT");
    await pick(screen.getByLabelText("Expires in"), "5 minutes");
    await userEvent.click(screen.getByRole("button", { name: "Generate URL" }));
    expect(((await screen.findByLabelText("Signed URL")) as HTMLTextAreaElement).value).toContain("/signed/media/in/new.txt?expires=300");
    expect(st.signed[0]).toEqual({ key: "in/new.txt", method: "PUT", expires_in: 300 });
  });

  it("reports a missing bucket", async () => {
    setup();
    renderStudio("/storage/b/ghost");
    expect(await screen.findByText(/no such bucket/)).toBeInTheDocument();
  });
});

describe("Storage quota (A-21)", () => {
  it("makes no entitlements call without a subject, and asks for the caller's own or an organization's", async () => {
    const { mock } = setup();
    expect(await storageQuotaFor(null)).toBeNull();
    expect(await storageQuotaFor(undefined)).toBeNull();
    expect(mock.calls.filter((c) => c.includes("/entitlements"))).toEqual([]);
    expect(await storageQuotaFor("caller")).toBe(10 * 1024 * 1024 * 1024);
    expect(await storageQuotaFor("org:acme")).toBe(10 * 1024 * 1024 * 1024);
    expect(mock.calls.filter((c) => c.includes("/entitlements"))).toHaveLength(2);
  });
});

describe("Storage upload area and bucket rules", () => {
  it("offers a drop area that uploads dropped files", async () => {
    const { st } = setup();
    renderStudio("/storage/b/docs");
    await screen.findByRole("button", { name: "Select guide.md" });
    expect(screen.getByRole("region", { name: "Drop files here or choose files" })).toBeInTheDocument();
    const area = screen.getByLabelText("Drop files here");
    const file = new File(["dropped"], "d.txt", { type: "text/plain" });
    fireEvent.drop(area, { dataTransfer: { files: [file] } });
    expect(await screen.findByRole("button", { name: "Select d.txt" })).toBeInTheDocument();
    expect(st.puts).toEqual([{ key: "d.txt", size: 7, type: "text/plain" }]);
  });

  it("shows the bucket's tenant in its header and edits object rules on the Rules tab", async () => {
    const { mock, st } = setup();
    st.buckets.push(bucket("acme/uploads", { tenant: "acme" }));
    st.versions["acme/uploads/x.txt"] = [];
    const rules: Record<string, string | null> = {};
    mock.on("GET", "/api/v1/policies/resources/{kind}", () => ({ status: 200, body: { kind: "bucket", fields: [{ name: "owner", type: "text" }, { name: "folders", type: "json" }], operations: ["list", "view", "create", "update", "delete"], actions: [], items: [] } }));
    mock.on("PUT", "/api/v1/policies/resources/{kind}/{name}", ({ params, body }) => { Object.assign(rules, (body as { rules: Record<string, string> }).rules, { name: params.name }); return { status: 200, body: {} }; });
    renderStudio("/storage/b/acme%2Fuploads/rules");
    expect(await screen.findByText(/Tenant acme/)).toBeInTheDocument();
    expect(await screen.findByText(/An expression may also test the item/)).toBeInTheDocument();
    await pick(screen.getByLabelText("View (read one) rule"), "expr");
    await userEvent.clear(screen.getByLabelText("View (read one) expression"));
    await userEvent.type(screen.getByLabelText("View (read one) expression"), "owner = @request.auth.id");
    await userEvent.click(screen.getByRole("button", { name: "Save rules" }));
    await waitFor(() => expect(rules).toMatchObject({ view: "owner = @request.auth.id", name: "acme/uploads" }));
  });

  it("puts Use from code in the bucket header", async () => {
    setup();
    renderStudio("/storage/b/media");
    await userEvent.click(await screen.findByRole("button", { name: "Use from code" }));
    expect(await screen.findByText(/\/api\/v1\/storage\/buckets\/media/)).toBeInTheDocument();
  });
});

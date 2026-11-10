import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi, envelope } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installStorage } from "../../test/storageMock";
import { uploadObject } from "../../api";
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
  it("lists buckets with usage against the quota", async () => {
    setup();
    renderStudio("/storage");
    expect(await screen.findByRole("table", { name: "Buckets" })).toBeInTheDocument();
    expect(rows("Buckets")).toHaveLength(3);
    expect(screen.getByRole("link", { name: "media" })).toBeInTheDocument();
    expect(await screen.findByLabelText("Storage totals")).toHaveTextContent("2 buckets · 4 objects · 5 versions");
    expect(screen.getByLabelText("Quota usage")).toHaveTextContent("2.31 KiB of 10.0 GiB used (0%)");
    expect(screen.getByRole("progressbar", { name: "Storage quota" })).toHaveAttribute("aria-valuenow", "0");
  });

  it("shows per-bucket usage from the usage endpoint", async () => {
    const { mock } = setup();
    renderStudio("/storage");
    expect(await screen.findByLabelText("Usage of media")).toHaveTextContent("2.01 KiB · 3 objects");
    expect(await screen.findByLabelText("Usage of docs")).toHaveTextContent("300 B · 1 object");
    expect(mock.problems).toEqual([]);
  });

  it("creates a bucket and reports a refused name", async () => {
    const { st } = setup();
    renderStudio("/storage");
    await screen.findByRole("table", { name: "Buckets" });
    await userEvent.type(screen.getByLabelText("Name"), "ab");
    await userEvent.click(screen.getByRole("button", { name: "Create bucket" }));
    expect(await screen.findByText(/invalid_bucket_name/)).toBeInTheDocument();

    await userEvent.clear(screen.getByLabelText("Name"));
    await userEvent.type(screen.getByLabelText("Name"), "reports");
    await pick(screen.getByLabelText("Visibility"), "public");
    await userEvent.click(screen.getByLabelText("Versioning"));
    await userEvent.type(screen.getByLabelText("Max object size"), "10 MiB");
    await userEvent.click(screen.getByRole("button", { name: "Create bucket" }));
    await waitFor(() => expect(rows("Buckets")).toHaveLength(4));
    expect(st.buckets.find((b) => b.name === "reports")).toMatchObject({ visibility: "public", versioning: true, max_object_bytes: 10 * 1024 * 1024 });
  });

  it("deletes an empty bucket after confirmation, and a full one only with the force option", async () => {
    const { st } = setup();
    renderStudio("/storage");
    await screen.findByRole("table", { name: "Buckets" });
    await userEvent.click(screen.getByRole("button", { name: "Delete bucket docs" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete bucket docs" }));
    expect(await screen.findByText(/bucket_not_empty/)).toBeInTheDocument();
    expect(st.buckets).toHaveLength(2);

    await userEvent.click(screen.getByLabelText("Delete with their objects"));
    await userEvent.click(screen.getByRole("button", { name: "Delete bucket docs" }));
    await userEvent.click(screen.getByRole("button", { name: "Confirm delete bucket docs" }));
    await waitFor(() => expect(st.buckets.map((b) => b.name)).toEqual(["media"]));
    expect(screen.queryByText(/bucket_not_empty/)).not.toBeInTheDocument();
  });

  it("runs the reconciler and shows its report", async () => {
    const { mock } = setup();
    renderStudio("/storage");
    await userEvent.click(await screen.findByRole("button", { name: "Run reconcile" }));
    expect(await screen.findByLabelText("Reconcile report")).toHaveTextContent("1 uploads aborted, 2 staging files deleted, 3 orphan blobs deleted, 4 unreferenced blobs deleted");
    expect(mock.calls).toContain("post /api/v1/storage/maintenance/reconcile");
  });

  it("explains when instance totals are not permitted and shows no quota", async () => {
    const { mock, st } = setup();
    st.quota = null;
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
    expect(await within(details).findByText("Version restored as the current version.")).toBeInTheDocument();
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
    expect(await within(details).findByText("Copied to copy/readme.txt.")).toBeInTheDocument();
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
    await userEvent.click(await screen.findByRole("button", { name: "Confirm delete object" }));
    expect(await within(details).findByText("delete marker")).toBeInTheDocument();
    expect(st.versions["media/readme.txt"]![0]!.delete_marker).toBe(true);
    await waitFor(() => expect(screen.queryByRole("button", { name: "Select readme.txt" })).not.toBeInTheDocument());
  });

  it("deletes an object in an unversioned bucket and closes the panel", async () => {
    const { st } = setup();
    renderStudio("/storage/b/docs?key=guide.md");
    const details = await screen.findByLabelText("Object details");
    await userEvent.click(await within(details).findByRole("button", { name: "Delete object" }));
    await userEvent.click(await screen.findByRole("button", { name: "Confirm delete object" }));
    await waitFor(() => expect(screen.queryByLabelText("Object details")).not.toBeInTheDocument());
    expect(st.versions["docs/guide.md"]).toBeUndefined();
    expect(await screen.findByText("This folder is empty.")).toBeInTheDocument();
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
    await userEvent.click(screen.getByLabelText(/Keep versions/));
    await userEvent.type(screen.getByLabelText("Max object size"), "10 MiB");
    await userEvent.type(screen.getByLabelText(/Allowed content types/), "image/png{enter}text/plain");
    await userEvent.click(screen.getByRole("button", { name: "Save policy" }));
    expect(await screen.findByText("Saved.")).toBeInTheDocument();
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
    expect(await screen.findByRole("alert")).toHaveTextContent("Enter a size like");
    expect(mock.calls.some((c) => c.startsWith("patch"))).toBe(false);
  });

  it("lists, adds and removes prefix ACL entries", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media/access");
    const t = await screen.findByRole("table", { name: "ACL entries" });
    expect(within(t).getByText("role:editor")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Prefix"), "docs/");
    await userEvent.type(screen.getByLabelText("Grantee"), "user:bo");
    await pick(screen.getByLabelText("Permission"), "admin");
    await userEvent.click(screen.getByRole("button", { name: "Add entry" }));
    await waitFor(() => expect(within(screen.getByRole("table", { name: "ACL entries" })).getAllByRole("row")).toHaveLength(3));
    expect(st.acls.media!.entries[1]).toEqual({ prefix: "docs/", grantee: "user:bo", permission: "admin" });

    await userEvent.click(screen.getByRole("button", { name: "Remove role:editor on photos/" }));
    await waitFor(() => expect(st.acls.media!.entries).toHaveLength(1));
    expect(await screen.findByText("ACL saved.")).toBeInTheDocument();
  });

  it("says when the ACL is empty", async () => {
    setup();
    renderStudio("/storage/b/docs/access");
    expect(await screen.findByText(/No entries/)).toBeInTheDocument();
  });

  it("generates an upload (PUT) signed URL with the chosen expiry", async () => {
    const { st } = setup();
    renderStudio("/storage/b/media/share");
    await userEvent.type(await screen.findByLabelText("Key"), "in/new.txt");
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

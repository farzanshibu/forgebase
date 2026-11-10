import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installBackups } from "../../test/backupMock";
import { parseCount } from "./util";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup() {
  const mock = signedIn(new MockApi().install());
  return { mock, st: installBackups(mock) };
}

describe("System backups helpers", () => {
  it("parses retention inputs", () => {
    expect(parseCount("", 0)).toEqual({ ok: true });
    expect(parseCount(" 3 ", 0)).toEqual({ ok: true, value: 3 });
    expect(parseCount("0", 0)).toEqual({ ok: true, value: 0 });
    expect(parseCount("0", 1)).toEqual({ ok: false });
    expect(parseCount("-1", 0)).toEqual({ ok: false });
    expect(parseCount("1.5", 0)).toEqual({ ok: false });
  });
});

describe("System Studio backups", () => {
  it("is a tab next to the health page", async () => {
    setup();
    renderStudio("/system");
    await userEvent.click(await screen.findByRole("link", { name: "Backups" }));
    expect(await screen.findByRole("table", { name: "Snapshots" })).toBeInTheDocument();
  });

  it("lists snapshots with size, time and verified state, and shows the CLI restore instructions", async () => {
    const { mock } = setup();
    renderStudio("/system/backups");
    const table = await screen.findByRole("table", { name: "Snapshots" });
    const rows = within(table).getAllByRole("row");
    expect(rows).toHaveLength(3);
    expect(within(rows[1]!).getByText("2026-11-09T10-00-00Z")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("2.01 KiB")).toBeInTheDocument();
    expect(within(rows[1]!).getByText("not verified")).toBeInTheDocument();
    expect(within(rows[2]!).getByText("5.00 MiB")).toBeInTheDocument();
    expect(within(rows[2]!).getByText("before upgrade")).toBeInTheDocument();
    expect(screen.getByLabelText("Restore command")).toHaveTextContent("forgebase restore");
    expect(screen.getByText(/server stopped|Stop the server/)).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("says so when there are no snapshots", async () => {
    const { st } = setup();
    st.items = [];
    renderStudio("/system/backups");
    expect(await screen.findByText(/No snapshots yet/)).toBeInTheDocument();
  });

  it("creates a snapshot with a label and the retention", async () => {
    const { mock, st } = setup();
    renderStudio("/system/backups");
    await screen.findByRole("table", { name: "Snapshots" });
    await userEvent.type(screen.getByLabelText(/Label/), "nightly");
    await userEvent.type(screen.getByLabelText("Keep last"), "5");
    await userEvent.click(screen.getByLabelText("Include indexes"));
    await userEvent.click(screen.getByRole("button", { name: "Create snapshot" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Created snapshot 2026-11-10T01-00-00Z");
    expect(st.created).toEqual([{ label: "nightly", include_indexes: false, retention: { keep_last: 5 } }]);
    expect(await screen.findByText("nightly")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("verifies a snapshot and shows the report, including problems", async () => {
    const { mock, st } = setup();
    st.broken["2026-11-08T10-00-00Z"] = ["forgebase.db: sha256 mismatch"];
    renderStudio("/system/backups");
    const table = await screen.findByRole("table", { name: "Snapshots" });
    await userEvent.click(within(table).getByRole("button", { name: "Verify 2026-11-09T10-00-00Z" }));
    expect(await within(table).findByText("verified")).toBeInTheDocument();
    expect(await screen.findByLabelText("Warnings")).toHaveTextContent("master.key");
    await userEvent.click(within(table).getByRole("button", { name: "Verify 2026-11-08T10-00-00Z" }));
    expect(await within(table).findByText("1 problem")).toBeInTheDocument();
    expect(await screen.findByLabelText("Problems")).toHaveTextContent("sha256 mismatch");
    expect(screen.getByText(/This snapshot cannot be restored\./)).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("prunes with the retention after a confirm, and reports what went", async () => {
    const { mock, st } = setup();
    renderStudio("/system/backups");
    await screen.findByRole("table", { name: "Snapshots" });
    expect(screen.getByRole("button", { name: "Prune now" })).toBeDisabled();
    await userEvent.type(screen.getByLabelText("Keep last"), "1");
    await userEvent.type(screen.getByLabelText(/Delete older than/), "30");
    await userEvent.click(screen.getByRole("button", { name: "Prune now" }));
    expect(st.pruned).toEqual([]);
    await userEvent.click(screen.getByRole("button", { name: "Confirm prune now" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Pruned 1 snapshot: 2026-11-08T10-00-00Z");
    expect(st.pruned).toEqual([{ keep_last: 1, max_age_days: 30 }]);
    await waitFor(() => expect(within(screen.getByRole("table", { name: "Snapshots" })).getAllByRole("row")).toHaveLength(2));
    expect(mock.problems).toEqual([]);
  });

  it("rejects an invalid retention without calling the server", async () => {
    const { st } = setup();
    renderStudio("/system/backups");
    await screen.findByRole("table", { name: "Snapshots" });
    await userEvent.type(screen.getByLabelText(/Delete older than/), "0");
    expect(screen.getByRole("alert")).toHaveTextContent(/whole number/);
    expect(screen.getByRole("button", { name: "Create snapshot" })).toBeDisabled();
    expect(screen.getByRole("button", { name: "Prune now" })).toBeDisabled();
    expect(st.pruned).toEqual([]);
  });

  it("deletes a snapshot only after a confirm", async () => {
    const { mock, st } = setup();
    renderStudio("/system/backups");
    const table = await screen.findByRole("table", { name: "Snapshots" });
    const row = within(table).getAllByRole("row")[1]!;
    await userEvent.click(within(row).getByRole("button", { name: "Delete" }));
    expect(st.items).toHaveLength(2);
    await userEvent.click(within(row).getByRole("button", { name: "Confirm delete" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Deleted snapshot 2026-11-09T10-00-00Z");
    expect(st.items.map((s) => s.id)).toEqual(["2026-11-08T10-00-00Z"]);
    expect(mock.problems).toEqual([]);
  });

  it("tells a non-administrator to sign in or ask for access", async () => {
    const { st } = setup();
    st.forbidden = true;
    renderStudio("/system/backups");
    expect(await screen.findByText("Sign-in required")).toBeInTheDocument();
  });
});

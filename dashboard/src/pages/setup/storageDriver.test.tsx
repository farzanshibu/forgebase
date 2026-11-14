import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { FeatureSetting, ServerFeature } from "../../api";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installSetup, sampleFeatures } from "../../test/setupMock";
import { pick } from "../../test/pick";
import { groupSettings } from "./settingUtil";
import { DRIVER_ENV, driverOf } from "./storageDrivers";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

const setting = (key: string, label: string, o: Partial<FeatureSetting> = {}): FeatureSetting => ({
  key, label, help: `Help for ${label}.`, type: "string", unit: null, options: null, value: "", default: "", source: "default", editable: true, restart_required: true, secret: false, ...o,
});

/** The storage feature with the ADR-0040 settings the server's registry now lists, as it reports them. */
function features(driver: string, over: Record<string, Partial<FeatureSetting>> = {}): ServerFeature[] {
  const all = sampleFeatures();
  const storage = all.find((f) => f.id === "storage")!;
  const extra: FeatureSetting[] = [
    setting("storage.driver", "Storage driver", { type: "enum", options: ["local", "s3", "gcs", "azure"], value: driver, default: "local" }),
    setting("storage.s3_bucket", "S3 bucket"),
    setting("storage.s3_part_size", "S3 upload part size", { type: "int", value: 8388608, default: 8388608, unit: "bytes" }),
    setting("storage.s3_secret_key", "S3 secret key", { secret: true, editable: false, value: null, default: null }),
    setting("storage.gcs_bucket", "GCS bucket"),
    setting("storage.gcs_prefix", "GCS folder prefix"),
    setting("storage.gcs_endpoint", "GCS endpoint", { type: "url" }),
    setting("storage.gcs_credentials_json", "GCS service-account key", { secret: true, editable: false, value: null, default: null }),
    setting("storage.gcs_access_token", "GCS access token", { secret: true, editable: false, value: null, default: null }),
    setting("storage.azure_account", "Azure storage account"),
    setting("storage.azure_container", "Azure container"),
    setting("storage.azure_prefix", "Azure folder prefix"),
    setting("storage.azure_endpoint", "Azure endpoint", { type: "url" }),
    setting("storage.azure_account_key", "Azure account key", { secret: true, editable: false, value: null, default: null }),
    setting("storage.azure_client_secret", "Azure client secret", { secret: true, editable: false, value: null, default: null }),
  ].map((s) => ({ ...s, ...(over[s.key] ?? {}) }));
  storage.settings = [...storage.settings, ...extra];
  return all;
}

function setup(driver: string, over: Record<string, Partial<FeatureSetting>> = {}) {
  const mock = new MockApi().install();
  const st = installSetup(mock, { features: features(driver, over) });
  signedIn(mock);
  return { mock, st };
}

const storageCard = async () => {
  renderStudio("/system/features");
  const region = await screen.findByRole("region", { name: "Settings" });
  return within(region).getByLabelText("Object storage settings");
};

describe("storage driver settings", () => {
  it("groups the driver settings by driver and keeps the shared ones first", () => {
    const storage = features("gcs").find((f) => f.id === "storage")!;
    const groups = groupSettings(storage);
    expect(groups.map((g) => g.title)).toEqual([null, "Amazon S3 and compatible", "Google Cloud Storage", "Azure Blob Storage"]);
    expect(groups[0]!.rows.map((r) => r.key)).toEqual(["storage.max_object_bytes", "storage.allowed_types", "storage.driver", "storage.s3_part_size"]);
    expect(groups[2]!.rows.map((r) => r.key)).toEqual(["storage.gcs_bucket", "storage.gcs_prefix", "storage.gcs_endpoint", "storage.gcs_credentials_json", "storage.gcs_access_token"]);
    expect(driverOf("storage.azure_endpoint")).toBe("azure");
    expect(driverOf("storage.s3_part_size")).toBeUndefined();
    expect(driverOf("storage.max_object_bytes")).toBeUndefined();
  });

  it("lists the environment variables of each remote driver", () => {
    expect(DRIVER_ENV.gcs!.vars.map((v) => v.name)).toEqual(["FORGEBASE_STORAGE_GCS_CREDENTIALS_JSON", "FORGEBASE_STORAGE_GCS_CREDENTIALS_FILE", "FORGEBASE_STORAGE_GCS_ACCESS_TOKEN"]);
    expect(DRIVER_ENV.azure!.vars.map((v) => v.name)).toContain("FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY");
    expect(DRIVER_ENV.s3!.vars.map((v) => v.name)).toEqual(["FORGEBASE_STORAGE_S3_ACCESS_KEY", "FORGEBASE_STORAGE_S3_SECRET_KEY", "FORGEBASE_STORAGE_S3_SESSION_TOKEN"]);
  });

  it("shows the gcs driver's bucket, prefix and endpoint fields, and offers gcs and azure in the driver picker", async () => {
    setup("gcs");
    const card = await storageCard();
    const group = within(card).getByRole("region", { name: "Object storage: Google Cloud Storage" });
    expect(within(group).getByLabelText("GCS bucket")).toBeEnabled();
    expect(within(group).getByLabelText("GCS folder prefix")).toBeEnabled();
    expect(within(group).getByLabelText("GCS endpoint")).toHaveAttribute("type", "url");
    expect(within(group).getByText(/Used when the storage driver is/)).toHaveTextContent("gcs.");
    await userEvent.click(within(card).getByRole("combobox", { name: "Storage driver" }));
    expect(within(await screen.findByRole("listbox")).getAllByRole("option").map((o) => o.getAttribute("data-value"))).toEqual(["local", "s3", "gcs", "azure"]);
  });

  it("shows which environment variables to set for gcs, never takes a secret, and says which are set", async () => {
    setup("gcs", { "storage.gcs_access_token": { source: "env" } });
    const card = await storageCard();
    const guide = within(card).getByRole("region", { name: "Storage credentials" });
    expect(guide).toHaveTextContent("Credentials for Google Cloud Storage");
    expect(guide).toHaveTextContent(/Credentials are environment-only/);
    expect(guide).toHaveTextContent(/never typed into this form, stored in the database or written to the config file/);
    const list = within(guide).getByRole("list", { name: "Google Cloud Storage environment variables" });
    for (const name of ["FORGEBASE_STORAGE_GCS_CREDENTIALS_JSON", "FORGEBASE_STORAGE_GCS_CREDENTIALS_FILE", "FORGEBASE_STORAGE_GCS_ACCESS_TOKEN"]) expect(within(list).getByText(name)).toBeInTheDocument();
    expect(within(list).getByText("GOOGLE_APPLICATION_CREDENTIALS works too)", { exact: false })).toBeInTheDocument();
    const rows = within(list).getAllByRole("listitem");
    expect(within(rows[0]!).getByText("not set")).toBeInTheDocument();
    expect(within(rows[2]!).getByText("set")).toBeInTheDocument();
    expect(guide).toHaveTextContent(/metadata server/);
    expect(within(guide).getByRole("button", { name: "Copy FORGEBASE_STORAGE_GCS_CREDENTIALS_JSON" })).toBeInTheDocument();
    // The secret settings carry help and a "secret" badge, and no input.
    const key = card.querySelector("[data-setting='storage.gcs_credentials_json']") as HTMLElement;
    expect(within(key).getByText("secret")).toBeInTheDocument();
    expect(within(key).queryByRole("textbox")).not.toBeInTheDocument();
    expect(within(card).queryByLabelText("GCS service-account key")).not.toBeInTheDocument();
  });

  it("switches the guide and the group notes with the driver, and saves the driver and its fields in one PATCH", async () => {
    const { st } = setup("local", { "storage.azure_account_key": { source: "env" } });
    const card = await storageCard();
    expect(within(card).getByRole("region", { name: "Storage credentials" })).toHaveTextContent(/local.*needs no credentials/);
    await pick(within(card).getByRole("combobox", { name: "Storage driver" }), "azure");
    const guide = await within(card).findByRole("region", { name: "Storage credentials" });
    expect(guide).toHaveTextContent("Credentials for Azure Blob Storage");
    for (const name of ["FORGEBASE_STORAGE_AZURE_ACCOUNT_KEY", "FORGEBASE_STORAGE_AZURE_SAS_TOKEN", "FORGEBASE_STORAGE_AZURE_TENANT_ID", "FORGEBASE_STORAGE_AZURE_CLIENT_ID", "FORGEBASE_STORAGE_AZURE_CLIENT_SECRET"]) expect(within(guide).getByText(name)).toBeInTheDocument();
    expect(within(guide).getAllByText("set")).toHaveLength(1);
    expect(guide).toHaveTextContent(/managed identity/);
    const gcs = within(card).getByRole("region", { name: "Object storage: Google Cloud Storage" });
    expect(gcs).toHaveTextContent(/it is not the selected driver, so these are not used now/);
    await userEvent.type(within(card).getByLabelText("Azure storage account"), "myaccount");
    await userEvent.type(within(card).getByLabelText("Azure container"), "blobs");
    await userEvent.click(within(screen.getByRole("region", { name: "Save changes" })).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(st.patches).toEqual([{ set: { "storage.driver": "azure", "storage.azure_account": "myaccount", "storage.azure_container": "blobs" } }]));
  });
});

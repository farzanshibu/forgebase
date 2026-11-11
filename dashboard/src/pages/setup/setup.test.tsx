import { afterEach, describe, expect, it, vi } from "vitest";
import { screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MockApi } from "../../test/mockApi";
import { renderStudio, signedIn } from "../../test/helpers";
import { installSetup, SETUP_TOKEN, type SetupState } from "../../test/setupMock";
import { envelope } from "../../test/mockApi";
import { disabledStudioPaths } from "./features";
import { sampleFeatures } from "../../test/setupMock";

afterEach(() => {
  vi.unstubAllGlobals();
  localStorage.clear();
});

function setup(init: Partial<SetupState> = {}, signed = true) {
  const mock = new MockApi().install();
  const st = installSetup(mock, init);
  signedIn(mock, signed);
  return { mock, st };
}

const card = (id: string) => document.querySelector<HTMLElement>(`[data-feature="${id}"]`)!;

describe("first-run redirects", () => {
  it("login redirects to /setup while setup is required", async () => {
    setup({ required: true }, false);
    renderStudio("/");
    expect(await screen.findByRole("form", { name: "Create administrator" })).toBeInTheDocument();
    expect(screen.queryByRole("form", { name: "Sign in" })).not.toBeInTheDocument();
  });

  it("shows the normal login once setup is complete, and /setup anonymous goes to it", async () => {
    setup({ required: false }, false);
    renderStudio("/setup");
    expect(await screen.findByRole("form", { name: "Sign in" })).toBeInTheDocument();
  });

  it("/setup redirects a signed-in administrator to Overview after setup", async () => {
    setup({ required: false });
    renderStudio("/setup");
    expect(await screen.findByRole("navigation", { name: "Studios" })).toBeInTheDocument();
    expect(screen.queryByText("Set up ForgeBase")).not.toBeInTheDocument();
  });
});

describe("step 1: create administrator", () => {
  it("prefills the token from ?token= and signs in on success, then shows the feature map", async () => {
    const { st } = setup({ required: true }, false);
    renderStudio(`/setup?token=${SETUP_TOKEN}`);
    const form = await screen.findByRole("form", { name: "Create administrator" });
    expect(within(form).getByLabelText("Setup token")).toHaveValue(SETUP_TOKEN);
    await userEvent.type(within(form).getByLabelText("Email"), "root@example.com");
    await userEvent.type(within(form).getByLabelText("Password"), "correct-horse");
    await userEvent.type(within(form).getByLabelText("Confirm password"), "correct-horse");
    await userEvent.click(within(form).getByRole("button", { name: "Create administrator" }));
    expect(await screen.findByRole("region", { name: "Feature map" })).toBeInTheDocument();
    expect(st.created).toEqual([{ token: SETUP_TOKEN, email: "root@example.com", password: "correct-horse" }]);
    expect(screen.getByText("Administrator")).toBeInTheDocument();
  });

  it("rejects mismatched passwords without calling the server", async () => {
    const { st } = setup({ required: true }, false);
    renderStudio(`/setup?token=${SETUP_TOKEN}`);
    const form = await screen.findByRole("form", { name: "Create administrator" });
    await userEvent.type(within(form).getByLabelText("Email"), "root@example.com");
    await userEvent.type(within(form).getByLabelText("Password"), "correct-horse");
    await userEvent.type(within(form).getByLabelText("Confirm password"), "different");
    await userEvent.click(within(form).getByRole("button", { name: "Create administrator" }));
    expect(await screen.findByText("The passwords do not match")).toBeInTheDocument();
    expect(st.created).toEqual([]);
  });

  it("explains the password rule under the field", async () => {
    setup({ required: true }, false);
    renderStudio("/setup");
    const form = await screen.findByRole("form", { name: "Create administrator" });
    expect(within(form).getByLabelText("Password")).toHaveAccessibleDescription(/At least 10 characters/);
  });

  it("checks that Confirm matches when the field loses focus, and clears the message once they match", async () => {
    const { st } = setup({ required: true }, false);
    renderStudio("/setup");
    const form = await screen.findByRole("form", { name: "Create administrator" });
    await userEvent.type(within(form).getByLabelText("Password"), "correct-horse");
    const confirm = within(form).getByLabelText("Confirm password");
    await userEvent.type(confirm, "correct");
    expect(within(form).queryByText("The passwords do not match")).not.toBeInTheDocument();
    await userEvent.tab();
    expect(await within(form).findByText("The passwords do not match")).toBeInTheDocument();
    expect(confirm).toHaveAccessibleDescription("The passwords do not match");
    expect(confirm).toHaveAttribute("aria-invalid", "true");
    await userEvent.type(confirm, "-horse");
    await waitFor(() => expect(within(form).queryByText("The passwords do not match")).not.toBeInTheDocument());
    expect(st.created).toEqual([]);
  });

  it("keeps a field error out of the field's name and ties it with aria-describedby", async () => {
    setup({ required: true }, false);
    renderStudio("/setup?token=wrong");
    const form = await screen.findByRole("form", { name: "Create administrator" });
    await userEvent.type(within(form).getByLabelText("Email"), "root@example.com");
    await userEvent.type(within(form).getByLabelText("Password"), "correct-horse");
    await userEvent.type(within(form).getByLabelText("Confirm password"), "correct-horse");
    await userEvent.click(within(form).getByRole("button", { name: "Create administrator" }));
    const token = await within(form).findByLabelText("Setup token");
    expect(token).toHaveAccessibleDescription(/setup token is not valid/);
  });

  it("shows an invalid token and a weak password inline", async () => {
    setup({ required: true }, false);
    renderStudio("/setup?token=wrong");
    const form = await screen.findByRole("form", { name: "Create administrator" });
    await userEvent.type(within(form).getByLabelText("Email"), "root@example.com");
    await userEvent.type(within(form).getByLabelText("Password"), "correct-horse");
    await userEvent.type(within(form).getByLabelText("Confirm password"), "correct-horse");
    await userEvent.click(within(form).getByRole("button", { name: "Create administrator" }));
    expect(await screen.findByText(/setup token is not valid/)).toBeInTheDocument();
    await userEvent.clear(within(form).getByLabelText("Setup token"));
    await userEvent.type(within(form).getByLabelText("Setup token"), SETUP_TOKEN);
    await userEvent.clear(within(form).getByLabelText("Password"));
    await userEvent.type(within(form).getByLabelText("Password"), "short");
    await userEvent.clear(within(form).getByLabelText("Confirm password"));
    await userEvent.type(within(form).getByLabelText("Confirm password"), "short");
    await userEvent.click(within(form).getByRole("button", { name: "Create administrator" }));
    expect(await screen.findByText("must be at least 8 characters")).toBeInTheDocument();
  });
});

describe("step 2: feature map", () => {
  it("shows a card per feature with switches, always-on badges and studio links", async () => {
    setup({ required: false });
    renderStudio("/system/features");
    const map = await screen.findByRole("region", { name: "Feature map" });
    expect(within(map).getAllByRole("listitem")).toHaveLength(7);
    expect(within(card("database")).getByText("always on")).toBeInTheDocument();
    expect(within(card("server")).getByText("always on")).toBeInTheDocument();
    expect(within(card("storage")).getByRole("switch", { name: "Object storage enabled" })).toBeChecked();
    expect(within(card("billing")).getByRole("switch", { name: "Billing enabled" })).not.toBeChecked();
    expect(within(card("storage")).getByRole("link", { name: /Open Object storage studio/ })).toHaveAttribute("href", "/storage");
    expect(within(card("billing")).queryByRole("link")).not.toBeInTheDocument();
  });

  it("names the database engines with PostgreSQL first (FB-177)", async () => {
    setup({ required: false });
    renderStudio("/system/features");
    const note = await screen.findByTestId("database-engines-note");
    expect(note.textContent!.indexOf("PostgreSQL")).toBeGreaterThanOrEqual(0);
    expect(note.textContent!.indexOf("PostgreSQL")).toBeLessThan(note.textContent!.indexOf("SQLite"));
    expect(note).toHaveTextContent("forgebase db convert --to postgres");
  });

  it("makes an env-sourced switch read-only with the reason", async () => {
    setup();
    renderStudio("/system/features");
    await screen.findByRole("region", { name: "Feature map" });
    expect(within(card("resp")).getByRole("switch", { name: "Redis protocol enabled" })).toHaveAttribute("aria-disabled", "true");
    expect(within(card("resp")).getByText(/Set by an environment variable: read-only/)).toBeInTheDocument();
  });

  it("leaves out the settings of features the person switched off", async () => {
    setup();
    renderStudio("/system/features");
    await screen.findByRole("region", { name: "Feature map" });
    expect(screen.getByLabelText("Object storage settings")).toBeInTheDocument();
    await userEvent.click(within(card("storage")).getByRole("switch"));
    expect(screen.queryByLabelText("Object storage settings")).not.toBeInTheDocument();
    expect(screen.queryByText("Largest object")).not.toBeInTheDocument();
    expect(screen.getByText("Poll interval")).toBeInTheDocument();
  });

  it("shows the studio link for the saved state, not for an unsaved switch", async () => {
    setup();
    renderStudio("/system/features");
    await screen.findByRole("region", { name: "Feature map" });
    await userEvent.click(within(card("billing")).getByRole("switch"));
    expect(within(card("billing")).queryByRole("link", { name: /Open Billing studio/ })).not.toBeInTheDocument();
  });

  it("links each feature to its docs and explains the badges", async () => {
    const f = sampleFeatures();
    f.find((x) => x.id === "storage")!.docs = "docs/adr/0018-object-storage.md";
    setup({ features: f });
    renderStudio("/system/features");
    await screen.findByRole("region", { name: "Feature map" });
    expect(within(card("storage")).getByRole("link", { name: /Object storage docs/ })).toHaveAttribute("href", expect.stringContaining("docs/adr/0018-object-storage.md"));
    const tip = within(card("database")).getByRole("button", { name: /Always on/ });
    expect(tip).toHaveAccessibleDescription(/cannot be switched off/);
    expect(within(card("resp")).getByRole("button", { name: /Source: env/ })).toHaveAccessibleDescription(/environment variable/);
  });
});

const settingsRegion = async () => {
  renderStudio("/system/features");
  return screen.findByRole("region", { name: "Settings" });
};
const saveBar = () => screen.getByRole("region", { name: "Save changes" });

describe("Features and settings: settings", () => {
  it("renders typed fields with source badges, read-only env/flag fields and help-only secrets", async () => {
    setup();
    const cfg = await settingsRegion();
    expect(within(cfg).getByLabelText("Largest object")).toHaveValue("104857600");
    expect(within(cfg).getByText("bytes")).toBeInTheDocument();
    expect(within(cfg).getByLabelText("Allowed types")).toHaveValue("image/png\nimage/jpeg");
    expect(within(cfg).getByLabelText("Public URL")).toHaveAttribute("type", "url");
    const sources = Array.from(cfg.querySelectorAll("[data-setting]")).map((n) => [n.getAttribute("data-setting"), n.querySelector("[data-source]")?.getAttribute("data-source")]);
    expect(sources).toContainEqual(["storage.max_object_bytes", "default"]);
    expect(sources).toContainEqual(["storage.allowed_types", "file"]);
    expect(sources).toContainEqual(["log.level", "flag"]);
    const level = cfg.querySelector("[data-setting='log.level']") as HTMLElement;
    expect(within(level).getByRole("combobox")).toBeDisabled();
    expect(level).toHaveTextContent(/Read-only: Set by a command-line flag/);
    const secret = cfg.querySelector("[data-setting='server.master_key']") as HTMLElement;
    expect(within(secret).queryByRole("textbox")).not.toBeInTheDocument();
    expect(secret).toHaveTextContent("Set the FORGEBASE_MASTER_KEY environment variable.");
    expect(secret).toHaveTextContent("secret");
  });

  it("marks an env-sourced setting read-only", async () => {
    setup({ features: (() => { const f = sampleFeatures(); const s = f.find((x) => x.id === "resp")!.settings.find((x) => x.key === "resp.bind")!; s.source = "env"; s.editable = false; return f; })() });
    const cfg = await settingsRegion();
    const bind = cfg.querySelector("[data-setting='resp.bind']") as HTMLElement;
    expect(within(bind).getByRole("textbox")).toBeDisabled();
    expect(bind).toHaveTextContent(/Read-only: Set by an environment variable/);
    expect(within(bind).getByRole("textbox")).toHaveAccessibleDescription(/Read-only: Set by an environment variable/);
  });

  it("renders the database settings of FB-148: groups, a read-only map and a fixed setting", async () => {
    const f = sampleFeatures();
    f.find((x) => x.id === "database")!.settings = [
      { key: "database.backend", label: "Backend", help: "Which engine stores the data.", type: "enum", options: ["postgres", "sqlite"], value: "postgres", default: "postgres", source: "default", editable: false, restart_required: true, secret: false },
      { key: "database.postgres.pool_size", label: "Pool size", help: "Connections kept open.", type: "int", value: 10, default: 10, source: "default", editable: true, restart_required: true, secret: false },
      { key: "database.postgres.settings", label: "Engine settings", help: "Extra postgresql.conf lines.", type: "map", value: { max_connections: "100", shared_buffers: "256MB" }, default: {}, source: "file", editable: false, restart_required: true, secret: false },
      { key: "database.postgres.extensions.vector", label: "Vector extension", help: "pgvector.", type: "enum", options: ["auto", "on", "off"], value: "auto", default: "auto", source: "default", editable: true, restart_required: true, secret: false },
    ];
    setup({ features: f });
    const cfg = await settingsRegion();
    expect(within(cfg).getByRole("heading", { name: "PostgreSQL" })).toBeInTheDocument();
    expect(within(cfg).getByRole("heading", { name: "PostgreSQL extensions" })).toBeInTheDocument();
    const map = within(cfg).getByRole("table", { name: "Engine settings" });
    expect(within(map).getByText("max_connections")).toBeInTheDocument();
    expect(within(map).getByText("256MB")).toBeInTheDocument();
    const backend = cfg.querySelector("[data-setting='database.backend']") as HTMLElement;
    expect(within(backend).getByRole("combobox")).toBeDisabled();
    expect(backend).toHaveTextContent(/Read-only: This setting is fixed while the server runs/);
    expect(within(cfg).getByLabelText("Pool size")).toBeEnabled();
  });

  it("sends one PATCH with the changed keys only and toasts", async () => {
    const { st } = setup();
    const cfg = await settingsRegion();
    const size = within(cfg).getByLabelText("Largest object");
    await userEvent.clear(size);
    await userEvent.type(size, "2048");
    await userEvent.type(within(cfg).getByLabelText("Allowed types"), "\nimage/gif");
    // a field that is typed into and put back is not a change
    const poll = within(cfg).getByLabelText("Poll interval");
    await userEvent.type(poll, "0");
    await userEvent.type(poll, "{Backspace}");
    expect(within(saveBar()).getByText("2 unsaved changes")).toBeInTheDocument();
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(st.patches).toEqual([{ set: { "storage.max_object_bytes": 2048, "storage.allowed_types": ["image/png", "image/jpeg", "image/gif"] } }]));
    expect(await within(saveBar()).findByText("No changes")).toBeInTheDocument();
  });

  it("includes switch changes in the same PATCH", async () => {
    const { st } = setup();
    renderStudio("/system/features");
    await screen.findByRole("region", { name: "Feature map" });
    await userEvent.click(within(card("billing")).getByRole("switch"));
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(st.patches).toEqual([{ set: { "billing.enabled": true } }]));
  });

  it("keeps Save off while nothing changed, and Discard puts the values back", async () => {
    const { st } = setup();
    const cfg = await settingsRegion();
    expect(within(saveBar()).getByRole("button", { name: "Save changes" })).toBeDisabled();
    await userEvent.type(within(cfg).getByLabelText("Largest object"), "1");
    expect(within(saveBar()).getByRole("button", { name: "Save changes" })).toBeEnabled();
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Discard" }));
    expect(within(cfg).getByLabelText("Largest object")).toBeInTheDocument();
    expect(within(saveBar()).getByText("No changes")).toBeInTheDocument();
    expect(st.patches).toEqual([]);
  });

  it("blocks Save on a client-side type error and shows it inline", async () => {
    const { st } = setup();
    const cfg = await settingsRegion();
    await userEvent.type(within(cfg).getByLabelText("Largest object"), "x");
    expect(within(cfg).getByText(/Enter a whole number/)).toBeInTheDocument();
    expect(within(saveBar()).getByRole("button", { name: "Save changes" })).toBeDisabled();
    expect(st.patches).toEqual([]);
  });

  it("shows per-key validation errors inline and keeps the draft", async () => {
    const { st } = setup({ rejectKeys: { "storage.max_object_bytes": "must be at least 1024" } });
    const cfg = await settingsRegion();
    const size = within(cfg).getByLabelText("Largest object");
    await userEvent.clear(size);
    await userEvent.type(size, "5");
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    expect(await within(cfg).findByText("Largest object: must be at least 1024")).toBeInTheDocument();
    expect(size).toHaveValue("5");
    expect(size).toHaveAccessibleDescription(/must be at least 1024/);
    st.rejectKeys = {};
    await userEvent.clear(size);
    await userEvent.type(size, "4096");
    expect(within(cfg).queryByText(/must be at least 1024/)).not.toBeInTheDocument();
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    await waitFor(() => expect(st.patches).toHaveLength(2));
    expect(st.patches[1]).toEqual({ set: { "storage.max_object_bytes": 4096 } });
  });

  it("shows a refused write (409 setting_overridden)", async () => {
    const { mock } = setup();
    mock.custom.unshift({ method: "PATCH", template: "/api/system/config", handler: () => envelope(409, "setting_overridden", "storage.max_object_bytes is set by env") });
    const cfg = await settingsRegion();
    await userEvent.type(within(cfg).getByLabelText("Largest object"), "1");
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    expect(await within(cfg).findByText(/setting_overridden/)).toBeInTheDocument();
  });

  it("says a restart is needed and how", async () => {
    setup({ restartRequired: true });
    const cfg = await settingsRegion();
    await userEvent.type(within(cfg).getByLabelText("Largest object"), "1");
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    expect(await screen.findByText("Restart the server to finish")).toBeInTheDocument();
    expect(await screen.findByText(/service manager/)).toHaveTextContent("forgebase serve");
    expect(screen.getByText(/service manager/)).toHaveTextContent("/srv/forge/forgebase.toml");
  });

  it("does not ask for a restart when none is needed", async () => {
    setup();
    const cfg = await settingsRegion();
    await userEvent.type(within(cfg).getByLabelText("Largest object"), "1");
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    await within(saveBar()).findByText("No changes");
    expect(screen.queryByText("Restart the server to finish")).not.toBeInTheDocument();
  });
});

describe("first-run wizard: map, configure, done", () => {
  async function toConfigure() {
    renderStudio("/setup");
    await screen.findByRole("region", { name: "Feature map" });
    await userEvent.click(screen.getByRole("button", { name: "Next: configure" }));
    return screen.findByRole("region", { name: "Configure" });
  }

  it("sends one PATCH from the configure step and says what was saved", async () => {
    const { st } = setup({ required: true });
    const cfg = await toConfigure();
    await userEvent.type(within(cfg).getByLabelText("Largest object"), "1");
    expect(within(cfg).getByText("1 changed")).toBeInTheDocument();
    await userEvent.click(within(cfg).getByRole("button", { name: "Save and finish" }));
    const done = await screen.findByRole("region", { name: "Done" });
    expect(done).toHaveTextContent("storage.max_object_bytes");
    expect(within(done).getByText("No restart needed")).toBeInTheDocument();
    expect(st.patches).toHaveLength(1);
  });

  it("says a restart is needed and how", async () => {
    setup({ required: true, restartRequired: true });
    const cfg = await toConfigure();
    await userEvent.type(within(cfg).getByLabelText("Largest object"), "1");
    await userEvent.click(within(cfg).getByRole("button", { name: "Save and finish" }));
    const done = await screen.findByRole("region", { name: "Done" });
    expect(done).toHaveTextContent("forgebase serve");
    expect(done).toHaveTextContent("/srv/forge/forgebase.toml");
  });

  it("does not call the server when nothing changed", async () => {
    const { st } = setup({ required: true });
    const cfg = await toConfigure();
    await userEvent.click(within(cfg).getByRole("button", { name: "Finish" }));
    expect(await screen.findByText("Nothing changed, so nothing was written.")).toBeInTheDocument();
    expect(st.patches).toEqual([]);
  });

  it("carries on from the administrator step through to Overview", async () => {
    setup({ required: true }, false);
    renderStudio(`/setup?token=${SETUP_TOKEN}`);
    const form = await screen.findByRole("form", { name: "Create administrator" });
    await userEvent.type(within(form).getByLabelText("Email"), "root@example.com");
    await userEvent.type(within(form).getByLabelText("Password"), "correct-horse");
    await userEvent.type(within(form).getByLabelText("Confirm password"), "correct-horse");
    await userEvent.click(within(form).getByRole("button", { name: "Create administrator" }));
    await screen.findByRole("region", { name: "Feature map" });
    await userEvent.click(screen.getByRole("button", { name: "Next: configure" }));
    await userEvent.click(await screen.findByRole("button", { name: "Finish" }));
    await userEvent.click(await screen.findByRole("button", { name: "Go to Overview" }));
    expect(await screen.findByRole("navigation", { name: "Studios" })).toBeInTheDocument();
  });
});

describe("disabled features in the shell", () => {
  it("hides the studios of disabled features from the nav rail", async () => {
    setup();
    renderStudio("/");
    const nav = await screen.findByRole("navigation", { name: "Studios" });
    await waitFor(() => expect(within(nav).queryByText("Billing")).not.toBeInTheDocument());
    expect(within(nav).getByText("Storage")).toBeInTheDocument();
    expect(within(nav).getByText("System")).toBeInTheDocument();
    expect(within(nav).queryByText("Business")).not.toBeInTheDocument();
  });

  it("shows every studio when the feature list is not available", async () => {
    const mock = signedIn(new MockApi().install());
    renderStudio("/");
    const nav = await screen.findByRole("navigation", { name: "Studios" });
    expect(within(nav).getByText("Billing")).toBeInTheDocument();
    expect(mock.problems).toEqual([]);
  });

  it("renders a friendly disabled panel with a link to Features and settings", async () => {
    const { mock } = setup();
    mock.on("GET", "/api/v1/storage/buckets", () => envelope(404, "feature_disabled", "Object storage is disabled on this server"));
    renderStudio("/storage");
    const panel = await screen.findByRole("region", { name: "Feature disabled" });
    expect(panel).toHaveTextContent("Object storage is disabled on this server");
    await userEvent.click(within(panel).getByRole("link", { name: /Features (&|and) settings/ }));
    expect(await screen.findByRole("region", { name: "Feature map" })).toBeInTheDocument();
  });

  it("maps studios from studio_path, falling back to the module id", () => {
    const f = sampleFeatures();
    f.find((x) => x.id === "resp")!.enabled = false;
    f.find((x) => x.id === "jobs")!.enabled = false;
    f.find((x) => x.id === "jobs")!.studio_path = null;
    expect([...disabledStudioPaths(f)].sort()).toEqual(["billing", "jobs", "redis"]);
  });

  it("re-reads the nav after a save", async () => {
    setup();
    renderStudio("/system/features");
    await screen.findByRole("region", { name: "Feature map" });
    await userEvent.click(within(card("storage")).getByRole("switch"));
    await userEvent.click(within(saveBar()).getByRole("button", { name: "Save changes" }));
    const nav = screen.getByRole("navigation", { name: "Studios" });
    await waitFor(() => expect(within(nav).queryByText("Storage")).not.toBeInTheDocument());
  });
});

describe("System studio", () => {
  it("has a Features and settings tab", async () => {
    setup();
    renderStudio("/system");
    await userEvent.click(await screen.findByRole("tab", { name: "Features and settings" }));
    expect(await screen.findByRole("heading", { level: 1, name: "Features and settings" })).toBeInTheDocument();
  });
});

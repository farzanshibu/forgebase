import { cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryRouter, Route, Routes } from "react-router-dom";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { MockApi, makeCollection } from "../../test/mockApi";
import { registerAccess, type AccessState } from "./accessMock";
import { catalogue, allKeys, fromCells, toCells } from "./catalogue";
import AccessPage from "./AccessPage";

let mock: MockApi;
let st: AccessState;

function open(path: string) {
  return render(
    <MemoryRouter initialEntries={[path]}>
      <Routes><Route path="access/*" element={<AccessPage />} /></Routes>
    </MemoryRouter>,
  );
}
const rowOf = async (text: string) => (await screen.findAllByText(text))[0]!.closest("tr")!;
const override = (method: string, template: string, handler: Parameters<MockApi["on"]>[2]) => { mock.on(method, template, handler); mock.custom.unshift(mock.custom.pop()!); };
const last = (key: string) => mock.bodies[key]?.at(-1) as Record<string, unknown>;
const ORG = "/access/orgs/org_acme";

beforeEach(() => {
  mock = new MockApi({ collections: [makeCollection("posts", [{ name: "title", type: "text" }, { name: "secret", type: "text" }])] });
  st = registerAccess(mock);
  mock.install();
});
afterEach(() => { cleanup(); vi.unstubAllGlobals(); });

describe("catalogue", () => {
  it("round-trips allow/deny lists and keeps wildcard patterns as custom", () => {
    const keys = allKeys(catalogue(["posts"]));
    const { cells, customAllow, customDeny } = toCells(["users:read", "data:posts:*", "*"], ["billing:manage", "weird:thing"], keys);
    expect(cells).toEqual({ "users:read": "allow", "billing:manage": "deny" });
    expect(customAllow).toEqual(["data:posts:*", "*"]);
    expect(customDeny).toEqual(["weird:thing"]);
    expect(fromCells(cells, customAllow, customDeny)).toEqual({ allow: ["data:posts:*", "*", "users:read"], deny: ["weird:thing", "billing:manage"] });
  });
});

describe("organizations", () => {
  it("lists, creates and links to an organization", async () => {
    open("/access");
    expect(await screen.findByRole("link", { name: "acme" })).toHaveAttribute("href", "/access/orgs/org_acme");
    await userEvent.type(screen.getByLabelText("Org slug"), "globex");
    await userEvent.type(screen.getByLabelText("Org name"), "Globex");
    await userEvent.click(screen.getByRole("button", { name: "Create organization" }));
    expect(await screen.findByRole("link", { name: "globex" })).toBeInTheDocument();
    expect(last("post /api/v1/orgs")).toEqual({ slug: "globex", name: "Globex" });
  });

  it("renames and deletes an organization", async () => {
    open(ORG);
    expect(await screen.findByText("Acme Inc")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Rename to"), "Acme Corp");
    await userEvent.click(screen.getByRole("button", { name: "Rename" }));
    expect(await screen.findByText("Acme Corp")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Delete organization" }));
    await userEvent.click(screen.getByRole("button", { name: /confirm delete organization/i }));
    await waitFor(() => expect(st.orgs).toHaveLength(0));
  });

  it("shows an error state when the server refuses", async () => {
    override("GET", "/api/v1/orgs", () => ({ status: 401, body: { error: { code: "unauthenticated", message: "no", request_id: "r" } } }));
    open("/access");
    expect(await screen.findByRole("alert")).toHaveTextContent(/sign-in required/i);
  });
});

describe("projects and environments", () => {
  it("lists environments, adds one, creates and deletes a project", async () => {
    open(`${ORG}/projects`);
    expect(await screen.findByText("Web (web)")).toBeInTheDocument();
    expect(within(await screen.findByLabelText("Environments of web")).getByText("development")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("New environment in web"), "qa");
    await userEvent.click(screen.getByRole("button", { name: "Add environment" }));
    expect(await within(screen.getByLabelText("Environments of web")).findByText("qa")).toBeInTheDocument();

    await userEvent.type(screen.getByLabelText("Project slug"), "api");
    await userEvent.click(screen.getByRole("button", { name: "Create project" }));
    expect(await screen.findByText("api (api)")).toBeInTheDocument();
    expect(last("post /api/v1/orgs/{org}/projects")).toEqual({ slug: "api", name: "api" });

    await userEvent.click(screen.getAllByRole("button", { name: "Delete project" })[0]!);
    await userEvent.click(screen.getByRole("button", { name: /confirm delete project/i }));
    await waitFor(() => expect(st.projects.map((p) => p.slug)).toEqual(["api"]));
  });
});

describe("members", () => {
  it("changes roles, suspends, reactivates and removes", async () => {
    open(`${ORG}/members`);
    expect(await screen.findByText("user:users/alice")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Edit user:users/alice" }));
    expect(screen.getByLabelText("Principal")).toHaveValue("user:users/alice");
    await userEvent.click(screen.getByRole("checkbox", { name: "support" }));
    await userEvent.click(screen.getByRole("button", { name: "Save member" }));
    await waitFor(() => expect(st.members[0]!.roles).toEqual(["owner", "support"]));
    expect(last("put /api/v1/orgs/{org}/members")).toMatchObject({ principal: "user:users/alice", roles: ["owner", "support"] });

    await userEvent.click(screen.getByRole("button", { name: "Suspend user:users/alice" }));
    expect(await screen.findByRole("button", { name: "Reactivate user:users/alice" })).toBeInTheDocument();
    expect(st.members[0]!.status).toBe("suspended");
    await userEvent.click(screen.getByRole("button", { name: "Reactivate user:users/alice" }));
    await waitFor(() => expect(st.members[0]!.status).toBe("active"));

    await userEvent.click(screen.getByRole("button", { name: "Remove" }));
    await userEvent.click(screen.getByRole("button", { name: /confirm remove/i }));
    await waitFor(() => expect(st.members).toHaveLength(0));
  });

  it("adds a new member with a role set", async () => {
    open(`${ORG}/members`);
    await screen.findByText("user:users/alice");
    await userEvent.type(screen.getByLabelText("Principal"), "user:users/zed");
    await userEvent.click(await screen.findByRole("checkbox", { name: "staff" }));
    await userEvent.click(screen.getByRole("button", { name: "Save member" }));
    expect(await screen.findByText("user:users/zed")).toBeInTheDocument();
    expect(last("put /api/v1/orgs/{org}/members")).toEqual({ principal: "user:users/zed", roles: [], role_sets: ["staff"] });
  });
});

describe("teams", () => {
  it("renders the parent tree, creates a sub-team and manages members", async () => {
    open(`${ORG}/teams`);
    const tree = await screen.findByLabelText("Team tree");
    await waitFor(() => expect(tree.children).toHaveLength(1)); // only "eng" is top level
    const eng = tree.querySelector('[data-team="eng"]')!;
    expect(within(tree.children[0] as HTMLElement).getByText("Backend")).toBeInTheDocument();
    expect(eng.closest("li")!.querySelector(':scope > ul')!.querySelector('[data-team="backend"]')).not.toBeNull();
    expect(screen.getByText("deny: billing:manage")).toBeInTheDocument();

    await userEvent.type(screen.getByLabelText("Team key"), "qa");
    await userEvent.selectOptions(screen.getByLabelText("Parent team"), "backend");
    await userEvent.click(screen.getByRole("button", { name: "Create team" }));
    await waitFor(() => expect(tree.querySelector('[data-team="qa"]')).not.toBeNull());
    expect(last("post /api/v1/orgs/{org}/teams")).toMatchObject({ key: "qa", parent_id: "t_be" });

    await userEvent.type(screen.getByLabelText("Add member to backend"), "user:users/bob");
    await userEvent.click(screen.getByRole("button", { name: "Add to backend" }));
    expect(await screen.findByLabelText("Remove user:users/bob from backend")).toBeInTheDocument();
    await userEvent.click(screen.getByLabelText("Remove user:users/bob from backend"));
    await waitFor(() => expect(st.teams.find((t) => t.key === "backend")!.members).toEqual([]));

    await userEvent.click(screen.getByRole("button", { name: "Delete backend" }));
    await userEvent.click(screen.getByRole("button", { name: /confirm delete backend/i }));
    await waitFor(() => expect(st.teams.map((t) => t.key)).toEqual(["eng"])); // sub-team qa went with it
  });
});

describe("roles and the permission matrix", () => {
  const editRow = async (key: string) => {
    const row = await rowOf(key);
    await userEvent.click(within(row).getByRole("button", { name: /Edit|View/ }));
  };

  it("lists seeded and custom roles; Super Admin is not editable", async () => {
    open(`${ORG}/roles`);
    await rowOf("support");
    const sa = await rowOf("super_admin");
    expect(within(sa).getByText("not assignable")).toBeInTheDocument();
    expect(within(sa).getByRole("button", { name: "View" })).toBeInTheDocument();
    expect(within(await rowOf("owner")).getByText("seeded")).toBeInTheDocument();
    expect(within(await rowOf("owner")).queryByRole("button", { name: "Delete" })).toBeNull(); // seeded: no delete
  });

  it("toggling cells writes the right allow/deny lists and highlights deny", async () => {
    open(`${ORG}/roles`);
    await editRow("support");
    const deny = await screen.findByRole("button", { name: "billing:manage: deny" });
    expect(deny).toHaveAttribute("data-state", "deny");
    expect(deny.className).toMatch(/red/);
    expect(await screen.findByRole("button", { name: "data:posts:list: inherit" })).toBeInTheDocument();
    expect(within(screen.getByLabelText("Custom patterns")).getByText("allow data:posts:*")).toBeInTheDocument();

    await userEvent.click(screen.getByRole("button", { name: "acl:manage: inherit" })); // -> allow
    await userEvent.click(screen.getByRole("button", { name: "schema:read: inherit" })); // -> allow
    await userEvent.click(screen.getByRole("button", { name: "schema:read: allow" })); // -> deny
    await userEvent.click(screen.getByRole("button", { name: "users:read: allow" })); // -> deny
    await userEvent.click(screen.getByRole("button", { name: "users:read: deny" })); // -> inherit
    await userEvent.click(screen.getByRole("button", { name: "billing:manage: deny" })); // -> inherit
    await userEvent.click(screen.getByRole("button", { name: "Save role" }));
    await waitFor(() => expect(mock.bodies["put /api/v1/orgs/{org}/roles"]).toHaveLength(1));
    expect(last("put /api/v1/orgs/{org}/roles")).toMatchObject({ key: "support", allow: ["data:posts:*", "acl:manage"], deny: ["schema:read"] });
  });

  it("creates a custom role and deletes it", async () => {
    open(`${ORG}/roles`);
    await rowOf("support");
    await userEvent.click(screen.getByRole("button", { name: "New role" }));
    await userEvent.type(screen.getByLabelText("Key"), "auditor");
    await userEvent.click(screen.getByRole("button", { name: "org:read: inherit" }));
    await userEvent.click(screen.getByRole("button", { name: "Save role" }));
    const row = await rowOf("auditor");
    expect(last("put /api/v1/orgs/{org}/roles")).toMatchObject({ key: "auditor", allow: ["org:read"], deny: [] });
    await userEvent.click(within(row).getByRole("button", { name: "Delete" }));
    await userEvent.click(within(row).getByRole("button", { name: /confirm delete/i }));
    await waitFor(() => expect(st.roles.some((r) => r.key === "auditor")).toBe(false));
  });

  it("seeded roles are read-only for non-super-admins", async () => {
    st.superAdmin = false;
    open(`${ORG}/roles`);
    await editRow("owner");
    expect(screen.queryByRole("button", { name: "Save role" })).toBeNull();
    expect(await screen.findByRole("button", { name: "org:read: inherit" })).toBeDisabled();
  });

  it("shows the escalation-guard refusal", async () => {
    st.rolePutForbidden = "you cannot grant a permission you do not hold";
    open(`${ORG}/roles`);
    await editRow("support");
    await userEvent.click(await screen.findByRole("button", { name: "Save role" }));
    expect(await screen.findByRole("alert")).toHaveTextContent("you cannot grant a permission you do not hold");
    expect(screen.getByRole("alert")).toHaveTextContent("step permission");
  });
});

describe("role sets", () => {
  it("lists, edits and creates role sets", async () => {
    open(`${ORG}/role-sets`);
    expect(await screen.findByText("Staff")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Set key"), "ops");
    await userEvent.click(await screen.findByRole("checkbox", { name: "owner" }));
    await userEvent.click(screen.getByRole("button", { name: "Save role set" }));
    expect(await rowOf("ops")).toBeInTheDocument();
    expect(last("put /api/v1/orgs/{org}/role-sets")).toEqual({ key: "ops", name: "ops", roles: ["owner"] });
    await userEvent.click(screen.getByRole("button", { name: "Edit staff" }));
    expect(screen.getByLabelText("Set name")).toHaveValue("Staff");
    expect(screen.getByRole("checkbox", { name: "support" })).toBeChecked();
  });
});

describe("invitations", () => {
  it("shows the one-time token once", async () => {
    open(`${ORG}/invitations`);
    expect(await screen.findByText("new@acme.test")).toBeInTheDocument();
    await userEvent.type(screen.getByLabelText("Email"), "kim@acme.test");
    await userEvent.click(await screen.findByRole("checkbox", { name: "support" }));
    await userEvent.click(screen.getByRole("button", { name: "Send invitation" }));
    const box = await screen.findByRole("status", { name: "Invitation token" });
    expect(box).toHaveTextContent("tok_one_time_secret");
    expect(last("post /api/v1/orgs/{org}/invitations")).toEqual({ email: "kim@acme.test", roles: ["support"] });
    expect(await screen.findByText("kim@acme.test")).toBeInTheDocument(); // listed, but without a token
    await userEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByText("tok_one_time_secret")).toBeNull();
  });
});

describe("user overrides", () => {
  it("highlights deny rows, adds and removes overrides", async () => {
    open(`${ORG}/overrides`);
    const deny = (await screen.findByText("user:users/bob")).closest("tr")!;
    const allow = screen.getByText("user:users/carol").closest("tr")!;
    expect(deny).toHaveAttribute("data-effect", "deny");
    expect(deny.className).toMatch(/red/);
    expect(allow.className).not.toMatch(/red/);

    await userEvent.type(screen.getByLabelText("Override principal"), "user:users/zed");
    await userEvent.type(screen.getByLabelText("Override permission"), "teams:delete");
    await userEvent.click(screen.getByRole("button", { name: "Add override" }));
    const added = (await screen.findByText("user:users/zed")).closest("tr")!;
    expect(added).toHaveAttribute("data-effect", "deny"); // default effect
    expect(last("post /api/v1/overrides")).toEqual({ org_id: "org_acme", principal: "user:users/zed", permission: "teams:delete", effect: "deny" });

    await userEvent.click(within(deny).getByRole("button", { name: "Delete" }));
    await userEvent.click(within(deny).getByRole("button", { name: /confirm delete/i }));
    await waitFor(() => expect(st.overrides.some((o) => o.id === "ov1")).toBe(false));
  });
});

describe("resource ACL", () => {
  it("loads a resource, highlights deny, adds and removes entries", async () => {
    open(`${ORG}/acl`);
    await userEvent.type(await screen.findByLabelText("Resource id"), "prj_web");
    await userEvent.click(screen.getByRole("button", { name: "Load ACL" }));
    const deny = (await screen.findByText("user:users/dave")).closest("tr")!;
    expect(deny).toHaveAttribute("data-effect", "deny");
    expect(deny.className).toMatch(/red/);
    expect(screen.getByText("user:users/erin").closest("tr")!.className).not.toMatch(/red/);

    await userEvent.type(screen.getByLabelText("Grantee"), "user:users/zed");
    await userEvent.type(screen.getByLabelText("ACL permission"), "projects:share");
    await userEvent.selectOptions(screen.getByLabelText("ACL effect"), "deny");
    await userEvent.click(screen.getByRole("button", { name: "Add ACL entry" }));
    expect(await screen.findByText("user:users/zed")).toBeInTheDocument();
    expect(last("post /api/v1/acl")).toEqual({ resource_type: "project", resource_id: "prj_web", grantee: "user:users/zed", permission: "projects:share", effect: "deny" });

    await userEvent.click(within(deny).getByRole("button", { name: "Remove" }));
    await userEvent.click(within(deny).getByRole("button", { name: /confirm remove/i }));
    await waitFor(() => expect(screen.queryByText("user:users/dave")).toBeNull());
  });

  it("explains an empty ACL", async () => {
    open(`${ORG}/acl`);
    await userEvent.type(await screen.findByLabelText("Resource id"), "nothing");
    await userEvent.click(screen.getByRole("button", { name: "Load ACL" }));
    expect(await screen.findByText(/No ACL entries on project nothing/)).toBeInTheDocument();
  });
});

describe("realm level", () => {
  it("manages realms, bindings and suspensions", async () => {
    open("/access/realm");
    await userEvent.type(await screen.findByLabelText("Realm id"), "emea");
    await userEvent.click(screen.getByRole("button", { name: "Create realm" }));
    await waitFor(() => expect(st.realms.map((r) => r.id)).toEqual(["default", "emea"]));
    expect(last("post /api/v1/realms")).toEqual({ slug: "emea", name: "emea" });

    await userEvent.click(await screen.findByRole("button", { name: "Edit user:users/root" }));
    expect(screen.getByLabelText("Binding principal")).toHaveValue("user:users/root");
    await userEvent.click(screen.getByRole("button", { name: "Save binding" }));
    await waitFor(() => expect(last("put /api/v1/realm-bindings")).toMatchObject({ principal: "user:users/root", roles: ["owner"] }));

    const row = (await screen.findByText("user:users/mallory")).closest("tr")!;
    expect(row.className).toMatch(/red/);
    await userEvent.click(within(row).getByRole("button", { name: "Lift" }));
    await userEvent.click(within(row).getByRole("button", { name: /confirm lift/i }));
    await waitFor(() => expect(st.suspensions).toHaveLength(0));
    await userEvent.type(screen.getByLabelText("Suspend principal"), "user:users/eve");
    await userEvent.type(screen.getByLabelText("Reason"), "fraud");
    await userEvent.click(screen.getByRole("button", { name: "Suspend account" }));
    expect(await screen.findByText("user:users/eve")).toBeInTheDocument();
    expect(last("post /api/v1/suspensions")).toEqual({ principal: "user:users/eve", reason: "fraud", until: null });
  });
});

describe("collection rules", () => {
  const pick = async () => {
    open("/access/rules");
    await userEvent.selectOptions(await screen.findByLabelText("Collection"), "posts");
    await screen.findByText("Rules of posts");
  };

  it("loads rules: null is Locked, empty is Everyone, text is an expression", async () => {
    await pick();
    expect(screen.getByLabelText("List mode")).toHaveValue("open");
    expect(screen.getByLabelText("Update mode")).toHaveValue("none");
    expect(screen.getByLabelText("Create mode")).toHaveValue("expr");
    expect(screen.getByLabelText("Create expression")).toHaveValue('@request.auth.id != ""');
    expect(screen.getByLabelText("secret read expression")).toHaveValue('@request.auth.type = "admin"');
  });

  it("saves edited rules and field rules", async () => {
    await pick();
    await userEvent.selectOptions(screen.getByLabelText("List mode"), "none");
    await userEvent.selectOptions(screen.getByLabelText("Update mode"), "expr");
    const upd = screen.getByLabelText("Update expression");
    await userEvent.clear(upd);
    await userEvent.type(upd, "owner = @request.auth.id");
    await userEvent.selectOptions(screen.getByLabelText("title write mode"), "open");
    await userEvent.click(screen.getByRole("button", { name: "Save rules" }));
    expect(await screen.findByRole("status")).toHaveTextContent("Saved");
    expect(last("put /api/v1/policies/{collection}")).toEqual({
      list_rule: null, view_rule: "", create_rule: '@request.auth.id != ""', update_rule: "owner = @request.auth.id", delete_rule: null,
      fields: { secret: { read: '@request.auth.type = "admin"', write: null }, title: { read: null, write: "" } },
    });
  });

  it("shows the server's validation error for a bad rule", async () => {
    await pick();
    await userEvent.selectOptions(screen.getByLabelText("Delete mode"), "expr");
    const del = screen.getByLabelText("Delete expression");
    await userEvent.clear(del);
    await userEvent.type(del, "nosuchfield = 1");
    await userEvent.click(screen.getByRole("button", { name: "Save rules" }));
    const alert = await screen.findByRole("alert");
    expect(alert).toHaveTextContent("validation_failed");
    expect(alert).toHaveTextContent("nosuchfield");
    expect(screen.queryByText("Saved")).toBeNull();
  });

  it("locks all rules with the delete action", async () => {
    await pick();
    await userEvent.click(screen.getByRole("button", { name: "Lock all rules" }));
    await userEvent.click(screen.getByRole("button", { name: /confirm lock all rules/i }));
    await waitFor(() => expect(st.policies.posts).toBeUndefined());
    await waitFor(() => expect(screen.getByLabelText("List mode")).toHaveValue("none"));
  });
});

describe("rule tester", () => {
  it("shows allowed, and denied with the failed step", async () => {
    open("/access/tester");
    await userEvent.click(await screen.findByRole("button", { name: "Run check" }));
    const ok = await screen.findByRole("status", { name: "Check result" });
    expect(ok).toHaveTextContent("Allowed");
    expect(last("post /api/v1/authz/check")).toEqual({ permission: "data:posts:list" });

    const perm = screen.getByLabelText("Permission");
    await userEvent.clear(perm);
    await userEvent.type(perm, "data:denied:update");
    await userEvent.type(screen.getByLabelText("Principal"), "user:users/bob");
    await userEvent.type(screen.getByLabelText("Organization"), "acme");
    await userEvent.type(screen.getByLabelText("Resource type"), "project");
    await userEvent.type(screen.getByLabelText("Resource id"), "prj_web");
    await userEvent.click(screen.getByRole("button", { name: "Run check" }));
    await waitFor(() => expect(screen.getByRole("status", { name: "Check result" })).toHaveTextContent("Denied"));
    expect(screen.getByRole("status", { name: "Check result" })).toHaveTextContent("Failed step: permission");
    expect(last("post /api/v1/authz/check")).toEqual({ permission: "data:denied:update", org: "acme", resource_type: "project", resource_id: "prj_web", principal: "user:users/bob" });
  });
});

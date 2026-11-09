import { useState } from "react";
import { Link, NavLink, Navigate, Route, Routes, useNavigate, useParams } from "react-router-dom";
import { createOrg, deleteOrg, getOrg, listOrgs, renameOrg } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import RolesPanel from "./RolesPanel";
import RoleSetsPanel from "./RoleSetsPanel";
import { InvitationsTab, MembersTab, OverridesTab, ProjectsTab, TeamsTab } from "./OrgTabs";
import AclPanel from "./AclPanel";
import { ActionError, Empty, TextField } from "./shared";
import { useRun } from "./util";

export function OrgList() {
  const orgs = useLoad(listOrgs, "orgs");
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const { run, error } = useRun(() => { orgs.reload(); setSlug(""); setName(""); });
  return (
    <div className="space-y-4">
      <ErrorView error={orgs.error} />
      {orgs.loading && !orgs.error && <Loading what="Loading organizations" />}
      {orgs.data?.length === 0 && <Empty>No organizations yet.</Empty>}
      {orgs.data && orgs.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Slug</th><th>Name</th><th>Realm</th></tr></thead>
          <tbody>
            {orgs.data.map((o) => (
              <tr key={o.id} className="border-t border-zinc-800">
                <td className="py-2"><Link className="text-amber-300 hover:underline" to={`/access/orgs/${encodeURIComponent(o.id)}`}>{o.slug}</Link></td>
                <td>{o.name}</td><td>{o.realm_id}</td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="New organization">
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Org slug" value={slug} onChange={setSlug} />
          <TextField label="Org name" value={name} onChange={setName} />
          <Button tone="primary" disabled={!slug} onClick={() => void run(() => createOrg(slug, name || slug))}>Create organization</Button>
        </div>
      </Panel>
      <ActionError error={error} />
    </div>
  );
}

const TABS = [
  ["projects", "Projects"], ["members", "Members"], ["teams", "Teams"], ["roles", "Roles"], ["role-sets", "Role sets"],
  ["invitations", "Invitations"], ["overrides", "Overrides"], ["acl", "Resource ACL"],
] as const;

export function OrgDetail() {
  const { org = "" } = useParams();
  const nav = useNavigate();
  const o = useLoad((s) => getOrg(org, s), `org:${org}`);
  const [name, setName] = useState("");
  const { run, error } = useRun(() => o.reload());
  return (
    <div className="space-y-4">
      <Link to="/access" className="text-sm text-zinc-400 hover:text-zinc-100">← Organizations</Link>
      <ErrorView error={o.error} />
      {o.loading && !o.error && <Loading what="Loading organization" />}
      {o.data && (
        <>
          <div className="flex flex-wrap items-end justify-between gap-3">
            <h2 className="text-xl font-semibold">{o.data.name} <span className="font-mono text-sm text-zinc-500">{o.data.slug}</span></h2>
            <div className="flex items-end gap-2">
              <TextField label="Rename to" value={name} onChange={setName} />
              <Button disabled={!name} onClick={() => void run(async () => { await renameOrg(org, name); setName(""); })}>Rename</Button>
              <ConfirmButton label="Delete organization" onConfirm={() => void run(async () => { await deleteOrg(org); nav("/access"); })} />
            </div>
          </div>
          <ActionError error={error} />
          <nav aria-label="Organization sections" className="flex flex-wrap gap-1 border-b border-zinc-800">
            {TABS.map(([p, label]) => (
              <NavLink key={p} to={`/access/orgs/${encodeURIComponent(org)}/${p}`}
                className={({ isActive }) => `-mb-px border-b-2 px-3 py-2 text-sm ${isActive ? "border-amber-400 text-white" : "border-transparent text-zinc-400 hover:text-zinc-100"}`}>
                {label}
              </NavLink>
            ))}
          </nav>
          <Routes>
            <Route index element={<Navigate to="projects" replace />} />
            <Route path="projects" element={<ProjectsTab org={org} />} />
            <Route path="members" element={<MembersTab org={org} />} />
            <Route path="teams" element={<TeamsTab org={org} />} />
            <Route path="roles" element={<RolesPanel org={org} />} />
            <Route path="role-sets" element={<RoleSetsPanel org={org} />} />
            <Route path="invitations" element={<InvitationsTab org={org} />} />
            <Route path="overrides" element={<OverridesTab org={org} />} />
            <Route path="acl" element={<AclPanel />} />
          </Routes>
        </>
      )}
    </div>
  );
}

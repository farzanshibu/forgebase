import { useState } from "react";
import {
  addOverride, addTeamMember, createEnvironment, createProject, createTeam, deleteOverride, deleteProject, deleteTeam, getProject,
  invite, listInvitations, listMembers, listOverrides, listProjects, listRoles, listRoleSets, listTeams, putMember, removeMember,
  removeTeamMember, setMemberStatus, type Effect, type Team,
} from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, EffectBadge, MultiPick, TextField } from "./shared";
import { rowCls, useRun } from "./util";

// ---- Projects & environments -------------------------------------------------------

export function ProjectsTab({ org }: { org: string }) {
  const l = useLoad((s) => listProjects(org, s), `projects:${org}`);
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const { run, error } = useRun(() => { l.reload(); setSlug(""); setName(""); });
  return (
    <div className="space-y-4">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading projects" />}
      {l.data?.length === 0 && <Empty>No projects yet.</Empty>}
      {l.data?.map((p) => <ProjectRow key={p.id} id={p.id} onChanged={l.reload} />)}
      <Panel title="New project">
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Project slug" value={slug} onChange={setSlug} />
          <TextField label="Project name" value={name} onChange={setName} />
          <Button tone="primary" disabled={!slug} onClick={() => void run(() => createProject(org, slug, name || slug))}>Create project</Button>
        </div>
      </Panel>
      <ActionError error={error} />
    </div>
  );
}

function ProjectRow({ id, onChanged }: { id: string; onChanged: () => void }) {
  const p = useLoad((s) => getProject(id, s), `project:${id}`);
  const [env, setEnv] = useState("");
  const { run, error } = useRun(() => { p.reload(); setEnv(""); });
  if (!p.data) return p.error ? <ErrorView error={p.error} /> : <Loading what="Loading project" />;
  return (
    <Panel
      title={`${p.data.name} (${p.data.slug})`}
      actions={<ConfirmButton label="Delete project" onConfirm={() => void run(async () => { await deleteProject(id); onChanged(); })} />}
    >
      <ul aria-label={`Environments of ${p.data.slug}`} className="mb-3 flex flex-wrap gap-2 text-sm">
        {(p.data.environments ?? []).map((e) => <li key={e.id} className="rounded bg-zinc-800 px-2 py-0.5">{e.name}</li>)}
      </ul>
      <div className="flex items-end gap-2">
        <TextField label={`New environment in ${p.data.slug}`} value={env} onChange={setEnv} />
        <Button disabled={!env} onClick={() => void run(() => createEnvironment(id, env))}>Add environment</Button>
      </div>
      <ActionError error={error} />
    </Panel>
  );
}

// ---- Members ---------------------------------------------------------------------------

export function MembersTab({ org }: { org: string }) {
  const members = useLoad((s) => listMembers(org, s), `members:${org}`);
  const roles = useLoad((s) => listRoles(org, s), `roles:${org}`);
  const sets = useLoad((s) => listRoleSets(org, s), `sets:${org}`);
  const [principal, setPrincipal] = useState("");
  const [r, setR] = useState<string[]>([]);
  const [rs, setRs] = useState<string[]>([]);
  const { run, error } = useRun(() => { members.reload(); setPrincipal(""); setR([]); setRs([]); });
  return (
    <div className="space-y-4">
      <ErrorView error={members.error} />
      {members.loading && !members.error && <Loading what="Loading members" />}
      {members.data?.length === 0 && <Empty>No members.</Empty>}
      {members.data && members.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Principal</th><th>Roles</th><th>Role sets</th><th>Status</th><th /></tr></thead>
          <tbody>
            {members.data.map((m) => (
              <tr key={m.id} className={rowCls(m.status !== "active")}>
                <td className="py-2 font-mono">{m.principal}</td><td>{m.roles.join(", ")}</td><td>{m.role_sets.join(", ")}</td><td>{m.status}</td>
                <td className="space-x-2 text-right">
                  <Button onClick={() => { setPrincipal(m.principal); setR(m.roles); setRs(m.role_sets); }}>Edit {m.principal}</Button>
                  <Button onClick={() => void run(() => setMemberStatus(org, m.principal, m.status !== "active"))}>{m.status === "active" ? "Suspend" : "Reactivate"} {m.principal}</Button>
                  <ConfirmButton label="Remove" onConfirm={() => void run(() => removeMember(org, m.principal))} />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="Add or change a member">
        <TextField label="Principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" />
        <div className="mt-3 space-y-3">
          <MultiPick label="Roles" options={(roles.data ?? []).filter((x) => x.assignable).map((x) => x.key)} value={r} onChange={setR} />
          <MultiPick label="Role sets" options={(sets.data ?? []).map((x) => x.key)} value={rs} onChange={setRs} />
        </div>
        <div className="mt-3"><Button tone="primary" disabled={!principal} onClick={() => void run(() => putMember(org, principal, r, rs))}>Save member</Button></div>
      </Panel>
      <ActionError error={error} />
    </div>
  );
}

// ---- Teams (tree) --------------------------------------------------------------------

export function TeamsTab({ org }: { org: string }) {
  const teams = useLoad((s) => listTeams(org, s), `teams:${org}`);
  const [key, setKey] = useState("");
  const [name, setName] = useState("");
  const [parent, setParent] = useState("");
  const { run, error } = useRun(() => { teams.reload(); setKey(""); setName(""); });
  const all = teams.data ?? [];
  const children = (id: string | null) => all.filter((t) => (t.parent_id ?? null) === id);
  const node = (t: Team) => (
    <li key={t.id} className="mt-2">
      <TeamCard team={t} onChange={teams.reload} />
      {children(t.id).length > 0 && <ul className="ml-6 border-l border-zinc-800 pl-3">{children(t.id).map(node)}</ul>}
    </li>
  );
  return (
    <div className="space-y-4">
      <ErrorView error={teams.error} />
      {teams.loading && !teams.error && <Loading what="Loading teams" />}
      {teams.data?.length === 0 && <Empty>No teams yet.</Empty>}
      <ul aria-label="Team tree">{children(null).map(node)}</ul>
      <Panel title="New team">
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Team key" value={key} onChange={setKey} />
          <TextField label="Team name" value={name} onChange={setName} />
          <label className="flex flex-col gap-1 text-xs text-zinc-400">Parent team
            <select className={inputCls} value={parent} onChange={(e) => setParent(e.target.value)}>
              <option value="">(top level)</option>
              {all.map((t) => <option key={t.id} value={t.id}>{t.key}</option>)}
            </select>
          </label>
          <Button tone="primary" disabled={!key} onClick={() => void run(() => createTeam(org, { key, name: name || key, parent_id: parent || null }))}>Create team</Button>
        </div>
      </Panel>
      <ActionError error={error} />
    </div>
  );
}

function TeamCard({ team, onChange }: { team: Team; onChange: () => void }) {
  const [who, setWho] = useState("");
  const { run, error } = useRun(() => { onChange(); setWho(""); });
  return (
    <div className="rounded-md border border-zinc-800 p-3 text-sm" data-team={team.key}>
      <div className="flex items-center justify-between">
        <span><span className="font-medium">{team.name}</span> <span className="font-mono text-xs text-zinc-500">{team.key}</span>
          {team.roles.length > 0 && <span className="ml-2 text-xs text-zinc-400">roles: {team.roles.join(", ")}</span>}
          {team.deny.length > 0 && <span className="ml-2 rounded bg-red-500/20 px-1 text-xs text-red-300">deny: {team.deny.join(", ")}</span>}
        </span>
        <ConfirmButton label={`Delete ${team.key}`} onConfirm={() => void run(() => deleteTeam(team.id))} />
      </div>
      <ul className="mt-2 flex flex-wrap gap-2">
        {team.members.map((m) => (
          <li key={m} className="flex items-center gap-1 rounded bg-zinc-800 px-2 py-0.5 font-mono text-xs">
            {m}<button type="button" aria-label={`Remove ${m} from ${team.key}`} className="text-zinc-400 hover:text-red-300" onClick={() => void run(() => removeTeamMember(team.id, m))}>×</button>
          </li>
        ))}
      </ul>
      <div className="mt-2 flex items-center gap-2">
        <input aria-label={`Add member to ${team.key}`} className={inputCls} placeholder="user:users/abc" value={who} onChange={(e) => setWho(e.target.value)} />
        <Button disabled={!who} onClick={() => void run(() => addTeamMember(team.id, who))}>Add to {team.key}</Button>
      </div>
      <ActionError error={error} />
    </div>
  );
}

// ---- Invitations ------------------------------------------------------------------

export function InvitationsTab({ org }: { org: string }) {
  const inv = useLoad((s) => listInvitations(org, s), `inv:${org}`);
  const roles = useLoad((s) => listRoles(org, s), `roles:${org}`);
  const [email, setEmail] = useState("");
  const [picked, setPicked] = useState<string[]>([]);
  const [token, setToken] = useState<string>();
  const { run, error } = useRun(() => { inv.reload(); setEmail(""); setPicked([]); });
  return (
    <div className="space-y-4">
      <ErrorView error={inv.error} />
      {inv.loading && !inv.error && <Loading what="Loading invitations" />}
      {inv.data?.length === 0 && <Empty>No pending invitations.</Empty>}
      {inv.data && inv.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Email</th><th>Roles</th><th>Status</th><th>Expires</th></tr></thead>
          <tbody>{inv.data.map((i) => <tr key={i.id} className="border-t border-zinc-800"><td className="py-2">{i.email}</td><td>{i.roles.join(", ")}</td><td>{i.status ?? "pending"}</td><td>{String(i.expires_at)}</td></tr>)}</tbody>
        </table>
      )}
      <Panel title="Invite">
        <TextField label="Email" value={email} onChange={setEmail} />
        <div className="mt-3"><MultiPick label="Roles" options={(roles.data ?? []).filter((r) => r.assignable).map((r) => r.key)} value={picked} onChange={setPicked} /></div>
        <div className="mt-3"><Button tone="primary" disabled={!email} onClick={() => void run(async () => { const v = await invite(org, email, picked); setToken(v.token); })}>Send invitation</Button></div>
      </Panel>
      {token && (
        <div role="status" aria-label="Invitation token" className="rounded-lg border border-amber-700/60 bg-amber-500/10 p-4 text-sm">
          <p className="text-amber-300">One-time token — copy it now, it is not shown again.</p>
          <p className="mt-1 break-all font-mono">{token}</p>
          <Button className="mt-2" onClick={() => setToken(undefined)}>Dismiss</Button>
        </div>
      )}
      <ActionError error={error} />
    </div>
  );
}

// ---- User overrides ---------------------------------------------------------------

export function OverridesTab({ org }: { org: string }) {
  const l = useLoad((s) => listOverrides(org, s), `overrides:${org}`);
  const [principal, setPrincipal] = useState("");
  const [permission, setPermission] = useState("");
  const [effect, setEffect] = useState<Effect>("deny");
  const { run, error } = useRun(() => { l.reload(); setPrincipal(""); setPermission(""); });
  return (
    <div className="space-y-4">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading overrides" />}
      {l.data?.length === 0 && <Empty>No user overrides.</Empty>}
      {l.data && l.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Principal</th><th>Permission</th><th>Effect</th><th /></tr></thead>
          <tbody>
            {l.data.map((o) => (
              <tr key={o.id} data-effect={o.effect} className={rowCls(o.effect === "deny")}>
                <td className="py-2 font-mono">{o.principal}</td><td className="font-mono">{o.permission}</td><td><EffectBadge effect={o.effect} /></td>
                <td className="text-right"><ConfirmButton label="Delete" onConfirm={() => void run(() => deleteOverride(o.id))} /></td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      <Panel title="Add override">
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Override principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" />
          <TextField label="Override permission" value={permission} onChange={setPermission} placeholder="projects:delete" />
          <label className="flex flex-col gap-1 text-xs text-zinc-400">Effect
            <select className={inputCls} value={effect} onChange={(e) => setEffect(e.target.value as Effect)}><option value="allow">allow</option><option value="deny">deny</option></select>
          </label>
          <Button tone="primary" disabled={!principal || !permission} onClick={() => void run(() => addOverride({ org_id: org, principal, permission, effect }))}>Add override</Button>
        </div>
      </Panel>
      <ActionError error={error} />
    </div>
  );
}

import { Cancel01Icon, UserGroupIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { useState } from "react";
import {
  addOverride, addTeamMember, createEnvironment, createProject, createTeam, deleteOverride, deleteProject, deleteTeam, getProject,
  invite, listInvitations, listMembers, listOverrides, listProjects, listRoles, listRoleSets, listTeams, putMember, removeMember,
  removeTeamMember, setMemberStatus, type Effect, type Team,
} from "../../api";
import { Sel } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonCards, SkeletonTable } from "../../components/Skeleton";
import { Button, ConfirmButton, CopyIcon } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, EffectBadge, Field, FormCard, MultiPick, Principal, TextField } from "./shared";
import { rowCls, useRun } from "./util";
import { Badge } from "../../components/StatusBadge";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { Chip, Chips } from "../../components/Chips";
import { When } from "../../components/Time";

const PRINCIPAL_HELP = "Who: user:<collection>/<id>, such as user:users/abc.";

// ---- Projects & environments -------------------------------------------------------

export function ProjectsTab({ org }: { org: string }) {
  const l = useLoad((s) => listProjects(org, s), `projects:${org}`);
  const [slug, setSlug] = useState("");
  const [name, setName] = useState("");
  const { run, error, busy } = useRun(() => { l.reload(); setSlug(""); setName(""); });
  return (
    <div className="space-y-4">
      {!!l.error && <ErrorPanel error={l.error} onRetry={l.reload} />}
      {l.loading && !l.data && !l.error && <SkeletonCards count={2} label="Loading projects" />}
      {l.data?.length === 0 && <EmptyState compact title="No projects yet" why="Projects hold an organization's resources, each with its own environments such as development and production. Add one below." />}
      {!!l.data?.length && <div className="grid gap-4 lg:grid-cols-2">{l.data.map((p) => <ProjectCard key={p.id} id={p.id} onChanged={l.reload} />)}</div>}
      <FormCard title="New project" description="A project gets its environments after you create it.">
        <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); if (slug) void run(() => createProject(org, slug, name || slug)); }}>
          <TextField label="Project slug" value={slug} onChange={setSlug} placeholder="web" mono />
          <TextField label="Project name" value={name} onChange={setName} placeholder="Web app" />
          <Button type="submit" tone="primary" disabled={!slug || busy}>Create project</Button>
        </form>
      </FormCard>
      <ActionError error={error} />
    </div>
  );
}

function ProjectCard({ id, onChanged }: { id: string; onChanged: () => void }) {
  const p = useLoad((s) => getProject(id, s), `project:${id}`);
  const [env, setEnv] = useState("");
  const { run, error, busy } = useRun(() => { p.reload(); setEnv(""); });
  if (!p.data) return p.error ? <ErrorPanel error={p.error} onRetry={p.reload} /> : <SkeletonCards count={1} label="Loading project" />;
  const envs = p.data.environments ?? [];
  return (
    <section aria-label={`Project ${p.data.slug}`} className="flex flex-col rounded-xl border border-border bg-card">
      <header className="flex items-start justify-between gap-3 border-b border-border px-4 py-3">
        <div className="min-w-0">
          <h3 className="truncate text-sm font-semibold">{p.data.name}</h3>
          <code className="font-mono text-xs text-muted-foreground">{p.data.slug}</code>
        </div>
        <ConfirmButton label="Delete project" consequence={`Project ${p.data.slug} and its environments are removed.`} onConfirm={() => void run(async () => { await deleteProject(id); onChanged(); })} />
      </header>
      <div className="flex-1 space-y-3 px-4 py-3">
        <div>
          <p className="mb-1.5 text-xs font-medium text-muted-foreground">Environments</p>
          {envs.length === 0 ? (
            <p className="text-xs text-muted-foreground">No environments yet.</p>
          ) : (
            <ul aria-label={`Environments of ${p.data.slug}`} className="flex flex-wrap gap-1.5">
              {envs.map((e) => <li key={e.id}><Chip>{e.name}</Chip></li>)}
            </ul>
          )}
        </div>
        <form className="flex items-end gap-2" onSubmit={(e) => { e.preventDefault(); if (env) void run(() => createEnvironment(id, env)); }}>
          <TextField label={`New environment in ${p.data.slug}`} value={env} onChange={setEnv} placeholder="staging" mono className="flex-1" />
          <Button type="submit" disabled={!env || busy}>Add environment</Button>
        </form>
        <ActionError error={error} />
      </div>
    </section>
  );
}

// ---- Members ---------------------------------------------------------------------------

export function MembersTab({ org }: { org: string }) {
  const members = useLoad((s) => listMembers(org, s), `members:${org}`);
  const roles = useLoad((s) => listRoles(org, s), `roles:${org}`);
  const sets = useLoad((s) => listRoleSets(org, s), `sets:${org}`);
  const [principal, setPrincipal] = useState("");
  const [editing, setEditing] = useState(false);
  const [r, setR] = useState<string[]>([]);
  const [rs, setRs] = useState<string[]>([]);
  const reset = () => { setPrincipal(""); setR([]); setRs([]); setEditing(false); };
  const { run, error, busy } = useRun(() => { members.reload(); reset(); });
  return (
    <div className="space-y-4">
      <TableCard title="Members" count={members.data?.length} description="Accounts that belong to this organization, with the roles they hold in it.">
        {!!members.error && <CardBody><ErrorPanel error={members.error} onRetry={members.reload} /></CardBody>}
        {members.loading && !members.data && !members.error && <CardBody><SkeletonTable rows={3} columns={5} label="Loading members" /></CardBody>}
        {members.data?.length === 0 && <CardBody><EmptyState compact title="No members yet" why="Members are accounts that belong to this organization. Invite someone or add an account below." /></CardBody>}
        {!!members.data?.length && (
          <Table framed className="text-sm" aria-label="Members">
            <Table.Header className={HEAD_ROW}><Table.Row><Th>Member</Th><Th>Roles</Th><Th>Role sets</Th><Th>Status</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {members.data.map((m) => (
                <Table.Row key={m.id} className={rowCls(m.status !== "active")}>
                  <Table.Cell className="max-w-72"><Principal value={m.principal} /></Table.Cell>
                  <Table.Cell><Chips items={m.roles} max={4} /></Table.Cell>
                  <Table.Cell><Chips items={m.role_sets} max={3} /></Table.Cell>
                  <Table.Cell>{m.status === "active" ? <Badge status="active">Active</Badge> : <Badge status={m.status}>{m.status === "suspended" ? "Suspended" : m.status}</Badge>}</Table.Cell>
                  <Table.Cell align="end">
                    <span className="inline-flex items-center gap-2">
                      <Button aria-label={`Edit ${m.principal}`} onClick={() => { setPrincipal(m.principal); setR(m.roles); setRs(m.role_sets); setEditing(true); }}>Edit</Button>
                      <Button aria-label={`${m.status === "active" ? "Suspend" : "Reactivate"} ${m.principal}`} disabled={busy} onClick={() => void run(() => setMemberStatus(org, m.principal, m.status !== "active"))}>
                        {m.status === "active" ? "Suspend" : "Reactivate"}
                      </Button>
                      <ConfirmButton label="Remove" consequence={`${m.principal} leaves the organization and loses the roles it gave.`} onConfirm={() => void run(() => removeMember(org, m.principal))} />
                    </span>
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <FormCard
        title={editing ? `Change ${principal}` : "Add a member"}
        description="Give an account roles in this organization. Saving an existing member replaces its roles."
        footer={
          <>
            {(editing || principal) && <Button onClick={reset}>Cancel</Button>}
            <Button tone="primary" disabled={!principal || busy} onClick={() => void run(() => putMember(org, principal, r, rs))}>Save member</Button>
          </>
        }
      >
        <TextField label="Principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" help={PRINCIPAL_HELP} mono className="max-w-md" />
        <MultiPick label="Roles" options={(roles.data ?? []).filter((x) => x.assignable).map((x) => x.key)} value={r} onChange={setR} />
        <MultiPick label="Role sets" options={(sets.data ?? []).map((x) => x.key)} value={rs} onChange={setRs} empty="No role sets yet. Make one in the Role sets tab." />
      </FormCard>
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
  const { run, error, busy } = useRun(() => { teams.reload(); setKey(""); setName(""); });
  const all = teams.data ?? [];
  const children = (id: string | null) => all.filter((t) => (t.parent_id ?? null) === id);
  const node = (t: Team) => (
    <li key={t.id} className="mt-2">
      <TeamCard team={t} onChange={teams.reload} />
      {children(t.id).length > 0 && <ul className="ml-5 border-l-2 border-border pl-4">{children(t.id).map(node)}</ul>}
    </li>
  );
  return (
    <div className="space-y-4">
      {!!teams.error && <ErrorPanel error={teams.error} onRetry={teams.reload} />}
      {teams.loading && !teams.data && !teams.error && <SkeletonCards count={2} label="Loading teams" />}
      {teams.data?.length === 0 && <EmptyState compact title="No teams yet" why="A team is a named group of members that can be given roles together. Teams can sit inside other teams." />}
      <ul aria-label="Team tree" className="-mt-2">{children(null).map(node)}</ul>
      <FormCard title="New team" description="Pick a parent to nest it: members of a sub-team also get the parent team's roles.">
        <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); if (key) void run(() => createTeam(org, { key, name: name || key, parent_id: parent || null })); }}>
          <TextField label="Team key" value={key} onChange={setKey} placeholder="backend" mono />
          <TextField label="Team name" value={name} onChange={setName} placeholder="Backend" />
          <Field label="Parent team">
            <Sel value={parent} onChange={setParent} options={[{ value: "", label: "None (top level)" }, ...all.map((t) => ({ value: t.id, label: t.key }))]} className="min-w-40" />
          </Field>
          <Button type="submit" tone="primary" disabled={!key || busy}>Create team</Button>
        </form>
      </FormCard>
      <ActionError error={error} />
    </div>
  );
}

function TeamCard({ team, onChange }: { team: Team; onChange: () => void }) {
  const [who, setWho] = useState("");
  const { run, error, busy } = useRun(() => { onChange(); setWho(""); });
  return (
    <div className="rounded-xl border border-border bg-card text-sm" data-team={team.key}>
      <div className="flex flex-wrap items-start justify-between gap-3 px-4 py-3">
        <div className="flex min-w-0 items-start gap-2.5">
          <span aria-hidden="true" className="mt-0.5 flex size-7 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
            <HugeiconsIcon icon={UserGroupIcon} size={14} />
          </span>
          <div className="min-w-0">
            <p className="flex flex-wrap items-baseline gap-x-2">
              <span className="font-medium">{team.name}</span>
              <code className="font-mono text-xs text-muted-foreground">{team.key}</code>
              <span className="text-xs text-muted-foreground">{team.members.length} {team.members.length === 1 ? "member" : "members"}</span>
            </p>
            {(team.roles.length > 0 || team.deny.length > 0) && (
              <p className="mt-1.5 flex flex-wrap items-center gap-1">
                {team.roles.map((r) => <Chip key={r}>{r}</Chip>)}
                {team.deny.length > 0 && <Chip tone="bad">deny: {team.deny.join(", ")}</Chip>}
              </p>
            )}
          </div>
        </div>
        <ConfirmButton label={`Delete ${team.key}`} consequence="The team and its sub-teams are removed. Members keep their own roles." onConfirm={() => void run(() => deleteTeam(team.id))} />
      </div>
      <div className="space-y-2 border-t border-border px-4 py-3">
        {team.members.length > 0 && (
          <ul className="flex flex-wrap gap-1.5" aria-label={`Members of ${team.key}`}>
            {team.members.map((m) => (
              <li key={m} className="flex h-6 items-center gap-1 rounded-md border border-border bg-muted/60 ps-2 font-mono text-xs">
                {m}
                <button type="button" aria-label={`Remove ${m} from ${team.key}`} className="flex size-5 items-center justify-center rounded text-muted-foreground hover:bg-destructive/10 hover:text-destructive focus-visible:outline-2 focus-visible:outline-ring" onClick={() => void run(() => removeTeamMember(team.id, m))}>
                  <HugeiconsIcon icon={Cancel01Icon} size={12} aria-hidden="true" />
                </button>
              </li>
            ))}
          </ul>
        )}
        <form className="flex items-center gap-2" onSubmit={(e) => { e.preventDefault(); if (who) void run(() => addTeamMember(team.id, who)); }}>
          <Input size="sm" className="max-w-xs font-mono" aria-label={`Add member to ${team.key}`} placeholder="user:users/abc" value={who} onChange={(e) => setWho(e.target.value)} />
          <Button type="submit" disabled={!who || busy}>Add to {team.key}</Button>
        </form>
        <ActionError error={error} />
      </div>
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
  const { run, error, busy } = useRun(() => { inv.reload(); setEmail(""); setPicked([]); });
  return (
    <div className="space-y-4">
      {token && (
        <div role="status" aria-label="Invitation token" className="space-y-2 rounded-xl border border-warning/40 bg-warning/10 p-4 text-sm">
          <p className="font-medium">Invitation sent. Copy the one-time token now: it is not shown again.</p>
          <p className="flex items-center gap-1 rounded-lg border border-border bg-card px-3 py-2">
            <code className="min-w-0 flex-1 select-all break-all font-mono text-xs">{token}</code>
            <CopyIcon text={token} label="Copy token" />
          </p>
          <Button onClick={() => setToken(undefined)}>Dismiss</Button>
        </div>
      )}
      <TableCard title="Invitations" count={inv.data?.length} description="Invitations wait here until the person accepts them or they expire.">
        {!!inv.error && <CardBody><ErrorPanel error={inv.error} onRetry={inv.reload} /></CardBody>}
        {inv.loading && !inv.data && !inv.error && <CardBody><SkeletonTable rows={2} columns={4} label="Loading invitations" /></CardBody>}
        {inv.data?.length === 0 && <CardBody><EmptyState compact title="No pending invitations" why="Invite someone by e-mail below. They join with the roles you pick." /></CardBody>}
        {!!inv.data?.length && (
          <Table framed className="text-sm" aria-label="Invitations">
            <Table.Header className={HEAD_ROW}><Table.Row><Th>E-mail</Th><Th>Roles</Th><Th>Status</Th><Th>Expires</Th></Table.Row></Table.Header>
            <Table.Body>{inv.data.map((i) => (
              <Table.Row key={i.id}>
                <Table.Cell>{i.email}</Table.Cell>
                <Table.Cell><Chips items={i.roles} max={4} /></Table.Cell>
                <Table.Cell>{(i.status ?? "pending") === "pending" ? <Badge status="pending">Pending</Badge> : <Badge status={i.status ?? "pending"} />}</Table.Cell>
                <Table.Cell className="text-muted-foreground"><When value={i.expires_at} /></Table.Cell>
              </Table.Row>
            ))}</Table.Body>
          </Table>
        )}
      </TableCard>
      <FormCard
        title="Invite someone"
        description="They get the roles you pick when they accept."
        footer={<Button tone="primary" disabled={!email || busy} onClick={() => void run(async () => { const v = await invite(org, email, picked); setToken(v.token); })}>Send invitation</Button>}
      >
        <TextField label="Email" value={email} onChange={setEmail} placeholder="name@example.com" className="max-w-md" />
        <MultiPick label="Roles" options={(roles.data ?? []).filter((r) => r.assignable).map((r) => r.key)} value={picked} onChange={setPicked} />
      </FormCard>
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
  const { run, error, busy } = useRun(() => { l.reload(); setPrincipal(""); setPermission(""); });
  return (
    <div className="space-y-4">
      <TableCard title="User overrides" count={l.data?.length} description="An override grants or denies one permission to one account, whatever its roles say. A deny always wins.">
        {!!l.error && <CardBody><ErrorPanel error={l.error} onRetry={l.reload} /></CardBody>}
        {l.loading && !l.data && !l.error && <CardBody><SkeletonTable rows={2} columns={4} label="Loading overrides" /></CardBody>}
        {l.data?.length === 0 && <CardBody><EmptyState compact title="No user overrides" why="Use one for an exception, such as taking projects:delete away from one member." /></CardBody>}
        {!!l.data?.length && (
          <Table framed className="text-sm" aria-label="User overrides">
            <Table.Header className={HEAD_ROW}><Table.Row><Th>Principal</Th><Th>Permission</Th><Th>Effect</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {l.data.map((o) => (
                <Table.Row key={o.id} data-effect={o.effect} className={rowCls(o.effect === "deny")}>
                  <Table.Cell className="max-w-72"><Principal value={o.principal} /></Table.Cell>
                  <Table.Cell><code className="font-mono text-xs">{o.permission}</code></Table.Cell>
                  <Table.Cell><EffectBadge effect={o.effect} /></Table.Cell>
                  <Table.Cell align="end"><ConfirmButton label="Delete" consequence={`The ${o.effect} of ${o.permission} for ${o.principal} is removed; its roles decide again.`} onConfirm={() => void run(() => deleteOverride(o.id))} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
      <FormCard title="Add override">
        <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); if (principal && permission) void run(() => addOverride({ org_id: org, principal, permission, effect })); }}>
          <TextField label="Override principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" mono />
          <TextField label="Override permission" value={permission} onChange={setPermission} placeholder="projects:delete" mono />
          <Field label="Effect">
            <Sel value={effect} onChange={(v) => setEffect(v as Effect)} options={[{ value: "deny", label: "Deny" }, { value: "allow", label: "Allow" }]} className="min-w-28" />
          </Field>
          <Button type="submit" tone="primary" disabled={!principal || !permission || busy}>Add override</Button>
        </form>
      </FormCard>
      <ActionError error={error} />
    </div>
  );
}

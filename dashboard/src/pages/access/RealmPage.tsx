import { Table } from "panelui-web/table";
import { useState } from "react";
import { useAdminScope } from "../../auth/scope";
import { createRealm, liftSuspension, listGlobalSuspensions, listRealmBindings, listRealms, listRoles, listRoleSets, putRealmBinding, send, suspendPrincipal } from "../../api";
import { Sel } from "../../components/controls";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, ConfirmButton } from "../../components/ui";
import { useLoad } from "../../hooks";
import RolesPanel from "./RolesPanel";
import RoleSetsPanel from "./RoleSetsPanel";
import { ActionError, Field, FormCard, MultiPick, Principal, TextField } from "./shared";
import { rowCls, useRun } from "./util";
import { CardBody, HEAD_ROW, TableCard, Th } from "../../components/TableCard";
import { Chip, Chips } from "../../components/Chips";
import { When } from "../../components/Time";

const SECTIONS = [
  ["realms", "Realms"],
  ["realm-roles", "Roles"],
  ["realm-role-sets", "Role sets"],
  ["realm-bindings", "Bindings"],
  ["realm-admins", "Administrators"],
  ["realm-suspensions", "Suspensions"],
] as const;

/** Realm level: realms, realm roles, role sets, bindings, realm administrators and global suspensions. */
export default function RealmPage() {
  return (
    <div className="space-y-8">
      <nav aria-label="On this page" className="flex flex-wrap items-center gap-1 text-xs">
        <span className="me-1 text-muted-foreground">Jump to</span>
        {SECTIONS.map(([id, label]) => (
          <a key={id} href={`#${id}`} onClick={(e) => { e.preventDefault(); document.getElementById(id)?.scrollIntoView?.({ behavior: "smooth", block: "start" }); }}
            className="rounded-md border border-border px-2 py-1 font-medium text-muted-foreground hover:border-foreground/30 hover:text-foreground focus-visible:outline-2 focus-visible:outline-ring">
            {label}
          </a>
        ))}
      </nav>
      <div id="realms" className="scroll-mt-4"><Realms /></div>
      <div id="realm-roles" className="scroll-mt-4"><RolesPanel title="Realm roles" /></div>
      <div id="realm-role-sets" className="scroll-mt-4"><RoleSetsPanel title="Realm role sets" /></div>
      <div id="realm-bindings" className="scroll-mt-4"><Bindings /></div>
      <div id="realm-admins" className="scroll-mt-4"><RealmAdmins /></div>
      <div id="realm-suspensions" className="scroll-mt-4"><Suspensions /></div>
    </div>
  );
}

function Realms() {
  const l = useLoad(listRealms, "realms");
  const { platform } = useAdminScope();
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const { run, error, busy } = useRun(() => { l.reload(); setId(""); setName(""); });
  return (
    <div className="space-y-4">
      <TableCard
        title={<span className="inline-flex items-center gap-1.5">Realms <HelpTip term="realm" /></span>}
        aria-label="Realms"
        count={l.data?.length}
        description="A realm is a separate space of users, settings and branding. Everything on this page applies to the realm you are working in."
      >
        {!!l.error && <CardBody><ErrorPanel error={l.error} onRetry={l.reload} /></CardBody>}
        {l.loading && !l.data && !l.error && <CardBody><SkeletonTable rows={1} columns={2} label="Loading realms" /></CardBody>}
        {l.data?.length === 0 && <CardBody><EmptyState compact title="No realms yet" why="A realm is a separate space of users and settings. Create one below." /></CardBody>}
        {!!l.data?.length && (
          <Table framed className="text-sm" aria-label="Realm list">
            <Table.Header className={HEAD_ROW}><Table.Row><Th>Name</Th><Th>Id</Th></Table.Row></Table.Header>
            <Table.Body>
              {l.data.map((r) => (
                <Table.Row key={r.id}>
                  <Table.Cell className="font-medium">{r.name}</Table.Cell>
                  <Table.Cell><Chip>{r.id}</Chip></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        {platform && (
          <form className="flex flex-wrap items-end gap-3 border-t border-border px-4 py-3" onSubmit={(e) => { e.preventDefault(); if (id) void run(() => createRealm(id, name || id)); }}>
            <TextField label="Realm id" value={id} onChange={setId} placeholder="emea" mono />
            <TextField label="Realm name" value={name} onChange={setName} placeholder="Europe" />
            <Button type="submit" tone="primary" disabled={!id || busy}>Create realm</Button>
          </form>
        )}
      </TableCard>
      <ActionError error={error} />
    </div>
  );
}

function Bindings() {
  const l = useLoad(listRealmBindings, "bindings");
  const roles = useLoad((s) => listRoles(undefined, s), "roles:realm");
  const sets = useLoad((s) => listRoleSets(undefined, s), "sets:realm");
  const [principal, setPrincipal] = useState("");
  const [r, setR] = useState<string[]>([]);
  const [rs, setRs] = useState<string[]>([]);
  const reset = () => { setPrincipal(""); setR([]); setRs([]); };
  const { run, error, busy } = useRun(() => { l.reload(); reset(); });
  return (
    <div className="space-y-4">
      <TableCard title="Realm bindings" count={l.data?.length} description="A binding gives one account roles across the whole realm, in every organization.">
        {!!l.error && <CardBody><ErrorPanel error={l.error} onRetry={l.reload} /></CardBody>}
        {l.loading && !l.data && !l.error && <CardBody><SkeletonTable rows={2} columns={3} label="Loading bindings" /></CardBody>}
        {l.data?.length === 0 && <CardBody><EmptyState compact title="No realm bindings yet" why="Bind an account below to give it realm-wide roles, for example a support lead." /></CardBody>}
        {!!l.data?.length && (
          <Table framed className="text-sm" aria-label="Realm bindings list">
            <Table.Header className={HEAD_ROW}><Table.Row><Th>Principal</Th><Th>Roles</Th><Th>Role sets</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>{l.data.map((b) => (
              <Table.Row key={b.principal} aria-selected={principal === b.principal || undefined}>
                <Table.Cell className="max-w-72"><Principal value={b.principal} /></Table.Cell>
                <Table.Cell><Chips items={b.roles} max={4} /></Table.Cell>
                <Table.Cell><Chips items={b.role_sets} max={3} /></Table.Cell>
                <Table.Cell align="end"><Button aria-label={`Edit ${b.principal}`} onClick={() => { setPrincipal(b.principal); setR(b.roles); setRs(b.role_sets); }}>Edit</Button></Table.Cell>
              </Table.Row>
            ))}</Table.Body>
          </Table>
        )}
      </TableCard>
      <FormCard
        title="Bind realm roles"
        description="Saving replaces the account's realm roles and role sets."
        footer={
          <>
            {principal && <Button onClick={reset}>Cancel</Button>}
            <Button tone="primary" disabled={!principal || busy} onClick={() => void run(() => putRealmBinding(principal, r, rs))}>Save binding</Button>
          </>
        }
      >
        <TextField label="Binding principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" mono className="max-w-md" />
        <MultiPick label="Binding roles" options={(roles.data ?? []).filter((x) => x.assignable).map((x) => x.key)} value={r} onChange={setR} />
        <MultiPick label="Binding role sets" options={(sets.data ?? []).map((x) => x.key)} value={rs} onChange={setRs} empty="No realm role sets yet." />
      </FormCard>
      <ActionError error={error} />
    </div>
  );
}

function Suspensions() {
  const l = useLoad(listGlobalSuspensions, "suspensions");
  const [principal, setPrincipal] = useState("");
  const [reason, setReason] = useState("");
  const { run, error, busy } = useRun(() => { l.reload(); setPrincipal(""); setReason(""); });
  return (
    <div className="space-y-4">
      <TableCard title="Suspensions" count={l.data?.length} description="A suspended account is refused everywhere in the realm until you lift the suspension.">
        {!!l.error && <CardBody><ErrorPanel error={l.error} onRetry={l.reload} /></CardBody>}
        {l.loading && !l.data && !l.error && <CardBody><SkeletonTable rows={1} columns={3} label="Loading suspensions" /></CardBody>}
        {l.data?.length === 0 && <CardBody><EmptyState compact title="No suspended accounts" why="An account you suspend appears here until you lift the suspension." /></CardBody>}
        {!!l.data?.length && (
          <Table framed className="text-sm" aria-label="Suspended accounts">
            <Table.Header className={HEAD_ROW}><Table.Row><Th>Principal</Th><Th>Reason</Th><Th>Until</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>{l.data.map((s) => (
              <Table.Row key={s.principal} className={rowCls(true)}>
                <Table.Cell className="max-w-72"><Principal value={s.principal} /></Table.Cell>
                <Table.Cell>{s.reason}</Table.Cell>
                <Table.Cell className="text-muted-foreground">{s.until ? <When value={s.until} /> : "Until lifted"}</Table.Cell>
                <Table.Cell align="end"><ConfirmButton label="Lift" consequence={`${s.principal} can sign in and call the API again.`} onConfirm={() => void run(() => liftSuspension(s.principal))} /></Table.Cell>
              </Table.Row>
            ))}</Table.Body>
          </Table>
        )}
        <form className="flex flex-wrap items-end gap-3 border-t border-border px-4 py-3" onSubmit={(e) => { e.preventDefault(); if (principal && reason) void run(() => suspendPrincipal(principal, reason)); }}>
          <TextField label="Suspend principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" mono />
          <TextField label="Reason" value={reason} onChange={setReason} placeholder="Abuse report 42" />
          <Button type="submit" tone="danger" disabled={!principal || !reason || busy}>Suspend account</Button>
        </form>
      </TableCard>
      <ActionError error={error} />
    </div>
  );
}

const ADMIN_ROLE = "realm-admin";

/**
 * Makes an account an administrator of one realm: it holds `identity:admin` there (FB-137). Two calls, both scoped to the
 * realm with X-ForgeBase-Realm: save the `realm-admin` role, then bind the account to it.
 */
function RealmAdmins() {
  const { platform, realm: own } = useAdminScope();
  const realms = useLoad(listRealms, "realms:admins");
  const [pickedRealm, setRealm] = useState("");
  const realm = platform ? pickedRealm || realms.data?.[0]?.id || "" : own ?? "";
  const [principal, setPrincipal] = useState("");
  const [done, setDone] = useState<string>();
  const { run, error, busy } = useRun();
  return (
    <div className="space-y-4">
      <FormCard
        title="Realm administrators"
        description={<>A realm administrator manages the users, keys and sign-in settings of one realm, and nothing else on the server. It holds the <code className="font-mono">identity:admin</code> permission in that realm.</>}
      >
        <form className="flex flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); }}>
          {platform && (
            <Field label="Administrator of realm">
              <Sel aria-label="Administrator of realm" value={realm} onChange={setRealm} options={(realms.data ?? []).map((r) => ({ value: r.id, label: r.name ? `${r.name} (${r.id})` : r.id }))} className="min-w-48" />
            </Field>
          )}
          <TextField label="Account to make administrator" value={principal} onChange={(v) => { setPrincipal(v); setDone(undefined); }} placeholder="user:users/abc" mono />
          <Button type="submit" tone="primary" disabled={!principal || !realm || busy} onClick={() => void run(async () => {
            const h = { "X-ForgeBase-Realm": realm };
            await send("PUT", "/api/v1/roles", { key: ADMIN_ROLE, name: "Realm administrator", description: "Manages the identity of this realm", allow: ["identity:admin"], deny: [] }, h);
            await send("PUT", "/api/v1/realm-bindings", { principal, roles: [ADMIN_ROLE], role_sets: [] }, h);
            setDone(`${principal} is now an administrator of ${realm}.`);
          })}>Make realm administrator</Button>
        </form>
        {done && <p role="status" className="rounded-lg border border-success/40 bg-success/10 px-3 py-2 text-sm text-success">{done}</p>}
      </FormCard>
      <ActionError error={error} />
    </div>
  );
}

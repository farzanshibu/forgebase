import { useState } from "react";
import { HelpTip } from "../../components/HelpTip";
import { useAdminScope } from "../../auth/scope";
import { send } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { createRealm, listGlobalSuspensions, liftSuspension, listRealmBindings, listRealms, listRoles, listRoleSets, putRealmBinding, suspendPrincipal } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import RolesPanel from "./RolesPanel";
import RoleSetsPanel from "./RoleSetsPanel";
import { ActionError, MultiPick, TextField } from "./shared";
import { rowCls, useRun } from "./util";
import { Sel } from "../../components/controls";
import { Table } from "panelui-web/table";

/** Realm level: realms, realm roles, role sets, bindings and global suspensions. */
export default function RealmPage() {
  return (
    <div className="space-y-8">
      <section><h2 className="mb-3 flex items-center gap-1 text-lg font-medium">Realms <HelpTip term="realm" /></h2><Realms /></section>
      <section><h2 className="mb-3 text-lg font-medium">Realm roles</h2><RolesPanel /></section>
      <section><h2 className="mb-3 text-lg font-medium">Realm role sets</h2><RoleSetsPanel /></section>
      <section><h2 className="mb-3 text-lg font-medium">Realm bindings</h2><Bindings /></section>
      <section><h2 className="mb-3 text-lg font-medium">Realm administrators</h2><RealmAdmins /></section>
      <section><h2 className="mb-3 text-lg font-medium">Suspensions</h2><Suspensions /></section>
    </div>
  );
}

function Realms() {
  const l = useLoad(listRealms, "realms");
  const { platform } = useAdminScope();
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const { run, error } = useRun(() => { l.reload(); setId(""); setName(""); });
  return (
    <div className="space-y-3">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading realms" />}
      {l.data?.length === 0 && <EmptyState compact title="No realms." why="A realm is a separate space of users and settings. Create one below." />}
      {l.data && <ul className="text-sm">{l.data.map((r) => <li key={r.id} className="border-t border-border py-1.5"><span className="font-mono">{r.id}</span> — {r.name}</li>)}</ul>}
      {platform && (
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Realm id" value={id} onChange={setId} />
          <TextField label="Realm name" value={name} onChange={setName} />
          <Button tone="primary" disabled={!id} onClick={() => void run(() => createRealm(id, name || id))}>Create realm</Button>
        </div>
      )}
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
  const { run, error } = useRun(() => { l.reload(); setPrincipal(""); setR([]); setRs([]); });
  return (
    <div className="space-y-3">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading bindings" />}
      {l.data?.length === 0 && <EmptyState compact title="No realm bindings." why="A binding gives one account roles across the whole realm." />}
      {l.data && l.data.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Principal</Table.Head><Table.Head>Roles</Table.Head><Table.Head>Role sets</Table.Head><Table.Head /></Table.Row></Table.Header>
          <Table.Body>{l.data.map((b) => (
            <Table.Row key={b.principal} className="border-t border-border"><Table.Cell className="py-2 font-mono">{b.principal}</Table.Cell><Table.Cell>{b.roles.join(", ")}</Table.Cell><Table.Cell>{b.role_sets.join(", ")}</Table.Cell>
              <Table.Cell className="text-right"><Button onClick={() => { setPrincipal(b.principal); setR(b.roles); setRs(b.role_sets); }}>Edit {b.principal}</Button></Table.Cell></Table.Row>
          ))}</Table.Body>
        </Table>
      )}
      <Panel title="Bind realm roles">
        <TextField label="Binding principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" />
        <div className="mt-3 space-y-3">
          <MultiPick label="Binding roles" options={(roles.data ?? []).filter((x) => x.assignable).map((x) => x.key)} value={r} onChange={setR} />
          <MultiPick label="Binding role sets" options={(sets.data ?? []).map((x) => x.key)} value={rs} onChange={setRs} />
        </div>
        <div className="mt-3"><Button tone="primary" disabled={!principal} onClick={() => void run(() => putRealmBinding(principal, r, rs))}>Save binding</Button></div>
      </Panel>
      <ActionError error={error} />
    </div>
  );
}

function Suspensions() {
  const l = useLoad(listGlobalSuspensions, "suspensions");
  const [principal, setPrincipal] = useState("");
  const [reason, setReason] = useState("");
  const { run, error } = useRun(() => { l.reload(); setPrincipal(""); setReason(""); });
  return (
    <div className="space-y-3">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading suspensions" />}
      {l.data?.length === 0 && <EmptyState compact title="No suspended accounts." why="An account you suspend appears here until you lift the suspension." />}
      {l.data && l.data.length > 0 && (
        <Table className="w-full text-left text-sm">
          <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Principal</Table.Head><Table.Head>Reason</Table.Head><Table.Head>Until</Table.Head><Table.Head /></Table.Row></Table.Header>
          <Table.Body>{l.data.map((s) => (
            <Table.Row key={s.principal} className={rowCls(true)}><Table.Cell className="py-2 font-mono">{s.principal}</Table.Cell><Table.Cell>{s.reason}</Table.Cell><Table.Cell>{s.until ? String(s.until) : "until lifted"}</Table.Cell>
              <Table.Cell className="text-right"><ConfirmButton label="Lift" onConfirm={() => void run(() => liftSuspension(s.principal))} /></Table.Cell></Table.Row>
          ))}</Table.Body>
        </Table>
      )}
      <div className="flex flex-wrap items-end gap-3">
        <TextField label="Suspend principal" value={principal} onChange={setPrincipal} placeholder="user:users/abc" />
        <TextField label="Reason" value={reason} onChange={setReason} />
        <Button tone="danger" disabled={!principal || !reason} onClick={() => void run(() => suspendPrincipal(principal, reason))}>Suspend account</Button>
      </div>
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
    <div className="space-y-3">
      <p className="text-sm text-muted-foreground">
        A realm administrator manages the users, keys and sign-in settings of one realm, and nothing else on the server. It needs the <code>identity:admin</code> permission in that realm.
      </p>
      <div className="flex flex-wrap items-end gap-3">
        {platform && (
          <label className="flex flex-col gap-1 text-xs text-muted-foreground">Administrator of realm
            <Sel aria-label="Administrator of realm" value={realm} onChange={setRealm} options={(realms.data ?? []).map((r) => r.id)} />
          </label>
        )}
        <TextField label="Account to make administrator" value={principal} onChange={(v) => { setPrincipal(v); setDone(undefined); }} placeholder="user:users/abc" />
        <Button tone="primary" disabled={!principal || !realm || busy} onClick={() => void run(async () => {
          const h = { "X-ForgeBase-Realm": realm };
          await send("PUT", "/api/v1/roles", { key: ADMIN_ROLE, name: "Realm administrator", description: "Manages the identity of this realm", allow: ["identity:admin"], deny: [] }, h);
          await send("PUT", "/api/v1/realm-bindings", { principal, roles: [ADMIN_ROLE], role_sets: [] }, h);
          setDone(`${principal} is now an administrator of ${realm}.`);
        })}>Make realm administrator</Button>
      </div>
      {done && <p role="status" className="text-sm text-success">{done}</p>}
      <ActionError error={error} />
    </div>
  );
}

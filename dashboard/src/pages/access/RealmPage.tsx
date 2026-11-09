import { useState } from "react";
import { createRealm, listGlobalSuspensions, liftSuspension, listRealmBindings, listRealms, listRoles, listRoleSets, putRealmBinding, suspendPrincipal } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import RolesPanel from "./RolesPanel";
import RoleSetsPanel from "./RoleSetsPanel";
import { ActionError, Empty, MultiPick, TextField } from "./shared";
import { rowCls, useRun } from "./util";

/** Realm level: realms, realm roles, role sets, bindings and global suspensions. */
export default function RealmPage() {
  return (
    <div className="space-y-8">
      <section><h2 className="mb-3 text-lg font-medium">Realms</h2><Realms /></section>
      <section><h2 className="mb-3 text-lg font-medium">Realm roles</h2><RolesPanel /></section>
      <section><h2 className="mb-3 text-lg font-medium">Realm role sets</h2><RoleSetsPanel /></section>
      <section><h2 className="mb-3 text-lg font-medium">Realm bindings</h2><Bindings /></section>
      <section><h2 className="mb-3 text-lg font-medium">Suspensions</h2><Suspensions /></section>
    </div>
  );
}

function Realms() {
  const l = useLoad(listRealms, "realms");
  const [id, setId] = useState("");
  const [name, setName] = useState("");
  const { run, error } = useRun(() => { l.reload(); setId(""); setName(""); });
  return (
    <div className="space-y-3">
      <ErrorView error={l.error} />
      {l.loading && !l.error && <Loading what="Loading realms" />}
      {l.data?.length === 0 && <Empty>No realms.</Empty>}
      {l.data && <ul className="text-sm">{l.data.map((r) => <li key={r.id} className="border-t border-zinc-800 py-1.5"><span className="font-mono">{r.id}</span> — {r.name}</li>)}</ul>}
      <div className="flex flex-wrap items-end gap-3">
        <TextField label="Realm id" value={id} onChange={setId} />
        <TextField label="Realm name" value={name} onChange={setName} />
        <Button tone="primary" disabled={!id} onClick={() => void run(() => createRealm(id, name || id))}>Create realm</Button>
      </div>
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
      {l.data?.length === 0 && <Empty>No realm bindings.</Empty>}
      {l.data && l.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Principal</th><th>Roles</th><th>Role sets</th><th /></tr></thead>
          <tbody>{l.data.map((b) => (
            <tr key={b.principal} className="border-t border-zinc-800"><td className="py-2 font-mono">{b.principal}</td><td>{b.roles.join(", ")}</td><td>{b.role_sets.join(", ")}</td>
              <td className="text-right"><Button onClick={() => { setPrincipal(b.principal); setR(b.roles); setRs(b.role_sets); }}>Edit {b.principal}</Button></td></tr>
          ))}</tbody>
        </table>
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
      {l.data?.length === 0 && <Empty>No suspended accounts.</Empty>}
      {l.data && l.data.length > 0 && (
        <table className="w-full text-left text-sm">
          <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Principal</th><th>Reason</th><th>Until</th><th /></tr></thead>
          <tbody>{l.data.map((s) => (
            <tr key={s.principal} className={rowCls(true)}><td className="py-2 font-mono">{s.principal}</td><td>{s.reason}</td><td>{s.until ? String(s.until) : "until lifted"}</td>
              <td className="text-right"><ConfirmButton label="Lift" onConfirm={() => void run(() => liftSuspension(s.principal))} /></td></tr>
          ))}</tbody>
        </table>
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

import { useState } from "react";
import {
  createServiceAccount, createServiceAccountKey, deleteServiceAccount, listServiceAccounts, revokeKey, serviceAccountKeys,
  type ServiceAccountView,
} from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { Badge, SecretModal, THEAD, Th, fmtTime, splitList, useAction } from "./common";

export default function ServiceAccounts() {
  const list = useLoad(listServiceAccounts, "sas");
  const [name, setName] = useState("");
  const [description, setDescription] = useState("");
  const [role, setRole] = useState("service");
  const [open, setOpen] = useState<string>();
  const act = useAction();
  return (
    <div className="space-y-4">
      <Panel title="New service account">
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            if (await act.run(() => createServiceAccount({ name: name.trim(), description, role }))) {
              setName("");
              setDescription("");
              list.reload();
            }
          }}
        >
          <input aria-label="Service account name" className={inputCls} placeholder="Name" value={name} onChange={(e) => setName(e.target.value)} />
          <input aria-label="Description" className={`${inputCls} w-64`} placeholder="Description" value={description} onChange={(e) => setDescription(e.target.value)} />
          <select aria-label="Role" className={inputCls} value={role} onChange={(e) => setRole(e.target.value)}>
            <option value="service">service</option>
            <option value="admin">admin</option>
          </select>
          <Button type="submit" tone="primary" disabled={act.busy || !name.trim()}>
            Create
          </Button>
        </form>
        {act.errorView}
      </Panel>
      <ErrorView error={list.error} />
      {list.loading && !list.error && <Loading what="Loading service accounts" />}
      {list.data?.length === 0 && <p className="text-sm text-zinc-400">No service accounts yet.</p>}
      {!!list.data?.length && (
        <table className="w-full text-sm">
          <thead className={THEAD}>
            <tr><Th>Name</Th><Th>Role</Th><Th>Created</Th><Th>Status</Th><Th> </Th></tr>
          </thead>
          <tbody>
            {list.data.map((s) => (
              <tr key={s.id} className="border-t border-zinc-800">
                <td className="py-2 pr-3">
                  {s.name} <span className="text-xs text-zinc-500">{s.description}</span>
                </td>
                <td className="pr-3">{s.role === "admin" ? <Badge tone="warn">admin</Badge> : s.role}</td>
                <td className="pr-3">{fmtTime(s.created_at)}</td>
                <td className="pr-3">{s.disabled ? <Badge tone="bad">disabled</Badge> : <Badge tone="ok">active</Badge>}</td>
                <td className="space-x-2 whitespace-nowrap">
                  <Button aria-label={`Keys of ${s.name}`} onClick={() => setOpen(open === s.id ? undefined : s.id)}>
                    Keys
                  </Button>
                  <ConfirmButton
                    label="Delete"
                    onConfirm={async () => {
                      if (await act.run(() => deleteServiceAccount(s.id))) list.reload();
                    }}
                  />
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      )}
      {open && list.data?.find((s) => s.id === open) && <Keys sa={list.data.find((s) => s.id === open)!} />}
    </div>
  );
}

function Keys({ sa }: { sa: ServiceAccountView }) {
  const keys = useLoad((s) => serviceAccountKeys(sa.id, s), `sak:${sa.id}`);
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState("");
  const [days, setDays] = useState("");
  const [secret, setSecret] = useState<string>();
  const act = useAction();
  return (
    <Panel title={`Keys of ${sa.name}`}>
      <form
        className="mb-3 flex flex-wrap items-end gap-2"
        onSubmit={async (e) => {
          e.preventDefault();
          const ok = await act.run(async () => {
            const k = await createServiceAccountKey(sa.id, {
              name: name.trim(),
              scopes: splitList(scopes),
              ...(days ? { expires_in_days: Number(days) } : {}),
            });
            setSecret(k.key);
          });
          if (ok) {
            setName("");
            setScopes("");
            setDays("");
            keys.reload();
          }
        }}
      >
        <input aria-label="Key name" className={inputCls} placeholder="Key name" value={name} onChange={(e) => setName(e.target.value)} />
        <input aria-label="Scopes" className={`${inputCls} w-64`} placeholder="collections:read collections:write:posts" value={scopes} onChange={(e) => setScopes(e.target.value)} />
        <input aria-label="Expires in days" className={`${inputCls} w-28`} type="number" min={1} placeholder="Days" value={days} onChange={(e) => setDays(e.target.value)} />
        <Button type="submit" tone="primary" disabled={act.busy || !name.trim() || splitList(scopes).length === 0}>
          Create key
        </Button>
      </form>
      {act.errorView}
      <ErrorView error={keys.error} />
      {keys.loading && !keys.error && <Loading what="Loading keys" />}
      {keys.data?.length === 0 && <p className="text-sm text-zinc-400">No keys.</p>}
      {!!keys.data?.length && <KeyTable rows={keys.data} onRevoked={keys.reload} />}
      {secret && <SecretModal label="API key" secret={secret} onClose={() => setSecret(undefined)} />}
    </Panel>
  );
}

export function KeyTable({ rows, onRevoked, showOwner }: { rows: import("../../api").ApiKeyView[]; onRevoked: () => void; showOwner?: boolean }) {
  const act = useAction();
  return (
    <>
      {act.errorView}
      <table className="w-full text-sm">
        <thead className={THEAD}>
          <tr><Th>Name</Th>{showOwner && <Th>Kind</Th>}{showOwner && <Th>Owner</Th>}<Th>Scopes</Th><Th>Expires</Th><Th>Last used</Th><Th> </Th></tr>
        </thead>
        <tbody>
          {rows.map((k) => (
            <tr key={k.id} className="border-t border-zinc-800">
              <td className="py-2 pr-3">
                {k.name} <span className="font-mono text-xs text-zinc-500">{k.id}</span>
              </td>
              {showOwner && <td className="pr-3">{k.kind}</td>}
              {showOwner && <td className="pr-3 font-mono text-xs">{k.owner_type}:{k.owner_id}</td>}
              <td className="pr-3 text-xs">{k.scopes.join(", ")}</td>
              <td className="pr-3">{fmtTime(k.expires_at)}</td>
              <td className="pr-3">{fmtTime(k.last_used_at)}</td>
              <td>
                {k.revoked ? (
                  <Badge tone="bad">revoked</Badge>
                ) : (
                  <ConfirmButton
                    label="Revoke"
                    onConfirm={async () => {
                      if (await act.run(() => revokeKey(k.id))) onRevoked();
                    }}
                  />
                )}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}

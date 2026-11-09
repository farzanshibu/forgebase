import { useState } from "react";
import { listOidcKeys, listRealmConsents, revokeRealmConsent, rotateOidcKeys } from "../../api";
import { ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { fmtIso, useRealm } from "./util";

/** Remembered user consents of the realm (filter by client, revocable) and OIDC signing keys with rotation. */
export default function ConsentsPage() {
  const realm = useRealm();
  const [client, setClient] = useState("");
  const l = useLoad((sig) => listRealmConsents(realm, client || undefined, sig), `consents:${realm}:${client}`);
  const { run, error } = useRun(l.reload);
  const rows = l.data ?? [];
  const keys = useLoad((sig) => listOidcKeys(realm, sig), `oidc-keys:${realm}`);
  const [kid, setKid] = useState<string>();
  const rot = useRun(keys.reload);
  return (
    <div className="space-y-6">
      <Panel title={`Consents in ${realm}`}>
        <ErrorView error={l.error} />
        <TextField label="Filter by client id" value={client} onChange={setClient} placeholder="all clients" />
        {l.loading && !l.error && <Loading what="Loading consents" />}
        {l.data && rows.length === 0 && <Empty>No consents in this realm.</Empty>}
        {rows.length > 0 && (
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Client app</th><th>User</th><th>Scopes</th><th>Granted</th><th>Last used</th><th /></tr></thead>
            <tbody>
              {rows.map((c) => (
                <tr key={c.id} className="border-t border-zinc-800">
                  <td className="py-1.5">{c.client_name} <span className="font-mono text-xs text-zinc-500">{c.client_id}</span></td>
                  <td className="font-mono text-xs">{c.collection}/{c.user_id}</td>
                  <td className="font-mono text-xs">{c.scopes.join(" ")}</td>
                  <td className="text-xs text-zinc-400">{fmtIso(c.created_at)}</td>
                  <td className="text-xs text-zinc-400">{fmtIso(c.updated_at)}</td>
                  <td className="text-right"><ConfirmButton label={`Revoke ${c.client_name}`} onConfirm={() => void run(() => revokeRealmConsent(realm, c.id))} /></td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <ActionError error={error} />
      </Panel>
      <Panel title="OIDC signing keys">
        <p className="text-sm text-zinc-300">Rotation adds a new signing key to the realm's JWKS; tokens signed by the previous key stay verifiable until they expire.</p>
        <div className="mt-3 flex items-center gap-3">
          <ConfirmButton label={`Rotate keys for ${realm}`} onConfirm={() => void rot.run(async () => setKid((await rotateOidcKeys(realm)).kid))} />
          {kid && <span role="status" className="text-sm text-emerald-300">New key id <code className="font-mono">{kid}</code></span>}
        </div>
        <ErrorView error={keys.error} />
        {keys.data && (
          <table className="mt-3 w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Key id</th><th>Status</th><th>Created</th><th>Verifies until</th></tr></thead>
            <tbody>
              {keys.data.items.map((k) => (
                <tr key={k.kid} className="border-t border-zinc-800">
                  <td className="py-1.5 font-mono text-xs">{k.kid}</td>
                  <td>{k.kid === keys.data!.current ? "current" : k.status}</td>
                  <td className="text-xs text-zinc-400">{fmtIso(k.created_at)}</td>
                  <td className="text-xs text-zinc-400">{k.status === "active" ? "in use" : fmtIso(k.verify_until)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
        <Note>Discovery document: <span className="font-mono">/oidc/{realm}/.well-known/openid-configuration</span></Note>
        <ActionError error={rot.error} />
      </Panel>
    </div>
  );
}

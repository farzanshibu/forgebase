import { useState } from "react";
import { listConsents, revokeConsent, rotateOidcKeys } from "../../api";
import { ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { fmtIso, useRealm } from "./util";

/** Remembered user consents (revocable) and OIDC signing key rotation for the realm. */
export default function ConsentsPage() {
  const realm = useRealm();
  const l = useLoad(listConsents, "consents");
  const { run, error } = useRun(l.reload);
  const rows = (l.data ?? []).filter((c) => c.realm === realm);
  const [kid, setKid] = useState<string>();
  const rot = useRun();
  return (
    <div className="space-y-6">
      <Panel title={`Consents in ${realm}`}>
        <ErrorView error={l.error} />
        {l.loading && !l.error && <Loading what="Loading consents" />}
        {l.data && rows.length === 0 && <Empty>No consents in this realm.</Empty>}
        {rows.length > 0 && (
          <table className="w-full text-left text-sm">
            <thead className="text-xs uppercase tracking-wide text-zinc-500"><tr><th className="py-2">Client app</th><th>Scopes</th><th>Granted</th><th>Last used</th><th /></tr></thead>
            <tbody>
              {rows.map((c) => (
                <tr key={c.id} className="border-t border-zinc-800">
                  <td className="py-1.5">{c.client_name} <span className="font-mono text-xs text-zinc-500">{c.client_id}</span></td>
                  <td className="font-mono text-xs">{c.scopes.join(" ")}</td>
                  <td className="text-xs text-zinc-400">{fmtIso(c.created_at)}</td>
                  <td className="text-xs text-zinc-400">{fmtIso(c.updated_at)}</td>
                  <td className="text-right"><ConfirmButton label={`Revoke ${c.client_name}`} onConfirm={() => void run(() => revokeConsent(c.id))} /></td>
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
        <Note>Discovery document: <span className="font-mono">/oidc/{realm}/.well-known/openid-configuration</span></Note>
        <ActionError error={rot.error} />
      </Panel>
    </div>
  );
}

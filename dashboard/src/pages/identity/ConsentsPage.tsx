import { useState } from "react";
import { listOidcKeys, listRealmConsents, revokeRealmConsent, rotateOidcKeys } from "../../api";
import { ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { fmtIso, useRealm } from "./util";
import { Table } from "panelui-web/table";

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
          <Table className="w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Client app</Table.Head><Table.Head>User</Table.Head><Table.Head>Scopes</Table.Head><Table.Head>Granted</Table.Head><Table.Head>Last used</Table.Head><Table.Head /></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((c) => (
                <Table.Row key={c.id} className="border-t border-border">
                  <Table.Cell className="py-1.5">{c.client_name} <span className="font-mono text-xs text-muted-foreground">{c.client_id}</span></Table.Cell>
                  <Table.Cell className="font-mono text-xs">{c.collection}/{c.user_id}</Table.Cell>
                  <Table.Cell className="font-mono text-xs">{c.scopes.join(" ")}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtIso(c.created_at)}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtIso(c.updated_at)}</Table.Cell>
                  <Table.Cell className="text-right"><ConfirmButton label={`Revoke ${c.client_name}`} onConfirm={() => void run(() => revokeRealmConsent(realm, c.id))} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        <ActionError error={error} />
      </Panel>
      <Panel title="OIDC signing keys">
        <p className="text-sm text-foreground/80">Rotation adds a new signing key to the realm's JWKS; tokens signed by the previous key stay verifiable until they expire.</p>
        <div className="mt-3 flex items-center gap-3">
          <ConfirmButton label={`Rotate keys for ${realm}`} onConfirm={() => void rot.run(async () => setKid((await rotateOidcKeys(realm)).kid))} />
          {kid && <span role="status" className="text-sm text-success">New key id <code className="font-mono">{kid}</code></span>}
        </div>
        <ErrorView error={keys.error} />
        {keys.data && (
          <Table className="mt-3 w-full text-left text-sm">
            <Table.Header className="text-xs uppercase tracking-wide text-muted-foreground"><Table.Row><Table.Head className="py-2">Key id</Table.Head><Table.Head>Status</Table.Head><Table.Head>Created</Table.Head><Table.Head>Verifies until</Table.Head></Table.Row></Table.Header>
            <Table.Body>
              {keys.data.items.map((k) => (
                <Table.Row key={k.kid} className="border-t border-border">
                  <Table.Cell className="py-1.5 font-mono text-xs">{k.kid}</Table.Cell>
                  <Table.Cell>{k.kid === keys.data!.current ? "current" : k.status}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtIso(k.created_at)}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{k.status === "active" ? "in use" : fmtIso(k.verify_until)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        <Note>Discovery document: <span className="font-mono">/oidc/{realm}/.well-known/openid-configuration</span></Note>
        <ActionError error={rot.error} />
      </Panel>
    </div>
  );
}

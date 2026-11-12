import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { listOidcKeys, listRealmConsents, revokeRealmConsent, rotateOidcKeys } from "../../api";
import { SkeletonTable } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { TableCard, Tag } from "../billing/shared";
import { Field } from "./shared";
import { fmtIso, useRealm } from "./util";
import { Table } from "panelui-web/table";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";
import { ConfirmButton, CopyIcon } from "../../components/ui";
import { IdText } from "../../components/IdText";
import { Th } from "../../components/TableCard";

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
  const discovery = `/oidc/${realm}/.well-known/openid-configuration`;
  return (
    <div className="space-y-6">
      <TableCard
        title="Consents"
        count={l.data ? rows.length : undefined}
        description="A consent is a user's remembered “yes” to a client app, so the app is not asked about again. Revoke one to ask the user next time."
        toolbar={
          <Field label="Filter by client id" className="w-72">
            <Input size="sm" className="font-mono" value={client} placeholder="All clients" onChange={(e) => setClient(e.target.value)} />
          </Field>
        }
      >
        <ActionError error={l.error} />
        {l.loading && !l.error && <SkeletonTable rows={3} columns={5} label="Loading consents" />}
        {l.data && rows.length === 0 && <EmptyState compact title="No consents in this realm" why={client ? "No consent for this client id. Clear the filter to see all." : "A consent records that a user allowed a client app to act for them."} />}
        {rows.length > 0 && (
          <Table framed aria-label="Consents">
            <Table.Header><Table.Row><Th>Client app</Th><Th>User</Th><Th>Scopes</Th><Th>Granted</Th><Th>Last used</Th><Th><span className="sr-only">Actions</span></Th></Table.Row></Table.Header>
            <Table.Body>
              {rows.map((c) => (
                <Table.Row key={c.id}>
                  <Table.Cell><span className="block font-medium">{c.client_name}</span><span className="font-mono text-xs text-muted-foreground">{c.client_id}</span></Table.Cell>
                  <Table.Cell className="font-mono text-xs">{c.collection}/{c.user_id}</Table.Cell>
                  <Table.Cell><span className="flex max-w-xs flex-wrap gap-1">{c.scopes.map((s) => <Tag key={s}>{s}</Tag>)}</span></Table.Cell>
                  <Table.Cell className="whitespace-nowrap text-xs text-muted-foreground">{fmtIso(c.created_at)}</Table.Cell>
                  <Table.Cell className="whitespace-nowrap text-xs text-muted-foreground">{fmtIso(c.updated_at)}</Table.Cell>
                  <Table.Cell className="text-right"><ConfirmButton label={`Revoke ${c.client_name}`} short="Revoke" consequence="The user is asked for consent again next time the app signs them in." onConfirm={() => void run(() => revokeRealmConsent(realm, c.id))} /></Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
        <ActionError error={error} />
      </TableCard>
      <TableCard
        title="OIDC signing keys"
        description="Rotation adds a new signing key to the realm's JWKS; tokens signed by the previous key stay verifiable until they expire."
        actions={<ConfirmButton label={`Rotate keys for ${realm}`} short="Rotate keys" consequence="New tokens are signed with a new key. Tokens already issued keep working until they expire." onConfirm={() => void rot.run(async () => setKid((await rotateOidcKeys(realm)).kid))} />}
        footer={
          <p className="flex flex-wrap items-center gap-1 text-xs text-muted-foreground">
            Discovery document: <span className="font-mono text-foreground">{discovery}</span>
            <CopyIcon text={`${location.origin}${discovery}`} label="Copy discovery URL" />
          </p>
        }
      >
        {kid && <p role="status" className="text-sm text-success">New key id <code className="font-mono">{kid}</code></p>}
        <ActionError error={keys.error} />
        <ActionError error={rot.error} />
        {keys.data && (
          <Table framed aria-label="Signing keys">
            <Table.Header><Table.Row><Th>Key id</Th><Th>Status</Th><Th>Created</Th><Th>Verifies until</Th></Table.Row></Table.Header>
            <Table.Body>
              {keys.data.items.map((k) => (
                <Table.Row key={k.kid}>
                  <Table.Cell><IdText value={k.kid} what="key id" /></Table.Cell>
                  <Table.Cell><Badge status={k.kid === keys.data!.current ? "current" : k.status} /></Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{fmtIso(k.created_at)}</Table.Cell>
                  <Table.Cell className="text-xs text-muted-foreground">{k.status === "active" ? "in use" : fmtIso(k.verify_until)}</Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </TableCard>
    </div>
  );
}

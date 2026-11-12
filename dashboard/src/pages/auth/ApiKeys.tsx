import { useMemo, useState } from "react";
import { listApiKeys, listServiceAccounts } from "../../api";
import { useOptionalSession } from "../../auth/session";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import CreateKeyDialog from "./CreateKeyDialog";
import { KeyTable } from "./KeyTable";
import { useAdminEmails } from "./words";
import { CardBody, TableCard } from "../../components/TableCard";

/** API keys for apps (the main list and the Create API key sheet, `?new=1`) and, below, personal tokens for scripts you run yourself. */
export default function ApiKeys() {
  const list = useLoad(listApiKeys, "apikeys");
  const accounts = useLoad(listServiceAccounts, "apikeys:accounts");
  const admins = useAdminEmails();
  const me = useOptionalSession()?.admin;
  const [creating, setCreating] = useCreateParam();
  const [newToken, setNewToken] = useState(false);
  const keys = (list.data ?? []).filter((k) => k.kind !== "pat");
  const tokens = (list.data ?? []).filter((k) => k.kind === "pat");
  const owners = useMemo(() => {
    const m = new Map<string, string>(admins);
    for (const a of accounts.data ?? []) m.set(a.id, a.name);
    if (me) m.set(me.id, "You");
    return m;
  }, [admins, accounts.data, me]);
  const active = (rows: typeof keys) => rows.filter((k) => !k.revoked).length;

  if (list.error) return <ErrorPanel error={list.error} onRetry={list.reload} />;

  return (
    <div className="space-y-6">
      <TableCard
        title="API keys"
        count={list.data ? active(keys) : undefined}
        description="Keys your apps send with each request, in the X-API-Key header. Each belongs to a service account and can only do what its scopes allow."
      >
        {list.loading && !list.data && <CardBody><SkeletonTable rows={3} columns={6} label="Loading keys" /></CardBody>}
        {list.data && keys.length === 0 && (
          <CardBody>
            <EmptyState
              title="No API keys yet"
              why="Your app needs a key to call ForgeBase. Make one that can only do what the app needs, for example read one collection."
              action={<Button tone="primary" onClick={() => setCreating(true)}>Create API key</Button>}
              docs="docs/adr/0004-auth.md"
            />
          </CardBody>
        )}
        {keys.length > 0 && <KeyTable rows={keys} owners={owners} showOwner onRevoked={list.reload} label="API keys list" />}
      </TableCard>

      {list.data && (
        <TableCard
          title="Personal tokens"
          count={active(tokens)}
          description="For scripts you run yourself. A token acts as you; use an API key for anything that runs in production."
          actions={<Button onClick={() => setNewToken(true)}>New personal token</Button>}
        >
          {tokens.length === 0 ? (
            <CardBody>
              <EmptyState compact title="No personal tokens" why="Make one when a script of yours needs to call ForgeBase as you." />
            </CardBody>
          ) : (
            <KeyTable rows={tokens} owners={owners} showOwner onRevoked={list.reload} label="Personal tokens list" />
          )}
        </TableCard>
      )}
      <CreateKeyDialog open={creating} onOpenChange={setCreating} onCreated={() => { list.reload(); accounts.reload(); }} />
      <CreateKeyDialog kind="token" open={newToken} onOpenChange={setNewToken} onCreated={list.reload} />
    </div>
  );
}

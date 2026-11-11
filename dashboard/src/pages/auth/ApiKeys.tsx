import { useState } from "react";
import { listApiKeys } from "../../api";
import { EmptyState } from "../../components/EmptyState";
import { ErrorPanel } from "../../components/ErrorPanel";
import { SkeletonTable } from "../../components/Skeleton";
import { Button } from "../../components/ui";
import { useCreateParam, useLoad } from "../../hooks";
import CreateKeyDialog from "./CreateKeyDialog";
import { KeyTable } from "./ServiceAccounts";

/** API keys for apps (the main list and the Create API key sheet, `?new=1`) and, below, personal tokens for scripts you run yourself. */
export default function ApiKeys() {
  const list = useLoad(listApiKeys, "apikeys");
  const [creating, setCreating] = useCreateParam();
  const [newToken, setNewToken] = useState(false);
  const keys = (list.data ?? []).filter((k) => k.kind !== "pat");
  const tokens = (list.data ?? []).filter((k) => k.kind === "pat");
  return (
    <div className="space-y-8">
      <ErrorPanel error={list.error} onRetry={list.reload} />
      {list.loading && !list.error && <SkeletonTable rows={3} columns={6} label="Loading keys" />}
      {list.data && (
        <section aria-label="API keys" className="space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <h2 className="text-lg font-semibold tracking-tight">API keys</h2>
              <p className="text-sm text-muted-foreground">Keys your apps send with each request. Each belongs to a service account.</p>
            </div>
            <Button tone="primary" onClick={() => setCreating(true)}>Create API key</Button>
          </div>
          {keys.length === 0 ? (
            <EmptyState
              title="No API keys yet"
              why="Your app needs a key to call ForgeBase. Make one that can only do what the app needs, for example read one collection."
              action={<Button tone="primary" onClick={() => setCreating(true)}>Create API key</Button>}
              docs="docs/adr/0004-auth.md"
            />
          ) : (
            <KeyTable rows={keys} showOwner onRevoked={list.reload} />
          )}
        </section>
      )}
      {list.data && (
        <section aria-label="Personal tokens" className="space-y-3">
          <div className="flex flex-wrap items-center justify-between gap-2">
            <div>
              <h2 className="text-lg font-semibold tracking-tight">Personal tokens (for scripts you run yourself)</h2>
              <p className="text-sm text-muted-foreground">A token acts as you. Use a key above for anything that runs in production.</p>
            </div>
            <Button onClick={() => setNewToken(true)}>New personal token</Button>
          </div>
          {tokens.length === 0 ? (
            <EmptyState compact title="No personal tokens" why="Make one when a script of yours needs to call ForgeBase as you." />
          ) : (
            <KeyTable rows={tokens} showOwner onRevoked={list.reload} />
          )}
        </section>
      )}
      <CreateKeyDialog open={creating} onOpenChange={setCreating} onCreated={list.reload} />
      <CreateKeyDialog kind="token" open={newToken} onOpenChange={setNewToken} onCreated={list.reload} />
    </div>
  );
}

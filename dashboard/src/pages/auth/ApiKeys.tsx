import { useState } from "react";
import { createPat, listApiKeys } from "../../api";
import { Button, ErrorView, Loading, Panel, inputCls } from "../../components/ui";
import { useLoad } from "../../hooks";
import { SecretModal, splitList, useAction } from "./common";
import { KeyTable } from "./ServiceAccounts";

export default function ApiKeys() {
  const list = useLoad(listApiKeys, "apikeys");
  const [name, setName] = useState("");
  const [scopes, setScopes] = useState("");
  const [days, setDays] = useState("");
  const [secret, setSecret] = useState<string>();
  const act = useAction();
  return (
    <div className="space-y-4">
      <Panel title="New personal access token (for you)">
        <form
          className="flex flex-wrap items-end gap-2"
          onSubmit={async (e) => {
            e.preventDefault();
            const ok = await act.run(async () => {
              const k = await createPat({ name: name.trim(), scopes: splitList(scopes), ...(days ? { expires_in_days: Number(days) } : {}) });
              setSecret(k.key);
            });
            if (ok) {
              setName("");
              setScopes("");
              setDays("");
              list.reload();
            }
          }}
        >
          <input aria-label="Token name" className={inputCls} placeholder="Token name" value={name} onChange={(e) => setName(e.target.value)} />
          <input aria-label="Token scopes" className={`${inputCls} w-64`} placeholder="collections:read" value={scopes} onChange={(e) => setScopes(e.target.value)} />
          <input aria-label="Token expires in days" className={`${inputCls} w-28`} type="number" min={1} placeholder="Days" value={days} onChange={(e) => setDays(e.target.value)} />
          <Button type="submit" tone="primary" disabled={act.busy || !name.trim() || splitList(scopes).length === 0}>
            Create token
          </Button>
        </form>
        {act.errorView}
      </Panel>
      <ErrorView error={list.error} />
      {list.loading && !list.error && <Loading what="Loading keys" />}
      {list.data?.length === 0 && <p className="text-sm text-zinc-400">No API keys or tokens.</p>}
      {!!list.data?.length && <KeyTable showOwner rows={list.data} onRevoked={list.reload} />}
      {secret && <SecretModal label="Token" secret={secret} onClose={() => setSecret(undefined)} />}
    </div>
  );
}

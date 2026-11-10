import { useState } from "react";
import { addDomain, deleteDomain, listDomains, reissueDomain, verifyDomain, type Domain } from "../../api";
import { Button, ConfirmButton, ErrorView, Loading, Panel } from "../../components/ui";
import { useLoad } from "../../hooks";
import { ActionError, Empty, TextField } from "../access/shared";
import { useRun } from "../access/util";
import { Note } from "./shared";
import { fmtTime, useRealm } from "./util";

/** Custom domains: claim a host, publish the TXT record, verify, see status. */
export default function DomainsPage() {
  const realm = useRealm();
  const l = useLoad((s) => listDomains(realm, s), `domains:${realm}`);
  const [host, setHost] = useState("");
  const [msg, setMsg] = useState<Record<string, string>>({});
  const { run, error, busy } = useRun(l.reload);
  return (
    <div className="space-y-6">
      <Panel title={`Custom domains of ${realm}`}>
        <ErrorView error={l.error} />
        {l.loading && !l.error && <Loading what="Loading domains" />}
        {l.data?.length === 0 && <Empty>No custom domains yet.</Empty>}
        <ul className="space-y-3">
          {l.data?.map((d) => (
            <li key={d.id} className="rounded-md border border-border p-3 text-sm">
              <div className="flex flex-wrap items-center justify-between gap-2">
                <span><span className="font-mono">{d.host}</span> <StatusBadge d={d} /></span>
                <span className="space-x-2">
                  {d.status === "pending" && <Button aria-label={`Verify ${d.host}`} disabled={busy} onClick={() => void run(async () => {
                    const r = await verifyDomain(d.id);
                    setMsg((m) => ({ ...m, [d.id]: r.verified ? "" : (r.reason ?? "the TXT record was not found yet") }));
                  })}>Verify</Button>}
                  {d.status === "pending" && <Button aria-label={`New token ${d.host}`} onClick={() => void run(() => reissueDomain(d.id))}>New token</Button>}
                  <ConfirmButton label={`Remove ${d.host}`} onConfirm={() => void run(() => deleteDomain(d.id))} />
                </span>
              </div>
              {d.verification && d.status === "pending" && (
                <dl aria-label={`TXT record for ${d.host}`} className="mt-2 grid grid-cols-[6rem_1fr] gap-x-3 gap-y-1 text-xs">
                  <dt className="text-muted-foreground">Type</dt><dd className="font-mono">{d.verification.record_type}</dd>
                  <dt className="text-muted-foreground">Name</dt><dd className="font-mono break-all">{d.verification.record_name}.{d.host}</dd>
                  <dt className="text-muted-foreground">Value</dt><dd className="font-mono break-all">{d.verification.record_value}</dd>
                  <dt className="text-muted-foreground">Expires</dt><dd>{fmtTime(d.verification.expires_at)}</dd>
                </dl>
              )}
              {msg[d.id] && <p role="status" className="mt-2 text-xs text-warning">Not verified: {msg[d.id]}</p>}
              {d.last_error && d.status === "pending" && <p className="mt-1 text-xs text-muted-foreground">Last check {fmtTime(d.last_checked)}: {d.last_error}</p>}
              {d.status === "verified" && <p className="mt-1 text-xs text-muted-foreground">Verified {fmtTime(d.verified_at)}</p>}
            </li>
          ))}
        </ul>
        <ActionError error={error} />
      </Panel>
      <Panel title="Claim a domain">
        <div className="flex flex-wrap items-end gap-3">
          <TextField label="Host" value={host} onChange={setHost} placeholder="login.example.com" />
          <Button tone="primary" disabled={!host || busy} onClick={() => void run(async () => { await addDomain(host.trim().toLowerCase(), realm); setHost(""); })}>Claim domain</Button>
        </div>
        <Note>After claiming, create the TXT record shown, wait for DNS to propagate, then verify. Requests arriving on a verified host use this realm's branding.</Note>
      </Panel>
    </div>
  );
}

function StatusBadge({ d }: { d: Domain }) {
  return <span data-status={d.status} className={`ml-2 rounded px-1.5 py-0.5 text-xs ${d.status === "verified" ? "bg-success/15 text-success" : "bg-warning/15 text-warning"}`}>{d.status}</span>;
}

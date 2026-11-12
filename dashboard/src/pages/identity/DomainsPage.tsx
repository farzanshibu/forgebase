import { useState } from "react";
import { EmptyState } from "../../components/EmptyState";
import { addDomain, deleteDomain, listDomains, reissueDomain, verifyDomain, type Domain } from "../../api";
import { Button, ConfirmButton, CopyIcon } from "../../components/ui";
import { SkeletonLines } from "../../components/Skeleton";
import { useLoad } from "../../hooks";
import { ActionError } from "../access/shared";
import { useRun } from "../access/util";
import { TableCard } from "../billing/shared";
import { Field } from "./shared";
import { fmtTime, useRealm } from "./util";
import { Input } from "panelui-web/input";
import { Badge } from "../../components/StatusBadge";

const HOST = /^(?=.{1,253}$)([a-z0-9]([a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z0-9-]{2,63}$/;

/** Custom domains: claim a host, publish the TXT record, verify, see status. */
export default function DomainsPage() {
  const realm = useRealm();
  const l = useLoad((s) => listDomains(realm, s), `domains:${realm}`);
  const [host, setHost] = useState("");
  const [msg, setMsg] = useState<Record<string, string>>({});
  const { run, error, busy } = useRun(l.reload);
  const clean = host.trim().toLowerCase();
  const hostError = clean && !HOST.test(clean) ? "A host name like login.example.com" : undefined;
  return (
    <div className="space-y-6">
      <TableCard
        title="Custom domains"
        count={l.data?.length}
        description={<>Serve realm <span className="font-mono">{realm}</span>'s sign-in pages from your own address. Requests on a verified host use this realm's branding.</>}
        toolbar={
          <form className="flex w-full flex-wrap items-end gap-3" onSubmit={(e) => { e.preventDefault(); if (!hostError && clean) void run(async () => { await addDomain(clean, realm); setHost(""); }); }}>
            <Field label="Host" className="w-72" help={hostError}>
              <Input size="sm" className="font-mono" value={host} placeholder="login.example.com" aria-invalid={hostError ? true : undefined} onChange={(e) => setHost(e.target.value)} />
            </Field>
            <Button type="submit" tone="primary" disabled={!clean || !!hostError || busy}>Claim domain</Button>
            <p className="basis-full text-xs text-muted-foreground">After claiming, add the TXT record shown to your DNS, wait for it to propagate, then verify.</p>
          </form>
        }
      >
        <ActionError error={l.error} />
        <ActionError error={error} />
        {l.loading && !l.error && <div className="p-4"><SkeletonLines lines={2} label="Loading domains" /></div>}
        {l.data?.length === 0 && <EmptyState compact title="No custom domains yet" why="A custom domain lets sign-in pages use your own address, such as login.example.com. Claim one above." />}
        {l.data && l.data.length > 0 && (
          <ul aria-label="Domains" className="divide-y divide-border">
            {l.data.map((d) => (
              <li key={d.id} className="space-y-3 px-4 py-4 text-sm">
                <div className="flex flex-wrap items-center justify-between gap-2">
                  <span className="flex min-w-0 items-center gap-2"><span className="truncate font-mono font-medium">{d.host}</span> <StatusBadge d={d} /></span>
                  <span className="flex flex-wrap gap-2">
                    {d.status === "pending" && <Button aria-label={`New token ${d.host}`} disabled={busy} onClick={() => void run(() => reissueDomain(d.id))}>New token</Button>}
                    {d.status === "pending" && <Button tone="primary" aria-label={`Verify ${d.host}`} disabled={busy} onClick={() => void run(async () => {
                      const r = await verifyDomain(d.id);
                      setMsg((m) => ({ ...m, [d.id]: r.verified ? "" : (r.reason ?? "the TXT record was not found yet") }));
                    })}>Verify</Button>}
                    <ConfirmButton label={`Remove ${d.host}`} short="Remove" consequence="Sign-in pages stop answering on this host. You can claim it again later." onConfirm={() => void run(() => deleteDomain(d.id))} />
                  </span>
                </div>
                {d.verification && d.status === "pending" && (
                  <div className="rounded-lg border border-border bg-muted/30">
                    <p className="border-b border-border px-3 py-2 text-xs font-medium">Add this record at your DNS provider</p>
                    <dl aria-label={`TXT record for ${d.host}`} className="grid grid-cols-[5rem_minmax(0,1fr)] gap-x-3 gap-y-2 px-3 py-2.5 text-xs">
                      <dt className="text-muted-foreground">Type</dt><dd className="font-mono">{d.verification.record_type}</dd>
                      <dt className="text-muted-foreground">Name</dt>
                      <dd className="flex min-w-0 items-center gap-1"><span className="break-all font-mono">{d.verification.record_name}.{d.host}</span><CopyIcon text={`${d.verification.record_name}.${d.host}`} label="Copy record name" /></dd>
                      <dt className="text-muted-foreground">Value</dt>
                      <dd className="flex min-w-0 items-center gap-1"><span className="break-all font-mono">{d.verification.record_value}</span><CopyIcon text={d.verification.record_value} label="Copy record value" /></dd>
                      <dt className="text-muted-foreground">Expires</dt><dd>{fmtTime(d.verification.expires_at)}</dd>
                    </dl>
                  </div>
                )}
                {msg[d.id] && <p role="status" className="text-xs font-medium text-warning">Not verified: {msg[d.id]}</p>}
                {d.last_error && d.status === "pending" && <p className="text-xs text-muted-foreground">Last check {fmtTime(d.last_checked)}: {d.last_error}</p>}
                {d.status === "verified" && <p className="text-xs text-muted-foreground">Verified {fmtTime(d.verified_at)}</p>}
              </li>
            ))}
          </ul>
        )}
      </TableCard>
    </div>
  );
}

function StatusBadge({ d }: { d: Domain }) {
  return <span data-status={d.status}><Badge status={d.status === "verified" ? "verified" : "pending"}>{d.status}</Badge></span>;
}

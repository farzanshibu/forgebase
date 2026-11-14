import { LockPasswordIcon } from "@hugeicons/core-free-icons";
import type { TlsStatus } from "../../api";
import { Badge } from "../../components/StatusBadge";
import { Chips } from "../../components/Chips";
import { Callout, When } from "../jobs/ops";
import { Card, Row } from "./cards";
import { TLS_WARN_DAYS, expiryWords } from "./util";

const MODE_WORDS: Record<string, string> = { off: "Off", files: "Certificate files", acme: "Automatic (ACME)" };

/**
 * The server's own HTTPS (ADR-0033, `info.tls`): the mode, the certificate it serves with a warning when it expires within
 * 14 days (the same threshold as `forgebase doctor`), and in ACME mode the domains and when it renews next.
 */
export default function TlsCard({ tls }: { tls: TlsStatus | undefined }) {
  if (!tls) return null;
  if (tls.mode === "off") {
    return (
      <Card title="HTTPS (TLS)" icon={LockPasswordIcon}>
        <dl><Row label="Mode" value={<Badge tone="neutral">off</Badge>} /></dl>
        <p className="mt-2 text-xs text-muted-foreground">The server speaks plain HTTP. Terminate TLS in a proxy in front of it, or set <code>tls.mode</code> to <code>files</code> or <code>acme</code>.</p>
      </Card>
    );
  }
  const cert = tls.certificate ?? undefined;
  const left = cert ? expiryWords(cert.expires_in_secs) : undefined;
  return (
    <Card title="HTTPS (TLS)" icon={LockPasswordIcon}>
      {left && left.level !== "ok" && (
        <div className="mb-3">
          <Callout tone={left.level === "bad" ? "bad" : "warn"} role={left.level === "bad" ? "alert" : "status"} title={left.level === "bad" ? "The certificate has expired" : `The certificate expires within ${TLS_WARN_DAYS} days`}>
            {left.text}. {tls.mode === "acme" ? "Renewal normally happens well before this: check the last error below." : "Replace the certificate files; the server reloads them without a restart."}
          </Callout>
        </div>
      )}
      <dl>
        <Row label="Mode" value={<Badge tone="ok" dot>{MODE_WORDS[tls.mode] ?? tls.mode}</Badge>} />
        {tls.min_version && <Row label="Oldest TLS version" value={tls.min_version} />}
        {tls.hsts_max_age_secs !== undefined && <Row label="HSTS" value={tls.hsts_max_age_secs > 0 ? `${tls.hsts_max_age_secs.toLocaleString()} s` : "off"} />}
        {tls.http_listener && <Row label="HTTP listener" value={tls.http_listener} mono />}
        {tls.resp !== undefined && <Row label="Redis protocol" value={tls.resp ? "TLS" : "plain"} />}
        {cert ? (
          <>
            <Row label="Certificate" value={cert.subject || "(no subject)"} />
            <Row label="Issuer" value={cert.issuer} />
            <Row label="Valid until" value={<span title={cert.not_after}><When iso={cert.not_after} /></span>} />
            <Row label="Expires in" value={left ? <Badge tone={left.level}>{left.text}</Badge> : "—"} />
            <div className="border-b border-border/60 py-2 text-sm"><dt className="text-muted-foreground">Names</dt><dd className="mt-1"><Chips items={cert.names} max={8} /></dd></div>
          </>
        ) : (
          <Row label="Certificate" value={<Badge tone="warn">none loaded yet</Badge>} />
        )}
        {tls.mode === "files" && (
          <>
            {tls.cert_file && <Row label="Certificate file" value={tls.cert_file} mono />}
            {tls.last_reload !== undefined && <Row label="Last reload" value={<When iso={tls.last_reload} fallback="Not yet" />} />}
          </>
        )}
        {tls.mode === "acme" && (
          <>
            <div className="border-b border-border/60 py-2 text-sm"><dt className="text-muted-foreground">Domains</dt><dd className="mt-1"><Chips items={tls.domains ?? []} /></dd></div>
            <div className="border-b border-border/60 py-2 text-sm"><dt className="text-muted-foreground">Domain checks</dt><dd className="mt-1"><Chips items={tls.challenges ?? []} /></dd></div>
            {tls.directory_url && <Row label="Certificate authority" value={tls.directory_url} mono />}
            <Row label="Last renewal" value={<When iso={tls.last_renewal} fallback="Not yet" />} />
            <Row label="Next renewal" value={<When iso={tls.next_renewal} fallback="Not scheduled" />} />
            <Row label="Renewal failures" value={<Badge tone={(tls.renewal_failures ?? 0) > 0 ? "bad" : "ok"}>{tls.renewal_failures ?? 0}</Badge>} />
          </>
        )}
      </dl>
      {tls.last_error && (
        <p role="alert" className="mt-3 rounded-md border border-destructive/40 bg-destructive/10 p-2 text-sm text-destructive">
          Last error{tls.last_error_at ? <> (<When iso={tls.last_error_at} />)</> : ""}: {tls.last_error}
        </p>
      )}
    </Card>
  );
}

/**
 * Direct logins (FB-155): named PostgreSQL logins for tools such as psql, `/api/v1/system/db-logins`. A login is bound
 * to one principal (and one tenant) and sees only what that principal may see. The password is shown once, when the
 * login is created or rotated; ForgeBase keeps only its SCRAM verifier. `has_claims: false` means the principal or
 * tenant is gone, so the login sees nothing.
 */
import { useState } from "react";
import { Input } from "panelui-web/input";
import { Table } from "panelui-web/table";
import { createDbLogin, deleteDbLogin, listDbLogins, rotateDbLogin, type LoginSecret } from "../../api";
import { Check } from "../../components/controls";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { CreateSheet, DetailSheet } from "../../components/CreateSheet";
import { EmptyState } from "../../components/EmptyState";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { SkeletonTable } from "../../components/Skeleton";
import { Button, CopyButton, ErrorView } from "../../components/ui";
import { fieldErrors } from "../../errors";
import { accessWords, seesNothing } from "./pgHelpers";
import { useCreateParam, useLoad } from "../../hooks";
import { notify } from "../../toaster";
import { Badge } from "panelui-web/badge";
import { TableCard } from "./parts";

const NAME = /^[a-z][a-z0-9_]{0,39}$/;
const PRINCIPAL = /^(user:[^/\s]+\/[^\s]+|service:[^\s]+)$/;

/** Shows a new password once. Closing it is the only way to dismiss: there is no second look. */
function SecretSheet({ secret, onClose }: { secret: LoginSecret | undefined; onClose: () => void }) {
  return (
    <DetailSheet
      open={!!secret}
      onOpenChange={(o) => { if (!o) onClose(); }}
      title={secret ? `Password for ${secret.login.role}` : "Password"}
      description="Shown once. ForgeBase keeps only a verifier, so it cannot show this password again."
      footer={<Button tone="primary" onClick={onClose}>I have copied it</Button>}
    >
      {secret && (
        <>
          <div role="alert" className="rounded-lg border border-warning/50 bg-warning/10 p-3 text-sm">Copy the password now. To get a new one later, rotate it.</div>
          <div>
            <p className="mb-1 text-xs text-muted-foreground">Password</p>
            <div className="flex items-center gap-2"><code data-testid="login-password" className="min-w-0 flex-1 break-all rounded border border-border p-2 font-mono text-sm">{secret.password}</code><CopyButton text={secret.password} label="Copy password" /></div>
          </div>
          <div>
            <p className="mb-1 text-xs text-muted-foreground">Connect with psql (it asks for the password)</p>
            <div className="flex items-center gap-2"><code data-testid="login-hint" className="min-w-0 flex-1 break-all rounded border border-border p-2 font-mono text-xs">psql {secret.hint}</code><CopyButton text={`psql ${secret.hint}`} label="Copy command" /></div>
          </div>
          {seesNothing(secret.login) && <p role="status" className="text-sm text-warning">This login sees nothing yet: its user, service account or tenant does not exist.</p>}
        </>
      )}
    </DetailSheet>
  );
}

function NewLoginSheet({ open, onOpenChange, onCreated }: { open: boolean; onOpenChange: (o: boolean) => void; onCreated: (s: LoginSecret) => void }) {
  const [name, setName] = useState("");
  const [principal, setPrincipal] = useState("");
  const [tenant, setTenant] = useState("");
  const [readonly, setReadonly] = useState(true);
  const [admin, setAdmin] = useState(false);
  const [limit, setLimit] = useState("");
  const [validUntil, setValidUntil] = useState("");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  const nameError = !name ? "Required" : NAME.test(name) ? undefined : "Lower-case letters, digits and underscores, starting with a letter (at most 40).";
  const principalError = !principal && admin ? undefined : !principal ? "Required" : PRINCIPAL.test(principal) ? undefined : "Use user:<collection>/<id> or service:<id>.";
  const limitError = limit && !/^\d+$/.test(limit) ? "A whole number from 1 to 100." : undefined;
  const by = fieldErrors(saveError);
  const ok = !nameError && !principalError && !limitError;
  return (
    <CreateSheet
      open={open} onOpenChange={(o) => { onOpenChange(o); if (!o) setTouched(false); }}
      title="New direct login"
      description="A PostgreSQL login for one user or service account, for tools such as psql."
      submitLabel="Create login"
      canSubmit={ok}
      onError={(e) => { setSaveError(e); setTouched(true); }}
      onSubmit={async () => {
        const s = await createDbLogin({
          name, ...(principal ? { principal } : {}), ...(tenant ? { tenant } : {}), readonly, admin,
          ...(limit ? { connection_limit: Number(limit) } : {}), ...(validUntil ? { valid_until: new Date(validUntil).toISOString() } : {}),
        });
        onCreated(s);
        setName(""); setPrincipal(""); setTenant(""); setLimit(""); setValidUntil(""); setAdmin(false); setReadonly(true); setTouched(false);
        return { what: `Login ${s.login.name}` };
      }}
    >
      <FormField label="Name" required help="The PostgreSQL role is fbu_<name>." error={(touched ? nameError : undefined) ?? by.name}>
        {(p) => <Input {...p} value={name} onChange={(e) => { setName(e.target.value); setTouched(true); }} placeholder="reporting" />}
      </FormField>
      <FormField label="Acts as" required={!admin} help="user:<collection>/<id> or service:<id>. Row and field rules apply as for that principal." error={(touched ? principalError : undefined) ?? by.principal}>
        {(p) => <Input {...p} value={principal} onChange={(e) => { setPrincipal(e.target.value); setTouched(true); }} placeholder="service:reporting" />}
      </FormField>
      <FormField label="Tenant" help="Optional. Limits the login to one tenant." error={by.tenant}>
        {(p) => <Input {...p} value={tenant} onChange={(e) => setTenant(e.target.value)} />}
      </FormField>
      <Check checked={readonly} onChange={setReadonly} label="Read only" description="The login can read but not write." />
      <Check checked={admin} onChange={setAdmin} label="Administrator" description="SQL editor rights; bypasses row-level security. Platform administrators only." />
      <FormField label="Connection limit" help="1 to 100, default 10." error={(touched ? limitError : undefined) ?? by.connection_limit}>
        {(p) => <Input {...p} inputMode="numeric" value={limit} onChange={(e) => setLimit(e.target.value)} />}
      </FormField>
      <FormField label="Expires" help="Optional. The login stops working after this date." error={by.valid_until}>
        {(p) => <Input {...p} type="date" value={validUntil} onChange={(e) => setValidUntil(e.target.value)} />}
      </FormField>
    </CreateSheet>
  );
}

export default function Logins() {
  const [creating, onCreatingChange] = useCreateParam(); // the header's "New direct login" opens it with ?new=1
  const { data, error, loading, reload } = useLoad(listDbLogins, "db-logins");
  const [secret, setSecret] = useState<LoginSecret>();
  const [problem, setProblem] = useState<unknown>();
  async function rotate(name: string) {
    setProblem(undefined);
    try {
      setSecret(await rotateDbLogin(name));
      reload();
    } catch (e) {
      setProblem(e);
    }
  }
  return (
    <div className="space-y-4">
      <p className="text-sm text-muted-foreground">A <HelpTip term="direct_login">direct login</HelpTip> lets a tool such as psql connect to PostgreSQL as one user or service account. It sees only what that principal may see.</p>
      <ErrorView error={error} onRetry={reload} />
      <ErrorView error={problem} />
      {loading && !data && !error && <SkeletonTable rows={3} columns={6} label="Loading direct logins" />}
      {data && data.length === 0 && (
        <EmptyState
          title="No direct logins yet"
          why="A direct login lets a tool such as psql, a BI dashboard or a report job read your data with the same row rules as one user or service account."
          action={<Button tone="primary" onClick={() => onCreatingChange(true)}>New direct login</Button>}
          docs="docs/database.md"
        />
      )}
      {data && data.length > 0 && (
        <TableCard title="Direct logins" count={data.length}>
          <Table framed density="compact" aria-label="Direct logins">
            <Table.Header className="bg-muted/40">
              <Table.Row><Table.Head>Login</Table.Head><Table.Head>Acts as</Table.Head><Table.Head>Access</Table.Head><Table.Head>Expires</Table.Head><Table.Head>Status</Table.Head><Table.Head align="end"><span className="sr-only">Actions</span></Table.Head></Table.Row>
            </Table.Header>
            <Table.Body>
              {data.map((l) => (
                <Table.Row key={l.name}>
                  <Table.Cell className="py-2 font-mono text-xs">{l.role}</Table.Cell>
                  <Table.Cell className="py-2 text-xs"><span className="font-mono">{l.principal}</span>{l.tenant ? <span className="text-muted-foreground"> · tenant {l.tenant}</span> : ""}</Table.Cell>
                  <Table.Cell className="py-2 text-xs">{accessWords(l)}</Table.Cell>
                  <Table.Cell className="py-2 whitespace-nowrap text-xs tabular-nums text-muted-foreground">{l.valid_until ? l.valid_until.slice(0, 10) : "never"}</Table.Cell>
                  <Table.Cell className="py-2">
                    {seesNothing(l)
                      ? <Badge variant="warning" size="sm" dot title="The user, service account or tenant it acts as no longer exists, so every query returns no rows.">Sees nothing</Badge>
                      : <Badge variant="success" size="sm" dot>{l.admin ? "Full access" : "Active"}</Badge>}
                  </Table.Cell>
                  <Table.Cell align="end" className="space-x-2 py-2 whitespace-nowrap">
                    <ConfirmDialog
                      trigger={<Button aria-label={`Rotate ${l.name}`}>Rotate password</Button>}
                      title={`Rotate the password of ${l.role}?`}
                      consequence="The old password stops working for new connections. Open sessions stay. You will see the new password once."
                      confirmLabel="Rotate password"
                      tone="warning"
                      onConfirm={() => rotate(l.name)}
                    />
                    <ConfirmDialog
                      trigger={<Button tone="danger" aria-label={`Revoke ${l.name}`}>Revoke</Button>}
                      title={`Revoke login ${l.role}?`}
                      consequence="Its sessions end now and the PostgreSQL role is removed. Tools that use it stop working."
                      confirmLabel="Revoke login"
                      confirmText={l.name}
                      onConfirm={async () => { await deleteDbLogin(l.name); notify.deleted(`Login ${l.name}`); reload(); }}
                    />
                  </Table.Cell>
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        </TableCard>
      )}
      <NewLoginSheet open={creating} onOpenChange={onCreatingChange} onCreated={(s) => { setSecret(s); reload(); }} />
      <SecretSheet secret={secret} onClose={() => setSecret(undefined)} />
    </div>
  );
}

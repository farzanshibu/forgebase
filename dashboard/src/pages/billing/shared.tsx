/**
 * Billing's own building blocks (Identity borrows them): money in tabular digits, a dash for a missing value, filter
 * rows, form sections, the success note, customer links and the provider cards. Badges, stat cards, ids, times and
 * column headings are the shared components (components/StatusBadge, Stats, IdText, Time, TableCard).
 *
 * Usage:
 *   <TableCard title="Invoices" count={rows.length} toolbar={<Filter label="Status">{(id) => <Sel id={id} … />}</Filter>}>
 *     <ErrorView error={e} />                      // an error, empty state or skeleton here is padded for you
 *     <Table framed aria-label="Invoices">…<Table.Cell align="end"><Money … /></Table.Cell>…</Table>
 *   </TableCard>
 *   <Badge status="past_due" />                    // components/StatusBadge: "past due", red
 */
import { Tick02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { useId, type ComponentProps, type ReactNode } from "react";
import { Link } from "react-router-dom";
import { isBillingDisabled, type ProviderAccount } from "../../api";
import { Chip } from "../../components/Chips";
import { Badge } from "../../components/StatusBadge";
import { TableCard as BaseTableCard } from "../../components/TableCard";
import { CopyIcon, ErrorView } from "../../components/ui";
import { formatMoney } from "./money";

/** A quiet tag for a kind (recurring, metered, plan): not a state, so no colour. */
export function Tag({ children }: { children: ReactNode }) {
  return <Chip mono={false}>{children}</Chip>;
}

/** An amount in its currency, in equal-width digits. Put it in a cell with `align="end"`. */
export const Money = ({ amount, currency, className = "" }: { amount: number; currency: string; className?: string }) => (
  <span className={`whitespace-nowrap tabular-nums ${className}`.trim()}>{formatMoney(amount, currency)}</span>
);

/** A dash for a value that is not there, so an empty cell never looks broken. */
export const None = () => <span className="text-muted-foreground" title="None">—</span>;

/**
 * The shared TableCard; an error, empty state, skeleton or paragraph placed straight in it gets the card's padding,
 * so Billing and Identity pages need no CardBody around them.
 */
export function TableCard({ children, ...p }: ComponentProps<typeof BaseTableCard>) {
  return (
    <BaseTableCard {...p}>
      <div className="[&>section]:m-4 [&>[role=status]]:p-4 [&>[role=alert]]:m-4 [&>p]:px-4 [&>p]:py-3">{children}</div>
    </BaseTableCard>
  );
}

/** A control with its label on the left, for filter rows. The child gets the id to put on the control. */
export function Filter({ label, children }: { label: string; children: (id: string) => ReactNode }) {
  const id = `flt${useId().replace(/:/g, "")}`;
  return (
    <div className="flex items-center gap-2">
      <label htmlFor={id} className="whitespace-nowrap text-xs font-medium text-muted-foreground">
        {label}
      </label>
      {children(id)}
    </div>
  );
}

/** The billing routes answer 404 while billing is off: say so instead of showing a raw error. */
export function Disabled() {
  return (
    <div role="status" aria-label="Billing disabled" className="rounded-xl border border-border bg-card p-6">
      <h2 className="text-lg font-semibold tracking-tight">Billing is disabled on this server</h2>
      <p className="mt-2 max-w-3xl text-sm text-muted-foreground">
        Billing is on by default, so it was turned off here (<code className="rounded bg-muted px-1">[billing] enabled = false</code> or <code className="rounded bg-muted px-1">FORGEBASE_BILLING_ENABLED=false</code>),
        or this server is an older build without it. Set <code className="rounded bg-muted px-1">[billing] enabled = true</code> (or remove the setting) and restart to turn it on, or use <Link className="text-primary underline-offset-2 hover:underline" to="/system/features">Features and settings</Link>.
      </p>
    </div>
  );
}

/** Error view that turns a 404 on a billing route into the disabled state. */
export function BillingError({ error }: { error: unknown }) {
  if (!error) return null;
  if (isBillingDisabled(error)) return <Disabled />;
  return <ErrorView error={error} />;
}

/** A customer (`org:acme` or a user key), linked to its page in Customers. */
export function SubjectLink({ subject }: { subject: string }) {
  return (
    <Link className="inline-block max-w-[14rem] truncate align-middle font-mono text-xs text-primary hover:underline" title={subject} to={`/billing/customers?subject=${encodeURIComponent(subject)}`}>
      {subject}
    </Link>
  );
}

/** A labelled control; `help` sits under it, outside the label, so the label stays the control's whole name. */
export function Field({ label, help, children, className = "" }: { label: string; help?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <div className={`flex min-w-0 flex-col gap-1.5 ${className}`.trim()}>
      <label className="flex flex-col gap-1.5 text-sm font-medium text-foreground">
        {label}
        <span className="font-normal">{children}</span>
      </label>
      {help && <p className="text-xs text-muted-foreground">{help}</p>}
    </div>
  );
}

/** A titled group inside a long form or a sheet. */
export function FormSection({ title, description, children }: { title: string; description?: ReactNode; children: ReactNode }) {
  return (
    <div className="space-y-3 border-t border-border pt-4 first:border-t-0 first:pt-0">
      <div>
        <h3 className="text-sm font-semibold">{title}</h3>
        {description && <p className="mt-0.5 text-xs text-muted-foreground">{description}</p>}
      </div>
      {children}
    </div>
  );
}

/** The quiet success line a form shows after its action (`useAct().note`). */
export function Note({ children, className = "" }: { children?: ReactNode; className?: string }) {
  if (!children) return null;
  return (
    <p role="status" className={`flex items-center gap-1.5 text-sm text-success ${className}`.trim()}>
      <HugeiconsIcon icon={Tick02Icon} size={14} strokeWidth={2} aria-hidden="true" />
      {children}
    </p>
  );
}

/** A payment or store provider: name, what it is, its state, then its facts and form. */
export function ProviderFrame({ label, title, kind, account, children }: { label: string; title: string; kind: string; account?: ProviderAccount; children: ReactNode }) {
  return (
    <section aria-label={label} className="flex min-w-0 flex-col overflow-hidden rounded-xl border border-border bg-card">
      <header className="flex items-start justify-between gap-3 border-b border-border px-4 py-3">
        <div className="min-w-0">
          <h3 className="font-semibold">{title}</h3>
          <p className="text-xs text-muted-foreground">{kind}</p>
        </div>
        <Badge status={account ? (account.enabled ? "configured" : "disabled") : "off"}>{account ? (account.enabled ? "configured" : "disabled") : "not configured"}</Badge>
      </header>
      {children}
    </section>
  );
}

/** What the server knows about a saved provider: the key hint (never the key), its id, the API base and the webhook URL. */
export function ProviderFacts({ account, hook = "Webhook URL" }: { account: ProviderAccount; hook?: string }) {
  const url = `${typeof location === "undefined" ? "" : location.origin}${account.webhook_path}`;
  return (
    <dl className="grid grid-cols-[auto_1fr] gap-x-3 gap-y-1 text-xs">
      <dt className="text-muted-foreground">Credentials</dt>
      <dd className="font-mono">{account.hint ? `Key ${account.hint}` : "Credentials sealed"}{account.key_id ? <span className="text-muted-foreground">, key id {account.key_id}</span> : null}</dd>
      {account.api_base && (
        <>
          <dt className="text-muted-foreground">API base</dt>
          <dd className="truncate font-mono" title={account.api_base}>{account.api_base}</dd>
        </>
      )}
      <dt className="text-muted-foreground">{hook}</dt>
      <dd className="flex min-w-0 items-center gap-1">
        <span className="truncate font-mono" title={url}>{account.webhook_path}</span>
        <CopyIcon text={url} label={`Copy ${hook.toLowerCase()}`} />
      </dd>
    </dl>
  );
}

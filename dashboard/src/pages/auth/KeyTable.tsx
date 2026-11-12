/**
 * The list of API keys and personal tokens, used on the API keys tab and in a service account's Keys panel.
 *
 * Usage:
 *   <KeyTable rows={keys} owners={names} onRevoked={reload} showOwner />
 *
 * The secret is never shown: a key is named by its public prefix (`fbk_<id>_••••`), which is also what the server
 * logs. Scopes are folded per area into plain-word chips; the raw scope strings are on hover.
 */
import { Key01Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Table } from "panelui-web/table";
import { revokeKey, type ApiKeyView } from "../../api";
import { ConfirmDialog } from "../../components/ConfirmDialog";
import { Button } from "../../components/ui";
import { notify } from "../../toaster";
import { summarizeScopes } from "./scopes";
import { Badge } from "../../components/StatusBadge";
import { Chip } from "../../components/Chips";
import { HEAD_ROW, Th } from "../../components/TableCard";
import { When } from "../../components/Time";

const SOON = 14 * 86_400_000;

/** The public part of a key, as the server prints it: `fbk_<id>_` then the secret, which is masked. */
const keyPrefix = (k: Pick<ApiKeyView, "id" | "kind">) => `${k.kind === "pat" ? "fbp" : "fbk"}_${k.id}_`;

function Expiry({ at }: { at: string | null }) {
  if (!at) return <span className="text-muted-foreground">Never</span>;
  const t = new Date(at).getTime();
  // eslint-disable-next-line react-hooks/purity -- a list shows the time it was drawn
  const now = Date.now();
  if (t <= now) return <Badge status="expired" title={at}>Expired</Badge>;
  return <When value={at} className={t - now < SOON ? "font-medium text-warning" : "text-muted-foreground"} />;
}

function Scopes({ scopes }: { scopes: string[] }) {
  const chips = summarizeScopes(scopes);
  if (chips.length === 0) return <span className="text-muted-foreground">No scopes</span>;
  const shown = chips.slice(0, 3);
  const rest = chips.slice(3);
  return (
    <span className="flex flex-wrap items-center gap-1">
      {shown.map((c) => (
        <Chip key={c.label} mono={false} tone={c.wide ? "warn" : undefined} title={c.scopes.join("\n")}>
          {c.label}
        </Chip>
      ))}
      {rest.length > 0 && (
        <span title={rest.map((c) => c.label).join("\n")} className="inline-flex h-5 items-center px-1 text-xs text-muted-foreground">
          +{rest.length} more
        </span>
      )}
    </span>
  );
}

export function KeyTable({ rows, onRevoked, showOwner, owners, label = "Keys" }: {
  rows: ApiKeyView[];
  onRevoked: () => void;
  showOwner?: boolean;
  /** Owner ids to names: service account names, and "You" for the signed-in administrator. */
  owners?: Map<string, string>;
  label?: string;
}) {
  const sorted = [...rows].sort((a, b) => Number(a.revoked) - Number(b.revoked));
  const tenants = rows.some((k) => k.tenant);
  return (
    <Table framed aria-label={label} className="text-sm">
      <Table.Header className={HEAD_ROW}>
        <Table.Row>
          <Th>Name</Th>
          {showOwner && <Th>Owner</Th>}
          <Th>Scopes</Th>
          {tenants && <Th>Tenant</Th>}
          <Th>Expires</Th>
          <Table.Head className="hidden xl:table-cell">Last used</Table.Head>
          <Th><span className="sr-only">Actions</span></Th>
        </Table.Row>
      </Table.Header>
      <Table.Body>
        {sorted.map((k) => (
          <Table.Row key={k.id} className={k.revoked ? "opacity-60" : ""}>
            <Table.Cell className="max-w-64">
              <span className="flex min-w-0 items-center gap-2.5">
                <span aria-hidden="true" className="flex size-8 shrink-0 items-center justify-center rounded-lg border border-border bg-muted/60 text-muted-foreground">
                  <HugeiconsIcon icon={Key01Icon} size={15} />
                </span>
                <span className="flex min-w-0 flex-col">
                  <span className="truncate font-medium">{k.name}</span>
                  <code className="truncate font-mono text-xs text-muted-foreground" title={`${keyPrefix(k)}… (the secret part is never shown again)`}>
                    {keyPrefix(k)}<span aria-label="hidden secret">••••••</span>
                  </code>
                </span>
              </span>
            </Table.Cell>
            {showOwner && (
              <Table.Cell className="max-w-48">
                <span className="flex min-w-0 flex-col">
                  <span className="truncate">{owners?.get(k.owner_id) ?? <span className="font-mono text-xs">{k.owner_id}</span>}</span>
                  <span className="whitespace-nowrap text-xs text-muted-foreground">{k.owner_type === "service_account" ? "Service account" : k.kind === "pat" ? "Personal token" : k.owner_type}</span>
                </span>
              </Table.Cell>
            )}
            <Table.Cell className="max-w-80"><Scopes scopes={k.scopes} /></Table.Cell>
            {tenants && <Table.Cell className="font-mono text-xs">{k.tenant ?? <span className="font-sans text-muted-foreground">Any</span>}</Table.Cell>}
            <Table.Cell>
              <Expiry at={k.expires_at} />
              {/* Narrow screens drop the Last used column: say it here. */}
              <span className="block text-xs text-muted-foreground xl:hidden">{k.last_used_at ? <>Used <When value={k.last_used_at} /></> : "Never used"}</span>
            </Table.Cell>
            <Table.Cell className="hidden text-muted-foreground xl:table-cell"><When value={k.last_used_at} fallback="Never" /></Table.Cell>
            <Table.Cell align="end">
              {k.revoked ? (
                <Badge status="revoked">Revoked</Badge>
              ) : (
                <ConfirmDialog
                  trigger={<Button tone="danger" aria-label={`Revoke ${k.name}`}>Revoke</Button>}
                  title={`Revoke ${k.name}?`}
                  consequence="Anything that uses this key gets 401 from now on. This cannot be undone."
                  confirmLabel="Revoke key"
                  onConfirm={async () => {
                    await revokeKey(k.id);
                    notify.deleted(`Key ${k.name}`);
                    onRevoked();
                  }}
                />
              )}
            </Table.Cell>
          </Table.Row>
        ))}
      </Table.Body>
    </Table>
  );
}

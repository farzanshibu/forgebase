import { Cancel01Icon, Tick02Icon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Table } from "panelui-web/table";
import type { ReactNode } from "react";
import { ACTION_HELP, DATA_ACTIONS, catalogue, allKeys, fromCells, keyOf, NEXT, toCells, type Cell } from "./catalogue";

const STATE_WORDS: Record<Cell, string> = { inherit: "not set", allow: "allowed", deny: "denied" };
const NEXT_WORDS: Record<Cell, string> = { inherit: "allow", allow: "deny", deny: "clear" };

/** Plain names for the catalogue's resources; the key itself shows under the name. */
const RESOURCE_WORDS: Record<string, string> = {
  org: "Organization",
  users: "Members",
  teams: "Teams",
  projects: "Projects",
  roles: "Roles",
  acl: "Resource ACLs",
  billing: "Billing",
  api_keys: "API keys",
  analytics: "Analytics",
  ai: "AI",
  schema: "Schema",
  engine: "Engines",
};

const tone = (state: Cell) =>
  state === "deny" ? "border-destructive/40 bg-destructive/10 text-destructive"
  : state === "allow" ? "border-success/40 bg-success/10 text-success"
  : "border-dashed border-border text-muted-foreground hover:not-disabled:border-foreground/30 hover:not-disabled:text-foreground";

function StateIcon({ state }: { state: Cell }) {
  if (state === "inherit") return <span aria-hidden="true" className="inline-block size-3 text-center leading-3">·</span>;
  return <HugeiconsIcon icon={state === "allow" ? Tick02Icon : Cancel01Icon} size={12} strokeWidth={2.5} aria-hidden="true" />;
}

/**
 * Tri-state permissions over the `resource:action` catalogue: each click cycles not set → allow → deny. Organization
 * resources are rows of action chips; collections are a grid (list, view, create, update, delete) with a pinned header
 * and first column. Emits the role's allow/deny lists on every toggle.
 */
export default function PermissionMatrix({
  allow, deny, collections, readOnly, onChange,
}: {
  allow: string[]; deny: string[]; collections: string[]; readOnly?: boolean;
  onChange: (allow: string[], deny: string[]) => void;
}) {
  const rows = catalogue(collections);
  const { cells, customAllow, customDeny } = toCells(allow, deny, allKeys(rows));
  const platform = rows.filter((r) => !r.resource.startsWith("data:"));
  const data = rows.filter((r) => r.resource.startsWith("data:"));
  const allowed = Object.values(cells).filter((v) => v === "allow").length;
  const denied = Object.values(cells).filter((v) => v === "deny").length;
  const toggle = (key: string) => {
    const next = { ...cells, [key]: NEXT[cells[key] ?? "inherit"] };
    const out = fromCells(next, customAllow, customDeny);
    onChange(out.allow, out.deny);
  };
  const cellButton = (key: string, children: ReactNode, className = "") => {
    const state = cells[key] ?? "inherit";
    const help = ACTION_HELP[key];
    return (
      <button
        type="button"
        disabled={readOnly}
        aria-label={`${key}: ${state}`}
        data-state={state}
        title={`${key}: ${STATE_WORDS[state]}${readOnly ? "" : `. Click to ${NEXT_WORDS[state]}.`}${help ? ` ${help}.` : ""}`}
        onClick={() => toggle(key)}
        className={`inline-flex items-center justify-center gap-1 rounded-md border text-xs font-medium transition-colors focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring disabled:cursor-not-allowed disabled:opacity-80 ${tone(state)} ${className}`}
      >
        {children}
      </button>
    );
  };

  return (
    <div className="space-y-4">
      <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 text-xs text-muted-foreground">
        <p className="flex flex-wrap items-center gap-x-3 gap-y-1">
          <span className="inline-flex items-center gap-1"><span className={`inline-flex size-4 items-center justify-center rounded border ${tone("inherit")}`}><StateIcon state="inherit" /></span>Not set</span>
          <span className="inline-flex items-center gap-1"><span className={`inline-flex size-4 items-center justify-center rounded border ${tone("allow")}`}><StateIcon state="allow" /></span>Allow</span>
          <span className="inline-flex items-center gap-1"><span className={`inline-flex size-4 items-center justify-center rounded border ${tone("deny")}`}><StateIcon state="deny" /></span>Deny (wins over any allow)</span>
          {!readOnly && <span>Click a permission to cycle.</span>}
        </p>
        <p aria-live="polite" className="tabular-nums"><span className="text-success">{allowed} allowed</span> · <span className={denied ? "text-destructive" : ""}>{denied} denied</span></p>
      </div>

      <section aria-label="Organization permissions" className="overflow-hidden rounded-lg border border-border">
        <Table aria-label="Permission matrix" density="compact" framed className="text-sm">
          <Table.Header className="bg-muted/40">
            <Table.Row>
              <Table.Head className="w-48">Resource</Table.Head>
              <Table.Head>Actions</Table.Head>
            </Table.Row>
          </Table.Header>
          <Table.Body>
            {platform.map((r) => (
              <Table.Row key={r.resource}>
                <Table.Head scope="row" className="h-auto py-2 align-top font-normal">
                  <span className="block text-sm font-medium text-foreground">{RESOURCE_WORDS[r.resource] ?? r.resource}</span>
                  <code className="font-mono text-xs text-muted-foreground">{r.resource}</code>
                </Table.Head>
                <Table.Cell className="py-2">
                  <span className="flex flex-wrap gap-1.5">
                    {r.actions.map((a) => cellButton(keyOf(r.resource, a), <><StateIcon state={cells[keyOf(r.resource, a)] ?? "inherit"} /><span className="font-mono">{a}</span></>, "h-6 px-2"))}
                  </span>
                </Table.Cell>
              </Table.Row>
            ))}
          </Table.Body>
        </Table>
      </section>

      <section aria-label="Collection permissions" className="overflow-hidden rounded-lg border border-border">
        {data.length === 0 ? (
          <p className="px-4 py-3 text-xs text-muted-foreground">No collections yet. Each collection you create gets a row here for its records: list, view, create, update and delete.</p>
        ) : (
          <Table aria-label="Collection permission matrix" density="compact" framed maxHeight="22rem" className="text-sm">
            <Table.Header className="in-data-[sticky]:bg-muted">
              <Table.Row>
                <Table.Head className="sticky left-0 z-20 bg-muted">Collection records</Table.Head>
                {DATA_ACTIONS.map((a) => <Table.Head key={a} align="center" className="w-20 font-mono">{a}</Table.Head>)}
              </Table.Row>
            </Table.Header>
            <Table.Body>
              {data.map((r) => (
                <Table.Row key={r.resource}>
                  <Table.Head scope="row" className="sticky left-0 z-10 h-auto bg-card py-1.5 font-normal">
                    <code className="font-mono text-xs text-foreground/90">{r.resource}</code>
                  </Table.Head>
                  {DATA_ACTIONS.map((a) => {
                    const key = keyOf(r.resource, a);
                    const state = cells[key] ?? "inherit";
                    return (
                      <Table.Cell key={a} align="center" className="py-1.5">
                        {cellButton(key, <><StateIcon state={state} /><span>{state === "inherit" ? "" : state === "allow" ? "Allow" : "Deny"}</span></>, "h-6 min-w-16 px-1.5")}
                      </Table.Cell>
                    );
                  })}
                </Table.Row>
              ))}
            </Table.Body>
          </Table>
        )}
      </section>

      <dl aria-label="Permission help" className="space-y-0.5 text-xs text-muted-foreground">
        {Object.entries(ACTION_HELP).map(([k, text]) => (
          <div key={k} className="flex flex-wrap gap-x-2"><dt className="font-mono text-foreground/80">{k}</dt><dd>{text}</dd></div>
        ))}
      </dl>
      {(customAllow.length > 0 || customDeny.length > 0) && (
        <div aria-label="Custom patterns" className="rounded-lg border border-border bg-muted/30 p-3 text-xs text-muted-foreground">
          <p className="mb-1.5">Wildcard and custom patterns the grid cannot show. They are kept as they are when you save.</p>
          <ul className="flex flex-wrap gap-1.5">
            {customAllow.map((p) => <li key={`a${p}`} className="rounded-md border border-success/30 bg-success/10 px-1.5 py-0.5 font-mono text-success">allow {p}</li>)}
            {customDeny.map((p) => <li key={`d${p}`} className="rounded-md border border-destructive/30 bg-destructive/10 px-1.5 py-0.5 font-mono text-destructive">deny {p}</li>)}
          </ul>
        </div>
      )}
    </div>
  );
}

/**
 * Create API key (audit A-08): one side sheet that makes a key an app can use, without knowing the scope grammar.
 *
 * Usage:
 *   const [creating, setCreating] = useCreateParam();                 // /auth/keys?new=1 opens it (Overview, palette)
 *   <CreateKeyDialog open={creating} onOpenChange={setCreating} onCreated={reload} />
 *   <CreateKeyDialog kind="token" … />                                // a personal token for yourself, no service account
 *
 * Steps in the sheet: a name (the app), the service account that owns the key (a new one named after the app by
 * default), what the key may do (checkboxes per area, the scope string under each), an expiry and an optional tenant.
 * The secret is shown once, next to a curl example that uses the placeholder `<YOUR_API_KEY>`.
 */
import { useRef, useState, type ReactNode } from "react";
import { Input } from "panelui-web/input";
import {
  createPat, createServiceAccount, createServiceAccountKey, listBuckets, listCollections, listServiceAccounts, type CreatedKey,
} from "../../api";
import { Check, Sel } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { HelpTip } from "../../components/HelpTip";
import { CopyButton } from "../../components/ui";
import { fieldErrors } from "../../errors";
import { useLoad } from "../../hooks";
import { serverUrl } from "../../snippets";
import { validate } from "../../validate";
import { SecretModal } from "./common";
import { AREAS, EXPIRIES, MCP_SCOPE, buildScopes, describeScope, emptyChoice, readExample, summarizeScopes, type AreaChoice, type ScopeArea } from "./scopes";
import { Chip } from "../../components/Chips";

const NEW_ACCOUNT = "__new__";

/** The date `days` from today, for the expiry help line. */
const dateIn = (days: number) => new Date(Date.now() + days * 86_400_000).toLocaleDateString(undefined, { dateStyle: "medium" });

export interface CreateKeyDialogProps {
  open: boolean;
  onOpenChange: (open: boolean) => void;
  /** `key` belongs to a service account (for an app); `token` is a personal token for the signed-in person. */
  kind?: "key" | "token";
  /** Pre-select this service account (its Keys panel opens the dialog). */
  accountId?: string;
  onCreated?: () => void;
}

export default function CreateKeyDialog({ open, onOpenChange, kind = "key", accountId, onCreated }: CreateKeyDialogProps) {
  const [made, setMade] = useState<{ key: CreatedKey; scopes: string[] }>();
  return (
    <>
      {open && <KeyForm open onOpenChange={onOpenChange} kind={kind} accountId={accountId} onMade={(m) => { setMade(m); onCreated?.(); }} />}
      {made && (
        <SecretModal label={kind === "token" ? "Token" : "API key"} name={made.key.name} secret={made.key.key} onClose={() => setMade(undefined)}>
          <KeyExample scopes={made.scopes} />
        </SecretModal>
      )}
    </>
  );
}

function KeyExample({ scopes }: { scopes: string[] }) {
  const ex = readExample(scopes, serverUrl());
  const curl = ex?.curl ?? `curl -H "X-API-Key: <YOUR_API_KEY>" \\\n  "${serverUrl()}/api/v1/collections"`;
  return (
    <div className="min-w-0 space-y-2 border-t border-border pt-3">
      <div className="flex items-center justify-between gap-2">
        <p className="text-sm font-medium">Try it: {ex ? ex.what.charAt(0).toLowerCase() + ex.what.slice(1) : "send it in the X-API-Key header"}</p>
        <CopyButton text={curl} label="Copy command" />
      </div>
      <pre aria-label="curl example" className="max-w-full overflow-x-auto rounded-lg border border-border bg-muted/50 p-3 font-mono text-xs leading-relaxed">
        {curl}
      </pre>
      <p className="text-xs text-muted-foreground">Replace <code>{"<YOUR_API_KEY>"}</code> with the secret above. Keep it out of your source code.</p>
      <div className="flex flex-wrap gap-1" aria-label="Scopes of the new key">
        {summarizeScopes(scopes).map((c) => (
          <Chip key={c.label} mono={false} tone={c.wide ? "warn" : undefined} title={c.scopes.join("\n")}>{c.label}</Chip>
        ))}
      </div>
    </div>
  );
}

function KeyForm({ open, onOpenChange, kind, accountId, onMade }: CreateKeyDialogProps & { onMade: (m: { key: CreatedKey; scopes: string[] }) => void }) {
  const accounts = useLoad(listServiceAccounts, "createkey:accounts");
  const collections = useLoad(listCollections, "createkey:collections");
  const buckets = useLoad(listBuckets, "createkey:buckets");
  const [name, setName] = useState("");
  const [account, setAccount] = useState(accountId ?? NEW_ACCOUNT);
  const [choices, setChoices] = useState<Record<string, AreaChoice>>({});
  // MCP (ADR-0030): the key may also be used by an AI agent, with the same scopes as above.
  const [agents, setAgents] = useState(false);
  const [expiry, setExpiry] = useState(90);
  const [tenant, setTenant] = useState("");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  // A retry after a failed key request must not create the service account twice.
  const created = useRef<string | undefined>(undefined);

  const choiceOf = (area: string) => choices[area] ?? emptyChoice(area);
  const setChoice = (area: string, patch: Partial<AreaChoice>) => setChoices((c) => ({ ...c, [area]: { ...(c[area] ?? emptyChoice(area)), ...patch } }));
  const areaScopes = buildScopes(AREAS.map((a) => choiceOf(a.id)));
  const scopes = agents ? [...areaScopes, MCP_SCOPE] : areaScopes;
  const names: Record<string, string[]> = {
    collections: (collections.data ?? []).map((c) => c.name),
    storage: (buckets.data ?? []).map((b) => b.name),
  };
  const nameError = (touched ? validate.required(kind === "token" ? "a name for the token" : "the app's name")(name) : undefined) ?? fieldErrors(saveError).name;
  const scopeError = touched && areaScopes.length === 0 ? "Pick at least one thing the key may do." : undefined;
  const canSubmit = name.trim() !== "" && areaScopes.length > 0;
  const accountOptions = [
    { value: NEW_ACCOUNT, label: name.trim() ? `New service account “${name.trim()}”` : "New service account named after the app" },
    ...(accounts.data ?? []).map((a) => ({ value: a.id, label: `${a.name}${a.role === "admin" ? " (administrator)" : ""}` })),
  ];

  return (
    <CreateSheet
      open={open}
      onOpenChange={onOpenChange}
      title={kind === "token" ? "New personal token" : "Create API key"}
      description={kind === "token"
        ? "A token for scripts you run yourself. It acts as you, limited to what you pick."
        : "A key your app sends with each request. It acts as a service account, limited to what you pick."}
      submitLabel={kind === "token" ? "Create token" : "Create key"}
      canSubmit={canSubmit}
      onError={setSaveError}
      onSubmit={async () => {
        setTouched(true);
        const body = { name: name.trim(), scopes, ...(expiry ? { expires_in_days: expiry } : {}), ...(tenant.trim() ? { tenant: tenant.trim() } : {}) };
        let key: CreatedKey;
        if (kind === "token") key = await createPat(body);
        else {
          let id = account;
          if (account === NEW_ACCOUNT) {
            created.current ??= (await createServiceAccount({ name: name.trim(), description: `Made with the API key ${name.trim()}`, role: "service" })).id;
            id = created.current;
          }
          key = await createServiceAccountKey(id, body);
        }
        onMade({ key, scopes });
      }}
    >
      <FormSection title="Details">
        <FormField label={kind === "token" ? "Name" : "App name"} required help={kind === "token" ? "Shown in the list, so you can tell your tokens apart." : "Shown in the list. A new service account gets the same name."} error={nameError}>
          {(p) => <Input {...p} className="w-full" value={name} placeholder={kind === "token" ? "my script" : "my-web-app"} autoComplete="off" onChange={(e) => { setName(e.target.value); setTouched(true); }} />}
        </FormField>

        {kind === "key" && (
          <FormField label="Service account" term="service_account" help="The identity that owns the key. Turn it off or delete it and the key stops working.">
            {(p) => <Sel {...p} size="md" className="w-full" value={account} onChange={setAccount} options={accountOptions} />}
          </FormField>
        )}
      </FormSection>

      <fieldset className="space-y-2" aria-describedby="key-scopes-help">
        <div className="flex flex-wrap items-end justify-between gap-2">
          <legend className="flex items-center gap-1 text-sm font-semibold">
            What can it do? <HelpTip term="scope" />
          </legend>
          <span className="flex items-center gap-1 text-xs" role="group" aria-label="Quick picks">
            <QuickPick onClick={() => setChoices(Object.fromEntries(AREAS.map((a) => [a.id, { ...choiceOf(a.id), read: true, write: false }])))}>Read only</QuickPick>
            <span aria-hidden="true" className="text-muted-foreground">·</span>
            <QuickPick onClick={() => setChoices(Object.fromEntries(AREAS.map((a) => [a.id, { ...choiceOf(a.id), read: true, write: true }])))}>Read and write</QuickPick>
            <span aria-hidden="true" className="text-muted-foreground">·</span>
            <QuickPick onClick={() => setChoices({})}>Clear</QuickPick>
          </span>
        </div>
        <p id="key-scopes-help" className="text-xs text-muted-foreground">
          Tick only what the app needs. Each choice is a <em>scope</em>, shown in code under it. Write does not include read: tick both when the app needs both.
        </p>
        <div className="divide-y divide-border overflow-hidden rounded-lg border border-border">
          {AREAS.map((a) => (
            <AreaPicker key={a.id} area={a} choice={choiceOf(a.id)} known={names[a.id]} onChange={(patch) => setChoice(a.id, patch)} />
          ))}
        </div>
        {scopeError && <p role="alert" className="text-xs font-medium text-destructive">{scopeError}</p>}
        <div className="rounded-lg border border-border px-3 py-2.5">
          <Check
            checked={agents}
            onChange={setAgents}
            label="Let AI agents use this key (MCP)"
            description={<>An agent such as Claude Code or Cursor connects to <code>/api/mcp</code> with this key and can do only what is ticked above. The MCP server must be on in Features and settings. <code>{MCP_SCOPE}</code></>}
            aria-label="Let AI agents use this key through the MCP server"
          />
        </div>
      </fieldset>

      <FormSection title="Limits">
        <FormField label="Expires" help={expiry ? `The key stops working on ${dateIn(expiry)}. A key that expires limits the harm if it leaks.` : "The key works until you revoke it. Prefer an expiry for keys that leave your servers."}>
          {(p) => <Sel {...p} size="md" className="w-full" value={expiry} onChange={setExpiry} options={EXPIRIES} />}
        </FormField>

        <FormField label="Tenant (optional)" help="Bind the key to one tenant, a separate customer or workspace of yours: everything it does acts in that tenant. Leave empty for an unbound key." error={fieldErrors(saveError).tenant}>
          {(p) => <Input {...p} className="w-full font-mono" value={tenant} placeholder="acme" autoComplete="off" spellCheck={false} onChange={(e) => setTenant(e.target.value)} />}
        </FormField>
      </FormSection>

      <div className="rounded-lg border border-border bg-muted/40 p-3" aria-live="polite">
        <p className="text-xs font-medium text-muted-foreground">
          {scopes.length === 0 ? "No scopes yet: the key would not be able to do anything." : `${scopes.length} ${scopes.length === 1 ? "scope" : "scopes"}`}
        </p>
        {scopes.length > 0 && (
          <ul className="mt-2 flex flex-wrap gap-1" aria-label="Scopes">
            {scopes.map((s) => (
              <li key={s}><code title={describeScope(s)} className="inline-flex h-5 items-center rounded-md border border-border bg-card px-1.5 font-mono text-[0.6875rem]">{s}</code></li>
            ))}
          </ul>
        )}
      </div>
    </CreateSheet>
  );
}

function FormSection({ title, children }: { title: string; children: ReactNode }) {
  return (
    <section className="space-y-4" aria-label={title}>
      <h3 className="text-sm font-semibold">{title}</h3>
      {children}
    </section>
  );
}

function QuickPick({ onClick, children }: { onClick: () => void; children: ReactNode }) {
  return (
    <button type="button" onClick={onClick} className="rounded px-1 font-medium text-primary underline-offset-2 hover:underline focus-visible:outline-2 focus-visible:outline-offset-1 focus-visible:outline-ring">
      {children}
    </button>
  );
}

/** One area: Read and Write, each with the scope under it, and which resources it applies to. */
function AreaPicker({ area, choice, known, onChange }: { area: ScopeArea; choice: AreaChoice; known?: string[]; onChange: (p: Partial<AreaChoice>) => void }) {
  const on = choice.read || choice.write;
  const list = known ?? [];
  const scopeText = (access: "read" | "write") => buildScopes([{ ...choice, [access]: true, [access === "read" ? "write" : "read"]: false }]).join(" ");
  const toggle = (name: string, v: boolean) => onChange({ only: v ? [...choice.only, name] : choice.only.filter((x) => x !== name) });
  return (
    <div className={`px-3 py-2.5 transition-colors ${on ? "bg-primary/5" : ""}`} role="group" aria-label={area.label}>
      <p className="flex items-center gap-2 text-sm font-medium">
        {area.label}
        {on && <span className="rounded-full bg-primary/10 px-1.5 text-[0.6875rem] font-medium text-primary">{[choice.read && "read", choice.write && "write"].filter(Boolean).join(" + ")}</span>}
      </p>
      <div className="mt-1.5 grid gap-x-4 gap-y-1.5 sm:grid-cols-2">
        <Check checked={choice.read} onChange={(v) => onChange({ read: v })} label={`${area.read}`} description={<code>{scopeText("read")}</code>} aria-label={`${area.label}: ${area.read}`} />
        <Check checked={choice.write} onChange={(v) => onChange({ write: v })} label={`${area.write}`} description={<code>{scopeText("write")}</code>} aria-label={`${area.label}: ${area.write}`} />
      </div>
      {on && list.length > 0 && (
        <div className="mt-2.5 space-y-1.5 border-t border-border pt-2.5">
          <Check checked={!choice.narrow} onChange={(v) => onChange({ narrow: !v, only: [] })} label={`All ${area.resource}s`} aria-label={`${area.label}: all ${area.resource}s`} />
          {choice.narrow && (
            <ul className="ms-6 grid max-h-40 gap-1 overflow-y-auto sm:grid-cols-2" aria-label={`Only these ${area.resource}s`}>
              {list.map((n) => (
                <li key={n}>
                  <Check checked={choice.only.includes(n)} onChange={(v) => toggle(n, v)} label={<span className="font-mono text-xs">{n}</span>} aria-label={`${area.label}: only ${n}`} />
                </li>
              ))}
            </ul>
          )}
          {choice.narrow && choice.only.length === 0 && <p role="alert" className="ms-6 text-xs font-medium text-destructive">Pick at least one {area.resource}, or tick All {area.resource}s.</p>}
        </div>
      )}
      {on && list.length === 0 && (
        <div className="mt-2.5 border-t border-border pt-2.5">
          <label className="block text-xs text-muted-foreground">
            Only this {area.resource} (optional)
            <Input
              size="sm" className="mt-1 w-full font-mono" aria-label={`${area.label}: only this ${area.resource}`} placeholder={`all ${area.resource}s`} value={choice.only[0] ?? ""}
              onChange={(e) => onChange({ narrow: Boolean(e.target.value.trim()), only: e.target.value.trim() ? [e.target.value.trim()] : [] })}
            />
          </label>
        </div>
      )}
    </div>
  );
}

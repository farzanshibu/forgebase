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
import { useRef, useState } from "react";
import { Input } from "panelui-web/input";
import {
  createPat, createServiceAccount, createServiceAccountKey, listBuckets, listCollections, listServiceAccounts, type CreatedKey,
} from "../../api";
import { Check, Sel } from "../../components/controls";
import { CreateSheet } from "../../components/CreateSheet";
import { FormField } from "../../components/FormField";
import { fieldErrors } from "../../errors";
import { useLoad } from "../../hooks";
import { serverUrl } from "../../snippets";
import { validate } from "../../validate";
import { SecretModal } from "./common";
import { AREAS, EXPIRIES, buildScopes, emptyChoice, readExample, type AreaChoice, type ScopeArea } from "./scopes";

const NEW_ACCOUNT = "__new__";

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
        <SecretModal label={kind === "token" ? "Token" : "API key"} secret={made.key.key} onClose={() => setMade(undefined)}>
          <KeyExample scopes={made.scopes} />
        </SecretModal>
      )}
    </>
  );
}

function KeyExample({ scopes }: { scopes: string[] }) {
  const ex = readExample(scopes, serverUrl());
  return (
    <div className="mt-3 space-y-1">
      <p className="text-sm font-medium">{ex ? ex.what : "Send it in the X-API-Key header"}</p>
      <pre aria-label="curl example" className="overflow-x-auto rounded bg-background p-2 font-mono text-xs">
        {ex?.curl ?? `curl -H "X-API-Key: <YOUR_API_KEY>" \\\n  "${serverUrl()}/api/v1/collections"`}
      </pre>
      <p className="text-xs text-muted-foreground">Replace <code>{"<YOUR_API_KEY>"}</code> with the secret above. Keep it out of your source code.</p>
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
  const [expiry, setExpiry] = useState(90);
  const [tenant, setTenant] = useState("");
  const [touched, setTouched] = useState(false);
  const [saveError, setSaveError] = useState<unknown>();
  // A retry after a failed key request must not create the service account twice.
  const created = useRef<string | undefined>(undefined);

  const choiceOf = (area: string) => choices[area] ?? emptyChoice(area);
  const setChoice = (area: string, patch: Partial<AreaChoice>) => setChoices((c) => ({ ...c, [area]: { ...(c[area] ?? emptyChoice(area)), ...patch } }));
  const scopes = buildScopes(AREAS.map((a) => choiceOf(a.id)));
  const names: Record<string, string[]> = {
    collections: (collections.data ?? []).map((c) => c.name),
    storage: (buckets.data ?? []).map((b) => b.name),
  };
  const nameError = (touched ? validate.required(kind === "token" ? "a name for the token" : "the app's name")(name) : undefined) ?? fieldErrors(saveError).name;
  const scopeError = touched && scopes.length === 0 ? "Pick at least one thing the key may do." : undefined;
  const canSubmit = name.trim() !== "" && scopes.length > 0;
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
      <FormField label={kind === "token" ? "Name" : "App name"} required help={kind === "token" ? "Shown in the list, so you can tell your tokens apart." : "Shown in the list. A new service account gets the same name."} error={nameError}>
        {(p) => <Input {...p} className="w-full" value={name} placeholder={kind === "token" ? "my script" : "my-web-app"} onChange={(e) => { setName(e.target.value); setTouched(true); }} />}
      </FormField>

      {kind === "key" && (
        <FormField label="Service account" term="service_account" help="The identity that owns the key. Revoke or delete it and the key stops working.">
          {(p) => <Sel {...p} size="md" className="w-full" value={account} onChange={setAccount} options={accountOptions} />}
        </FormField>
      )}

      <fieldset className="space-y-3" aria-describedby="key-scopes-help">
        <legend className="flex items-center gap-1 text-sm font-medium">What can it do?</legend>
        <p id="key-scopes-help" className="text-xs text-muted-foreground">
          Each choice is a <em>scope</em>, shown under it. Write does not include read: tick both when the app needs both.
        </p>
        {AREAS.map((a) => (
          <AreaPicker key={a.id} area={a} choice={choiceOf(a.id)} known={names[a.id]} onChange={(patch) => setChoice(a.id, patch)} />
        ))}
        {scopeError && <p role="alert" className="text-xs font-medium text-destructive">{scopeError}</p>}
      </fieldset>

      <FormField label="Expires" help="A key that expires limits the harm if it leaks. You can always make a new one.">
        {(p) => <Sel {...p} size="md" className="w-full" value={expiry} onChange={setExpiry} options={EXPIRIES} />}
      </FormField>

      <FormField label="Tenant (optional)" help="Bind the key to one tenant, a separate customer or workspace of yours: everything it does acts in that tenant. Leave empty for an unbound key." error={fieldErrors(saveError).tenant}>
        {(p) => <Input {...p} className="w-full font-mono" value={tenant} placeholder="acme" autoComplete="off" spellCheck={false} onChange={(e) => setTenant(e.target.value)} />}
      </FormField>

      {scopes.length > 0 && (
        <p className="text-xs text-muted-foreground" aria-live="polite">
          Scopes: <code>{scopes.join(" ")}</code>
        </p>
      )}
    </CreateSheet>
  );
}

/** One area: Read and Write, each with the scope under it, and which resources it applies to. */
function AreaPicker({ area, choice, known, onChange }: { area: ScopeArea; choice: AreaChoice; known?: string[]; onChange: (p: Partial<AreaChoice>) => void }) {
  const on = choice.read || choice.write;
  const list = known ?? [];
  const scopeText = (access: "read" | "write") => buildScopes([{ ...choice, [access]: true, [access === "read" ? "write" : "read"]: false }]).join(" ");
  const toggle = (name: string, v: boolean) => onChange({ only: v ? [...choice.only, name] : choice.only.filter((x) => x !== name) });
  return (
    <div className="rounded-lg border border-border p-3" role="group" aria-label={area.label}>
      <p className="text-sm font-medium">{area.label}</p>
      <div className="mt-2 grid gap-2">
        <Check checked={choice.read} onChange={(v) => onChange({ read: v })} label={`${area.read}`} description={<code>{scopeText("read")}</code>} aria-label={`${area.label}: ${area.read}`} />
        <Check checked={choice.write} onChange={(v) => onChange({ write: v })} label={`${area.write}`} description={<code>{scopeText("write")}</code>} aria-label={`${area.label}: ${area.write}`} />
      </div>
      {on && list.length > 0 && (
        <div className="mt-3 space-y-1.5 border-t border-border pt-3">
          <Check checked={!choice.narrow} onChange={(v) => onChange({ narrow: !v, only: [] })} label={`All ${area.resource}s`} aria-label={`${area.label}: all ${area.resource}s`} />
          {choice.narrow && (
            <ul className="ms-6 grid max-h-40 gap-1 overflow-y-auto" aria-label={`Only these ${area.resource}s`}>
              {list.map((n) => (
                <li key={n}>
                  <Check checked={choice.only.includes(n)} onChange={(v) => toggle(n, v)} label={n} aria-label={`${area.label}: only ${n}`} />
                </li>
              ))}
            </ul>
          )}
          {choice.narrow && choice.only.length === 0 && <p role="alert" className="ms-6 text-xs font-medium text-destructive">Pick at least one {area.resource}, or tick All {area.resource}s.</p>}
        </div>
      )}
      {on && list.length === 0 && (
        <div className="mt-3 border-t border-border pt-3">
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

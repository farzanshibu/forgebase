/**
 * RunAsPicker (FB-155): "Run as" for the SQL editor on PostgreSQL. Administrator (the default) runs as the editor's
 * own role; a user or service account runs the statement as a direct login bound to it would, so row and field rules
 * apply. A tenant narrows it further. The tenant box remembers tenants used before.
 */
import { useId } from "react";
import { Input } from "panelui-web/input";
import { listCollections, listServiceAccounts, searchUsers } from "../../api";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import type { RunAs } from "./sqlRequest";

export const ADMIN = "";
const TENANTS_KEY = "forgebase.sql.tenants";

export interface Principal { key: string; label: string }

/** Users of every auth collection (first 50 each) and the service accounts. */
export async function loadPrincipals(signal?: AbortSignal): Promise<Principal[]> {
  const out: Principal[] = [];
  const collections = (await listCollections(signal)).filter((c) => c.type === "auth");
  for (const c of collections) {
    try {
      const page = await searchUsers(c.name, { per_page: 50 }, signal);
      for (const r of page.items) out.push({ key: `user:${c.name}/${String(r.id)}`, label: `${String(r.email ?? r.username ?? r.id)} (user in ${c.name})` });
    } catch {
      // A collection the caller cannot list is skipped: the rest of the picker still works.
    }
  }
  try {
    for (const s of await listServiceAccounts(signal)) out.push({ key: `service:${s.id}`, label: `${s.name} (service account)` });
  } catch {
    // No permission to list service accounts: users still work.
  }
  return out;
}

export function rememberedTenants(): string[] {
  try {
    const v: unknown = JSON.parse(localStorage.getItem(TENANTS_KEY) ?? "[]");
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === "string") : [];
  } catch {
    return [];
  }
}
export function rememberTenant(t: string) {
  if (!t) return;
  try {
    localStorage.setItem(TENANTS_KEY, JSON.stringify([t, ...rememberedTenants().filter((x) => x !== t)].slice(0, 10)));
  } catch {
    // Storage may be off (private window): the tenant is just not remembered.
  }
}

export default function RunAsPicker({ principal, tenant, onChange }: { principal: string; tenant: string; onChange: (p: string, t: string) => void }) {
  const list = useLoad(loadPrincipals, "run-as-principals");
  const listId = useId();
  const options = [{ value: ADMIN, label: "Administrator" }, ...(list.data ?? []).map((p) => ({ value: p.key, label: p.label }))];
  return (
    <div className="flex flex-wrap items-center gap-2 text-sm">
      <span className="text-muted-foreground" id={`${listId}-l`}>Run as</span>
      <Sel aria-label="Run as" className="w-64" value={principal} onChange={(v) => onChange(v, v === ADMIN ? "" : tenant)} options={options} />
      {principal !== ADMIN && (
        <>
          <Input size="sm" aria-label="Tenant" className="w-40" list={`${listId}-t`} placeholder="Tenant (optional)" value={tenant} onChange={(e) => onChange(principal, e.target.value)} />
          <datalist id={`${listId}-t`}>{rememberedTenants().map((t) => <option key={t} value={t} />)}</datalist>
        </>
      )}
    </div>
  );
}

export const toRunAs = (principal: string, tenant: string): RunAs | undefined => (principal === ADMIN ? undefined : { principal, ...(tenant.trim() ? { tenant: tenant.trim() } : {}) });

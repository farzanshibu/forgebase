/**
 * RunAsPicker (FB-155): "Run as" for the SQL editor on PostgreSQL. Administrator (the default) runs as the editor's
 * own role; a user or service account runs the statement as a direct login bound to it would, so row and field rules
 * apply. A tenant narrows it further. The tenant box remembers tenants used before.
 */
import { useId } from "react";
import { Input } from "panelui-web/input";
import { Sel } from "../../components/controls";
import { useLoad } from "../../hooks";
import { ADMIN, loadPrincipals, rememberedTenants } from "./pgHelpers";

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


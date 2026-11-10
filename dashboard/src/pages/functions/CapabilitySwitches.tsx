import { ADMIN_ONLY_CAPABILITIES, FUNCTION_CAPABILITIES } from "../../api";
import { Switch } from "panelui-web/switch";

/** One switch per capability. `service_role` is marked: only an administrator can grant it, and it bypasses row-level rules. */
export default function CapabilitySwitches({ value, onChange }: { value: string[]; onChange: (caps: string[]) => void }) {
  return (
    <fieldset>
      <legend className="text-sm">Capabilities</legend>
      <div className="mt-2 grid gap-2 sm:grid-cols-2 lg:grid-cols-3">
        {FUNCTION_CAPABILITIES.map((c) => {
          const admin = ADMIN_ONLY_CAPABILITIES.includes(c);
          return (
            <div key={c} className="flex items-center gap-2 text-sm">
              <Switch label={c} checked={value.includes(c)} onCheckedChange={(on) => onChange(on ? [...value, c] : value.filter((x) => x !== c))} />
              <span className="font-mono text-xs" aria-hidden>{c}</span>
              {admin && <span className="rounded bg-warning/15 px-1.5 py-0.5 text-xs text-warning" title="Only an administrator can grant this; the function then bypasses row-level rules">admin only</span>}
            </div>
          );
        })}
      </div>
    </fieldset>
  );
}

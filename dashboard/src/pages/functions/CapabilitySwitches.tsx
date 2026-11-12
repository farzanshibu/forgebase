import { ADMIN_ONLY_CAPABILITIES, FUNCTION_CAPABILITIES } from "../../api";
import { Switch } from "panelui-web/switch";

/** What each capability lets the function do, in plain words. */
const WHAT: Record<string, string> = {
  "db.read": "Read records",
  "db.write": "Create, change and delete records",
  kv: "Use the key-value store",
  log: "Write log lines",
  env: "Read environment values",
  secrets: "Read this function's secrets",
  fetch: "Call outside HTTP hosts",
  "vector.read": "Search vector collections",
  "vector.write": "Write vector records",
  "storage.read": "Read files",
  "storage.write": "Upload and delete files",
  "queues.publish": "Publish queue messages",
  "jobs.enqueue": "Start background jobs",
  "realtime.publish": "Send realtime events",
  service_role: "Skip row-level rules",
};

/** One switch per capability. `service_role` is marked: only an administrator can grant it, and it bypasses row-level rules. */
export default function CapabilitySwitches({ value, onChange }: { value: string[]; onChange: (caps: string[]) => void }) {
  return (
    <fieldset className="rounded-xl border border-border bg-card px-4 pb-4 pt-3">
      <legend className="px-1 text-sm font-medium">Capabilities</legend>
      <p className="text-xs text-muted-foreground">What the function may use. Grant only what it needs; {value.length} of {FUNCTION_CAPABILITIES.length} on.</p>
      <div className="mt-3 grid gap-x-4 gap-y-2.5 sm:grid-cols-2 lg:grid-cols-3">
        {FUNCTION_CAPABILITIES.map((c) => {
          const admin = ADMIN_ONLY_CAPABILITIES.includes(c);
          return (
            <div key={c} className="flex items-start gap-2.5 text-sm">
              <Switch label={c} checked={value.includes(c)} onCheckedChange={(on) => onChange(on ? [...value, c] : value.filter((x) => x !== c))} />
              <span className="min-w-0 leading-tight">
                <span className="flex flex-wrap items-center gap-1.5">
                  <span className="font-mono text-xs" aria-hidden>{c}</span>
                  {admin && <span className="rounded bg-warning/15 px-1.5 py-0.5 text-[0.6875rem] font-medium text-warning" title="Only an administrator can grant this; the function then bypasses row-level rules">admin only</span>}
                </span>
                <span className="block text-xs text-muted-foreground">{WHAT[c] ?? ""}</span>
              </span>
            </div>
          );
        })}
      </div>
    </fieldset>
  );
}

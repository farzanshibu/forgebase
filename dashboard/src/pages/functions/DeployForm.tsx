import { useState } from "react";
import { deployFunction, wasmToBase64 } from "../../api";
import type { Deployed, FunctionInfo, InvokeAccess, VersionConfig } from "../../api";
import { Button, ErrorView } from "../../components/ui";
import { FormField } from "../../components/FormField";
import { ACCESS_LABEL, csv, formatEnv, formatHooks, parseEnv, parseHooks, readFileBytes } from "./util";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { Check, Sel } from "../../components/controls";
import CapabilitySwitches from "./CapabilitySwitches";

interface Props {
  /** Fixed function name (deploying a new version); otherwise the name is typed. */
  fixedName?: string;
  /** Prefill from the active version. */
  info?: FunctionInfo;
  config?: VersionConfig;
  onDone: (d: Deployed) => void;
}

/** Upload a WebAssembly module and deploy it as a new version (or as a new function). */
export default function DeployForm({ fixedName, info, config, onDone }: Props) {
  const title = fixedName ? "Deploy new version" : "Deploy function";
  const [name, setName] = useState(fixedName ?? "");
  const [file, setFile] = useState<File>();
  const [description, setDescription] = useState(info?.description ?? "");
  const [access, setAccess] = useState<InvokeAccess>(info?.access ?? "permission");
  const [caps, setCaps] = useState<string[]>(config?.capabilities ?? []);
  const [hosts, setHosts] = useState((config?.fetch_hosts ?? []).join(", "));
  const [fuel, setFuel] = useState(config ? String(config.limits.fuel) : "");
  const [memory, setMemory] = useState(config ? String(config.limits.memory_bytes) : "");
  const [timeout, setTimeoutMs] = useState(config ? String(config.limits.timeout_ms) : "");
  const [conc, setConc] = useState(config ? String(config.limits.max_concurrency) : "");
  const [env, setEnv] = useState(formatEnv(config?.env ?? {}));
  const [hooks, setHooks] = useState(formatHooks(config?.hooks ?? []));
  const [activate, setActivate] = useState(true);
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setError(undefined);
    if (!file) return setError(new Error("Choose a .wasm file to upload"));
    setBusy(true);
    try {
      const limits: Record<string, number> = {};
      for (const [k, v] of [["fuel", fuel], ["memory_bytes", memory], ["timeout_ms", timeout], ["max_concurrency", conc]] as const) {
        if (v.trim()) {
          const n = Number(v);
          if (!Number.isInteger(n) || n <= 0) throw new Error(`Limit ${k} must be a positive whole number`);
          limits[k] = n;
        }
      }
      const out = await deployFunction({
        name,
        wasm: wasmToBase64(await readFileBytes(file)),
        description,
        access,
        capabilities: caps,
        fetch_hosts: csv(hosts),
        limits,
        env: parseEnv(env),
        hooks: parseHooks(hooks),
        activate,
      });
      onDone(out);
    } catch (err) {
      setError(err);
    } finally {
      setBusy(false);
    }
  }

  const num = (label: string, v: string, set: (s: string) => void, unit: string) => (
    <FormField label={label} help={unit}>
      {(p) => <Input {...p} size="sm" inputMode="numeric" className="w-full font-mono" value={v} onChange={(e) => set(e.target.value)} placeholder="default" />}
    </FormField>
  );
  const section = "grid gap-3 border-t border-border px-4 py-4";
  const heading = "text-xs font-medium uppercase tracking-wide text-muted-foreground md:col-span-4";

  return (
    <section aria-labelledby="fn-deploy-title" className="overflow-hidden rounded-xl border border-border bg-card">
      <header className="flex flex-wrap items-center justify-between gap-2 px-4 py-3">
        <div>
          <h2 id="fn-deploy-title" className="text-sm font-medium">{title}</h2>
          <p className="text-xs text-muted-foreground">Upload a compiled WebAssembly module. To write JavaScript or TypeScript instead, use New function or the Editor tab.</p>
        </div>
      </header>
      <form aria-label={title} onSubmit={(e) => void submit(e)}>
        <div className={`${section} md:grid-cols-4`}>
          <h3 className={heading}>Module</h3>
          <FormField label="Name" className="md:col-span-2" help={fixedName ? "A new version of this function." : "Lower-case letters, digits, _ or -, starting with a letter."}>
            {(p) => <Input {...p} size="sm" className="w-full font-mono" value={name} onChange={(e) => setName(e.target.value)} readOnly={!!fixedName} required pattern="[a-z][a-z0-9_-]{0,47}" title="Lower-case letters, digits, _ or -, starting with a letter" />}
          </FormField>
          <FormField label="WebAssembly module" className="md:col-span-2" help={file ? `${file.name}, ${file.size.toLocaleString("en-US")} bytes` : "A .wasm file."}>
            {(p) => <Input {...p} size="sm" type="file" accept=".wasm,application/wasm" className="w-full" onChange={(e) => setFile(e.target.files?.[0])} />}
          </FormField>
          <FormField label="Description" className="md:col-span-2">
            {(p) => <Input {...p} size="sm" className="w-full" value={description} onChange={(e) => setDescription(e.target.value)} />}
          </FormField>
          <FormField label="HTTP access" help="Who may call it over HTTP.">
            {(p) => <Sel {...p} aria-label="HTTP access" className="w-full" value={access} onChange={(v) => setAccess(v as InvokeAccess)} options={(Object.keys(ACCESS_LABEL) as InvokeAccess[]).map((a) => ({ value: a, label: ACCESS_LABEL[a] }))} />}
          </FormField>
          <div className="flex items-end pb-2">
            <Check checked={activate} onChange={setActivate} label="Activate this version" />
          </div>
        </div>
        <div className="border-t border-border px-4 py-4">
          <CapabilitySwitches value={caps} onChange={setCaps} />
        </div>
        <div className={`${section} md:grid-cols-4`}>
          <h3 className={heading}>Network and limits</h3>
          <FormField label="Fetch hosts" className="md:col-span-4" help="Comma separated. Empty allows any public host (with the fetch capability).">
            {(p) => <Input {...p} size="sm" className="w-full font-mono" value={hosts} onChange={(e) => setHosts(e.target.value)} placeholder="api.example.com, *.example.org" />}
          </FormField>
          {num("Fuel", fuel, setFuel, "Units of work per run.")}
          {num("Memory bytes", memory, setMemory, "Bytes of memory.")}
          {num("Timeout ms", timeout, setTimeoutMs, "Milliseconds per run.")}
          {num("Max concurrency", conc, setConc, "Runs at the same time.")}
        </div>
        <div className={`${section} md:grid-cols-2`}>
          <h3 className={`${heading} md:col-span-2`}>Environment and hooks</h3>
          <FormField label="Environment" help="KEY=value per line. Not secret: use the Secrets tab for keys and passwords.">
            {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-24 w-full font-mono text-xs" value={env} onChange={(e) => setEnv(e.target.value)} placeholder="MODE=production" />}
          </FormField>
          <FormField label="Record hooks" help="collection: events per line, such as after_create, after_update.">
            {(p) => <Textarea {...p} size="sm" autoResize={false} className="h-24 w-full font-mono text-xs" value={hooks} onChange={(e) => setHooks(e.target.value)} placeholder="orders: after_create, after_update" />}
          </FormField>
        </div>
        <div className="flex flex-wrap items-center gap-3 border-t border-border bg-muted/40 px-4 py-3">
          <Button tone="primary" type="submit" disabled={busy}>{busy ? "Deploying…" : "Deploy"}</Button>
          <div className="min-w-0 flex-1"><ErrorView error={error} /></div>
        </div>
      </form>
    </section>
  );
}

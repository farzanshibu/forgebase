import { useState } from "react";
import { FUNCTION_CAPABILITIES, deployFunction, wasmToBase64 } from "../../api";
import type { Deployed, FunctionInfo, InvokeAccess, VersionConfig } from "../../api";
import { Button, ErrorView, Panel } from "../../components/ui";
import { csv, formatEnv, formatHooks, parseEnv, parseHooks, readFileBytes } from "./util";
import { Input } from "panelui-web/input";
import { Textarea } from "panelui-web/textarea";
import { Sel } from "../../components/controls";

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

  const num = (label: string, v: string, set: (s: string) => void) => (
    <label className="text-sm">{label}<Input size="sm" inputMode="numeric" className="mt-1 w-full" value={v} onChange={(e) => set(e.target.value)} placeholder="default" /></label>
  );

  return (
    <Panel title={title}>
      <form aria-label={title} onSubmit={(e) => void submit(e)} className="grid gap-3 md:grid-cols-4">
        <label className="text-sm md:col-span-2">Name
          <Input size="sm" className="mt-1 w-full" value={name} onChange={(e) => setName(e.target.value)} readOnly={!!fixedName} required pattern="[a-z][a-z0-9_-]{0,47}" title="Lower-case letters, digits, _ or -, starting with a letter" />
        </label>
        <label className="text-sm md:col-span-2">WebAssembly module
          <Input size="sm" type="file" accept=".wasm,application/wasm" className="mt-1 w-full" onChange={(e) => setFile(e.target.files?.[0])} />
        </label>
        <label className="text-sm md:col-span-2">Description<Input size="sm" className="mt-1 w-full" value={description} onChange={(e) => setDescription(e.target.value)} /></label>
        <label className="text-sm">HTTP access
          <Sel aria-label="HTTP access" className="mt-1" value={access} onChange={(v) => setAccess(v as InvokeAccess)} options={[{ value: "permission", label: "permission" }, { value: "authenticated", label: "authenticated" }, { value: "public", label: "public" }]} />
        </label>
        <label className="flex items-end gap-2 pb-2 text-sm"><input type="checkbox" checked={activate} onChange={(e) => setActivate(e.target.checked)} />Activate this version</label>
        <fieldset className="md:col-span-4">
          <legend className="text-sm">Capabilities</legend>
          <div className="mt-1 flex flex-wrap gap-4 text-sm">
            {FUNCTION_CAPABILITIES.map((c) => (
              <label key={c} className="flex items-center gap-1.5">
                <input type="checkbox" checked={caps.includes(c)} onChange={(e) => setCaps(e.target.checked ? [...caps, c] : caps.filter((x) => x !== c))} />{c}
              </label>
            ))}
          </div>
        </fieldset>
        <label className="text-sm md:col-span-4">Fetch hosts (comma separated; empty allows any public host)
          <Input size="sm" className="mt-1 w-full" value={hosts} onChange={(e) => setHosts(e.target.value)} placeholder="api.example.com, *.example.org" />
        </label>
        {num("Fuel", fuel, setFuel)}
        {num("Memory bytes", memory, setMemory)}
        {num("Timeout ms", timeout, setTimeoutMs)}
        {num("Max concurrency", conc, setConc)}
        <label className="text-sm md:col-span-2">Environment (KEY=value per line; not secret)
          <Textarea size="sm" autoResize={false} className="mt-1 h-24 w-full font-mono text-xs" value={env} onChange={(e) => setEnv(e.target.value)} />
        </label>
        <label className="text-sm md:col-span-2">Record hooks (collection: events per line)
          <Textarea size="sm" autoResize={false} className="mt-1 h-24 w-full font-mono text-xs" value={hooks} onChange={(e) => setHooks(e.target.value)} placeholder="orders: after_create, after_update" />
        </label>
        <div className="md:col-span-4"><Button tone="primary" type="submit" disabled={busy}>Deploy</Button></div>
      </form>
      <div className="mt-3"><ErrorView error={error} /></div>
    </Panel>
  );
}

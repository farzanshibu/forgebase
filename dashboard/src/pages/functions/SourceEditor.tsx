import { lazy, Suspense, useEffect, useState } from "react";
import { deployFunction, getFunctionTypes } from "../../api";
import type { Deployed, FunctionLanguage, InvokeAccess, VersionConfig } from "../../api";
import { Button, ErrorView, Loading } from "../../components/ui";
import CapabilitySwitches from "./CapabilitySwitches";
import { FALLBACK_FORGEBASE_DTS } from "./fallbackTypes";
import { compileIssues } from "./util";
import type { CompileIssue } from "./util";

// Monaco is only fetched when an editor is shown, so the first page load is unchanged.
const CodeEditor = lazy(() => import("./CodeEditor"));

const draftKey = (key: string) => `forgebase.fn-draft.${key}`;

interface Draft { source: string; savedAt: string }

function readDraft(key: string): Draft | undefined {
  try {
    const raw = localStorage.getItem(draftKey(key));
    const d = raw ? (JSON.parse(raw) as Partial<Draft>) : undefined;
    return d && typeof d.source === "string" ? { source: d.source, savedAt: String(d.savedAt ?? "") } : undefined;
  } catch {
    return undefined;
  }
}
function writeDraft(key: string, source: string): boolean {
  try {
    localStorage.setItem(draftKey(key), JSON.stringify({ source, savedAt: new Date().toISOString() }));
    return true;
  } catch {
    return false;
  }
}
function clearDraft(key: string) {
  try {
    localStorage.removeItem(draftKey(key));
  } catch {
    /* storage unavailable: nothing to clear */
  }
}

export interface SourceEditorProps {
  name: string;
  language: FunctionLanguage;
  /** The code the editor starts from (the active version, or a template). */
  initialSource: string;
  /** Where the draft lives in browser storage. */
  storageKey: string;
  capabilities: string[];
  /** Carried into the deploy so a new version keeps the function's configuration. */
  config?: Partial<VersionConfig>;
  /** Only for a brand-new function. */
  description?: string;
  access?: InvokeAccess;
  onDeployed: (d: Deployed) => void;
}

/** Editor, capability switches and Deploy for one JS/TS function. */
export default function SourceEditor({ name, language, initialSource, storageKey, capabilities, config, description, access, onDeployed }: SourceEditorProps) {
  const [draft] = useState(() => readDraft(storageKey));
  const restored = draft !== undefined && draft.source !== initialSource;
  const [source, setSource] = useState(restored ? draft.source : initialSource);
  const [restoredAt, setRestoredAt] = useState(restored ? draft.savedAt : "");
  const [caps, setCaps] = useState(capabilities);
  const [note, setNote] = useState("");
  const [types, setTypes] = useState<{ dts: string; bundled: boolean }>();
  const [issues, setIssues] = useState<CompileIssue[]>([]);
  const [error, setError] = useState<unknown>();
  const [busy, setBusy] = useState(false);

  useEffect(() => {
    const ctrl = new AbortController();
    getFunctionTypes(ctrl.signal).then(
      (dts) => !ctrl.signal.aborted && setTypes({ dts, bundled: false }),
      () => !ctrl.signal.aborted && setTypes({ dts: FALLBACK_FORGEBASE_DTS, bundled: true }),
    );
    return () => ctrl.abort();
  }, []);

  function saveDraft() {
    setNote(writeDraft(storageKey, source) ? "Draft saved in this browser" : "Could not save the draft: browser storage is unavailable");
  }

  async function deploy() {
    setError(undefined);
    setIssues([]);
    setNote("");
    setBusy(true);
    try {
      const out = await deployFunction({
        name,
        source,
        language,
        capabilities: caps,
        ...(description !== undefined ? { description } : {}),
        ...(access !== undefined ? { access } : {}),
        ...(config?.fetch_hosts ? { fetch_hosts: config.fetch_hosts } : {}),
        ...(config?.limits ? { limits: config.limits } : {}),
        ...(config?.env ? { env: config.env } : {}),
        ...(config?.hooks ? { hooks: config.hooks } : {}),
        activate: true,
      });
      clearDraft(storageKey);
      onDeployed(out);
    } catch (e) {
      const found = compileIssues(e);
      if (found.length) setIssues(found);
      else setError(e);
    } finally {
      setBusy(false);
    }
  }

  return (
    <div
      className="space-y-3"
      onKeyDown={(e) => {
        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
          e.preventDefault();
          saveDraft();
        }
      }}
    >
      <div className="flex flex-wrap items-center justify-between gap-2 text-sm text-muted-foreground">
        <span>{language === "typescript" ? "TypeScript" : "JavaScript"} · <kbd className="rounded border border-border px-1">Ctrl/Cmd+S</kbd> saves a draft in this browser</span>
        {types && <span>Types: {types.bundled ? "bundled copy (the server's could not be loaded)" : "from the server"}</span>}
      </div>
      {restoredAt && (
        <p role="status" className="flex items-center gap-3 text-sm">
          Restored your unsaved draft{restoredAt ? ` from ${restoredAt.replace("T", " ").replace(/\.\d+Z$/, "Z")}` : ""}.
          <Button onClick={() => { setSource(initialSource); clearDraft(storageKey); setRestoredAt(""); }}>Discard draft</Button>
        </p>
      )}
      <Suspense fallback={<Loading what="Loading editor" />}>
        <CodeEditor label={`${name || "function"} source`} language={language} value={source} onChange={setSource} types={types?.dts} issues={issues} />
      </Suspense>
      {issues.length > 0 && (
        <ul aria-label="Compile errors" role="alert" className="space-y-1 rounded-md border border-destructive/50 p-3 text-sm">
          {issues.map((i, n) => (
            <li key={n}><span className="font-mono text-destructive">Line {i.line}, column {i.column}</span>: {i.message}</li>
          ))}
        </ul>
      )}
      <CapabilitySwitches value={caps} onChange={setCaps} />
      <div className="flex items-center gap-3">
        <Button tone="primary" onClick={() => void deploy()} disabled={busy || !name}>Deploy</Button>
        <Button onClick={saveDraft}>Save draft</Button>
        {note && <span role="status" className="text-sm text-muted-foreground">{note}</span>}
      </div>
      <ErrorView error={error} />
    </div>
  );
}

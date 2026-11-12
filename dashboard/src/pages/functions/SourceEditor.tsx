import { lazy, Suspense, useEffect, useState, type ReactNode } from "react";
import { FileCodeIcon } from "@hugeicons/core-free-icons";
import { HugeiconsIcon } from "@hugeicons/react";
import { Badge } from "panelui-web/badge";
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
  /** Accessible name of the editor; default "<name> source". Pass a fixed one when the name is still being typed. */
  label?: string;
  /** Shown in the status bar when nothing else is: which version is being edited and when it was deployed. */
  context?: ReactNode;
  onDeployed: (d: Deployed) => void;
}

/** IDE-lite editor for one JS/TS function: a toolbar (file, Save draft, Deploy), Monaco, a status bar, a Problems list and the capability switches. */
export default function SourceEditor({ name, language, initialSource, storageKey, capabilities, config, description, access, label, context, onDeployed }: SourceEditorProps) {
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

  const dirty = source !== initialSource;
  const fileName = language === "typescript" ? "index.ts" : "index.js";
  const typesWords = types ? (types.bundled ? "Types: bundled copy" : "Types: from the server") : "";
  return (
    <div
      className="space-y-4"
      onKeyDown={(e) => {
        if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === "s") {
          e.preventDefault();
          saveDraft();
        }
      }}
    >
      {restoredAt && (
        <p role="status" className="flex flex-wrap items-center justify-between gap-3 rounded-lg border border-warning/40 bg-warning/5 px-3 py-2 text-sm">
          <span>Restored your unsaved draft{restoredAt ? ` from ${restoredAt.replace("T", " ").replace(/\.\d+Z$/, "Z")}` : ""}.</span>
          <Button onClick={() => { setSource(initialSource); clearDraft(storageKey); setRestoredAt(""); }}>Discard draft</Button>
        </p>
      )}
      <section aria-label="Code editor" className="overflow-hidden rounded-xl border border-border bg-card">
        <div className="flex flex-wrap items-center justify-between gap-2 border-b border-border bg-muted/40 px-3 py-2">
          <div className="flex min-w-0 items-center gap-2 text-sm">
            <HugeiconsIcon icon={FileCodeIcon} size={16} strokeWidth={1.8} aria-hidden="true" className="shrink-0 text-muted-foreground" />
            <span className="truncate font-mono">{fileName}</span>
            <Badge variant="outline" size="sm">{language === "typescript" ? "TypeScript" : "JavaScript"}</Badge>
            {dirty && <span className="flex items-center gap-1 text-xs text-muted-foreground"><span aria-hidden="true" className="size-1.5 rounded-full bg-warning" />Not deployed</span>}
          </div>
          <div className="flex items-center gap-2">
            <Button onClick={saveDraft} title="Ctrl/Cmd+S">Save draft</Button>
            <Button tone="primary" onClick={() => void deploy()} disabled={busy || !name} title={!name ? "Give the function a valid name first" : "Deploy as the active version"}>{busy ? "Deploying…" : "Deploy"}</Button>
          </div>
        </div>
        <Suspense fallback={<div className="flex h-[28rem] items-center justify-center"><Loading what="Loading editor" /></div>}>
          <CodeEditor frameless label={label ?? `${name || "function"} source`} language={language} value={source} onChange={setSource} types={types?.dts} issues={issues} />
        </Suspense>
        <div className="flex flex-wrap items-center justify-between gap-x-4 gap-y-1 border-t border-border bg-muted/40 px-3 py-1.5 text-xs text-muted-foreground">
          <span className="flex min-w-0 items-center gap-2">
            {busy ? <span role="status">Deploying…</span>
              : issues.length > 0 ? <span className="font-medium text-destructive">Deploy failed: {issues.length} compile {issues.length === 1 ? "error" : "errors"}</span>
                : error ? <span className="font-medium text-destructive">Deploy failed</span>
                  : !name ? <span>Give the function a valid name to deploy it.</span>
                  : note ? <span role="status">{note}</span>
                    : context ?? null}
          </span>
          <span className="flex flex-wrap items-center gap-3">
            <span><kbd className="rounded border border-border bg-background px-1 font-mono">Ctrl/Cmd+S</kbd> saves a draft in this browser</span>
            {types && <span title={types.bundled ? "The server's types could not be loaded, so IntelliSense uses the copy bundled with the dashboard." : undefined}>{typesWords}</span>}
          </span>
        </div>
      </section>
      {issues.length > 0 && (
        <div className="overflow-hidden rounded-xl border border-destructive/40">
          <p className="border-b border-destructive/30 bg-destructive/5 px-3 py-1.5 text-xs font-medium text-destructive">Problems ({issues.length})</p>
          <ul aria-label="Compile errors" role="alert" className="divide-y divide-border text-sm">
            {issues.map((i, n) => (
              <li key={n} className="flex gap-3 px-3 py-2"><span className="shrink-0 font-mono text-xs leading-5 text-destructive">Line {i.line}, column {i.column}</span><span className="min-w-0 break-words">{i.message}</span></li>
            ))}
          </ul>
        </div>
      )}
      <ErrorView error={error} />
      <CapabilitySwitches value={caps} onChange={setCaps} />
    </div>
  );
}

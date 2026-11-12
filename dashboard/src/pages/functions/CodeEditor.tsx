import { useEffect, useRef } from "react";
import type { FunctionLanguage } from "../../api";
import { useTheme } from "../../theme/context";
import { monaco, setExtraLib } from "./monacoEnv";
import type { CompileIssue } from "./util";

interface Props {
  label: string;
  language: FunctionLanguage;
  value: string;
  onChange?: (v: string) => void;
  readOnly?: boolean;
  /** When set, shows a diff of `original` (left) against `value` (right). */
  original?: string;
  /** `forgebase` module declarations for IntelliSense. */
  types?: string;
  /** Compile errors to mark on their lines. */
  issues?: CompileIssue[];
  height?: string;
  /** Draw no border of its own (the editor sits inside a frame with a toolbar and a status bar). */
  frameless?: boolean;
}

const OWNER = "forgebase-deploy";

/** Monaco editor (or diff editor). Lazy-loaded by its users; everything Monaco is imported from here. */
export default function CodeEditor({ label, language, value, onChange, readOnly, original, types, issues, height = "28rem", frameless = false }: Props) {
  const { dark } = useTheme();
  const host = useRef<HTMLDivElement>(null);
  const model = useRef<monaco.editor.ITextModel>(null);
  const change = useRef(onChange);
  useEffect(() => {
    change.current = onChange;
  });
  const diff = original !== undefined;
  const first = useRef({ value, original, language, readOnly });

  useEffect(() => {
    const el = host.current;
    if (!el) return;
    const init = first.current;
    const modified = monaco.editor.createModel(init.value, init.language);
    model.current = modified;
    const common = { automaticLayout: true, minimap: { enabled: false }, scrollBeyondLastLine: false, fontSize: 13, readOnly: init.readOnly, ariaLabel: label } as const;
    let dispose: () => void;
    if (init.original !== undefined) {
      const orig = monaco.editor.createModel(init.original, init.language);
      const ed = monaco.editor.createDiffEditor(el, { ...common, renderSideBySide: true, originalEditable: false });
      ed.setModel({ original: orig, modified });
      dispose = () => { ed.dispose(); orig.dispose(); };
    } else {
      const ed = monaco.editor.create(el, { ...common, model: modified });
      dispose = () => ed.dispose();
    }
    const sub = modified.onDidChangeContent(() => change.current?.(modified.getValue()));
    return () => {
      sub.dispose();
      dispose();
      modified.dispose();
      model.current = null;
    };
    // The editor is created once per mount; props that change later are applied by the effects below.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [diff]);

  // External changes of the text (restoring a draft, switching version) reach the model without echoing back.
  useEffect(() => {
    const m = model.current;
    if (m && m.getValue() !== value) m.setValue(value);
  }, [value]);
  useEffect(() => {
    if (types) setExtraLib(types);
  }, [types]);
  useEffect(() => {
    monaco.editor.setTheme(dark ? "vs-dark" : "vs");
  }, [dark]);
  useEffect(() => {
    const m = model.current;
    if (!m) return;
    monaco.editor.setModelMarkers(m, OWNER, (issues ?? []).map((i) => ({
      severity: monaco.MarkerSeverity.Error,
      message: i.message,
      startLineNumber: i.line,
      startColumn: i.column,
      endLineNumber: i.line,
      endColumn: Math.max(i.column + 1, m.getLineMaxColumn?.(i.line) ?? i.column + 1),
    })));
  }, [issues]);

  return <div ref={host} role="region" aria-label={label} className={frameless ? "overflow-hidden" : "overflow-hidden rounded-lg border border-border"} style={{ height }} />;
}

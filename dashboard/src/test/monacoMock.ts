/**
 * A stand-in for `pages/functions/monacoEnv` in jsdom (Monaco itself needs a real browser).
 * The editor is a textarea backed by a model; the diff editor prints both sides. Use with
 * `vi.mock("./monacoEnv", async () => (await import("../../test/monacoMock")).monacoEnvMock())`.
 */
type Listener = () => void;

export interface FakeMarker { message: string; startLineNumber: number; startColumn: number; endLineNumber: number; endColumn: number; severity: number }

export const monacoState = {
  theme: "" as string,
  extraLib: undefined as string | undefined,
  markers: [] as FakeMarker[],
  workersConfigured: false,
  reset() {
    this.theme = "";
    this.extraLib = undefined;
    this.markers = [];
  },
};

class FakeModel {
  private listeners = new Set<Listener>();
  constructor(private value: string, readonly language: string) {}
  getValue() { return this.value; }
  setValue(v: string) { this.value = v; this.listeners.forEach((l) => l()); }
  onDidChangeContent(cb: Listener) { this.listeners.add(cb); return { dispose: () => this.listeners.delete(cb) }; }
  getLineMaxColumn(line: number) { return (this.value.split("\n")[line - 1]?.length ?? 0) + 1; }
  dispose() { this.listeners.clear(); }
}

export function monacoEnvMock() {
  const monaco = {
    MarkerSeverity: { Error: 8 },
    editor: {
      createModel: (value: string, language: string) => new FakeModel(value, language),
      create(el: HTMLElement, opts: { model: FakeModel; readOnly?: boolean; ariaLabel?: string }) {
        const ta = document.createElement("textarea");
        ta.setAttribute("aria-label", opts.ariaLabel ?? "editor");
        ta.readOnly = !!opts.readOnly;
        ta.value = opts.model.getValue();
        ta.addEventListener("input", () => { if (ta.value !== opts.model.getValue()) opts.model.setValue(ta.value); });
        const sub = opts.model.onDidChangeContent(() => { if (ta.value !== opts.model.getValue()) ta.value = opts.model.getValue(); });
        el.appendChild(ta);
        return { dispose: () => { sub.dispose(); ta.remove(); } };
      },
      createDiffEditor(el: HTMLElement) {
        const box = document.createElement("div");
        box.setAttribute("data-testid", "diff");
        el.appendChild(box);
        return {
          setModel({ original, modified }: { original: FakeModel; modified: FakeModel }) {
            box.innerHTML = "";
            for (const [side, m] of [["original", original], ["modified", modified]] as const) {
              const pre = document.createElement("pre");
              pre.setAttribute("data-side", side);
              pre.textContent = m.getValue();
              box.appendChild(pre);
            }
          },
          dispose: () => box.remove(),
        };
      },
      setTheme: (t: string) => { monacoState.theme = t; },
      setModelMarkers: (_m: FakeModel, _owner: string, markers: FakeMarker[]) => { monacoState.markers = markers; },
    },
  };
  return { monaco, setExtraLib: (dts: string) => { monacoState.extraLib = dts; } };
}

/** Type into the fake editor the way Monaco reports a change: set the text and fire `input`. */
export function setEditorCode(el: HTMLElement, code: string) {
  const ta = el as HTMLTextAreaElement;
  ta.value = code;
  ta.dispatchEvent(new Event("input", { bubbles: true }));
}

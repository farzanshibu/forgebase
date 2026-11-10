// Monaco with its workers, bundled with the app (Vite `?worker` imports): nothing loads from a CDN.
// Only CodeEditor imports this file, and CodeEditor is itself lazy, so Monaco lives in its own chunk.
import * as monaco from "./monacoSlim";
import EditorWorker from "monaco-esm/editor/editor.worker.js?worker";
import TsWorker from "monaco-esm/language/typescript/ts.worker.js?worker";

(self as unknown as { MonacoEnvironment: monaco.Environment }).MonacoEnvironment = {
  getWorker: (_id, label) => (label === "typescript" || label === "javascript" ? new TsWorker() : new EditorWorker()),
};

const compilerOptions: monaco.typescript.CompilerOptions = {
  target: monaco.typescript.ScriptTarget.ESNext,
  module: monaco.typescript.ModuleKind.ESNext,
  moduleResolution: monaco.typescript.ModuleResolutionKind.NodeJs,
  lib: ["es2023", "dom"],
  strict: true,
  allowNonTsExtensions: true,
  noEmit: true,
};
for (const defaults of [monaco.typescript.typescriptDefaults, monaco.typescript.javascriptDefaults]) {
  defaults.setCompilerOptions(compilerOptions);
  defaults.setEagerModelSync(true);
}

let libs: monaco.IDisposable[] = [];
/** Make `import ... from "forgebase"` resolve to these declarations in both languages. */
function setExtraLib(dts: string) {
  for (const l of libs) l.dispose();
  const path = "file:///node_modules/forgebase/index.d.ts";
  libs = [monaco.typescript.typescriptDefaults.addExtraLib(dts, path), monaco.typescript.javascriptDefaults.addExtraLib(dts, path)];
}

export { monaco, setExtraLib };

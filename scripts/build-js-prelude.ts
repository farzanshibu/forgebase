// Generates crates/forgebase-functions/js/prelude.js, the Web globals and
// `forgebase` module glue that the embedded engine evaluates (ADR-0027), from
// prelude.ts with Node's own type stripper. Types become whitespace, so every
// line and column of prelude.js is the same as in prelude.ts.
//
//   node scripts/build-js-prelude.ts           regenerate prelude.js
//   node scripts/build-js-prelude.ts --check   fail if the committed prelude.js
//                                              differs from what it would write
//
// Needs Node 22.18 or later (it runs .ts files and has stripTypeScriptTypes).
// Type-check prelude.ts with
//   dashboard/node_modules/.bin/tsc -p crates/forgebase-functions/js/tsconfig.json
// and rebuild the engine (scripts/build-js-engine.sh) after prelude.js changes.

import { readFileSync, writeFileSync } from "node:fs";
import { stripTypeScriptTypes } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const root = join(dirname(fileURLToPath(import.meta.url)), "..");
const source = join(root, "crates/forgebase-functions/js/prelude.ts");
const target = join(root, "crates/forgebase-functions/js/prelude.js");

const args = process.argv.slice(2);
const check = args.length === 1 && args[0] === "--check";
if (args.length > (check ? 1 : 0)) {
  console.error("usage: node scripts/build-js-prelude.ts [--check]");
  process.exit(2);
}

// stripTypeScriptTypes is still marked experimental; that warning is noise here.
process.removeAllListeners("warning");
process.on("warning", (w: Error) => {
  if (w.name !== "ExperimentalWarning") console.warn(`${w.name}: ${w.message}`);
});

const js: string = stripTypeScriptTypes(readFileSync(source, "utf8"), { mode: "strip" });

if (check) {
  let committed: string | null = null;
  try {
    committed = readFileSync(target, "utf8");
  } catch {}
  if (committed !== js) {
    console.error("crates/forgebase-functions/js/prelude.js is out of date with prelude.ts (or was edited by hand): run node scripts/build-js-prelude.ts");
    process.exit(1);
  }
  console.log("prelude.js is up to date");
} else {
  writeFileSync(target, js);
  console.log("wrote crates/forgebase-functions/js/prelude.js");
}

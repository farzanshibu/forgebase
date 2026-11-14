# Developer workflow

The inner loop of building an app on ForgeBase, with the `forgebase` binary you
already have: a local emulator that starts in a second and is thrown away on
exit, typed models of your collections, and tools to see what a function did.
Design: [ADR-0038](adr/0038-developer-workflow-cli.md). It builds on
[project as code](project-as-code.md): the emulator applies your project
directory (`forgebase/`) the way `forgebase deploy` does.

```sh
forgebase new my-app --template basic && cd my-app
forgebase dev --seed --watch               # server + project + fixtures, re-applied on save
forgebase gen types --lang ts --out src/forgebase.types.ts
forgebase functions invoke hello --path '?name=you' --trace --server http://127.0.0.1:8090 --token fbk_...
```

## 1. `forgebase dev`

```text
forgebase dev [--dir forgebase] [--port 8090|0] [--database sqlite|postgres]
              [--persist DIR] [--seed] [--watch] [--json]
forgebase dev seed  [--dir ..] [--server URL --token KEY]
forgebase dev reset [--dir ..] [--persist DIR] [--server URL --token KEY]
```

It starts the real server (the same code as `forgebase serve`) in the CLI
process, on a data directory of its own and with its own configuration, so a
`forgebase.toml` or `forgebase-data/` in the current directory is never read or
changed. Then it:

1. validates the project directory (`file:line` for every problem) and runs the
   functions' `build` commands;
2. creates an administrator and an API key for you;
3. applies the project: no prompt, deletions and **destructive changes
   allowed** (it says so; the emulator's data is disposable);
4. with `--seed`, loads `seed/<collection>.jsonl`;
5. prints the URL, the login and the key, and serves until Ctrl-C (or
   SIGTERM), then shuts down cleanly and deletes its data.

```text
ForgeBase dev emulator ready
  url:        http://127.0.0.1:8090
  dashboard:  http://127.0.0.1:8090/admin/
  admin:      dev@forgebase.local / qG3...
  api key:    fbk_...
  database:   sqlite (ephemeral: deleted on exit)
  project:    forgebase (watching for changes)
Press Ctrl-C to stop.
```

| Flag | Meaning |
|---|---|
| `--dir DIR` | The project directory (default `forgebase`). Without one, an empty emulator starts (with a warning); `--seed` and `--watch` need a project. |
| `--port N` | Global flag: default 8090, `0` picks a free port (use it in tests). `--host` works too. |
| `--database sqlite\|postgres` | Engine of a **new** data directory; else `FORGEBASE_DATABASE_BACKEND`, else the build's default. A persisted directory keeps its engine. PostgreSQL needs the bundled engine (the standard release) or `FORGEBASE_PG_ENGINE_DIR`. |
| `--persist DIR` | Keep the data in `DIR` instead of an ephemeral directory. The generated login and key are kept there (`dev-credentials.json`, mode 0600), so a restart is the same administrator and key. |
| `--seed` | Load the fixtures (below) after applying the project. |
| `--watch` | Apply again when project files change (below). |
| `--json` | One JSON line on standard output when ready (below); progress and logs go to standard error. |

`--data-dir` and `--config` are ignored by `dev` (use `--persist`). `dev` does
**not** read `FORGEBASE_URL` or `FORGEBASE_TOKEN`: your app's `.env` usually
points them at a real server.

### 1.1 Fixtures: `seed/<collection>.jsonl`

One JSON object per line; blank lines and `#` lines are skipped. `forgebase
project validate` checks every line; `apply` never loads them.

```text
# seed/todos.jsonl
{"id": "t1", "title": "Read the docs", "done": true}
{"title": "Ship it", "done": false}
```

Loading is **idempotent by record id**. A line with `id` is stored under it; a
line without one gets `seed-<hash>` derived from the collection and the line,
so loading twice never makes copies. An existing record that already holds the
values is left alone; one that differs is patched. Each collection reports its
counts, and every failure has its file and line:

```text
seed todos: 1 created, 1 updated, 1 unchanged, 1 FAILED
  error: forgebase/seed/todos.jsonl:4: todos record seed-1f3a...: validation_failed: invalid fields (title: required) (HTTP 400)
```

Files load in collection-name order and failed records are retried after the
rest, so a message that points at a room seeded later still loads. Fixtures
that relate to each other should use fixed ids.

`forgebase dev seed` loads the fixtures into the emulator running for this
project directory (found through a registry file in the temp directory, or
pass `--server URL --token KEY`). It exits 1 if a record failed.

`forgebase dev reset` deletes every record of the project's collections in the
running emulator and loads the fixtures again. It only runs against a server on
this machine. With no emulator running, `forgebase dev reset --persist DIR`
empties a persisted directory (after checking it is a dev directory), so the
next `forgebase dev --persist DIR --seed` starts from scratch.

### 1.2 `--watch`

The project directory is scanned every 250 ms; a change is applied after the
files have been quiet for 400 ms, so saving several files (or a build writing
its output) is one re-apply. Every change prints the plan and the outcome:

```text
change detected: collections/notes.json
Plan for my-app against http://127.0.0.1:8090

+ collection notes  [create]
...
project: 1 changed, 3 already in place, 0 failed, 0 skipped
```

- A file that does not validate, or an apply that fails, is reported and the
  emulator **keeps running**; fix the file and save again.
- Deleting a file deletes the resource, and a destructive edit (dropping a
  field, changing its type) runs, losing the data it drops. The plan says
  `dev: destructive changes are allowed here`.
- With `--seed`, fixtures are loaded again after each successful apply.
- `.git`, `node_modules`, `.schemas`, `target` and `dist` are not watched.

### 1.3 The ready line (`--json`)

```json
{"event":"ready","url":"http://127.0.0.1:53211","port":53211,
 "admin":{"email":"dev@forgebase.local","password":"..."},"api_key":"fbk_...",
 "database":"sqlite","data_dir":"/tmp/fbdev-aB3/data","persist":false,
 "project":"forgebase","applied":3,
 "seed":{"collections":{"todos":{"created":3,"updated":0,"unchanged":0,"failed":0}},"errors":[]},
 "pid":4242}
```

It is printed once the project is applied and seeded, so a test can start
using the server when it reads it. With `--watch`, each re-apply adds
`{"event":"reapplied","changed":[...],"applied":1,"failed":[],"seed_errors":[]}`.
A failed start exits 1 with the reason on standard error and no ready line.

The dev API key belongs to an `admin` service account: it passes the access
rules, like the administrator. To test your rules as an app user, sign a user
up (`/api/auth/...`) and use its token.

## 2. `forgebase gen types`

```text
forgebase gen types --lang ts|rust [--from-project DIR | --url SERVER [--token KEY]]
                    [--out FILE] [--check]
```

Typed models of every collection. The source is a project directory
(`--from-project`, no server needed; default `./forgebase`) or a running server
(`--url`, with `--token` or `FORGEBASE_TOKEN`). Both produce the same types for
the same schema, and the built-in `users` collection is included either way.
Without `--out` the types go to standard output.

The output is **deterministic**: collections sorted by name, fields in the
order you declared them, no timestamps or paths. `--check` writes nothing and
exits **2** when the `--out` file is missing or differs from what would be
generated, so CI can require it to be committed:

```yaml
- run: forgebase gen types --lang ts --out src/forgebase.types.ts --check
```

### 2.1 TypeScript (with `@forgebase/sdk`)

```ts
import { ForgeBase } from "@forgebase/sdk";
import type { Todos, TodosCreate, TodosUpdate } from "./forgebase.types";

const fb = new ForgeBase("http://localhost:8090", { apiKey });
const todos = fb.collection<Todos>("todos");     // RecordsApi<Todos>

const t = await todos.create({ title: "Ship it" } satisfies TodosCreate);
t.done;                                          // boolean | null
await todos.update(t.id, { done: true } satisfies TodosUpdate, { version: t._version });
```

Per collection the file has: `Todos` (a record as the API returns it),
`TodosCreate`, `TodosUpdate`, a union alias for each `select` field
(`TodosStatus = "open" | "done"`), and the maps `Collections`,
`CollectionInputs`, `CollectionName` and `collectionNames`. They are `type`
aliases rather than interfaces on purpose: only a type alias satisfies the SDK's
`Row = Record<string, unknown>` bound. The same file also has `JsonValue` and
`GeoPoint`.

| Field | Record type | In `…Create` |
|---|---|---|
| system | `id: string`, `created`, `updated: string` (RFC 3339), `_version: number` | `id?` (pick your own) |
| `text` / `email` / `url` / `date` | `string` | |
| `number` | `number` | |
| `bool` | `boolean` | |
| `select` | union of the values; an array when `max_select > 1` | |
| `json` | `JsonValue` | |
| `array` | `string[]`, `number[]`, `boolean[]` or `JsonValue[]` | |
| `relation` / `file` | `string` (record id / file name), `string[]` when `max_select > 1` | |
| `geo` | `GeoPoint` (`{lat, lon}`) | |
| `vector` | `number[]` | |
| `password` | not in the record: never returned | `string` |
| not `required` | `T \| null` | optional, `T \| null` |
| `required` with a default | `T` | optional |
| `hidden` | optional (administrators see it) | |
| flexible schema | `& Record<string, unknown>` | same |
| `view` | `Record<string, unknown>` | none |
| `auth` | + `email`, `email_visibility`, `verified` | + `email`, `password` |

### 2.2 Rust (with `forgebase-sdk`)

```rust
mod types;                                     // generated: src/types.rs
use types::{Todos, TodosCreate, TodosUpdate};

let todos = fb.collection(Todos::COLLECTION).typed::<Todos>();
let t = todos.create(&TodosCreate::new("Ship it".into())).await?;
let t = todos.update(&t.id, &TodosUpdate { done: Some(true), ..Default::default() }).await?;
```

`serde` structs (`Serialize + Deserialize`), an enum per `select` field,
`TodosUpdate` with every field an `Option` (`Default`) that is skipped when
`None`, and `COLLECTION_NAMES`. Fields named differently from Rust conventions
(`createdBy`, `type`) are renamed with `#[serde(rename)]` or a raw identifier.
Add `serde` (with `derive`) and `serde_json` to the crate. `TodosCreate::new`
takes the required fields and leaves the rest `None`; set the others on the
struct afterwards.

Dart, Kotlin and Swift models are not generated yet.

## 3. Debugging functions

```text
forgebase functions invoke <name> [--data JSON|@file] [--method M] [--path /sub?query] [--trace] [--json]
forgebase functions logs   <name> [-n N] [--follow] [--interval SECONDS] [--json]
```

`invoke` takes the server and credential like `deploy` (`--server`, `--token`,
or `FB_URL` / `FB_TOKEN`; the emulator's URL and key are in its banner):

```text
$ forgebase functions invoke hello --path '?name=cave' --trace --server $URL --token $KEY
GET /api/v1/functions/hello/invoke?name=cave
status:    200
duration:  11.4 ms (round trip)
request:   fbcli-S9yDmUsOQ9Rz
trace id:  4bf92f3577b34da6a3ce929d0e0e4736
response:  application/json
  {
    "caller": null,
    "message": "Hello, cave!"
  }
execution: #12 v3 http ok in 6 ms, 3142388 fuel
logs:
  [info] greeting cave
trace:
  fuel 3142388/100000000, peak memory 1280 KiB of 67108 KiB
  host calls (2):
    +    0.12 ms  db.get             1.00 ms  ok
    +    1.30 ms  kv                 0.21 ms  ok
```

- **What it shows**: HTTP status, round-trip time, the request id it sent
  (`X-Request-Id`, which finds the execution), the trace id, the response, and
  the execution of that request: version, trigger, status, server-side duration,
  fuel, the error if it failed, and the log lines. The exit code is 1 when the
  status is not 2xx (a function that does not exist, a failure, a timeout).
- **Trace id.** The function's `trace_id` ([ADR-0035](adr/0035-tracing-alerting-slos.md)),
  else the response's `traceparent`. The emulator always has trace context on;
  on a server it needs `[observability.otlp] enabled`.
- **`--trace`** adds the host-call timeline (database, KV, fetch, storage...
  with offsets and durations), cold start, fuel and memory limits, and
  per-operation totals, from the insights API (`/api/v1/insights`, per-invocation
  function profiles). On a server without that API, with a credential that may
  not read it, or with profiling off, a single line says why and the rest is
  unchanged.
- **`--data`** is a JSON document or `@file` (at most 1 MiB; a body that is not
  JSON is sent as text); it makes the method `POST` unless `--method` says
  otherwise. `--path` is the sub-path and query after `/invoke`. `--json` prints
  one object (`status`, `body`, `execution`, `trace_id`, `profile`, ...).
- **`logs --follow`** prints the newest executions, then every new one as it
  arrives (with its log lines and trace id) until Ctrl-C. If the server stops
  answering (a restart, an emulator reload) it says so once and keeps trying.

## 4. Test suites

`forgebase dev --port 0 --json` is a fixture for SDK and app tests: start it in
the suite's setup, read the ready line, point the client at `url` with
`api_key`, stop it with SIGINT in the teardown (it deletes its data). Each run
gets a fresh server on a free port, so suites in parallel do not collide.
`forgebase` must be on the `PATH` (or use its full path); `--database sqlite`
keeps start-up fastest on every build.

### 4.1 JavaScript (Vitest `globalSetup`)

```ts
// test/forgebase.setup.ts
import { spawn, type ChildProcess } from "node:child_process";
import type { GlobalSetupContext } from "vitest/node";

let child: ChildProcess | undefined;

export async function setup({ provide }: GlobalSetupContext) {
  child = spawn(
    "forgebase",
    ["dev", "--port", "0", "--json", "--seed", "--database", "sqlite", "--dir", "forgebase"],
    { stdio: ["ignore", "pipe", "inherit"] },       // logs go to stderr: keep them visible
  );
  const ready = await new Promise<{ url: string; api_key: string }>((resolve, reject) => {
    child!.once("exit", (code) => reject(new Error(`forgebase dev exited with ${code} before it was ready`)));
    let buf = "";
    child!.stdout!.on("data", (chunk: Buffer) => {
      buf += chunk.toString();
      for (let i = buf.indexOf("\n"); i >= 0; i = buf.indexOf("\n")) {
        const line = buf.slice(0, i);
        buf = buf.slice(i + 1);
        try {
          const v = JSON.parse(line);
          if (v.event === "ready") resolve(v);
        } catch {
          /* not a JSON line */
        }
      }
    });
  });
  provide("forgebase", { url: ready.url, apiKey: ready.api_key });
}

export async function teardown() {
  if (!child || child.exitCode !== null) return;
  const exited = new Promise((resolve) => child!.once("exit", resolve));
  child.kill("SIGINT");                              // a clean stop deletes the emulator's data
  await exited;
}

declare module "vitest" {
  export interface ProvidedContext {
    forgebase: { url: string; apiKey: string };
  }
}
```

```ts
// vitest.config.ts
import { defineConfig } from "vitest/config";
export default defineConfig({ test: { globalSetup: ["./test/forgebase.setup.ts"] } });

// test/todos.test.ts
import { inject, expect, test } from "vitest";
import { ForgeBase } from "@forgebase/sdk";
import type { Todos } from "../src/forgebase.types";

test("lists the seeded todos", async () => {
  const { url, apiKey } = inject("forgebase");
  const fb = new ForgeBase(url, { apiKey });
  const page = await fb.collection<Todos>("todos").list();
  expect(page.items.length).toBeGreaterThan(0);
});
```

Between tests that change data, call `forgebase dev reset` (it finds the
emulator by the project directory) or delete what the test created.

### 4.2 Rust (a test helper)

```rust
// tests/common/mod.rs
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

pub struct Dev {
    child: Child,
    pub url: String,
    pub api_key: String,
}

impl Dev {
    /// Starts `forgebase dev` on a free port; returns when the project is applied and seeded.
    pub fn start(project_dir: &str) -> Dev {
        let mut child = Command::new("forgebase")
            .args(["dev", "--port", "0", "--json", "--seed", "--database", "sqlite", "--dir", project_dir])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .spawn()
            .expect("forgebase is on the PATH");
        let out = child.stdout.take().unwrap();
        for line in BufReader::new(out).lines() {
            let line = line.unwrap();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) {
                if v["event"] == "ready" {
                    return Dev {
                        child,
                        url: v["url"].as_str().unwrap().to_owned(),
                        api_key: v["api_key"].as_str().unwrap().to_owned(),
                    };
                }
            }
        }
        panic!("forgebase dev exited before it was ready");
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        // SIGINT is the clean stop (it deletes the data); fall back to a kill.
        #[cfg(unix)]
        let _ = Command::new("kill").args(["-INT", &self.child.id().to_string()]).status();
        #[cfg(not(unix))]
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
```

```rust
// tests/todos.rs
mod common;

#[tokio::test]
async fn lists_the_seeded_todos() {
    let dev = common::Dev::start("forgebase");
    let fb = forgebase_sdk::ForgeBase::builder(&dev.url).api_key(&dev.api_key).build().unwrap();
    let page = fb.collection::<serde_json::Value>("todos").list(&Default::default()).await.unwrap();
    assert!(!page.items.is_empty());
}
```

(Share one `Dev` across tests with a `std::sync::OnceLock` and stop it in a
`ctor`/`atexit` hook if starting one per test is too slow; each start takes
about a second on SQLite.)

### 4.3 Dart

```dart
// test/forgebase_dev.dart
import 'dart:async';
import 'dart:convert';
import 'dart:io';

class ForgeBaseDev {
  ForgeBaseDev._(this._process, this.url, this.apiKey);
  final Process _process;
  final String url;
  final String apiKey;

  /// Starts `forgebase dev` on a free port; completes when the project is applied and seeded.
  static Future<ForgeBaseDev> start({String dir = 'forgebase'}) async {
    final p = await Process.start('forgebase',
        ['dev', '--port', '0', '--json', '--seed', '--database', 'sqlite', '--dir', dir]);
    p.stderr.drain<void>(); // keep the pipe empty; print it while debugging
    final ready = Completer<Map<String, dynamic>>();
    p.stdout.transform(utf8.decoder).transform(const LineSplitter()).listen((line) {
      try {
        final v = jsonDecode(line) as Map<String, dynamic>;
        if (v['event'] == 'ready' && !ready.isCompleted) ready.complete(v);
      } catch (_) {/* not a JSON line */}
    });
    unawaited(p.exitCode.then((c) {
      if (!ready.isCompleted) ready.completeError('forgebase dev exited with $c before it was ready');
    }));
    final v = await ready.future.timeout(const Duration(minutes: 2));
    return ForgeBaseDev._(p, v['url'] as String, v['api_key'] as String);
  }

  /// Ctrl-C: a clean stop deletes the emulator's data.
  Future<void> stop() async {
    _process.kill(ProcessSignal.sigint);
    await _process.exitCode;
  }
}
```

```dart
// test/todos_test.dart
import 'package:forgebase/forgebase.dart';
import 'package:test/test.dart';
import 'forgebase_dev.dart';

void main() {
  late ForgeBaseDev dev;
  setUpAll(() async => dev = await ForgeBaseDev.start());
  tearDownAll(() => dev.stop());

  test('lists the seeded todos', () async {
    final fb = ForgeBase(dev.url, apiKey: dev.apiKey);
    final page = await fb.collection('todos').list();
    expect(page.items, isNotEmpty);
  });
}
```

### 4.4 Other languages and CI

Anything that can spawn a process and read lines works the same way: Kotlin
(`ProcessBuilder` + `readLine`), Swift (`Process` + `Pipe`), Python
(`subprocess.Popen` + `proc.stdout.readline`). In CI, install the release
binary (see the workflow `forgebase new` writes), run the tests, and the
emulator needs nothing else: SQLite is built in.

## 5. Notes and limits

- The emulator is the real server, so what works there works on `forgebase
  serve`. It is for development: its credentials are generated per run, and it
  listens on `127.0.0.1` unless you pass `--host`.
- Webhook subscriptions deliver to the URLs in your project; use
  `forgebase queues listen --forward-to` to receive them locally.
- Function secrets come from environment variables (`[secrets]` in
  `function.toml`), as in `forgebase deploy`: export them, or put them in
  `.env`, before `forgebase dev`. A variable that is not set does not stop the
  emulator: the secret gets the value `forgebase-dev-placeholder` and `dev`
  warns.
- `--watch` re-runs a function's `build` command on every change.

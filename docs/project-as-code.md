# Project as code

Keep a ForgeBase server's configuration — collections, functions, queues,
buckets, access rules, OAuth clients — as files in your app's repository,
review changes in pull requests, and apply them from your machine or CI with
the `forgebase` binary. No Terraform, no state file: the files are the desired
state, the server is the actual state. Design: [ADR-0037](adr/0037-project-as-code.md).
(For infrastructure teams, the [Terraform provider](deployment.md#10-configuration-as-code-terraform)
manages the same resources.)

## 1. Quick start

```sh
forgebase new my-app --template basic      # or: functions, realtime-chat
cd my-app
cp .env.example .env                       # set FORGEBASE_URL and FORGEBASE_TOKEN
forgebase project plan                     # what would change (exit 2: changes)
forgebase deploy                           # build functions, plan, confirm, apply
```

Already have a configured server? Write its state into a project:

```sh
FORGEBASE_URL=https://api.example.com FORGEBASE_TOKEN=fbk_... forgebase project pull --dir forgebase
forgebase project plan                     # exit 0: the files match the server
```

The credential is an API key of a service account with the `admin` role (or a
personal access token of an administrator). Engine resource rules and collection
rules need a super-administrator. To create one key:

```sh
# Signed in as an administrator:
#   POST /api/auth/service-accounts            {"name": "ci", "role": "admin"}
#   POST /api/auth/service-accounts/{id}/keys  {"name": "ci", "scopes": ["*"]}
```

## 2. Commands

| Command | What it does | Exit codes |
|---|---|---|
| `forgebase new <dir> [--template basic\|functions\|realtime-chat] [--name N] [--project-dir forgebase] [--force]` | Scaffolds an app: the project directory, samples, seed data, `.env.example`, `.gitignore`, README, `.github/workflows/forgebase.yml`. | 0 / 1 |
| `forgebase project validate [--json]` | Checks every file against its schema; no server needed. | 0 valid / 1 |
| `forgebase project plan [--prune] [--json] [--out FILE]` | Compares the project with the server and prints what `apply` would do. | **0** no changes / **2** changes / **1** error |
| `forgebase project apply [--prune] [--allow-destructive] [--auto-approve] [--plan FILE] [--update-secrets] [--secrets-out FILE] [--json]` | Makes the server match the project. | 0 all changes succeeded / 1 |
| `forgebase project drift [--json]` | Reports differences made outside the project (dashboard, API, another tool). | **0** none / **2** drift / **1** error |
| `forgebase project pull [--force]` | Writes the server's state into the project directory. Never writes secrets. | 0 / 1 |
| `forgebase project schemas` | Rewrites `<dir>/.schemas/*.schema.json`. | 0 / 1 |
| `forgebase deploy [apply flags] [--skip-build]` | Runs each function's `build` command, then plans and applies. | 0 / 1 |
| `forgebase dev [--seed] [--watch]` | A local emulator that applies this project to a throw-away server ([dev-workflow.md](dev-workflow.md)). | |

Options of every `project` command and of `deploy`:

| Option | Environment | Meaning |
|---|---|---|
| `--dir DIR` | | The project directory (default `./forgebase`). |
| `--server URL` | `FORGEBASE_URL` (also `FB_URL`) | `http://` or `https://` base URL, optionally with a path prefix. Default: the project's `url`, else the local `forgebase.toml` bind address. |
| `--token TOKEN` | `FORGEBASE_TOKEN` (also `FB_TOKEN`) | API key (`fbk_…`) or personal access token (`fbp_…`). |
| `--admin-email EMAIL` | `FORGEBASE_ADMIN_EMAIL` | Sign in as this administrator instead; password from `--password-stdin` or `FORGEBASE_ADMIN_PASSWORD`. |
| `--env NAME` | | Use `[environments.NAME]` of the project file and that ForgeBase environment (§5). |
| `--realm REALM` | | Realm of OAuth clients, sent as `X-Forgebase-Realm`. |
| `--ca-file FILE` | `FORGEBASE_CA_FILE` | Extra PEM CA certificates for an `https://` server with a private CA. Certificates are always verified. |

## 3. Layout

```text
forgebase/
├── forgebase.project.toml          project file
├── .schemas/*.schema.json          JSON Schemas for editors (generated; commit them)
├── collections/<name>.json
├── functions/<name>/function.toml  + the code: index.ts, index.mts, index.js, index.mjs, module.wasm or main.wasm
├── queues/topics/<name>.json
├── queues/subscriptions/<name>.json
├── storage/buckets/<name>.json
├── policies/<resource kind>.json
├── oauth-clients/<name>.json
└── seed/<collection>.jsonl         fixtures for `forgebase dev --seed`; never applied
```

A resource's name is its file name (or the file's `name` key, which must then
match). Characters outside `[A-Za-z0-9._-]` are percent-encoded in file names:
topic `orders:new` lives in `queues/topics/orders%3Anew.json`. Every JSON file
may start with `"$schema"` pointing at `.schemas/` (pull and new write it) and
every TOML file with a `#:schema` comment, so VS Code, JetBrains IDEs and Taplo
validate as you type. Unknown keys are errors, reported with file and line:

```text
error: forgebase/collections/posts.json:7: fields[1].maxlength: unknown key "maxlength" (expected one of: default, hidden, max_length, min_length, name, renamed_from, required, type, unique)
forgebase/functions/send/function.toml:4: limits.speed: unknown key "speed" (expected one of: fuel, max_concurrency, memory_bytes, timeout_ms)
2 problems in the project
```

### 3.1 `forgebase.project.toml`

```toml
#:schema .schemas/project.schema.json
format = 1                # layout format
name = "my-app"
manage = ["collections", "functions", "queues", "storage", "policies", "oauth-clients"]

[defaults]                # every command; --server / FORGEBASE_URL override url
url = "http://127.0.0.1:8090"
realm = "default"

[environments.staging]    # forgebase deploy --env staging
url = "https://staging.example.com"

[environments.production]
url = "https://api.example.com"
project = "my-app"        # optional: a ForgeBase project (id or slug) ...
environment = "production" # ... and its environment (default: the table name)
```

`manage` lists what the project owns (default: everything). A kind left out is
never changed, pruned or reported as drift — useful when a project only manages
collections.

### 3.2 `collections/<name>.json`

The server's collection definition without server-managed values (ids,
versions, timestamps, system fields), plus `indexes` and `rules`:

```json
{
  "$schema": "../.schemas/collection.schema.json",
  "name": "posts",
  "type": "base",
  "fields": [
    { "name": "title", "type": "text", "max_length": 200, "required": true },
    { "name": "author", "type": "relation", "collection": "users" },
    { "name": "tags", "type": "select", "max_select": 5, "values": ["news", "howto"] },
    { "name": "body", "type": "text", "renamed_from": "content" }
  ],
  "indexes": [
    { "name": "posts_author_created", "keys": [{ "path": "author" }, { "path": "created", "desc": true }] }
  ],
  "rules": {
    "list_rule": "",
    "view_rule": "",
    "create_rule": "@request.auth.id != \"\"",
    "update_rule": "author = @request.auth.id",
    "delete_rule": null,
    "fields": { "title": { "read": "", "write": null } }
  },
  "schema_mode": "strict",
  "versioned": false
}
```

| Key | Default | Notes |
|---|---|---|
| `type` | `base` | `base`, `auth` or `view`. Cannot change: a change replaces the collection (destructive). |
| `schema_mode` | `strict` | `strict` or `flexible`. |
| `versioned` | `false` | Keep every record version. |
| `view_query` | | The `SELECT` of a view (views declare no fields). |
| `options` | `{}` | Options of other modules (`tenant_scoped`, ...). Only the keys listed are managed. `tenant_scoped` cannot change. |
| `fields[]` | `[]` | In order. Common keys: `name`, `type`, `required`, `unique`, `hidden`, `default`, `renamed_from`. Type options sit beside `type` (`max_length`, `values`, `max_select`, `dimensions`, ...; see the collection API). A relation names its target with `collection` (a name). |
| `indexes[]` | | Manual indexes: `name` (unique across the server), `keys` (`path`, `desc`), `unique`, `kind` (`btree`, `fulltext`, `trigram`), `partial`. Omit the key to leave the server's indexes alone. |
| `rules` | | `list_rule`, `view_rule`, `create_rule`, `update_rule`, `delete_rule`, `fields.<name>.read/write`: `null` locks, `""` allows everyone, anything else is a filter. Omit the key to leave the rules alone. |
| `renamed_from` | | On the collection or a field: rename on the server, keeping the data. Remove it after the apply. |

System fields of `auth` collections (`email`, `password`, ...) are not listed;
the server keeps them. The built-in `users` collection is managed only when the
project has `collections/users.json`.

### 3.3 `functions/<name>/function.toml`

```toml
#:schema ../../.schemas/function.schema.json
description = "Sends the welcome email"
access = "permission"        # public | authenticated | permission (default)
entry = "index.ts"           # default: the first of index.ts, index.mts, index.js, index.mjs, module.wasm, main.wasm
build = "npm run build"      # optional: `forgebase deploy` runs it in this directory first
capabilities = ["log", "db.read", "fetch", "secrets"]
fetch_hosts = ["api.mailer.example"]
kv_rules = []                # omit to leave the server's kv rules alone

[env]                        # plain, non-secret values
FROM = "hello@example.com"

[limits]                     # keys left out keep the server's value
timeout_ms = 10000

[secrets]                    # secret name = environment variable read at apply time
MAILER_KEY = "MAILER_API_KEY"

[[hooks]]
collection = "users"
events = ["after_create"]
```

A function is one ES module (ADR-0027); for several source files, set `build`
to a bundler command that writes `entry`. A change of code or settings deploys
a new version and activates it. The plan compares the code by SHA-256, so an
unchanged file deploys nothing.

### 3.4 Queues, buckets, rules, clients

`queues/topics/<name>.json`:

```json
{ "labels": { "team": "orders" }, "schema": null, "source": { "collection": "orders", "ops": ["create", "update"] }, "retention_ms": 604800000 }
```

`retention_ms`, `retention_bytes`, `max_message_bytes` and `dedup_window_ms`
keep the server's value when left out. A changed `source` replaces the topic
(destructive).

`queues/subscriptions/<name>.json`:

```json
{
  "topic": "order-events",
  "push": { "kind": "webhook", "endpoint": "https://hooks.example.com/orders", "secret_env": "ORDERS_HOOK_SECRET" },
  "max_delivery_attempts": 5,
  "dead_letter_topic": "order-events-dlq",
  "labels": {},
  "start": "earliest"
}
```

`topic`, `filter` and `ordering` cannot change (a change replaces the
subscription). `start` applies when the subscription is created. A webhook's
signing secret is issued by the server when the subscription is created or its
endpoint changes; it is never in the files (see §6).

`storage/buckets/<name>.json`:

```json
{ "visibility": "authenticated", "versioning": false, "max_object_bytes": 10485760, "allowed_content_types": ["image/*"], "acl": [{ "grantee": "role:editor", "permission": "write", "prefix": "public/" }] }
```

`acl` is the bucket's whole ACL; omit it to leave the ACL alone.

`policies/<resource kind>.json` (engine resource rules, ADR-0008):

```json
{ "rules": { "uploads": { "view": "", "list": null }, "*": { "read": "@request.auth.id != \"\"" } } }
```

The file name is the resource kind: `kv_namespace`, `vector_collection`,
`graph_label`, `graph_edge_type`, `bucket`, `queue_topic`, `queue_subscription`
or `realtime_channel`.

`oauth-clients/<name>.json` (identified by `name` inside the realm):

```json
{ "type": "confidential", "redirect_uris": ["https://app.example.com/callback"], "scopes": ["openid", "profile", "email"], "skip_consent": true, "secret_env": "WEB_CLIENT_SECRET" }
```

### 3.5 `seed/<collection>.jsonl`

One JSON object per line; blank lines and `#` lines are skipped. `apply` never
loads seeds; `forgebase dev --seed` (the local emulator) does. `validate`
checks every line.

```text
{"title": "Read the docs", "done": true}
{"title": "Ship it", "done": false}
```

## 4. How plan and apply decide

- **Create / update / replace / delete.** A resource only on the server is
  deleted only with `--prune`; otherwise the plan lists it under *Not in the
  project*. Resources the server creates itself (`users`, `forgebase-job-results`)
  are never pruned.
- **Destructive changes** need `--allow-destructive`: dropping a field or a
  collection, changing a field's type, a multi-value field becoming
  single-value, deleting a bucket (with its objects), a topic, a subscription,
  a function or a client, and every replacement. Without the flag, `apply`
  refuses before it changes anything.
- **Data migrations** are shown under the change: a dropped field or a changed
  storage class rebuilds the table in one transaction (it rolls back if a row
  does not convert); a new unique field builds a unique index over the
  existing records. The plan notes the schema version step, recorded in
  `GET /api/v1/collections/<name>/history`.
- **Order.** Collections (relation targets first), buckets, topics, functions,
  subscriptions, rules, clients; deletions last, in reverse.
- **Concurrency.** Each change remembers the server state it was planned
  against. Right before it runs, the resource is read again: if someone changed
  it in the meantime, the change fails with `changed_since_plan` and nothing is
  overwritten. `plan --out plan.json` + `apply --plan plan.json` extends this to
  the time between review and apply.
- **Partial failure.** A failed change does not stop independent ones; changes
  that need it are skipped. Fix the problem and run `apply` again: it plans only
  what is still different. Applying the same saved plan again skips what is
  already in place.

Example:

```text
Plan for my-app against https://api.example.com

~ collection posts  [update, destructive]
    fields.legacy: { "name": "legacy", "type": "text", ... } -> (absent)
    fields.title.max_length: 100 -> 200
    ! destructive: drops field "legacy" and its data
    * data migration: the table is rebuilt in one transaction (existing records are copied and checked against the new definition)
    note: schema version v3 -> v4 (recorded in GET /api/v1/collections/posts/history)
+ function send-welcome  [create]

Plan: 1 to create, 1 to update, 0 to replace, 0 to delete (1 destructive: needs --allow-destructive).
Not in the project (kept; `--prune` deletes them): bucket scratch
```

## 5. Environments and remote servers

`--env NAME` selects `[environments.NAME]` from the project file, merged over
`[defaults]`. Typical: one server per environment, each with its own `url` (and
its own `FORGEBASE_TOKEN` in CI). When the table (or `[defaults]`) names a
ForgeBase `project`, the CLI resolves its environment (`environment`, default
`NAME`) and sends `X-Forgebase-Org` and `X-Forgebase-Tenant`, so tenant-owned
resources (buckets, topics, subscriptions) are created in that environment.

Remote servers are reached over `https://` with verified certificates (the
Mozilla CA set, plus `--ca-file`). A URL may carry a path prefix
(`https://example.com/forgebase`) when a proxy serves ForgeBase below `/`.

## 6. Secrets

- Project files never hold secrets, and `pull` never writes them.
- Function secrets: `[secrets] NAME = "ENV_VAR"`. `apply` sends the value of
  `ENV_VAR` when the server lacks the secret (values cannot be compared;
  `--update-secrets` sends them all again) and deletes secrets the file no
  longer lists. `pull` writes `NAME = "<FUNCTION>_<NAME>"` references.
- Server-issued secrets (a webhook subscription's signing secret, a
  confidential OAuth client's secret) are shown once, when created.
  `apply --secrets-out FILE` writes them as `NAME=value` (mode `0600`; refused
  inside the project directory), with `NAME` from `push.secret_env` /
  `secret_env` or `FORGEBASE_SUBSCRIPTION_<NAME>_SECRET` /
  `FORGEBASE_OAUTH_CLIENT_<NAME>_SECRET`. Without the flag, apply warns that it
  did not keep them; rotate the client secret (`POST /api/auth/clients/{id}/secret`)
  or change the webhook endpoint to get a new one.

## 7. CI recipe

`forgebase new` writes `.github/workflows/forgebase.yml`:

- **Pull requests** that touch the project: install the release CLI (archive
  checksum verified), `forgebase project validate`, then `forgebase project
  plan` with the plan in the job summary. Exit code 2 (changes) passes; 1 fails
  the check.
- **`main`**: `forgebase deploy --auto-approve` in the `production` GitHub
  environment (add required reviewers there for a manual gate). Destructive
  changes still need `--allow-destructive`, deletions `--prune`: add them to the
  workflow deliberately, or run them by hand.

Repository secrets: `FORGEBASE_URL`, `FORGEBASE_TOKEN`, and every variable named
in a function's `[secrets]`.

A drift check on a schedule:

```yaml
on:
  schedule: [{ cron: "0 6 * * *" }]
jobs:
  drift:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      # ... install the CLI as in forgebase.yml ...
      - run: forgebase project drift --dir forgebase   # exit 2 fails the job when the server drifted
        env:
          FORGEBASE_URL: ${{ secrets.FORGEBASE_URL }}
          FORGEBASE_TOKEN: ${{ secrets.FORGEBASE_TOKEN }}
```

## 8. Limits

- The concurrency check runs in the CLI: the API has no version precondition
  for configuration resources, so a change landing between the re-read and the
  write of the same resource is not detected.
- An apply is not one transaction across resources; it converges on re-run.
- Not in format 1: roles, service accounts, API keys, vector collections,
  feature flags, entitlement plans (use the API or the Terraform provider).
- OAuth clients are matched by name inside the realm; duplicate names on the
  server stop the plan until one is renamed.

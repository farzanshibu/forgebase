# ADR-0037: Project as code (`forgebase project`, `forgebase deploy`, `forgebase new`)

- Status: accepted
- Date: 2026-11-14
- Scope: `crates/forgebase-cli` (`src/project/**`, `templates/project/**`, the `project`, `deploy` and
  `new` commands, `tests/project_e2e.rs`), `crates/forgebase-functions` (one read endpoint),
  `crates/forgebase-core` (five environment names the config loader lets through). No migration.
- Builds on: ADR-0003 (collections, schema history, indexes), ADR-0008 (collection and engine resource
  rules, tenancy headers), ADR-0017 / ADR-0027 (functions, versions, secrets, hooks), ADR-0018 (buckets,
  ACLs), ADR-0019 (OAuth client apps), ADR-0022 (topics, subscriptions, push delivery).
- Reference for users: [docs/project-as-code.md](../project-as-code.md).

## Context

A ForgeBase server is configured by clicking in the dashboard or by calling the REST API. The Terraform
provider (`integrations/terraform-provider-forgebase`) serves infrastructure teams, but an app developer
should not need Terraform, a state backend and HCL to keep a few collections and functions in git, review
schema changes in pull requests and deploy from CI. What is missing is a declarative project directory and a
plan/apply workflow inside the `forgebase` binary they already have.

## Decisions

### 1. The CLI talks to the public REST API only

`forgebase project` and `forgebase deploy` never open a data directory. They reach the server over HTTP or
HTTPS (rustls with the Mozilla roots through `forgebase-auth`'s outbound client; `--ca-file` /
`FORGEBASE_CA_FILE` adds a private CA; certificates are always verified), so the same commands work against a
local `forgebase serve` and a remote server behind TLS. The server needs no new state: the project directory
is the desired state and the server is the actual state. There is no state file, so there is nothing to lock,
lose or leak.

The server URL is, in order: `--server`, `FORGEBASE_URL` (or `FB_URL`, as the other CLI commands), the
project file's target (§7), the local config's bind address. The credential is `--token`, `FORGEBASE_TOKEN`
(or `FB_TOKEN`): an API key of a service account with the `admin` role, or a personal access token. Instead,
`--admin-email` (or `FORGEBASE_ADMIN_EMAIL`) signs in an administrator through `POST /api/auth/admins/login`
with the password from `--password-stdin` or `FORGEBASE_ADMIN_PASSWORD`. These five `FORGEBASE_*` names are
now accepted (and ignored) by the server's config loader, which rejected them as unknown variables before.

### 2. The layout (a stable contract)

The dev emulator (`forgebase dev`), typegen and the dashboard rely on this layout; it changes only with a new
`format` number.

```text
forgebase/                          the project directory (--dir, default ./forgebase)
├── forgebase.project.toml          format = 1, name, manage, [defaults], [environments.<name>]
├── .schemas/                       JSON Schemas of every file kind (generated; commit them)
│   ├── project.schema.json  collection.schema.json  function.schema.json  topic.schema.json
│   └── subscription.schema.json  bucket.schema.json  policy.schema.json  oauth-client.schema.json
├── collections/<name>.json         a collection: type, schema_mode, versioned, view_query, options,
│                                   fields, indexes, rules
├── functions/<name>/function.toml  a function's settings
├── functions/<name>/<entry>        its code: index.ts | index.mts | index.js | index.mjs | module.wasm |
│                                   main.wasm (first that exists), or `entry = "..."`
├── queues/topics/<name>.json       a topic
├── queues/subscriptions/<name>.json a subscription
├── storage/buckets/<name>.json     a bucket (and optionally its whole ACL)
├── policies/<resource kind>.json   engine resource rules of one kind (kv_namespace, vector_collection,
│                                   graph_label, graph_edge_type, bucket, queue_topic,
│                                   queue_subscription, realtime_channel)
├── oauth-clients/<name>.json       an OAuth / OIDC client app
└── seed/<collection>.jsonl         fixtures: one JSON object per line, `#` comment lines; used by
                                    `forgebase dev --seed`, never by apply
```

- **Identity.** A resource's name is the file's `name` key when present (it must then equal the file name),
  else the file name without `.json`; a function's is its directory name. Characters outside
  `[A-Za-z0-9._-]` are percent-encoded in file names (topic `orders:new` is `orders%3Anew.json`). An engine
  rule's name is `<resource kind>/<resource name>` (`*` allowed).
- **Collections** hold the server's collection definition minus server-managed values (`id`, `system`,
  `version`, `created`, `updated`, field ids, system fields). Type options sit beside `type`, as in the API.
  A relation names its target by `collection` (a name), not `collection_id`. `renamed_from` on the
  collection or on a field renames on the server and keeps the data. `indexes` lists manual indexes
  (`name`, `keys` of `{path, desc}`, `unique`, `kind`, `partial`); `rules` holds `list_rule`, `view_rule`,
  `create_rule`, `update_rule`, `delete_rule` and per-field `fields.<name>.{read,write}` (`null` locks,
  `""` opens).
- **Functions** (`function.toml`): `description`, `access`, `entry`, `build`, `capabilities`,
  `fetch_hosts`, `[env]` (plain values), `[limits]`, `[[hooks]]`, `[[kv_rules]]`, and `[secrets]` mapping a
  secret name to the **environment variable** that holds its value at apply time.
- **Secrets are never in files.** Function secret values come from environment variables. A webhook
  subscription's signing secret and a confidential client's secret are issued by the server once;
  `push.secret_env` / `secret_env` name the variable they belong in, and `apply --secrets-out FILE` (refused
  inside the project directory, written `0600`) exports newly issued ones as `NAME=value`.
- **`manage`** in the project file lists the areas the project owns (`collections`, `functions`, `queues`,
  `storage`, `policies`, `oauth-clients`; default all). Kinds left out are neither changed nor reported.

### 3. Validation: one schema per kind, every problem with `file:line`

Every file is checked against the schema of its kind before anything talks to a server, and every problem of
every file is reported in one run as `path:line: json.path: message`. The schemas in `.schemas/` are the same
documents the CLI validates with (`forgebase project schemas` rewrites them), so an editor and the CLI cannot
disagree. Unknown keys are errors, as in the server's collection API. Field option keys are checked per field
type; names are checked with the server's own rules (`forgebase-schema`); duplicates (two files for one
resource, two collections declaring the same index name) are errors. `forgebase project validate` runs only
this step (no server needed).

### 4. Canonical specs and what a file manages

Both sides become a *spec*: canonical JSON with defaults filled in (`required: false`, `max_select: 1`,
`visibility: "private"`, ...), lists sorted where order has no meaning, whole-valued floats as integers and
server values left out. Two specs are equal exactly when nothing needs to change.

A file manages what it says. Optional settings with server defaults (topic retention and limits,
subscription deadlines and backoff, function limits per key, OAuth grant types and scopes) keep the
server's value when the file leaves them out. `options` is managed key by key. `indexes`, `rules`, a bucket's
`acl` and a function's `kv_rules` are managed when the key is present (pull always writes them). System
fields of auth collections and resources the server creates itself (the `users` collection, the
`forgebase-job-results` bucket) are never reported or pruned unless declared.

### 5. Plan

For each resource: **create**, **update**, **replace** (a setting that cannot change in place changed:
collection `type` or `tenant_scoped`, a topic's record `source`, a subscription's `topic`, `filter` or
`ordering`, an OAuth client's `type`) or **delete** (a server resource the project does not declare; planned
only with `--prune`, otherwise listed as *not in the project*). Each change carries a structural diff
(`fields.title.max_length: 100 -> 200`, `fields[order]`), and:

- **destructive** reasons: dropping a field, changing a field's type, a multi-value field becoming
  single-value, deleting a collection, bucket, topic, subscription, function or client, any replacement.
  Applying them needs `--allow-destructive`.
- **data migration** notes, from ADR-0003 §4: a dropped field or a changed storage class (type,
  `max_select` across 1, `integer`, relation target or `cascade_delete`) rebuilds the table in one
  transaction, which fails and rolls back if a row does not convert; a new unique field builds a unique
  index over existing rows. Collection updates note the schema version step (`v3 -> v4`) the server records
  in `GET /api/v1/collections/{name}/history`, the before/after record of the change.

Order: collections (by relation dependencies; a relation to a collection created later in the same run is
added by a follow-up update), buckets, topics, functions, subscriptions, engine rules, OAuth clients; deletes
last in reverse order. References are checked before planning: a relation target, a subscription's topic or
dead-letter topic, a push function, a hook's collection or a topic source must be in the project or on the
server.

Exit codes (CI): `plan` and `drift` exit **0** when nothing differs, **2** when something does, **1** on an
error (invalid project, unreachable server, refused credential). `--json` prints the plan
(`format`, `server`, `changes[]` with `kind`, `name`, `action`, `diff`, `destructive`, `migration`, `notes`,
`fingerprint`, `desired_digest`, `depends_on`, `unmanaged[]`, `summary`); `--out FILE` saves it.

### 6. Apply: dependency order, optimistic concurrency, resumable

The config resources have no version precondition in the API (collection `PUT` ignores the `version` it is
sent; buckets, topics and subscriptions have none), so concurrency is checked by the client: every change
records the **fingerprint** of the server state it was planned against (the spec plus the server's revision:
collection schema version, function versions, `updated_at`). Right before a change runs, the resource is
read again:

- same fingerprint: the change runs;
- the server already has the project's state (an earlier, interrupted apply): the change is skipped as
  *already in place*;
- anything else: the change fails with `changed_since_plan`; nothing is overwritten.

`apply --plan FILE` applies a saved plan: the project's specs must still have the digests of the plan, the
server the fingerprints, and no new change may be needed; otherwise nothing runs. Changes of a saved plan that
are already in place are skipped, so the same plan file can be applied again after a partial failure.

A failed change does not stop independent ones; changes that depend on it (`depends_on`) are skipped, and the
exit code is 1. The next run plans only what is still different and converges. Each change is one or a few
API calls; collection changes are transactional DDL on the server, but an apply is not one transaction
across resources.

`apply` asks for confirmation on a terminal; without one it refuses unless `--auto-approve` (a saved plan was
reviewed when it was made). Destructive changes are refused before anything runs, without
`--allow-destructive`. Function secrets are sent when missing on the server (values cannot be compared;
`--update-secrets` sends them all) and deleted when the file no longer lists them.

### 7. Drift, pull, deploy, environments

- `drift` plans against the server and reports every difference — changed resources, resources missing on
  the server, resources only on the server — and exits 2 when there is any.
- `pull` writes the server's state as a project: keys in a fixed order (`name`, `type` first, then
  alphabetical), defaults left out, one line per short object, so pulling twice gives the same bytes and a
  `pull → plan` reports no changes. It refuses an existing project without `--force`, which also removes
  resource files the server no longer has (never `seed/` or other files). Function sources come from
  `/versions/{v}/source`, WebAssembly modules from the new `GET /api/v1/functions/{name}/versions/{v}/module`
  (`application/wasm`; `404 function_source_not_found` for JavaScript versions). Secrets are not written: the
  server does not return them, and function secrets become references to `<FUNCTION>_<SECRET>` variables.
- `deploy` runs each function's `build` command (in its directory, `sh -c` / `cmd /C`; the entry must exist
  afterwards), then plans and applies with the flags of `apply`.
- `--env NAME` selects `[environments.NAME]` of the project file, merged over `[defaults]` (`url`, `realm`,
  `project`, `environment`; `environment` defaults to `NAME`). When a ForgeBase `project` (id or slug) is set,
  the CLI resolves its environment and sends `X-Forgebase-Org` and `X-Forgebase-Tenant` (ADR-0008 §10), so
  tenant-owned resources (buckets, topics, subscriptions) land in that environment. A `realm` is sent as
  `X-Forgebase-Realm` and is the realm of OAuth clients.

### 8. `forgebase new`

`forgebase new <dir> --template basic|functions|realtime-chat` writes an app: the project directory with its
schemas, a sample collection (fields, an index, rules), a sample function (with the `forgebase.d.ts` types),
seed fixtures, `.env.example`, `.gitignore`, a README and `.github/workflows/forgebase.yml`, which installs
the release CLI (checksum verified), runs `forgebase project validate` and `plan` on pull requests (the plan
goes to the job summary) and `forgebase deploy --auto-approve` on `main`. `functions` adds a record hook with a
secret, a topic fed by the collection and a subscription; `realtime-chat` adds a relation, a before-create
moderation hook and a compound index.

### 9. Room for the webhook tooling

`push` is an object: attributes the webhook delivery work adds (secret rotation, auto-pause, a test send)
become further optional keys of `push` in `queues/subscriptions/*.json` without a format change, and
`secret_env` is where a caller-chosen signing secret will come from once the server accepts one.

## Consequences

- Developers and CI keep the server configuration next to the app code, review it in pull requests and apply
  it with one binary; infrastructure teams keep using Terraform for the same resources.
- The concurrency check is client side: a change made between the re-read and the write of the same resource
  (milliseconds) is not detected. A server-side precondition (`If-Match` on the config routes) would close
  that window; it is a follow-up for the owning crates.
- OAuth clients are identified by name inside a realm; two clients with one name on the server make plan
  fail until one is renamed.
- A function is one file (ADR-0027 §2); multi-file code needs a `build` command (a bundler) that writes the
  entry.
- Roles, service accounts, API keys, vector collections, feature flags and entitlement plans are not part of
  format 1.

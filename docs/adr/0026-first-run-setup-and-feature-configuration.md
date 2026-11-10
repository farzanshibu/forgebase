# ADR-0026: First-run setup and runtime feature configuration

- Status: accepted
- Date: 2026-11-11
- Work package: FB-91 (feature registry, module switches, configuration API).
  FB-90 (setup mode, setup token, `/api/setup/*`) adds its section after merge.
- Contract: [docs/requirements/first-run-setup.md](../requirements/first-run-setup.md) §3.
- Scope: `crates/forgebase-core/src/features.rs` (registry, writer), `config.rs`
  (switches, sources), `crates/forgebase-api/src/features.rs` (gate, endpoints),
  `crates/forgebase-server` (what starts), `crates/forgebase-realtime` (`dispatch`).
- Builds on: ADR-0001 (configuration precedence), ADR-0021 (billing switch),
  ADR-0022 (queue switch), ADR-0013 (RESP listener).

## Context

The first administrator was created only on the command line, and only billing,
queues and the Redis listener could be switched off. The human asked for a
first-run flow that ends in "all feature map and config". The dashboard
(FB-92) needs one place that lists every module, says whether it is on, shows
each setting with its type, value and origin, and can change the file.

## Decisions

### 1. One static registry

`forgebase_core::features::FEATURES` lists every module in display order:
id, name, one-line summary, whether it can be switched off, the setting that
switches it, its studio route, its documentation, the HTTP route patterns it
owns, and its settings. A setting has a dotted key (as in `forgebase.toml`),
label, help (naming its environment variable and flag), type (`bool`, `int`,
`string`, `enum` with options, `duration`, `list`, `url`), unit, and whether it
is a secret, editable or needs a restart.

Values are read from the resolved `Config` by serializing it and following the
dotted key, so a setting cannot be listed with a value the server does not use.
Tests check that every key in `forgebase.example.toml` and in the `forgebase
init` template is in the registry, and that every registry setting written to
a file is reported with source `file`.

### 2. Which modules can be switched off, and why not the others

Each switch is `[<section>] enabled`, default `true`, with an environment
variable `FORGEBASE_<SECTION>_ENABLED`.

| Module | Switch | Off means |
|---|---|---|
| `storage` | `storage.enabled` | `/api/v1/storage` answers `feature_disabled`; the service is not opened; no reconcile job handler; the `storage-reconcile` schedule is paused; job results too large for the row are refused. |
| `functions` | `functions.enabled` | `/api/v1/functions` off; the engine is not opened, so record writes skip function hooks; no after-hook dispatcher, no `function.*` job handlers; queue push to a function fails delivery. |
| `queues` | `queue.enabled` | as before (ADR-0022), now with `feature_disabled`. The section stays `[queue]`. |
| `realtime` | `realtime.enabled` | `/api/v1/realtime` off; no dispatcher, no fan-out receiver, no job events on `jobs:*`. The record-change outbox is still written and pruned: server functions (after-hooks) and queues read it. |
| `vector` | `vector.enabled` | `/api/v1/vector` off; indexes are not loaded, warmed or saved. |
| `graph` | `graph.enabled` | `/api/v1/graph` off (it has no background work). |
| `identity` | `identity.enabled` | The OIDC provider (`/api/auth/oidc/*`, `/api/auth/clients`, `/api/auth/consents`, `/api/auth/keys/oidc/*`, realm signing keys and consents) and realm management (`/api/v1/realms`, `/api/v1/realm-bindings`) are off. Sign-in settings of a realm (`/api/auth/realms/{realm}`, its OAuth providers and SMS) stay with `auth`, and users of existing realms keep signing in. |
| `access` | `access.enabled` | The management API of policies, roles, ACLs, organisations, teams, projects, invitations, overrides and suspensions is off. The policy engine keeps enforcing the stored rules: switching the API off never opens access. |
| `resp` | `resp.enabled` | The Redis protocol listener is not bound. Its default stays **`false`**: it is a second network listener without TLS, so it remains opt-in (ADR-0013). |
| `billing` | `billing.enabled` | As before (ADR-0021): not mounted, not documented; the code is now `feature_disabled` with the message "Billing is disabled on this server"; the `billing-maintenance` schedule is paused. |

Always on (`can_disable: false`), with the reason:

- `server`: the HTTP listener, logging and database settings (`server.*`,
  `log.*`, `database.*`, `data_dir`) belong to every module.
- `database`: collections, records and SQL are the core every module stores in.
- `auth`: administrators, users, sessions and API keys authenticate every request,
  including the configuration API itself.
- `kv`: server functions and the Redis listener run on it; it has no worker of
  its own beyond the snapshot.
- `jobs`: every module's maintenance runs as jobs (entitlement maintenance,
  storage reconcile, credit expiry, billing renewals, email delivery).
  Switching the job queue off would silently stop all of them.
- `entitlements`, `credits`: the policy pipeline (steps 4 and 6) and plan rate
  limits call them on every request; billing and queues meter through them.
- `customize`: its host router runs in front of every request (custom domains)
  and its agreement gate in the pipeline.
- `backups`: snapshots are the safety net for migrations and restores.

### 3. Gating

A middleware just inside the request-context layer (so the error envelope and
request id are set) answers `404 {"error":{"code":"feature_disabled","message":"<Name>
is disabled on this server"}}` for any path that matches a pattern of a
disabled module. It runs before authentication, like an unmounted route. A
pattern uses `*` for one segment and a trailing `**` for any number; empty
segments are ignored. The same patterns remove the module's paths from
`/api/openapi.json`. Modules with background work are not opened at all
(storage, functions, vector, queues, billing) or run without it (realtime).
System schedules of disabled modules are paused at start and resumed when the
module is on again; a schedule an administrator paused by hand is resumed too.

### 4. Precedence and sources

Precedence is unchanged: flags > `FORGEBASE_*` environment > file > defaults.
The loader now records which layer set each key (`LoadedConfig::sources`) and
keeps the layers (`ConfigOrigin`) so the server can resolve the file again.
A setting set in the file reports `file` even when its value equals the default.
Secrets report `env` when their variable is set. The S3 credential variables
are now accepted by the environment parser (names only are kept; the storage
driver reads the values), which fixes the startup error they caused.

### 5. The configuration API

- `GET /api/system/features` and `PATCH /api/system/config` are for
  administrators (and admin service accounts with the `system` scope) only. They
  do not use `ops_access`: its loopback allowance lets unauthenticated local
  callers read `/metrics`, which must not extend to the deployment's settings.
- `GET` resolves the current file with the environment and flags seen at
  start, so values are what the server will run after a restart.
  `restart_pending` is true after a write that needs a restart, or whenever the
  file resolves to something other than the running configuration (a hand edit).
  If the file cannot be read, values are the running ones and `config_error`
  says why.
- `PATCH` checks every key (unknown, secret, read-only, type, enum options, URL
  scheme, TOML type such as a port above 65535), then refuses keys set by the
  environment or a flag with `409 setting_overridden`, then validates the whole
  resolved configuration and reports every range problem at once
  (`Config::problems`). Only then does it write all keys in one atomic replace:
  `toml_edit` keeps comments, order and the trailing comment of a replaced
  value; a temporary file in the same directory is fsynced, given the old
  file's permissions and renamed over it. A missing section is appended. Writes
  are serialized. Without a config file, `forgebase.toml` is created next to
  the data directory (the directory `forgebase serve` looks in by default).
- Secrets (S3 credentials, the master key) are never listed with a value and
  never written; their help names the variable or file that sets them.
  `data_dir` is listed but not editable: a new value would point the server at
  other data.
- Nothing applies without a restart in this version: every setting has
  `restart_required: true` and `applied` is empty. Live application is a later,
  per-setting decision.

## Consequences

- The dashboard can render the feature map and settings from one endpoint and
  write changes without hand-editing TOML.
- `RunningServer::functions()` and `vector()` now return `Option`.
- `forgebase serve` starts with `start_loaded` so sources are known; servers
  built in code (`start(&Config)`) report every setting as `default` and write
  `forgebase.toml` next to their data directory.
- Disabling `functions` also disables record hooks; disabling `access` leaves
  existing rules enforced with no API to change them; both are stated in the
  settings' help.
- Jobs of user-created schedules for a disabled module (for example a function
  schedule) stay queued until the module is on again.

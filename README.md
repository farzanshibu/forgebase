# ForgeBase

ForgeBase is a single-binary backend with an embedded PostgreSQL database, an
admin dashboard, and the APIs for auth, data, realtime, storage, jobs,
functions and billing. SQLite is the optional one-file alternative.

## What is in this repository

- `forgebase` binary: `init`, `serve`, `migrate`, `status`, `doctor`, `backup`,
  `restore`, `export`, `import`, `version`, `admin`, `functions`, `queues`,
  `jobs`, `db`, `node`, `project`, `deploy`, `new`.
- PostgreSQL 18 as the default database: the engine is embedded in the
  standard release, runs as a child of `forgebase serve` and listens on a Unix
  socket only (TCP is off; Windows, which has no socket, uses loopback TCP).
  Or point ForgeBase at your own PostgreSQL server.
- SQLite (one file, WAL mode, one writer and a read pool) when you pick it with
  `forgebase init --database sqlite`; it is the default of the `-lite` release,
  which leaves the PostgreSQL engine out.
- Checksummed system migrations on both engines, and pre-migration snapshots
  on SQLite and the bundled PostgreSQL.
- HTTP API: health probes, Prometheus metrics, `/api/system/*`, `/api/auth/*`,
  `/api/v1/*` (collections, KV, graph, vector, storage, jobs, realtime,
  credits, billing, customization) and the OpenAPI document at
  `/api/openapi.json`.
- Admin dashboard at `/admin/`, embedded in the binary.
- Redis protocol listener (off by default), TOML config and `FORGEBASE_*`
  environment variables, `.env` for development.
- JavaScript SDK in [`sdk/js`](sdk/js).
- Project as code in the CLI: `forgebase new`, `forgebase project pull|plan|apply|drift`
  and `forgebase deploy` keep collections, functions, queues, buckets and rules as files
  next to your app and apply them from CI ([docs/project-as-code.md](docs/project-as-code.md)).
- Terraform provider in [`integrations/terraform-provider-forgebase`](integrations/terraform-provider-forgebase): collections, storage, queues, functions, identity and plans as code.
- Kotlin SDK for Android and the JVM in [`sdk/kotlin`](sdk/kotlin).
- Swift SDK for iOS, macOS, tvOS, watchOS and visionOS in [`sdk/swift`](sdk/swift).
- Dart / Flutter SDK in [`sdk/dart`](sdk/dart).
- Docker image, Docker Compose file, a hardened systemd unit, and a release
  workflow for five targets.

Status of each work package, and what is still to do, is in
[docs/PLAN.md](docs/PLAN.md). The product spec is in [docs/SPEC.md](docs/SPEC.md).

## Documentation

- [Architecture](docs/architecture.md): crates and how a request flows.
- [API](docs/api.md): conventions, auth, errors, pagination, OpenAPI.
- [Database](docs/database.md): PostgreSQL and SQLite, multi-model data, migrations, backups.
- [Security](docs/security.md): threat model summary (draft) and open items.
- [Deployment](docs/deployment.md): binary, Docker, systemd, configuration, upgrades.
- [Observability](docs/observability.md): OpenTelemetry tracing, alert rules and notifiers, SLOs.
- [SDKs](docs/sdk.md): JavaScript SDK, and the status of the Rust SDK.
- [Architecture decisions](docs/adr/).

## Build

Requirements: Rust 1.97 or newer (`rust-version` in `Cargo.toml`), Node.js 22,
and npm. The dashboard is compiled into the binary, so build it first.

```sh
cd dashboard && npm ci && npm run build && cd ..
cargo build --release
```

The binary is `target/release/forgebase`. Without `dashboard/dist`, the build
still succeeds, but `/admin/` shows a "not built" page.

Checks used in CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo deny check
(cd dashboard && npm run lint && npm test)
```

## Quick start

```sh
./target/release/forgebase init          # config, data directory, migrated database
./target/release/forgebase serve         # listens on http://127.0.0.1:8090
```

Then open <http://127.0.0.1:8090/admin/> and check <http://127.0.0.1:8090/health>.
Stop the server with Ctrl-C (SIGTERM also works).

`init` writes `forgebase.toml` in the project directory. It does not overwrite an
existing file.

**Database.** The standard release uses PostgreSQL by default. On a terminal,
`init` asks `Database: [1] PostgreSQL (default, bundled server) [2] SQLite (one
file)`; scripts pass the choice:

```sh
forgebase init                      # PostgreSQL (the bundled server)
forgebase init --database sqlite    # one file, <data dir>/forgebase.db
forgebase init --database-url postgres://…   # a PostgreSQL server you run
```

PostgreSQL cannot run as root: run as an unprivileged user, or use
`--database sqlite`. The `-lite` release has no PostgreSQL engine and
defaults to SQLite (`forgebase --version` says `lite`). An existing SQLite
project stays SQLite after an upgrade; `forgebase db convert --to postgres`
moves it. See [Upgrading to 0.3](docs/release-notes/0.3.md#upgrading-to-03).

## Commands

Commands and global options, as `forgebase --help` lists them:

```text
Commands:
  init       Create a project: config file, data directory and migrated database
  serve      Start the server and serve until Ctrl-C / SIGTERM
  migrate    Apply pending system migrations
  status     Show project, database and server state
  doctor     Check configuration, permissions, disk, database integrity, migrations, indexes, storage blobs, master key, snapshots and port
  backup     Back up the data directory online (a snapshot, or a directory you name)
  restore    Restore a verified backup or snapshot into a stopped data directory
  export     Export collections, records or the configuration
  import     Import collections, records or a configuration file
  version    Print version information
  admin      Manage administrator (superuser) accounts
  functions  Deploy WebAssembly functions and read their logs on a running server
  queues     Publish to durable topics, pull from subscriptions and follow a topic on a running server
  jobs       Follow long-running jobs on a running server
  db         The database engine: status, and psql on the bundled PostgreSQL
  node       Run this host's PostgreSQL as a managed replica of a ForgeBase server (engine only: no API, no services)
  project    The server's configuration as code: pull, plan, apply, drift (ADR-0037)
  deploy     Build functions and apply the project directory to a server (plan + apply)
  new        Create an app project: the project-as-code layout, samples, seed data, a CI workflow
  help       Print this message or the help of the given subcommand(s)

Options:
      --config <FILE>        Config file [default: ./forgebase.toml if it exists] [env: FORGEBASE_CONFIG]
      --data-dir <DIR>       Data directory [default: forgebase-data] [env: FORGEBASE_DATA_DIR]
      --host <HOST>          Bind host [default: 127.0.0.1] [env: FORGEBASE_HOST]
      --port <PORT>          Bind port; 0 picks a free port [default: 8090] [env: FORGEBASE_PORT]
      --log-level <FILTER>   Log filter, e.g. `info` or `forgebase=debug` [env: FORGEBASE_LOG_LEVEL]
      --log-format <FORMAT>  Log format: pretty or json [env: FORGEBASE_LOG_FORMAT]
      --resp                 Also serve the Redis protocol (RESP2/RESP3) for the built-in KV engine [env: FORGEBASE_RESP_ENABLED]
      --resp-host <HOST>     RESP bind host [default: 127.0.0.1] [env: FORGEBASE_RESP_HOST]
      --resp-port <PORT>     RESP bind port; 0 picks a free port [default: 6380] [env: FORGEBASE_RESP_PORT]
      --billing              Mount billing (on by default; set the env var/config to false to turn off) [env: FORGEBASE_BILLING_ENABLED]
      --mcp                  Open the MCP server for AI agents at /api/mcp (off by default) [env: FORGEBASE_MCP_ENABLED]
  -h, --help                 Print help
  -V, --version              Print version
```

Subcommand options: `migrate --status` lists applied and pending migrations
without changing anything; `status --json`, `doctor --json` and
`version --json` print JSON. `doctor` exits with code 1 if any check fails.
Backup, restore, export and import are described in
[docs/database.md §5](docs/database.md#5-backups-and-rollback); `db`
(`status`, `psql`, `dump`, `convert`, `login`) in
[docs/deployment.md §3.5](docs/deployment.md#35-database-postgresql-or-sqlite).

Subcommand flags: `forgebase <command> --help`.

## Configuration

Precedence, highest first:

1. Command-line flags
2. `FORGEBASE_*` environment variables
3. Config file (`forgebase.toml`)
4. Built-in defaults

The config file is `--config`, else `FORGEBASE_CONFIG`, else `./forgebase.toml`
when it exists. A named file that is missing is an error. Unknown keys in the
file and unknown `FORGEBASE_*` variables are errors, so typos are not silent.

Every key with its default is in [forgebase.example.toml](forgebase.example.toml).

### `.env` for development

If `./.env` exists in the working directory, `forgebase` loads it at startup.
Variables already set in the process environment are never overridden. The
file is ignored in production; it is listed in `.gitignore` and
`.dockerignore`.

## Data directory layout

```text
<data_dir>/                       created 0700 on Unix
├── forgebase.backend             which engine this directory holds
├── forgebase.db (+ -wal/-shm)    SQLite only: all structured data; created 0600
├── postgres/                     bundled PostgreSQL only: engine/, data/ (the cluster), run/ (socket), roles.secret
├── topology.json                 PostgreSQL: database sets and nodes
├── storage/                      canonical: object blobs (when storage is used)
├── auth/master.key               root key for auth secrets; keep a copy apart from the backups
├── indexes/                      derived: rebuildable indexes
├── backups/                      pre-migration snapshots; snapshots/ of `forgebase backup`
├── tmp/                          staging for in-flight writes; safe to delete when stopped
└── .forgebase.lock               held by the running server
```

`forgebase backup` copies the database, `storage/` and `indexes/` while the
server runs: SQLite with `VACUUM INTO`, the bundled PostgreSQL with
`pg_basebackup`, an external server with `pg_dump`. `forgebase restore`
puts a verified backup back into a stopped data directory. `indexes/` can be
deleted and rebuilt. Only one server may use a data directory at a time.

Restoring a pre-migration snapshot from `backups/` is the rollback for a bad
migration (SQLite and the bundled PostgreSQL; take a backup before you
upgrade an external server). See
[docs/database.md §5](docs/database.md#5-backups-and-rollback).

## Docker

```sh
docker compose up --build
```

This starts ForgeBase on <http://127.0.0.1:8090> with data in the
`forgebase-data` volume. The container listens on all interfaces inside the
container; the compose file publishes the port on loopback only. Keep it that way unless
a reverse proxy with TLS sits in front ([docs/security.md](docs/security.md)), or turn on
native TLS with your certificate files or Let's Encrypt (`[tls]`, [docs/deployment.md §5](docs/deployment.md#5-tls)).

Without Compose:

```sh
docker build -t forgebase .
docker run --rm -p 127.0.0.1:8090:8090 -v forgebase-data:/data forgebase
```

The image runs as a non-root user (uid 10001), keeps data in `/data`, and has a
health check on `/health`.

## Linux service

[deploy/systemd/forgebase.service](deploy/systemd/forgebase.service) runs
ForgeBase under a dedicated user with state in `/var/lib/forgebase`. The header
of the file has the install steps.

## Kubernetes

A Helm chart ([deploy/helm/forgebase](deploy/helm/forgebase/README.md)) and Kustomize
manifests ([deploy/kubernetes](deploy/kubernetes/README.md)) run ForgeBase as one
StatefulSet pod on a PVC, with the bundled PostgreSQL, an external PostgreSQL
server or SQLite. See [docs/deployment.md §9](docs/deployment.md#9-kubernetes).

## Roadmap

The phases, scope and acceptance criteria are in [docs/PLAN.md](docs/PLAN.md).
Architecture decisions are in [docs/adr/](docs/adr/).

## Licence

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE))
- MIT license ([LICENSE-MIT](LICENSE-MIT))

at your option.

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in ForgeBase by you, as defined in the Apache-2.0 license, shall be
dual licensed as above, without any additional terms or conditions.

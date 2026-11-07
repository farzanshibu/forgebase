# ForgeBase

ForgeBase is a single-binary backend with an embedded database and an admin
dashboard. This repository is **Phase 1 (core foundation)** only.

## What exists today (Phase 1)

- `forgebase` binary: `init`, `serve`, `migrate`, `status`, `doctor`, `version`.
- Embedded SQLite database in WAL mode, one writer and a read pool, with
  versioned, checksummed system migrations and pre-migration snapshots.
- HTTP server with health probes (`/health`, `/health/ready`), Prometheus
  metrics (`/metrics`), system endpoints under `/api/system/*`, and a
  structured error envelope with request ids.
- Admin dashboard at `/admin/`, embedded in the binary.
- TOML config and `FORGEBASE_*` environment variables, `.env` for development.
- Docker image, Docker Compose file, and a hardened systemd unit.

Not yet built: the data APIs (collections, records), authentication,
authorization, object storage, vector and graph engines, realtime, functions,
jobs, billing, and clustering. The plan for these is in
[docs/PLAN.md](docs/PLAN.md); the full product spec is in
[docs/SPEC.md](docs/SPEC.md).

## Build

Requirements: Rust 1.94 or newer (`rust-version` in `Cargo.toml`), Node.js 22,
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

## Commands

Output of `forgebase --help`:

```text
ForgeBase: a single-binary backend with an embedded database

Usage: forgebase [OPTIONS] <COMMAND>

Commands:
  init     Create a project: config file, data directory and migrated database
  serve    Start the server and serve until Ctrl-C / SIGTERM
  migrate  Apply pending system migrations
  status   Show project, database and server state
  doctor   Check database integrity, disk space, permissions, migrations and port
  version  Print version information
  help     Print this message or the help of the given subcommand(s)

Options:
      --config <FILE>        Config file [default: ./forgebase.toml if it exists] [env: FORGEBASE_CONFIG]
      --data-dir <DIR>       Data directory [default: forgebase-data] [env: FORGEBASE_DATA_DIR]
      --host <HOST>          Bind host [default: 127.0.0.1] [env: FORGEBASE_HOST]
      --port <PORT>          Bind port; 0 picks a free port [default: 8090] [env: FORGEBASE_PORT]
      --log-level <FILTER>   Log filter, e.g. `info` or `forgebase=debug` [env: FORGEBASE_LOG_LEVEL]
      --log-format <FORMAT>  Log format: pretty or json [env: FORGEBASE_LOG_FORMAT]
  -h, --help                 Print help
  -V, --version              Print version
```

Subcommand options: `migrate --status` lists applied and pending migrations
without changing anything; `status --json`, `doctor --json` and
`version --json` print JSON. `doctor` exits with code 1 if any check fails.

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
<data_dir>/                    created 0700 on Unix
├── forgebase.db (+ -wal/-shm) canonical: all structured data; created 0600
├── storage/                   canonical: object blobs (later phase)
├── indexes/                   derived: rebuildable indexes (later phase)
├── backups/                   pre-migration snapshots
├── tmp/                       staging for in-flight writes; safe to delete when stopped
└── .forgebase.lock            held by the running server
```

Back up the database file and `storage/` (when it exists). `indexes/` can be
deleted and rebuilt. Only one server may use a data directory at a time.

Restoring a pre-migration snapshot from `backups/` is the rollback for a bad
migration. Full backup and restore commands come later.

## Docker

```sh
docker compose up --build
```

This starts ForgeBase on <http://127.0.0.1:8090> with data in the
`forgebase-data` volume. The container listens on all interfaces inside the
container; the compose file publishes the port on loopback only, because admin
endpoints have no login yet.

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

# ADR-0028: Bundled PostgreSQL backend

- Status: proposed
- Date: 2026-11-11
- Work package: FB-95 (design); build packages FB-130 onward in
  [`docs/requirements/postgres-plan.md`](../requirements/postgres-plan.md).
- Scope: where the PostgreSQL and pgvector binaries come from and how they
  ship, how ForgeBase runs and upgrades the database server, backups,
  choosing the backend, moving a project from SQLite to PostgreSQL, the Rust
  side (`Backend`/`Dialect`, system migrations, tests), and what users get.
  Row-level security per service, and compiling ForgeBase rules to native
  PostgreSQL policies, are in
  [`docs/requirements/rls-coverage.md`](../requirements/rls-coverage.md).
- Builds on: ADR-0001 §2–4 (storage abstraction, single writer, data
  directory, migrations), ADR-0003 §2 (`Backend`, `WriteTx`, `Dialect`),
  ADR-0008 (policy engine), ADR-0011 and ADR-0022 (commit-ordered cursors),
  ADR-0024 (backups).

## Context

The human asked for two things (2026-11-11):

1. Real PostgreSQL, bundled: ForgeBase ships the PostgreSQL server binaries
   and starts, stops and upgrades them itself. Nobody installs PostgreSQL.
   Embedded SQLite stays the light default.
2. Proper row-level security on every service, "Supabase-like, everything
   available".

What exists today (read from the code on 2026-11-11):

- Only `forgebase-db` and `forgebase-migrations` import `sqlx`, built with
  the `sqlite` feature only. Every other crate talks to the database through
  `Arc<dyn Backend>` and `Statement { sql, params: Vec<Value> }`
  (ADR-0003 §2). That boundary held: no driver type leaks out.
- The SQL text itself is SQLite SQL, written by hand in each crate. About
  3 000 placeholders use SQLite's `?N` form (auth 740, billing 734,
  entitlements 308, jobs 225, queue 214, policy 148, credits 153, customize
  122, graph 92, data 70, functions 62, vector 50), plus about 40 bare `?`
  (storage).
- SQLite-only constructs in Rust code, by crate: data core catalog
  introspection (`sqlite_master`, `PRAGMA table_info/index_list/index_xinfo`,
  `sqlite_stat1`, `dbstat`), `STRICT` tables, table rebuilds with `rowid`
  and `legacy_alter_table`, FTS5 indexes and their triggers, `EXPLAIN`
  opcode inspection (`forgebase-data`, `forgebase-query`); `json_each` used
  to pass id lists (graph 9, vector 5, auth 1, data 2); `json_set`,
  `json_insert`, `json_remove` on delivery history (queue); `ORDER BY rowid`
  tie-breaks (jobs 8); `INSERT OR IGNORE` / `INSERT OR REPLACE` (auth,
  policy, jobs, storage, customize: 10); `last_insert_id` (queue, credits,
  SQL editor); a recursive CTE with `LIMIT` inside and two references to
  the recursive table (graph walk), which PostgreSQL rejects.
- 25 system migrations (`0001`–`0025`, no `0010`), 2 711 lines: 138
  `STRICT` tables, 66 `json_valid` checks, 22 triggers using
  `RAISE(ABORT, …)` (immutability of billing audit, credits ledger,
  entitlements, customize, vector), 19 `AUTOINCREMENT`, 7 `WITHOUT ROWID`,
  13 `unixepoch()`, 4 `COLLATE NOCASE` columns (admin email, service
  account name, collection name, invitation email).
- The concrete `forgebase_db::Database` type (SQLite) is used directly by
  `forgebase-api`, `-backup`, `-bench`, `-cli` and `-server` for open,
  migrate, checkpoint, snapshot and integrity checks.
- About 900 tests; 22 crates open their database through their own
  `tests/common` helper, plus 4 files in `forgebase-api/tests` and the
  black-box suites in `tests/integration`.
- SPEC §2: "a single self-contained executable"; "usable without installing
  PostgreSQL … for its default embedded experience"; §4.1: "a storage
  abstraction that can support PostgreSQL … without forcing the entire
  codebase to depend on PostgreSQL".

## Decisions at a glance

| # | Decision |
|---|---|
| 1 | Two backends behind the existing traits: `sqlite` (default, unchanged) and `postgres`. PostgreSQL runs either **managed** (ForgeBase's bundled engine) or **external** (a URL to any PostgreSQL ≥ 16, for clusters and hosted databases). |
| 2 | One PostgreSQL major per ForgeBase release line, **PostgreSQL 18** at the start, with **pgvector** and a fixed set of contrib extensions. |
| 3 | The engine is **built by ForgeBase CI from source** (pinned tarball SHA-256), per release target, into a relocatable archive. Prebuilt third-party binaries are used only to unblock development (§2). |
| 4 | The engine archive is **embedded in the `forgebase` binary** behind the cargo feature `bundled-postgres` (on in release builds) and extracted, verified, into the data directory on first use. A `-lite` archive without it is also published. |
| 5 | ForgeBase **supervises the `postgres` process** as a child: `initdb` on first start, Unix socket only by default, fast shutdown with the server, restart on crash, readiness gate. |
| 6 | **Parity first**: the PostgreSQL backend keeps ADR-0001's single writer, so commit order equals sequence order (realtime and queue cursors) and every read-modify-write stays correct. Lifting it is a separate, audited package. |
| 7 | **Placeholders are translated, not rewritten**: the PostgreSQL backend turns `?N` and `?` into `$N` when it prepares a statement. The ~3 000 call sites do not change. |
| 8 | System migrations get a **PostgreSQL twin**: one hand-reviewed baseline equal to SQLite version 25, then two files per migration from `0026` on, checked by a schema-parity test. |
| 9 | Backups use **`pg_basebackup`** inside the existing backup directory format; restore, snapshots and `doctor` keep their contracts. WAL archiving adds point-in-time recovery later. |
| 10 | `forgebase init --database sqlite|postgres`; `forgebase db convert --to postgres` moves an existing project with a verified table-by-table copy. A data directory records its backend and refuses to start with the other one. |
| 11 | The whole test suite runs on both backends in CI. |
| 12 | ForgeBase rules stay the single source of truth. On PostgreSQL they are **also compiled to native RLS policies** (defence in depth for the API, and enforcement for direct `psql` and driver logins). See `rls-coverage.md` §3. |

## 1. Versions and contents of the engine

- **PostgreSQL 18** (18.6 today; supported until 2030-11-14). PostgreSQL 19
  reaches general availability on 2026-11-19 (planned); ForgeBase moves to it
  after its first minor release, through the major-upgrade path of §4.6,
  which that move also proves. Community support is five years per major, so
  one major per ForgeBase release line is enough. External mode accepts
  PostgreSQL 16 or later (16 is the oldest supported major with the `IS
  JSON` predicate; 14 is end of life on 2026-11-12).
- **pgvector** 0.8.x (0.8.7 today; pinned per engine build).
- **Contrib extensions shipped**, each loadable with `CREATE EXTENSION` from
  the allow list (rls-coverage §3.6): `citext` (needed by the schema, §7.4),
  `pgcrypto`, `pg_trgm`, `unaccent`, `fuzzystrmatch`, `btree_gin`,
  `btree_gist`, `hstore`, `ltree`, `intarray`, `tablefunc`,
  `pg_stat_statements` (loaded through `shared_preload_libraries`, so the
  Database Studio can show query statistics), `amcheck` (used by `doctor`).
  `uuid-ossp` is left out (it needs an extra C library; the built-in
  `gen_random_uuid()` covers it).
- **Not shipped**: PostGIS (pulls GEOS, PROJ, GDAL: hundreds of MB and C++
  toolchains on five targets), TimescaleDB (Timescale License for the
  useful parts), `pg_cron` (ForgeBase Automations do this), procedural
  languages other than PL/pgSQL (PL/Python and PL/Perl need an interpreter
  on the host). Users who need them use external mode.
- **Programs shipped**: `postgres`, `initdb`, `pg_ctl` (Windows stop and
  promote only), `pg_basebackup`, `pg_verifybackup`, `pg_dump`,
  `pg_restore`, `pg_upgrade`, `pg_controldata`, `pg_isready`, `psql`.
  Everything else in `bin/` is dropped from the archive.

## 2. Getting the binaries

### 2.1 What exists (checked 2026-11-11)

| Source | Targets | Linux libc | pgvector | Size (18.x, x86_64) | Integrity |
|---|---|---|---|---|---|
| theseus-rs/postgresql-binaries (used by the `postgresql_embedded` crate and the Go `embedded-postgres` v2) | all five ForgeBase targets | glibc 2.36+ or **dynamic** musl 1.2.4 (needs the musl loader on the host) | no | 103 MB `.tar.gz`, ~270 MB unpacked (LLVM 160 MB, ICU 34 MB, Python 23 MB) | `.sha256`, SPDX SBOM, build provenance attestations |
| zonky embedded-postgres-binaries (Maven) | Linux, Alpine, macOS, Windows; amd64, arm64 … | glibc or dynamic musl (`$ORIGIN/../lib`) | no | ~15 MB jar (Linux) | PGP `.asc` + SHA-256 on Maven Central |
| EnterpriseDB zips | Windows x86_64, macOS only | n/a | no | 344 MB (Windows), mostly pgAdmin | none found |
| Distribution packages (Alpine, Debian, Homebrew) | per OS | per OS | as separate packages | n/a | per distribution |

Findings that decide the design:

- **No project ships pgvector**, so ForgeBase builds pgvector itself whatever
  it picks.
- **A static PostgreSQL is not possible**: PostgreSQL loads PL/pgSQL, text
  search dictionaries, replication modules and every extension with
  `dlopen`, and `dlopen` in a static musl binary is a stub. Every Linux
  option is dynamically linked.
- theseus's glibc build needs glibc 2.36, which excludes Ubuntu 22.04 (2.35)
  and RHEL 9 (2.34); its musl build needs a musl loader that glibc distros
  do not have. Its archives are large because they include LLVM (JIT), ICU
  and PL/Python.
- PostgreSQL 18 is current (18.6, supported to 2030-11-14). PostgreSQL 19
  general availability is planned for 2026-11-19; 14 reaches end of life on
  2026-11-12.

### 2.2 Decision: build the engine in ForgeBase CI

A new workflow, `.github/workflows/postgres-engine.yml`, builds one archive
per target from source, on demand (not on every push), and publishes it as
a GitHub release `engine-pg<version>-r<n>` with `SHA256SUMS` and GitHub
build-provenance attestations:

- **Sources**: `postgresql-<version>.tar.bz2` from ftp.postgresql.org and the
  pgvector tag tarball, both pinned by SHA-256 in `engine.lock` (checked into
  the repository). Dependencies (OpenSSL 3, zlib, lz4, zstd, libedit) are
  pinned the same way and linked statically into the engine where the
  platform allows.
- **Configure**: `--without-icu --without-llvm --without-perl
  --without-python --without-tcl --disable-nls --with-ssl=openssl --with-lz4
  --with-zstd --with-libedit-preferred` (libedit, not GPL readline, for
  `psql` line editing), `--disable-rpath`. pgvector with `make
  OPTFLAGS=""` (its default `-march=native` would produce binaries that
  crash on older CPUs). Windows uses PostgreSQL's meson build with MSVC and
  pgvector's `Makefile.win`.
- **Contents**: the programs and extensions of §1, `share/` (timezone data,
  extension SQL), and `share/licenses/` (§2.4). Everything else is dropped.
- **Expected size** (from summing the theseus 18.6 manifest without LLVM,
  ICU and Python; FB-130 replaces these with measured numbers): about
  14 MB of programs (`postgres` 11.4 MB), 6 MB of modules, 2 MB of `share/`,
  5 MB of OpenSSL: **30–35 MB unpacked, an estimated 10–14 MB compressed with
  zstd** per target.
- **Smoke test** in the same workflow, per target: extract, relocate,
  `initdb`, start, `CREATE EXTENSION vector`, a vector query, `pg_dump`,
  `pg_basebackup`, stop. Linux archives are additionally started on Debian
  12, Ubuntu 22.04, RHEL 9 (UBI), Alpine 3.20 and distroless containers.

Why not theseus prebuilt binaries: 7–10 times larger, glibc 2.36 floor or a
host musl loader, no pgvector, and a third party in the supply chain of
every ForgeBase release. They stay useful for development: the engine
crate accepts any PostgreSQL 18 directory with pgvector through
`FORGEBASE_PG_ENGINE_DIR` (a theseus archive plus a locally built pgvector,
Homebrew, or apt), so Rust work does not wait for FB-130.

### 2.3 Making Linux binaries run everywhere

The Linux engines are built on Alpine (musl) and ship musl's dynamic loader,
which is also its C library (`lib/ld-musl-<arch>.so.1`, about 0.7 MB, MIT).
The problem is that an ELF executable names its loader by absolute path
(`PT_INTERP`), and the extraction path is only known at run time.

- At build time every executable is linked with
  `-Wl,--dynamic-linker=/forgebase-pg-loader-<padding to 255 bytes>`.
- At extraction ForgeBase overwrites that NUL-terminated string in the
  `PT_INTERP` segment of each file in `bin/` with the absolute path of the
  extracted loader. The same technique conda uses for relocation; the edit is
  in place, needs no tool, and is about 100 lines of safe Rust (program-header
  parsing with the `object` crate). Libraries are found through
  `RUNPATH=$ORIGIN/../lib`. PostgreSQL computes `share/` and `lib/`
  relative to its own executable when moved, so nothing else needs patching.
- If the absolute loader path is longer than 254 bytes (a very deep data
  directory), ForgeBase extracts the engine under `$HOME/.cache/forgebase/`
  instead and says so.
- **Fallback** if the FB-130 spike shows this does not work on every target
  host: build against glibc 2.28 (manylinux_2_28 image; RHEL 8+, Debian 10+,
  Ubuntu 20.04+) with `$ORIGIN` RUNPATH, and document that Alpine hosts use
  the ForgeBase Docker image or external mode.

macOS engines use `@loader_path/../lib` install names and are ad-hoc signed
again after `install_name_tool` (Apple silicon refuses unsigned code). The
x86_64 macOS engine is cross-compiled on the arm64 runner (`clang -arch
x86_64`); configure checks run under Rosetta 2. Windows engines need nothing:
DLLs sit next to the executables.

### 2.4 Licences

| Component | Licence | Obligation |
|---|---|---|
| PostgreSQL | PostgreSQL License (permissive, BSD/MIT-like) | keep the copyright notice and the licence paragraphs in all copies |
| pgvector | PostgreSQL License | same |
| OpenSSL 3 | Apache-2.0 | include the licence; NOTICE if present |
| zlib | zlib | do not misrepresent origin |
| lz4 (library) | BSD-2-Clause | notice |
| zstd | BSD-3-Clause (dual BSD/GPLv2; ForgeBase uses BSD) | notice |
| libedit | BSD-3-Clause | notice |
| musl (Linux loader) | MIT | notice |

All are compatible with ForgeBase's MIT OR Apache-2.0. No GPL component is
shipped (readline, ICU's licence and LLVM are avoided by configuration).
The archive carries `share/licenses/<component>.txt`; the release archive
and `forgebase licenses` include them. `cargo deny` does not see C
components, so FB-130 adds `engine.lock` to the release checklist and a CI
check that every component listed in it has a licence file.

## 3. Shipping them

### 3.1 Decision: embedded in the binary

The engine archive is compiled into `forgebase` with `include_bytes!` when
the cargo feature `bundled-postgres` is on:

- `build.rs` of the new `forgebase-pgengine` crate reads
  `FORGEBASE_PG_ENGINE_ARCHIVE` (path to the target's archive), checks its
  SHA-256 against `engine.lock`, and embeds it. Without the variable the
  feature fails the build with a clear message; without the feature nothing
  is embedded and `--database postgres` explains why (§6.2). Builds never
  download anything (the hive and CI build `--offline`); the release
  workflow downloads the pinned engine release before `cargo build`.
- Release builds turn the feature on for all five targets. A second archive
  per target, `forgebase-<version>-<target>-lite`, is built without it.
- On first use ForgeBase extracts the archive (`zstd` + `tar` crates, both
  already in the local registry cache, so offline builds keep working)
  into `postgres/engine/<version>-<sha12>/` as `*.partial`, verifies every
  file against the manifest inside the archive, relocates (§2.3), marks files
  read-only, and renames. A crash leaves only a `.partial` directory that the
  next start deletes.

Why embedded rather than next to the binary in the archive: SPEC §2 makes a
single self-contained executable a critical requirement, the Docker image
and every install path already copy just one file, and an engine that
travels inside the binary can never be missing or mismatched. The price is
download size for people who stay on SQLite, which the `-lite` archive
answers.

### 3.2 Size impact

Estimates until FB-130 measures them:

| Target | Added to the release binary (compressed engine) | Unpacked per project (first PostgreSQL start) |
|---|---|---|
| x86_64-unknown-linux-musl | +10–14 MB | 30–35 MB |
| aarch64-unknown-linux-musl | +10–14 MB | 30–35 MB |
| x86_64-apple-darwin | +10–14 MB | 30–35 MB |
| aarch64-apple-darwin | +10–14 MB | 30–35 MB |
| x86_64-pc-windows-msvc | +12–16 MB | 35–45 MB (OpenSSL and compression DLLs) |

SQLite projects never extract anything. A shared `engine_dir` (§4.1) makes
the unpacked cost once per machine instead of once per project.

## 4. Running the engine

### 4.1 Layout

```text
<data_dir>/
├── forgebase.backend            "postgres-managed", "postgres-external" or "sqlite" (absent = sqlite)
├── postgres/
│   ├── engine/18.1-r1-<sha12>/  extracted engine, read-only files; bin/ lib/ share/
│   ├── data/                    PGDATA, mode 0700, owned by the user running ForgeBase
│   ├── run/                     Unix socket directory, mode 0700
│   ├── roles.secret             generated passwords of the bootstrap roles, mode 0600
│   └── forgebase-postgres.conf  written by ForgeBase on every start (included from postgresql.conf)
├── storage/  indexes/  backups/  tmp/  auth/master.key   unchanged (ADR-0001, ADR-0024)
└── .forgebase.lock
```

The engine lives **inside the data directory**, not in a shared cache. A
data directory therefore always has the exact binaries that wrote it, which
is what a major upgrade (§4.6) and a restore on another machine need. The
cost is about one unpacked engine per project (§3.2). Backups skip
`postgres/engine/` (it is re-extracted from the binary). With
`[database.postgres] engine_dir` an operator can point several projects at
one shared, read-only engine directory instead.

### 4.2 First start (`initdb`)

ForgeBase runs, with the extracted engine:

```text
initdb -D postgres/data -U forgebase_super --pwfile=<0600 temp file>
       --auth-local=scram-sha-256 --auth-host=scram-sha-256
       -E UTF8 --locale-provider=builtin --builtin-locale=C.UTF-8
       --data-checksums
```

- The `builtin` C.UTF-8 locale (PostgreSQL 17+) sorts by code point, the
  same order as SQLite's default `BINARY` collation, and does not depend on
  the host's libc or ICU. Sort and cursor results therefore match between
  backends and between machines.
- Bootstrap roles, created by `forgebase_super` right after `initdb`
  (rls-coverage §3.2 has the full model): `forgebase_owner` (NOLOGIN, owns
  every schema and table), `forgebase_server` (LOGIN, `BYPASSRLS`, the
  server's writer pool, member of `forgebase_owner`, not a superuser),
  `forgebase_reader` (LOGIN, `BYPASSRLS`, `SELECT` only,
  `default_transaction_read_only = on`; the read pool and read-only SQL),
  plus the NOLOGIN roles native RLS uses. Passwords are
  32 random bytes, stored in `postgres/roles.secret` (0600), never logged.
- `postgres` refuses to run as root. ForgeBase refuses `managed` mode as root
  with a clear message (`run as an unprivileged user, or use
  [database.postgres] mode = "external"`). The Docker image already runs as
  the `forgebase` user.

### 4.3 Start, stop and crash restart

- **Start**: the server starts the engine before `Database::open`, in this
  order: take `.forgebase.lock`; check `postgres/data/postmaster.pid` (a
  postmaster left behind by a killed server is stopped with a fast shutdown
  and restarted; ForgeBase holds the lock, so nothing else can own it);
  write `forgebase-postgres.conf`; spawn `postgres -D postgres/data` directly
  (not through `pg_ctl`, so ForgeBase owns the process and its output); wait
  for readiness (connect loop, 30 s, then fail startup with the engine's
  last log lines); open the pools; run migrations.
- **Listening**: by default `listen_addresses = ''` and
  `unix_socket_directories = '<data_dir>/postgres/run'`, so nothing on the
  network can reach the database. If that path is longer than the
  platform's socket path limit (103 bytes on macOS, 107 on Linux), ForgeBase uses `$TMPDIR/forgebase-<12 hex of the data
  dir path>/` (mode 0700). On Windows the engine listens on `127.0.0.1` on a
  free port chosen at start, with SCRAM passwords. Direct logins from other
  hosts are an explicit opt-in (`[database.postgres] listen`, TLS required
  off loopback; rls-coverage §3.5).
- **Logs**: `log_destination = stderr`; ForgeBase reads the child's stderr
  line by line into `tracing` (target `forgebase::postgres`), so engine logs
  share the server's format and level filter.
- **Stop**: on graceful shutdown ForgeBase closes its pools, sends `SIGINT`
  (fast shutdown: rolls back open transactions, checkpoints), waits 30 s,
  then `SIGQUIT`. On Windows it runs `pg_ctl stop -m fast`.
- **Parent death**: on Linux the child gets `PR_SET_PDEATHSIG = SIGINT`
  through `rustix` (safe API; the workspace forbids `unsafe`). On Windows the
  child is put in a job object with kill-on-close. On macOS there is no
  equivalent; the stale-postmaster check above covers it.
- **Crash restart**: PostgreSQL itself restarts crashed backends and
  recovers. If the postmaster exits, ForgeBase restarts it with backoff
  (1 s, 2 s, 4 s … 30 s; after 5 failures in 10 minutes it stops trying and
  `/health/ready` reports `database: failed`). While it is down, requests
  get `503 database_unavailable`; pools reconnect on their own.
- **Settings ForgeBase writes** (`forgebase-postgres.conf`, overridable in
  `[database.postgres.settings]`, a map of PostgreSQL parameters with a deny
  list for the ones ForgeBase owns: `listen_addresses`, `port`,
  `unix_socket_directories`, `data_directory`, `hba_file`,
  `shared_preload_libraries`): `shared_buffers = 128MB`,
  `max_connections = 64`, `wal_level = replica`, `synchronous_commit = on`
  (matches SQLite `synchronous = full`, SPEC §20), `log_min_duration_statement`
  from the slow-query threshold, `shared_preload_libraries =
  'pg_stat_statements'`.

### 4.4 Resource cost

Measured numbers come from FB-130; expected orders of magnitude: idle RSS of
the postmaster and its background workers 30–60 MB plus `shared_buffers`;
`initdb` 1–3 s once; engine start 0.3–1 s per server start. Docker's
default 64 MB `/dev/shm` is enough for these defaults; the compose example
documents `shm_size` for larger `shared_buffers`.

### 4.5 Minor upgrades

A ForgeBase release with a newer minor (18.1 → 18.3) extracts the new engine
next to the old one, starts the cluster with it (minors share the on-disk
format), and deletes the old engine directory after a clean start.

### 4.6 Major upgrades

- ForgeBase never upgrades a major on its own. A release that moves to a new
  major still starts existing clusters on the major they were created with,
  using the engine already in their data directory (§4.1). ForgeBase CI
  tests the current and the previous major for this reason.
- `forgebase db upgrade-engine` (server stopped): takes a backup (§5),
  runs `initdb` for the new major, then `pg_upgrade --link` (falls back to
  `--copy` across file systems), runs `ANALYZE`, and swaps the data
  directories with the same journal as restore (ADR-0024 §4). The old data
  stays in `backups/pre-upgrade-<time>/` until removed.
- Extensions: pgvector must exist for both majors; `pg_upgrade` checks it.
- External mode: upgrades are the operator's job; ForgeBase only checks the
  server version at start (≥ 16) and refuses an unsupported one.

## 5. Backups

ADR-0024's contract stays: a backup is a directory with `MANIFEST.json`
written last, built as `<dest>.partial`; restore is offline, journalled and
reversible; snapshots and retention are unchanged.

| | SQLite (today) | PostgreSQL managed |
|---|---|---|
| Database copy | `VACUUM INTO forgebase.db` | `pg_basebackup -D postgres/ -Fp -X stream --checkpoint=fast --manifest-checksums=SHA256`, as `forgebase_super` over the socket |
| Online | yes | yes (no lock, the server keeps running) |
| Verify | `integrity_check` on the copy | `pg_verifybackup`, plus the manifest hashes |
| Restore | swap `forgebase.db` | swap `postgres/data` (same journal); WAL replay on first start |
| Across majors | n/a | no: a physical backup restores on the same major only; the manifest records `engine: postgres 18.1`, and restore refuses a different major |
| Size | data size | data size plus WAL written during the copy |

- **Logical exports** (`forgebase export`/`import`, ADR-0024 §6) already go
  through `DataService` and work on both backends unchanged. `forgebase db
  dump` additionally runs `pg_dump -Fc` for a plain PostgreSQL dump that any
  PostgreSQL can restore.
- **Point-in-time recovery** (later package): `archive_mode = on` with
  `archive_command` copying WAL segments to `backups/wal/`; `restore --to
  <time>` replays from the newest base backup before that time. SQLite cannot
  offer this; it is a real gain of the PostgreSQL backend.
- **External mode**: `forgebase backup` makes a logical backup only
  (`pg_dump` of the ForgeBase schemas plus storage and indexes); physical
  backups and PITR are the hosting provider's job.

## 6. Choosing the backend and moving a project

### 6.1 Configuration

```toml
[database]
backend = "sqlite"             # "sqlite" (default) | "postgres"

[database.postgres]
mode = "managed"               # "managed" (bundled engine) | "external"
url = ""                       # external only; prefer FORGEBASE_DATABASE_URL (a secret)
pool_size = 16                 # read pool; the writer stays one connection (§7.3)
listen = ""                    # managed: "" = Unix socket only; "127.0.0.1:5433" enables TCP logins
engine_dir = ""                # managed: shared engine directory instead of <data_dir>/postgres/engine
native_rls = "logins"          # "off" | "logins" | "enforce" (rls-coverage §3.1)

[database.postgres.settings]   # extra postgresql.conf parameters (deny list in §4.3)
```

The SQLite keys (`synchronous`, `busy_timeout_ms`, `read_pool_size`) keep
their meaning for `sqlite`. Environment variables follow the existing
pattern (`FORGEBASE_DATABASE_BACKEND`, `FORGEBASE_DATABASE_POSTGRES_MODE`,
`FORGEBASE_DATABASE_URL`, …). All keys are registered in the FB-91 feature
registry (`database` is always on; `backend` and `mode` are
`restart_required` and not editable from the dashboard, since changing them
needs `db convert`).

### 6.2 `forgebase init`

- `forgebase init [DIR] --database sqlite|postgres` (default `sqlite`).
  `postgres` means managed; `--database-url URL` means external (the URL is
  written to `.env`, not to `forgebase.toml`).
- On a terminal, `init` asks `Database: [1] SQLite (default, one file)
  [2] PostgreSQL (bundled server)` after FB-90's admin prompt contract;
  non-interactive runs use the flag only.
- `init --database postgres` runs `initdb`, starts the engine once, applies
  migrations, creates the administrator (FB-90), and stops it.
- A binary built without `bundled-postgres` answers `--database postgres`
  with `this build has no bundled PostgreSQL; use --database-url or the
  standard release`.

### 6.3 The backend marker

`<data_dir>/forgebase.backend` is written by `init` and `db convert`. At
start the server compares it with the configuration and refuses a mismatch
(`database.backend is postgres, but this data directory holds a sqlite
database; run forgebase db convert`). This stops an edited config from
silently starting an empty PostgreSQL next to a full SQLite project.

### 6.4 `forgebase db convert --to postgres` (and back)

The existing export/import is not a full copy: it skips auth users, policy,
billing and every other system table, and reassigns timestamps (ADR-0024
§6). Moving a project needs a table-level copy:

1. Server stopped; take the data-directory lock; take a backup (§5).
2. Run SQLite migrations to the newest version; refuse a source that is not
   at the binary's newest version after that.
3. `initdb` (managed) or connect (external, empty target database required),
   apply the PostgreSQL migrations to the same version.
4. Copy every table (system tables, then collection tables in relation
   order) with `COPY … FROM STDIN` in one transaction per table group,
   converting types (§7.4): JSON text → `jsonb`, `BLOB` → `bytea`, SQLite
   `rowid` → `_rowid`; then `setval` every identity sequence to its maximum.
   Catalog-driven: the tool reads the table list from the PostgreSQL schema,
   so a table missing on either side fails the run.
5. Recreate collection tables' managed, automatic and full-text indexes from
   `_forgebase_indexes`; raw-SQL indexes made in the SQL editor (origin
   `sql`) are listed in the report for the administrator to recreate,
   because their SQLite SQL may not parse on PostgreSQL.
6. Verify: per table, the row count and a SHA-256 over the rows in primary
   key order in a canonical encoding must match. Any mismatch rolls back.
7. Write `forgebase.backend` and `[database] backend = "postgres"`; rename
   `forgebase.db` to `forgebase.db.pre-postgres` (kept, never deleted).

`--to sqlite` runs the same steps the other way, for taking a project back
to a single file. It refuses databases using PostgreSQL-only objects
(native policies written by hand, PL/pgSQL functions, extension types) and
lists them.

## 7. The Rust side

### 7.1 Driver

`sqlx` with the `postgres` feature added to the existing workspace entry
(`runtime-tokio`, `sqlite`, `postgres`), using `rustls` for TLS to external
servers. One driver family keeps one error and pool model, and `sqlx-postgres`
is pure Rust (no libpq). It is only compiled into `forgebase-db` and
`forgebase-migrations`, as today.

### 7.2 `Database` becomes a facade

```rust
pub enum DbTarget { Sqlite { path: PathBuf }, Postgres { url: SecretString, managed: bool } }
pub struct DbOptions { pub target: DbTarget, /* existing fields */ }

pub struct Database(Arc<Inner>);          // same name, same methods for callers
enum Inner { Sqlite(SqliteDatabase), Postgres(PgDatabase) }

impl Backend for Database { /* delegates */ }
impl Database {
    pub fn kind(&self) -> BackendKind;    // new
    // open, migrate, migration_status, schema_version, ping, pool_stats, info,
    // integrity_check, checkpoint, snapshot_to, close, meta_*, ensure_instance:
    // same signatures; PostgreSQL versions:
    //   checkpoint     -> CHECKPOINT (no-op for external)
    //   snapshot_to    -> refused; backups go through forgebase-backup's pg path (§5)
    //   integrity_check-> amcheck (bt_index_check on every index) + FK check
    //   info           -> version, size (pg_database_size), connections, WAL size
}
```

Callers in `forgebase-api`, `-backup`, `-bench`, `-cli`, `-server` keep
compiling; the few SQLite-only calls (`effective_pragmas`,
`sqlite_library_version`, `snapshot_to`) return `DbError::Unsupported` on
PostgreSQL and their callers branch on `kind()`. The engine supervisor of §4
is a new crate, `forgebase-pgengine`, used by `forgebase-server` and
`forgebase-cli` only; `forgebase-db` takes a URL and knows nothing about
processes.

### 7.3 `PgDatabase`

- **Pools**: one writer pool of exactly **one** connection
  (`forgebase_server`), and a read pool (`forgebase_reader`, read-only
  transactions). `begin(TxMode::Normal)` → `BEGIN ISOLATION LEVEL READ
  COMMITTED` on the writer; with one writer this behaves like SQLite's
  `BEGIN IMMEDIATE` (decision 6). `TxMode::SchemaChange` → `BEGIN` plus
  `SET CONSTRAINTS ALL DEFERRED` (foreign keys are created `DEFERRABLE
  INITIALLY IMMEDIATE`), which replaces SQLite's `foreign_keys = OFF` +
  `foreign_key_check`: violations surface at `COMMIT` and abort it.
- **Why keep one writer**: realtime and queue cursors rely on "sequence order
  is commit order" (ADR-0011, ADR-0022); dozens of services do
  read-then-write inside one transaction (counters, claims, uniqueness
  checks, ledger balances). Under PostgreSQL's concurrent writers these need
  `SELECT … FOR UPDATE`, `SKIP LOCKED`, or serialisable retries, one by one.
  That is FB-157, after the suite is green on PostgreSQL.
- **Drop without commit**: the connection is returned with `ROLLBACK`; if
  that fails, it is closed (same guarantee as ADR-0003 §2).
- **Placeholders**: before preparing, a small lexer (aware of `'…'`,
  `"…"`, `E'…'`, `$tag$…$tag$` and comments) rewrites `?N` to `$N` and a
  bare `?` to the next number, following SQLite's numbering rule. The result
  is cached per SQL string. PostgreSQL's `?`-family JSON operators are never
  used in ForgeBase SQL (they would be ambiguous); the dialect renders
  `jsonb_exists` instead.
- **Binding**: `Value` has no type information, but PostgreSQL is strictly
  typed (`text` will not compare with `bigint` or go into `jsonb`). The
  backend asks the server for the statement's parameter types (`describe`,
  cached with the translated SQL) and converts each `Value` to that type:
  `Text` → `text`, `jsonb` (parsed), `timestamptz`, `uuid`; `Integer` →
  `int2/4/8`, `bool` (0/1), `float8`, `numeric`; `Real` → `float4/8`,
  `numeric`; `Blob` → `bytea`. A value that cannot convert is
  `DbError::Statement` (400), never a silent cast.
- **Decoding**: `int*` → `Integer`, `bool` → `Integer` 0/1, `float*` and
  `numeric` → `Real`, `text`/`varchar`/`citext` → `Text`, `jsonb`/`json` →
  `Text` (compact JSON), `bytea` → `Blob`, `timestamptz` → `Text` in
  ForgeBase's fixed-width UTC format, anything else → `Text` via its output
  function. Callers see the same `Rows` as on SQLite.
- **Errors**: SQLSTATE classes map to the existing `DbError` variants:
  `23505` unique, `23503` foreign key, `23514` check, `23502` not null →
  `Constraint`; `25006` → `ReadOnly`; `42xxx` → `Statement`; `40001`,
  `40P01`, `55P03` → `Busy`; `08xxx`, `57P0x` → `Unavailable` (new variant,
  503); `XX001`, `XX002` → `Corrupt`. `P0001` (raised by the immutability
  trigger functions) → `Constraint { kind: Check }`, same as SQLite's
  `RAISE(ABORT)` today.
- **`explain`**: `EXPLAIN (FORMAT JSON)` mapped to `PlanStep`s (node type,
  relation, index name). **`statement_writes`**: DDL and utility statements
  (by the tokenizer's leading keyword) are writes; for `SELECT`, `WITH`,
  `VALUES` and `TABLE` the backend runs `EXPLAIN (FORMAT JSON)` (which
  plans without executing) and any `ModifyTable` node makes it a write (this
  catches `WITH … DELETE`, as SQLite's opcode check does today). Reads then
  run on the read-only role inside `READ ONLY` transactions, so a
  misclassified write still fails (`25006`) instead of writing. Functions
  with side effects called from a `SELECT` are limited by the role (§7.5),
  not by this check.
- **Slow-query log**: the same in-process ring as SQLite (backend-agnostic
  code moved to a shared module). `pg_stat_statements` adds server-side
  totals for the Database Studio.

### 7.4 `Dialect` additions

`Dialect` already covers quoting, placeholders, NULL-safe equality, JSON
extraction and iteration, `LIKE` escaping, literals, column types and the
plan prefix. FB-131 adds the following methods, with SQLite implementations
that return exactly what the code emits today, so the port packages can
switch call sites one at a time:

| Method | SQLite | PostgreSQL |
|---|---|---|
| `like_ci(expr, pat)` | `expr LIKE pat ESCAPE '\'` | `expr ILIKE pat ESCAPE '\'` |
| `order_nulls(dir)` | `ASC` / `DESC` (NULLs first ascending) | `ASC NULLS FIRST` / `DESC NULLS LAST` |
| `in_json_list(expr, param)` | `expr IN (SELECT value FROM json_each(param))` | `expr = ANY (SELECT jsonb_array_elements_text(param::jsonb))` |
| `json_array_agg(expr)` | `json_group_array(expr)` | `coalesce(jsonb_agg(expr), '[]'::jsonb)` |
| `json_array_append`, `json_set_last` | `json_insert(…, '$[#]', …)`, `json_set(…, '$[#-1].k', …)` | `jsonb_insert`, `jsonb_set(…, '{-1,k}', …)` |
| `insert_ignore(table, cols)` | `INSERT OR IGNORE INTO` | `INSERT INTO … ON CONFLICT DO NOTHING` |
| `upsert(table, cols, key)` | `INSERT … ON CONFLICT (key) DO UPDATE` (portable form replaces `INSERT OR REPLACE`) | same |
| `returning_id()` | `RETURNING seq` (replaces `last_insert_id`) | same |
| `row_order_col()` | `rowid` | `_rowid` (an identity column the PostgreSQL DDL adds where code orders by `rowid`) |
| `now_epoch()` | `unixepoch()` | `extract(epoch from now())::bigint` |
| `max_ident_len()` | 1 000 | 63; derived names (`auto_…`, `_forgebase_fts_…`) longer than that get a 10-hex hash suffix |
| `introspect_columns/indexes/tables`, `index_stats`, `object_size` | `PRAGMA …`, `sqlite_master`, `sqlite_stat1`, `dbstat` | `pg_catalog` / `information_schema`, `pg_stats`, `pg_relation_size` |
| `fulltext_*` (create, sync, match, rank) | FTS5 contentless table + triggers | a `tsvector` column `GENERATED ALWAYS AS (to_tsvector('simple', unaccent(…))) STORED` + GIN index; `websearch_to_tsquery`; `ts_rank` (ranks differ from FTS5 bm25: documented) |

Type mapping for DDL (system migrations and collection tables):

| SQLite (`STRICT`) | PostgreSQL |
|---|---|
| `INTEGER` | `bigint` |
| `INTEGER PRIMARY KEY AUTOINCREMENT` | `bigint GENERATED BY DEFAULT AS IDENTITY PRIMARY KEY` |
| `REAL` | `double precision` |
| `TEXT` | `text` |
| `TEXT CHECK (json_valid(x))` | `jsonb` |
| `TEXT … COLLATE NOCASE` | `citext` |
| `BLOB` | `bytea` |
| `bool` fields: `INTEGER CHECK (x IN (0,1))` | kept as `smallint CHECK (x IN (0,1))`, so SQL that compares with `0`/`1` keeps working |
| `WITHOUT ROWID`, `STRICT` | dropped (PostgreSQL is always strict) |
| trigger with `RAISE(ABORT, msg)` | PL/pgSQL trigger function `RAISE EXCEPTION USING ERRCODE = 'P0001'` |
| `DEFAULT (unixepoch())` | `DEFAULT extract(epoch from now())::bigint` |

Semantics the port must keep (each has a test in the both-backends suite):
NULLs sort first ascending (cursors); `~` is case-insensitive (ASCII on
SQLite, Unicode on PostgreSQL: documented); NOCASE columns compare
case-insensitively; JSON values come back as JSON (key order may differ:
`jsonb` sorts keys; ForgeBase already re-serialises records); identifiers:
SQLite treats quoted identifiers case-insensitively, PostgreSQL does not, so
the SQL editor on PostgreSQL needs `"Posts"` for a collection named `Posts`
(documented; ForgeBase's own SQL always quotes the stored name).

### 7.5 Data core on PostgreSQL

What ADR-0003 §2 listed as SQLite-specific moves behind `Dialect`/`Backend`:

- **Schema changes get simpler**: PostgreSQL DDL is transactional and has
  `ALTER COLUMN … TYPE … USING`, `DROP COLUMN` and `RENAME`, so the
  SQLite table rebuild (`_forgebase_tmp_<id>`, row copy with `rowid`,
  `legacy_alter_table`) becomes a sequence of `ALTER TABLE` statements in
  one `SchemaChange` transaction. The verification step after every change
  stays (views must still compile, columns must match).
- **Views**: `CREATE VIEW … WITH (security_invoker = true)` so a view never
  reads with its owner's rights (matters for native RLS).
- **SQL editor**: the same tokenizer contract with a PostgreSQL deny list:
  transaction control, `COPY … TO/FROM` a file or `PROGRAM`, `ALTER SYSTEM`,
  `LOAD`, `SET ROLE`/`SET SESSION AUTHORIZATION`/`RESET ROLE`, `CREATE
  EXTENSION` (admin API only, allow list), `CREATE … LANGUAGE c` or
  `internal`, `LISTEN`/`NOTIFY`/`UNLISTEN`, `VACUUM`/`CHECKPOINT`/`CLUSTER`
  outside the maintenance API, functions `pg_read_file`, `pg_read_binary_file`,
  `pg_ls_dir`, `lo_import`, `lo_export`, `pg_terminate_backend`,
  `pg_cancel_backend`, `pg_reload_conf`, `set_config` on `forgebase.*`
  settings, `dblink*`. The real guard is the role: the SQL editor runs as
  `forgebase_sql_admin`, which is not a superuser and has no
  `pg_read_server_files`/`pg_execute_server_program` membership (rls-coverage
  §3.2), so the deny list is defence in depth, not the boundary.
- **Indexes**: B-tree, unique, partial, expression and JSON-path indexes map
  directly (`((_data->'metadata'->>'brand'))`); full text per §7.4; index
  statistics from `pg_stats` and `pg_stat_user_indexes`; sizes from
  `pg_relation_size`; rebuild = `REINDEX INDEX CONCURRENTLY` is not allowed
  inside a transaction, so rebuild uses plain `REINDEX` (locks the table; the
  Studio warns).

### 7.6 System migrations on PostgreSQL

- **Schemas**: system tables live in schema `forgebase`, collection tables in
  `public`. The server's roles have `search_path = forgebase, public`, so
  ForgeBase SQL keeps its unqualified `_forgebase_*` names; direct logins
  see neither schema (rls-coverage §3.2). Collection names can never collide
  with system tables (`_forgebase*` is reserved, ADR-0003 §3).
- `migrations/postgres/0025_baseline.sql`: one hand-reviewed file that
  creates the schema SQLite has after `0025`, with the type mapping of §7.4,
  PL/pgSQL trigger functions for the 22 triggers, and the same seed rows.
  Applying it records versions 1–25 in `_forgebase_migrations` with
  `name` and `checksum` of the PostgreSQL file, so version numbers mean the
  same on both backends. There is no PostgreSQL replay of 25 historical
  migrations: no PostgreSQL database exists that is older than the baseline.
- From `0026` on, every migration is a pair: `migrations/system/NNNN_x.sql`
  (SQLite) and `migrations/postgres/NNNN_x.sql`. A unit test fails if a
  number exists for one backend only.
- **Schema-parity test** (in `forgebase-migrations`): migrate a SQLite and a
  PostgreSQL database to the head; compare tables, columns (through the type
  mapping), nullability, defaults (normalised), primary keys, unique
  constraints, foreign keys, indexes (columns, uniqueness, partial
  predicates normalised), check constraints by name, and trigger names. Any
  difference fails with a readable diff. This is what keeps two hand-written
  schemas honest.
- The runner keeps its rules (forward-only, checksums verified, one
  transaction per migration). PostgreSQL migrations run as `forgebase_owner`
  (via `SET ROLE` from `forgebase_server`), so every object has the right
  owner. The pre-migration snapshot of ADR-0001 §4 becomes a `pg_basebackup`
  into `backups/pre-migration-<version>/` for managed mode, and is skipped
  with a warning for external mode.

### 7.7 Tests on both backends

- `forgebase-db` gets a `testing` module (feature `testing`, dev-dependency
  only): `test_backend().await -> TestDb` reads `FORGEBASE_TEST_BACKEND`
  (`sqlite`, default, or `postgres`). For PostgreSQL it starts **one** engine
  per test binary (from `FORGEBASE_TEST_PG_ENGINE`: an extracted engine
  directory; CI uses the FB-130 artifact), migrates a template database once,
  and gives each test `CREATE DATABASE t_<uuid> TEMPLATE fb_template`
  (milliseconds), dropped at the end.
- Every `tests/common` helper switches to `test_backend()`. Tests that check
  SQLite internals (`PRAGMA`, WAL, `VACUUM INTO`) are marked
  `#[cfg_attr(…, ignore)]` for PostgreSQL with a reason, and get a PostgreSQL
  counterpart where the behaviour matters (e.g. backups).
- Black-box suites (`tests/integration`, including the 25 SPEC scenarios of
  FB-85) run the real binary with `--database postgres` in a second CI job.
- CI: the existing job stays SQLite; a new `test-postgres` job runs the whole
  workspace with `FORGEBASE_TEST_BACKEND=postgres` on Linux x86_64 on every
  push, and on macOS arm64 and Windows nightly. Wall-clock cost: one more
  full test run per push.
- Semantic tests that pin the differences in §7.4 (NULL order, case-insensitive
  match, NOCASE columns, JSON round trip, identity sequences after convert,
  commit-ordered cursors) run on both.

## 8. What users get with PostgreSQL

| Capability | SQLite backend | PostgreSQL backend |
|---|---|---|
| ForgeBase APIs, rules, Studio, SDKs | yes | yes, identical |
| Concurrent writers | no (one writer) | one writer at first (decision 6); concurrent after FB-157 |
| Native row-level security | no | yes: ForgeBase rules compiled to policies, plus policies you write (rls-coverage §3) |
| Direct `psql` / driver / BI-tool access | no (the file is locked by the server) | yes: `forgebase db psql`, and named database logins with RLS applied (opt-in) |
| PL/pgSQL functions, triggers, views in the SQL editor | SQLite triggers only | yes |
| Extensions | none | pgvector and the contrib list in §1 (allow list) |
| `LISTEN` / `NOTIFY` | no | yes for direct logins; ForgeBase uses it for multi-process cache invalidation later |
| Full-text search | FTS5 (bm25) | `tsvector` + GIN (`ts_rank`); different ranking |
| Point-in-time recovery | snapshots only | snapshots, then WAL archiving (FB-156) |
| One portable file | yes | no: a directory with a server process |
| Extra disk / memory | none | engine 50–100 MB unpacked per project (§3.2); 30–60 MB RAM idle plus `shared_buffers` |
| Single-binary download | ~as today | larger by the compressed engine (§3.2) |

## 9. Risks

| Risk | Impact | Mitigation |
|---|---|---|
| Linux engine portability (musl vs glibc hosts) | engine fails to start on some distros | §2.3 relocation design, proven by the FB-130 spike on Debian, Ubuntu, Fedora, Alpine and distroless before anything else depends on it |
| Semantic drift between backends (NULL order, case, types, JSON) | same API call, different answer | dialect methods with tests on both backends (§7.4); the whole suite on both (§7.7) |
| Two hand-written schemas drift | wrong constraint on one backend | schema-parity test (§7.6); paired-migration check |
| Type strictness breaks SQL that SQLite accepted | 400s on PostgreSQL | describe-driven binding (§7.3); suite on both backends |
| Windows process management | orphaned or stuck engine | job object; `pg_ctl stop`; stale-pid check; Windows in nightly CI |
| Supply chain of a C code base we build | compromised engine | source tarballs pinned by SHA-256 from postgresql.org; pgvector by tag and SHA-256; CI-built, checksummed, signed archives (§2.4) |
| Security of direct logins | data exposure | off by default; socket only; RLS forced on every table; roles without superuser (rls-coverage §3) |
| Engineering size | months of one-person work | 28 packages, each one temp session, up to 8 in parallel (`postgres-plan.md`) |

## Consequences

- ForgeBase gains a second storage engine without changing its service
  APIs; SQLite remains the default and loses nothing.
- Every new migration and every new SQL statement must work on both
  backends; CI enforces it.
- The release binary grows by the compressed engine; a `-lite` build stays
  available.
- Native RLS makes ForgeBase usable as a "real" PostgreSQL host, at the cost
  of maintaining a rule-to-policy compiler next to the rule evaluator.

## Sources

Checked 2026-11-11.

- theseus-rs/postgresql-binaries: targets, versions, musl/glibc floors,
  release assets and attestations —
  <https://github.com/theseus-rs/postgresql-binaries>,
  <https://github.com/theseus-rs/postgresql-binaries/blob/main/docs/support-policy.md>,
  <https://github.com/theseus-rs/postgresql-binaries/releases/tag/18.6.1>
- `postgresql_embedded` crate (bundled feature, binary sources) —
  <https://docs.rs/postgresql_embedded/latest/postgresql_embedded/>;
  `postgresql_extensions` — <https://docs.rs/postgresql_extensions/latest/postgresql_extensions/>
- zonky embedded-postgres-binaries and its Alpine build script —
  <https://github.com/zonkyio/embedded-postgres-binaries>,
  <https://github.com/zonkyio/embedded-postgres-binaries/blob/master/scripts/build-postgres-alpine.sh>,
  <https://repo1.maven.org/maven2/io/zonky/test/postgres/embedded-postgres-binaries-linux-amd64-alpine/18.6.0/>
- EDB binaries — <https://www.enterprisedb.com/download-postgresql-binaries>
- fergusstrange/embedded-postgres — <https://github.com/fergusstrange/embedded-postgres>
- `dlopen` in static musl — <https://openwall.com/lists/musl/2017/05/17/2>
- Alpine `postgresql18` package (configure flags, musl locale note) —
  <https://github.com/alpinelinux/aports/blob/master/main/postgresql18/APKBUILD>
- PostgreSQL build options — <https://www.postgresql.org/docs/current/install-make.html>
- pgvector (version, licence, PGXS and Windows builds, `OPTFLAGS`) —
  <https://github.com/pgvector/pgvector>
- PostgreSQL License — <https://www.postgresql.org/about/licence/>; OpenSSL —
  <https://openssl-library.org/source/license/index.html>; zlib —
  <https://zlib.net/zlib_license.html>; lz4 —
  <https://github.com/lz4/lz4/blob/dev/lib/LICENSE>; zstd —
  <https://github.com/facebook/zstd>
- Versioning policy and support dates —
  <https://www.postgresql.org/support/versioning/>; PostgreSQL 19 RC1 and
  release date — <https://www.postgresql.org/about/news/postgresql-19-rc-1-released-3409/>
- `pg_upgrade` — <https://www.postgresql.org/docs/current/pgupgrade.html>
- `initdb` (root refusal, checksums default, auth) —
  <https://www.postgresql.org/docs/current/app-initdb.html>; server root
  check — <https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/main/main.c>;
  Windows restricted token — <https://github.com/postgres/postgres/blob/REL_18_STABLE/src/bin/pg_ctl/pg_ctl.c>
- Shared memory and `RemoveIPC` — <https://www.postgresql.org/docs/current/kernel-resources.html>;
  `dynamic_shared_memory_type` — <https://www.postgresql.org/docs/current/runtime-config-resource.html>
- Unix socket path limits — <https://github.com/postgres/postgres/blob/REL_18_STABLE/src/backend/libpq/pqcomm.c>,
  <https://man7.org/linux/man-pages/man7/unix.7.html>
- `builtin` locale provider (PostgreSQL 17) — <https://www.postgresql.org/docs/release/17.0/>

## Addendum: FB-131, the PostgreSQL backend in `forgebase-db` (2026-11-11)

Built as §7.2–§7.4 and §7.7 describe. What callers and the next packages
need to know, and where the code differs from the text above.

**API.** `DbOptions.path` became `DbOptions.target: DbTarget`
(`DbTarget::Sqlite { path }` / `DbTarget::Postgres { url: SecretString,
managed }`); `DbOptions::from_config(cfg, path)` keeps its signature and
`from_config_target(cfg, target)` and `with_target` are new. The one struct
literal outside the crate (`crates/forgebase-data/tests/common/mod.rs`) was
changed. `Database` is an `Arc` facade over SQLite and PostgreSQL;
`kind()`, `location()` (URL without password), `last_commit_lsn()` and
`current_wal_lsn()` are new. `DbError::Unavailable` (SQLSTATE 08, 53, 57P0x,
I/O; 503) and `DbError::Unsupported` (400) are new; `DatabaseInfo` gained
`backend`, `server_version` and `connections`. `effective_pragmas` and
`snapshot_to` are `Unsupported` on PostgreSQL; `migrate` and
`migration_status` are `Unsupported` until FB-133 wires
`PgDatabase::migrate`; `schema_version` reads `_forgebase_migrations` when
the table exists.

**Reads** run as `BEGIN READ ONLY; <statement>; ROLLBACK` on the read pool
(connections also have `default_transaction_read_only = on`), always on the
extended protocol, so one read is one statement and any session change it
makes (`SET`, `set_config`) is rolled back with it. That is three round
trips per read on a Unix socket; FB-157 or a later tuning pass can trim it.

**Statements.** `?N`/`?` → `$N` by `translate_placeholders` (aware of
`'…'`, `E'…'`, `"…"`, `$tag$…$tag$`, `--` and nested `/* */`; `$N` already
in the text is kept). Unused parameter numbers are declared `text`; missing
parameters bind NULL, as on SQLite. Parameters PostgreSQL cannot infer
(`SELECT ?1`, `jsonb_build_array(?1)`; SQLSTATE 42P18) are learned on the
first prepare of an SQL text, under a savepoint so the transaction survives,
and then declared from the bound value's kind (`int8`, `float8`, `text`,
`bytea`); the SQL gets a `/* forgebase types: … */` comment so each kind
combination is its own cached statement. Parameter-less, uncached
statements on the writer (DDL scripts) use the simple protocol, which
accepts several statements as SQLite does.

**Binding** follows §7.3 with lossless conversions only: integral text into
`int*`, 0/1 and `t`/`f` into `bool`, numbers into `text` (SQLite's text form,
`3.0`), numbers or decimal text into `numeric`, RFC 3339 text into
`timestamptz`, a JSON array in `Text` into a one-dimensional array parameter.
Domains bind as their base type, enums and `citext` as text.

**Decoding** follows §7.3. `timestamptz` comes back at millisecond precision
(`YYYY-MM-DDTHH:MM:SS.mmmZ`, as `forgebase_schema` writes it); `jsonb` as
compact JSON with the server's key order; arrays as JSON arrays. *Differs:*
a type outside that set is returned as `Text` when its binary form is UTF-8
(enums, `citext`, domains over text) and as `Blob` otherwise, not through
its output function (that needs text-format results, which sqlx only gives
for the simple protocol). Empty results name their columns on PostgreSQL;
SQLite returns no column names for zero rows.

**Transactions.** One writer connection; `Normal` is `READ COMMITTED`,
`SchemaChange` adds `SET CONSTRAINTS ALL DEFERRED` (foreign-key violations
fail the `COMMIT` with `Constraint { ForeignKey }`). A dropped transaction is
rolled back on a spawned task before the writer returns to the pool (closed
if that fails). *Differs from SQLite:* a failed statement aborts a
PostgreSQL transaction; the backend then refuses `commit`
(`Statement("the transaction was rolled back …")`) rather than letting the
server silently turn it into a rollback. Code that ignores a statement error
and carries on in the same transaction (one test does:
`forgebase-customize/tests/customize.rs:720`) must change in its port
package. `ExecResult::last_insert_id` is 0 on PostgreSQL: use
`Dialect::returning_id`.

**Errors.** Unique, not-null and check messages are rendered in SQLite's
form (`UNIQUE constraint failed: posts.slug, posts.lang`), because
`forgebase_data::error::unique_fields` parses it; `P0001` keeps the trigger's
message.

**Schema changes.** A committed DDL statement (or `SchemaChange`
transaction) bumps a generation number. The writer clears its statement
cache right after a DDL statement; each reader keeps the generation it last
saw in the session setting `forgebase.schema_gen` (read in the same round
trip as `BEGIN`) and clears its cache when behind. A `0A000` "cached plan
must not change result type" on a read is retried once after clearing.

**Dialect.** Every §7.4 method exists on both dialects (SQLite returns
today's SQL; tests pin it). Choices the table above left open:
`json_extract` on PostgreSQL is `(expr->'a'->>'b')` (text, matching
JSON-path expression indexes; numeric comparisons need a cast in FB-143);
`in_json_list` compares as text; `now_epoch` is
`floor(extract(epoch from now()))::bigint` (a plain `::bigint` rounds, which
can be half a second in the future); `upsert(table, cols, key, source)`,
`insert_ignore(table, cols, source)`, `returning_id(col)`, `fit_ident(name)`
(63-byte limit, 10-hex FNV-1a suffix); the introspection methods return
`Statement`s with SQLite's column names (`cid, name, type, notnull,
dflt_value, pk`; `seq, name, unique, origin, partial`; `stat`).
`unaccent()` is only `STABLE`, so a generated `tsvector` column cannot call
it: the full-text methods use `_forgebase_unaccent(text)`, an `IMMUTABLE`
wrapper the PostgreSQL baseline must create (`PG_FULLTEXT_SETUP`, FB-133).
`fulltext_rank` is lower-is-better on both engines (`-ts_rank`).

**Sessions** set `DateStyle=ISO`, `TimeZone=UTC`, `IntervalStyle=postgres`,
`extra_float_digits=3` and `application_name=forgebase`. The backend does
not set `search_path`: the roles carry it (§7.6, FB-132/FB-133), so DDL that
must land in `public` should name the schema. Servers older than
PostgreSQL 16 are refused at open. A missing database is `NotFound`, or is
created (through the `postgres` database) with `create_if_missing`.

**Tests** (`forgebase_db::testing`, feature `testing`): `test_backend()`,
`test_backend_of(kind)`, `postgres_test_db()` (`None` = skip) and `TestDb`
(derefs to `Database`; `data_dir()`, `options()`). The engine comes from
`FORGEBASE_TEST_PG_ENGINE` or `FORGEBASE_PG_ENGINE_DIR`, runs on a Unix
socket under `FORGEBASE_TEST_PG_TMP` (default the system temp dir) with port
number 18200 and no TCP listener, and a watchdog shell stops it and deletes
its directory when the test binary exits; `FORGEBASE_TEST_PG_URL` uses an
existing server instead. Port packages add `forgebase-db = { workspace =
true, features = ["testing"] }` to their dev-dependencies.

**Seams for ADR-0029 (replicas, tenant shards), not built here.** The read
pool is `pg::ReadPools { primary, replicas, router: Arc<dyn ReadRouter> }`;
`ReadRouter::route(&ReadRequest { statement, replicas }) -> ReadTarget`
(`Primary` / `Replica(n)`, an unknown replica falls back to the primary),
default `PrimaryOnly`. Each commit runs `COMMIT; SELECT
pg_current_wal_insert_lsn()` in one round trip and records the position
(the insert position, not `pg_current_wal_lsn()`: with `synchronous_commit
= off` the write position can still be before the commit record) (`Database::last_commit_lsn() ->
Option<Lsn>`, monotonic); `Database::current_wal_lsn()` asks the primary.
`Database` is per target and cheap to clone, and the crate holds no global
connection state, so a shard map can hold one `Database` per shard.

## Addendum (FB-134, MDF-82): resource-scoped engine permissions and rules

Written 2026-11-11 by FB-134. The framework the engine RLS fixes build on
(`rls-coverage.md` G7, G9, G10, G14, G16; applied by FB-138 to FB-142). It
lives in `forgebase-data::access` (types, trait) and `forgebase-policy`
(evaluation, storage, HTTP); it is backend-neutral: rules compile through
`forgebase-query` with the backend's `Dialect`, and the new table uses SQL
valid on both engines.

**Permission keys.** The fifteen engine actions in
`forgebase_policy::perm::RESOURCE_ACTIONS` (`kv_*`, `vector_*`, `graph_*`,
`storage_*`, `queue_*`) take a resource segment: `engine:<action>:<name>`.
The name is everything after the second `:` (it may hold `:` and `.`), is
compared case-sensitively, and in a pattern may use `*` anywhere
(`engine:kv_read:cache.*`). `engine:<action>` keeps meaning "every resource":
granting it grants every resource key of that action, denying it denies them
all, and an explicit deny of a resource key beats a grant of the plain key.
The plain key is not widened for other actions, so `engine:read_jobs` does
**not** grant FB-142's `engine:read_jobs:all`. The escalation guard (roles,
memberships, overrides, invitations) applies the same rule: a holder of
`engine:kv_read:cache` cannot grant `engine:kv_read` or `engine:kv_read:*`.

**Decision.** `Authorizer::authorize_resource(ctx, action, &ResourceRef) ->
Decision`, a default method that falls back to `authorize(ctx, action, None)`,
so authorizers written before FB-134 keep their answers. The policy engine
runs the pipeline in order: authentication (anonymous callers continue, but
only a rule can grant them anything); realm and tenant exactly as for
`engine:<action>` today (signed-in callers must be active members of the
tenant's organization, or own or share its personal project); permission
(resource key or plain key; a `ResourceRef::owner` equal to the caller holds
every permission on it, a deny still wins); the entitlement hook, asked with
the plain key and the resource so FB-24's mapping is unchanged; then, only
when no permission grants, the **resource rule**; then the quota hook.
Privileged callers and the system are allowed, as everywhere. Admin actions
(`kv_admin`, `vector_admin`, `storage_admin`, `queue_admin`) are granted by
permissions only, never by a rule. Resource ACL entries
(`_forgebase_resource_permissions`) are **not** consulted for engine
resources: they are org-managed but engine names are shared across tenants,
so an org's entry would reach other orgs' tenants.

**Rules.** Table `_forgebase_resource_rules (kind, name, action, rule)`
(migration 0026; NULL rule = locked, `""` = everyone). Kinds:
`kv_namespace`, `vector_collection`, `graph_label`, `graph_edge_type`,
`bucket`, `queue_topic`, `queue_subscription`. Actions are the collection-rule
operations `list`, `view`, `create`, `update`, `delete`, plus `read` (list +
view) and `write` (create + update + delete); the exact operation wins over
its alias. Name `*` holds the rules of every resource of the kind that has no
rules of its own (a resource's own rule set replaces the `*` set as a whole).
A stored rule that no longer parses locks its action without falling back.
The grammar is the collection-rule grammar (ADR-0008 §6): `@request.*`
values are decided in Rust; a residual over item fields comes back as
`Decision::Filter(Predicate)`. Rules are validated when saved against the
kind's `ResourceSchema`: the schema the engine registered
(`Authorizer::register_resource_schema`), else `ResourceKind::default_schema`
(bucket: `key`, `created_by`, `size`, `content_type`, `folders`; vector: `id`,
`metadata`; graph label: `id`, `label`, `properties`; edge type: `id`,
`type`, `source`, `target`, `properties`; KV: `key`; queues: none). Only
declared fields may be read, and a rule action must be one of the schema's
operations. The engine compiles a filter with `ResourceSchema::compile(&mut
SqlBuilder, alias, &predicate)`; the SQL names each field as a column of the
alias, so the engine selects its items as a derived table exposing those
names. Engines that cannot apply a predicate treat `Filter` as a denial.

**HTTP** (super-admins, like collection rules; in OpenAPI):
`GET /api/v1/policies/resources/{kind}` (fields, operations, actions, every
resource with rules), `GET|PUT|DELETE /api/v1/policies/resources/{kind}/{name}`
(`PUT {rules: {action: rule|null}}` replaces the set), and
`POST /api/v1/policies/resources/{kind}/{name}/check` `{action, op,
principal?}` → `{decision: allow|filter|deny, step, reason}` for any caller
about themselves (super-admins may name a principal). `/api/v1/authz/check`
understands resource keys too.

**For the wave-1b packages.**
- Ask `authorize_resource` with the operation that matches the call (a scan
  or search is `List`, a get is `View`, ...). Keep calling the plain
  `authorize` for calls that span every resource (list namespaces, stats):
  only the plain key grants those.
- Register your schema at start (`authorizer.register_resource_schema(..)`)
  if the defaults above do not fit.
- Permission keys carry the action, not the kind:
  `engine:queue_read:<name>` covers a topic and a subscription of that name,
  and `engine:graph_read:<name>` a label and an edge type (FB-139, FB-141).
- Wrapping authorizers must forward `authorize_resource` and
  `register_resource_schema`; FB-134 did this for
  `forgebase_jobs::JobChannelAuthorizer`, which wraps the policy engine for
  the server's shared data service.
- PostgreSQL twin of migration 0026 (FB-133): the same table without
  `STRICT`, `updated_at BIGINT`. The statements were checked on
  PostgreSQL 18.3; the backend itself arrives with FB-131.
- Native RLS (FB-152): engine tables stay API-only (rls-coverage §3.4), so
  resource rules are not compiled into PostgreSQL policies.

## Addendum: FB-132, the engine in `forgebase-pgengine` (2026-11-11)

Built as §2.3, §3.1 and §4.1–§4.5 describe, with the ADR-0029 §1.3
settings. What callers and the next packages need, and where the code
differs from the text above.

**API.** `Engine::prepare(&Layout, &EngineConfig) -> EngineHandle`.
`Layout::primary(data_dir)` is `<data_dir>/postgres/{data,run,forgebase-postgres.conf}`;
`Layout::node(data_dir, name, NodeKind::Primary | NodeKind::Standby)` is
`postgres/nodes/<name>/…` with the same `postgres/engine/` and
`postgres/roles.secret`. `EngineConfig { source: Auto | Dir(path) |
Archive(path) | Embedded, listen, socket_port, settings, cron: ExtensionMode
(Auto | On | Off), slow_query, ready_timeout, stop_timeout, restart:
RestartPolicy, cache_dir, remove_old_engines, unsafe_fast }`.
`EngineHandle`: `url(Role) -> Secret` (database `forgebase`), `url_for(role,
db)`, `stop()` (fast), `stop_immediate()`, `health() -> EngineHealth { state:
Ready | Restarting | Failed | Stopped, pid, restarts, last_error }`,
`watch_health()`, `version()`, `engine()`, `socket_dir()`, `tcp_addr()`,
`port()`, `preloaded()`, `has_cron()`, `warnings()`, `recent_log()`.
`Role::{Super, Server, Reader}` are the login roles; `Secret` redacts in
`Debug`/`Display`. The caller holds `.forgebase.lock`.

**Engine sources.** `Auto` is the embedded archive (feature
`bundled-postgres`), else `FORGEBASE_PG_ENGINE_DIR`, else an error that
names `engine_dir`, external mode and the standard release. A `Dir` is used
in place and never modified. Every engine is checked with `postgres
--version` (which also proves the binary runs on the host); the managed
engine needs PostgreSQL 17 or later (the `builtin` locale), and a cluster
whose `PG_VERSION` differs from the engine's major is refused (§4.6).
Modules are looked up in `pg_config --pkglibdir` (when the engine has it),
`lib/postgresql/` and `lib/`.

**Archive format (FB-130 builds to this).** `tar` + `zstd`.
`forgebase-engine.json` (format 1), preferably the first entry: `postgres`
(`18.6`), `build` (`r1`), `target`, optional `loader`
(`lib/ld-musl-x86_64.so.1`) and `interp_placeholder` (default
`/forgebase-pg-loader-`), `features` (free map, e.g. `graph_cypher`),
`files: [{path, size, sha256, mode}]`, `links: [{path, target}]`. Entries may
sit under one top-level directory. Extraction refuses anything not in the
manifest, size or hash differences, missing files, absolute paths, `..`,
links whose target leaves the engine, and other entry types. It extracts into
`postgres/engine/<postgres>-<build>-<sha12>.partial`, patches `PT_INTERP` of
every ELF file in `bin/` when `loader` is set (to the final path), drops all
write bits, writes `forgebase-engine.sha256`, and renames. The long-path
fallback (loader path over 254 bytes) extracts into
`$XDG_CACHE_HOME/forgebase/engines/` or `$HOME/.cache/forgebase/engines/`.
`archive::verify_extracted` re-checks a directory for `doctor` (relocated
executables by size). `examples/pack_engine.rs` packs a directory and prints
the SHA-256.

**`engine.lock` (read by `build.rs`).** `[archives."<target triple>"]
sha256 = "<hex>"` (a `file` key is allowed and ignored); other tables are
ignored. `build.rs` reads `FORGEBASE_PG_ENGINE_ARCHIVE` and `engine.lock`
(or `FORGEBASE_PG_ENGINE_LOCK`) and fails the build, naming the variable or
both hashes, when the archive is missing, unpinned or different. Checked with
a packed Homebrew 18.3 engine: the embedded archive extracts and runs.

**`initdb`** has the flags of §4.2 plus `--locale=C` (the libc categories:
messages, numbers, money and time stay host-independent; collation and
ctype come from the builtin provider) and `--no-instructions`, runs with
`TZ=UTC`, writes into `data.partial` and renames, so an interrupted
`initdb` leaves nothing half-made. The password file is 0600 and deleted
afterwards.

**Bootstrap** runs on every start of a writable cluster, idempotently, as
`forgebase_super` (so a later ForgeBase can add roles or grants to existing
clusters): `forgebase_owner` (NOLOGIN, BYPASSRLS), `forgebase_rls` (NOLOGIN,
no BYPASSRLS), `forgebase_sql_admin` (NOLOGIN, BYPASSRLS),
`forgebase_server` (LOGIN, BYPASSRLS, member of owner and rls) and
`forgebase_reader` (LOGIN, BYPASSRLS, `default_transaction_read_only = on`);
none is a superuser or can create roles or databases. Server and reader get
`search_path = forgebase, public` (§7.6) and their passwords from
`roles.secret` (`role=password` lines, 0600; a file other users can read is
refused). Database `forgebase` (also `cron.database_name`) is owned by
`forgebase_owner`; `PUBLIC` loses all rights on it; the reader gets
`CONNECT` (the server has the owner's rights through membership). Schemas
`forgebase` and `public` are owned by `forgebase_owner`; the reader gets
`USAGE` and default `SELECT` on tables created by the owner or the server.
The bootstrap session sets `log_statement = none` and
`log_min_error_statement = panic`, so a failing statement never writes a
password into the log.

**Settings** (`forgebase-postgres.conf`, rewritten every start;
`include_if_exists = '../forgebase-postgres.conf'` is appended once to
`postgresql.conf`, relative so node layouts and moved data directories
work): `listen_addresses = ''`, `port`, `unix_socket_directories`,
`unix_socket_permissions = 0700`, `max_connections = 64`, `shared_buffers =
128MB`, `password_encryption = scram-sha-256`, `wal_level = logical`,
`max_wal_senders = 16`, `max_replication_slots = 16`,
`max_prepared_transactions = 16`, `max_worker_processes = 16`,
`synchronous_commit = on`, `shared_preload_libraries` (pg_stat_statements
and pg_cron, each only when the engine has the module), `cron.database_name
= 'forgebase'`, `cron.use_background_workers = on`, `cron.log_run = off`
(only when pg_cron is preloaded), `log_destination = stderr`,
`logging_collector = off`, `log_line_prefix = '[%p] '`, `lc_messages = C`,
`timezone` and `log_timezone = UTC`, `log_min_duration_statement` from the
slow-query threshold; standbys add `hot_standby`, `hot_standby_feedback` and
`max_standby_streaming_delay = 30s`. The three network keys are also passed
as `-c` options, so no file can widen what the engine listens on. User
settings replace defaults of the same name.

**pg_cron.** `cron = auto` with an engine that lacks it (Homebrew 18.3) logs a
warning, lists it in `warnings()`, and writes no `cron.*`; `on` refuses
before `initdb`; `off` never preloads it. Without the check, a missing
preload library stops the postmaster (`FATAL: could not access file
"pg_cron"`, seen on 18.3).

**Deny list** (`DENIED_SETTINGS`, plus every `cron.*` and `include*` key): the
§4.3 keys, the ADR-0029 §1.3 keys, and keys that run programs or load
libraries as the server's OS user (`archive_command`, `archive_library`,
`restore_command`, `archive_cleanup_command`, `recovery_end_command`,
`ssl_passphrase_command`, `dynamic_library_path`, `session_preload_libraries`,
`local_preload_libraries`, `jit_provider`), file and socket keys
(`config_file`, `ident_file`, `external_pid_file`, `unix_socket_group`,
`unix_socket_permissions`), replication wiring that carries credentials
(`primary_conninfo`, `primary_slot_name`; FB-168 writes them) and the log
format ForgeBase parses. Settings become editable from the dashboard
(FB-91), so a setting that runs a program would turn a ForgeBase
administrator into an OS shell. Keys must be PostgreSQL parameter names and
values one line.

**Process.** `postgres -D` is spawned directly in its own process group (a
terminal Ctrl-C reaches ForgeBase, which stops the engine in order). Its
stderr goes to `tracing` (target `forgebase::postgres`, field `node`;
`PANIC`/`FATAL`/`ERROR` → error, `WARNING` → warn, `LOG`/`INFO`/`NOTICE` →
info, `DEBUGn` → debug; `DETAIL`, `HINT`, `STATEMENT` keep the previous
level), and the last 40 lines are kept for error messages. Readiness is
`postmaster.pid` reporting `ready` for the pid we started (no failed-login
noise in the log), then one SCRAM login as `forgebase_super`; a timeout or
an early exit fails `prepare` with the engine's last lines (for example
`invalid value for parameter "shared_buffers"`). Stop: `SIGINT`, `SIGQUIT`
after `stop_timeout` (30 s), `SIGKILL` 5 s later; Windows `pg_ctl stop -m
fast`. Crash restart follows §4.3 (`RestartPolicy`, default 1 s doubling to
30 s, `Failed` after 5 failures in 10 minutes); a restart attempt that finds
the old backends still holding shared memory counts as a failure and is
retried.

**Stale `postmaster.pid`.** A dead pid is left to PostgreSQL, which removes
the file itself. A live pid whose socket (from the file) accepts a
connection is a leftover postmaster and gets a fast shutdown. A live pid that
never answers within the stop timeout is refused, never deleted: removing the
file under a live postmaster can corrupt the cluster.

**Differs from the text above.**
- *Parent death.* `PR_SET_PDEATHSIG` must be set in the child between fork
  and exec, which needs `Command::pre_exec`, an `unsafe fn` in std and
  tokio; the workspace has `unsafe_code = "forbid"`. Instead, on every Unix
  (macOS included, which the text said has no equivalent) a `/bin/sh`
  watcher blocks reading a pipe that only ForgeBase writes. When ForgeBase
  exits for any reason, `SIGKILL` included, the pipe closes and the watcher
  sends `SIGINT`, then `SIGQUIT` after 30 s. Hosts without `/bin/sh`
  (distroless) rely on the stale check at the next start; a container's
  processes stop with its PID 1 anyway.
- *Windows job object* is not built: it needs `windows-sys` with `unsafe` or a
  crate outside the offline cache. The stale check covers it. The Windows
  code paths (loopback TCP on a free port, `pg_ctl stop`) compile by
  construction but were not built or run here (no Windows toolchain);
  `postgres` refuses to start for a user with administrator rights, so
  FB-149 must test as a normal user.
- *Root refusal* checks the effective uid (`rustix`); Windows is not checked.
- *Listening.* `listen` accepts loopback addresses only until FB-153 brings
  TLS. The socket fallback directory under the temp dir must be owned by the
  user with mode 0700, so another user cannot pre-create it on a shared
  `/tmp`.
- *Minor upgrade.* After a clean start from an extracted engine (primary
  layout only), extracted engines of the same major are deleted; other majors
  stay for `upgrade-engine`, and engines in the cache fallback are kept.

**Seams for FB-168 and FB-174.** A `NodeKind::Standby` layout skips `initdb`
and the bootstrap and writes the hot-standby settings; its data directory
must exist (`pg_basebackup -R`), as the test does against a running primary.
A shard is `Layout::node(.., NodeKind::Primary)`: its own `initdb`, the same
passwords. FB-168 adds `forgebase_replicator` (a `Role` variant and a line
in `roles.secret`, which `RoleSecrets::fill_missing` creates for existing
clusters), slots, `primary_conninfo`, `max_slot_wal_keep_size` and
`idle_replication_slot_timeout`.

**Tests.** 44 unit tests (archive verification and tampering, relocation and
the long-path fallback, settings and deny list, socket fallback, stale pid,
secrets, URLs, `engine.lock`). Integration tests (skip without
`FORGEBASE_PG_ENGINE_DIR`): settings, roles, grants, locale and checksums;
stop and restart; `forgebase_db::Database` opened with the server and
reader URLs; crash restart; a leftover postmaster stopped first; an engine
from an archive extracted once with old minors removed; pg_cron modes and
bad settings; a primary, a shard and a standby side by side; and the
embedded archive (only in a `bundled-postgres` build).

## Addendum (FB-135, MDF-87): data core and SQL editor fixes

Written 2026-11-11 by FB-135. Closes `rls-coverage.md` G12, G13 and the
data-core part of G11. Code: `forgebase-data/src/{sql.rs,records.rs}`,
`forgebase-api/src/v1/sql.rs`; tests:
`crates/forgebase-server/tests/security_fb135.rs` (each test fails on the
code before FB-135). FB-134's resource API is not used: these are
collection records, schema introspection and an administrator-only tool,
which the collection decision and `ReadSchema`/`ReadSql` already cover.

**G12, `describe_table`.** Callers who are not privileged (anyone but the
system, administrators and admin service accounts) get the same 404
`collection_not_found` for a system table (`_forgebase*`, `sqlite_*`, any
letter case) as for a missing table; the name is checked before any lookup
and again on the resolved name. `list_tables` already left them out for
everyone; administrators can still describe them.

**G13, record versions.** `record_versions` always checks visibility:
- a live record must be readable by the caller (tenant clause and access
  predicate, record ACL included, as `get_record`);
- a live record the caller cannot see hides its history;
- a record that is gone (a tombstone, or removed by an engine cascade, which
  writes none) is judged by its **last version that holds data**: on a
  tenant-scoped collection its `_tenant` must be the caller's tenant
  (privileged callers without a tenant see every tenant), and the access
  predicate must hold over it. The predicate runs over a one-row derived
  table that rebuilds the record's columns from the version's data, bound as
  parameters (`SELECT 1 FROM (SELECT ?1 AS "id", …) AS t WHERE <predicate>`);
- a record with only a tombstone (versioning switched on after its last
  write) is visible to unrestricted callers only;
- an unknown id is `record_not_found` for everyone (it answered `[]` to
  unrestricted callers before).
Consequence: the owner of a deleted record keeps its history (it was 404
before for any caller with a rule or a tenant), and nobody else gets it.

**G13, cascade semantics (documented, unchanged).** A delete checks the
caller's Delete rule on the record they delete, never on the records a
cascade reaches. Referencing records are deleted (`cascade_delete`), have the
id pulled out of a multi-value relation, or block the delete (a required
relation left empty) without their collection's Delete/Update rules, field
rules or before-write guards. This is the semantics of PostgreSQL
foreign-key actions (`ON DELETE CASCADE` / `SET NULL` / `RESTRICT`), which
bypass row-level security, so native RLS (FB-152) keeps it. Cascades stay
in the tenant because relations never cross tenants. Multi-value relations
are walked by the data core (change events and tombstones for what it
deletes); single-value relations are engine foreign keys, so the engine
removes or nulls those rows without change events or tombstones.

**G11, SQL editor secrets (data-core part).**
- Secret columns: `password`, `token_key`, and every password field of the
  catalog (lower-case names).
- Results: a column with a secret name reads `"********"` (NULL stays NULL),
  and so does the value of a secret key inside a JSON document held in a text
  cell, at any depth (record versions hold `token_key`; JSON fields may hold
  a `password`), and a secret column of a change snapshot
  (`{"c": [columns], "v": [values]}`, which holds `token_key`). The result
  carries `secrets_masked: true`.
- A statement that names a secret column anywhere (a word of its text:
  identifiers, quoted identifiers, aliases, filters, `'$.token_key'` JSON
  paths, writes such as `UPDATE users SET name = token_key`) is refused with
  403 unless it sets `show_secrets`. This closes aliases, expressions and
  filter oracles. `explain` is not guarded (a plan holds no values).
- `"show_secrets": true` (in the body of `POST /sql` or `/sql/batch`;
  `SqlOptions` with `execute_sql_with` / `execute_sql_batch_with` in Rust,
  so `SqlStatement` is unchanged) returns the values unmasked, lets the
  statements name secret columns, and is audited: one `sql.show_secrets` row in
  `_forgebase_auth_audit` per call (actor, request id, impersonator, realm,
  detail `{statements, sql: [first 10 statements, 500 characters each]}`),
  readable at `GET /api/auth/audit?event=sql.show_secrets`. For a single read
  the row is written after the query and before the result is returned; if
  it cannot be written the call fails. For a batch it commits with the batch.
- Limits, on SQLite: this is a guard in front of an administrator-only tool,
  not a boundary. A name built at run time (`'pass' || 'word'` as a JSON
  path) or a view created with `show_secrets` is not seen. On PostgreSQL the
  boundary is the editor's role without `SELECT` on secret columns (FB-153);
  the masking and the audit stay as the user-facing behaviour.

**For FB-143 (data core on PostgreSQL).**
- New SQL was checked on PostgreSQL 18.3 with the parameter types
  `forgebase-db` declares (Text/Null → `text`, Integer → `int8`): the audit
  insert (`d.now_epoch()`, `CAST(?4 AS d.column_type(Json))` = `jsonb`), the
  live probe, and the derived-table predicate.
- The derived table binds JSON fields as text; on PostgreSQL a predicate
  that compares a `jsonb` column needs the value cast (`CAST(? AS jsonb)` per
  `Storage::Json`) and booleans stay `smallint` 0/1. Port it with the rest of
  the compiler's JSON handling.
- `table_info` still reads `sqlite_master`/`PRAGMA`: switch it to
  `Dialect::introspect_*` and keep introspection to the project schema, so
  `pg_catalog` and `information_schema` are never described; the
  `is_system_table` name check stays in front.
- The secret-name guard is lexical and backend-neutral; PostgreSQL's
  case-sensitive quoted identifiers do not change it (names compare
  lower-case).

**Open, outside FB-135's files.**
- OpenAPI: `SqlStatementBody` and `SqlResultSchema`
  (`forgebase-api/src/v1/schemas.rs`) should document `show_secrets` and
  `secrets_masked`.
- Dashboard: a "Show secrets" tick in the SQL editor that sends
  `show_secrets` (until then the dashboard shows masked values).
- A cascade that pulls an id out of a multi-value relation bumps the
  referencing record's `_version` without writing a version row (a gap in
  that record's history, not a visibility issue).

## Addendum: FB-133, the PostgreSQL system schema and migration runner (2026-11-11)

Built as §7.6 describes. What the next packages need to know, and where the
code differs from the text above.

**Files.** `migrations/postgres/0025_baseline.sql` (the schema SQLite has
after `0025`: 145 tables in schema `forgebase` plus the bookkeeping table,
117 named indexes plus one GIN index, 80 foreign keys, 22 triggers, the 12
seed rows); the runner
`crates/forgebase-migrations/src/postgres.rs`; the parity test
`crates/forgebase-migrations/tests/schema_parity.rs`. The baseline was drafted
mechanically from a SQLite database migrated to the head (`PRAGMA` data plus
each table's stored `CREATE TABLE` text, so every inline comment survives) and
then reviewed by hand; it applies the §7.4 type mapping exactly.

**Bookkeeping.** `forgebase._forgebase_migrations` has SQLite's columns. The
baseline records one row per SQLite version it stands for (1–9 and 11–25;
there is no 10), each with **the SQLite migration's name** and the baseline's
checksum. §7.6 said "name and checksum of the PostgreSQL file"; recording the
SQLite names keeps `forgebase migrate --status` identical on both backends,
and the checksum still pins the PostgreSQL file. A database where the baseline
is only partly recorded is refused.

**Runner** (`forgebase_migrations::postgres`): `migrate(conn)`,
`status(conn)`, `PgMigrator::{system, new(migrations, covers), status, run,
latest_version}`, `ensure_search_path(conn)`, `pairing_problems()`,
`POSTGRES_MIGRATIONS`, `BASELINE_VERSION`, `ROWID_TABLES`, `SCHEMA`,
`SEARCH_PATH`, `OWNER_ROLE`. Each step is one transaction that takes
`pg_advisory_xact_lock(0x66626d6967726174)`, then `SET LOCAL ROLE
forgebase_owner` when that role exists and the session user is a member,
then `SET LOCAL search_path = forgebase, public`, re-checks the bookkeeping,
runs the file (simple protocol, several statements) and records it. DDL is
transactional, so a failed migration leaves nothing; a deferred constraint
that fails at `COMMIT` is reported as that migration's failure. Two runners
racing apply each migration once (tested). There is no `user_version`
mirror on PostgreSQL.

**search_path.** §7.6 has the roles carry `search_path = forgebase,
public`. FB-132 sets it on the bundled engine's roles; for everything else
`ensure_search_path` (run at the end of every `migrate`) checks the session
user's default (`pg_settings.reset_val`) and, when it lacks `forgebase`,
and the role and database settings in `pg_db_role_setting` lack it too,
runs `ALTER ROLE <session_user> IN DATABASE <db> SET search_path =
forgebase, public` (an ordinary role may change its own defaults; scoped to
the one database so an operator's other databases are untouched), sets it on
the current session and closes the idle read-pool connections opened before
the change. Tested with an ordinary, non-superuser role that owns its
database (the external-server case): it migrates, owns the tables, and its
new sessions find the system tables unqualified. *Test engine:* copies of a
template database do not inherit per-database settings, so
`forgebase_db::testing` sets the search_path on its role
(`ALTER ROLE CURRENT_USER SET …`, the stand-in for FB-132's roles), now
fails a test when migration fails instead of leaving the database empty, and
copies the template with `STRATEGY FILE_COPY` (the migrated template has
about 450 relations: 0.7 s per test database instead of 3 s on the build
machine).

**Objects.** `CREATE EXTENSION IF NOT EXISTS citext` and `unaccent` `WITH
SCHEMA forgebase` (both are trusted extensions: a database owner may create
them without being a superuser). `_forgebase_unaccent(text)` is created by
a `DO` block that names the schema the `unaccent` extension actually lives
in, so it also works where `unaccent` was already installed elsewhere and
under any caller `search_path` (FB-131's `PG_FULLTEXT_SETUP` text resolves
`unaccent` through the caller's `search_path`; it is not needed at runtime
now). Function bodies name `forgebase.` for the same reason.
- **Triggers**: the 16 `RAISE(ABORT, msg)` triggers call one function,
  `forgebase._forgebase_abort()`, with the message as the trigger argument
  (SQLSTATE `P0001`, message unchanged, so FB-131's error mapping keeps it).
  The three invoice-line freeze triggers share
  `_forgebase_billing_lines_frozen()`: PostgreSQL trigger `WHEN` clauses
  cannot hold a subquery, so the function reads the invoice's status.
- **Vector keyword index**: SQLite's contentless FTS5 table
  `_forgebase_vector_fts` becomes a table `(rowid bigint PRIMARY KEY, content
  text, fts tsvector GENERATED ALWAYS AS (to_tsvector('simple',
  _forgebase_unaccent(content))) STORED)` with GIN index
  `_forgebase_vector_fts_gin`, kept in sync by the same three triggers (one
  function). FB-147 replaces `MATCH`/`bm25` in `forgebase-vector/src/store.rs`
  with `fts @@ websearch_to_tsquery(…)` / the dialect's full-text methods; the
  join `i.key = _forgebase_vector_fts.rowid` works as it is.
- **rowid**: `_forgebase_jobs` (the only system table the code orders by
  `rowid`) has `_rowid bigint GENERATED BY DEFAULT AS IDENTITY NOT NULL`
  (`postgres::ROWID_TABLES`). FB-146 orders by `Dialect::row_order_col()`;
  FB-151 copies SQLite's `rowid` into it and `setval`s it.
- The 17 `INTEGER PRIMARY KEY AUTOINCREMENT` columns are `bigint GENERATED
  BY DEFAULT AS IDENTITY`: explicit ids still insert, so FB-151 must
  `setval` each identity after copying.
- `DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))` is
  `to_char(now() AT TIME ZONE 'UTC', 'YYYY-MM-DD"T"HH24:MI:SS.MS"Z"')`;
  `phone GLOB '+[1-9]*'` is `phone ~ '^[+][1-9]'`; `LIKE '$argon2id$%'`
  stays (case-sensitive on PostgreSQL; argon2 hashes are lowercase).
- Seed rows take their times when the baseline runs, as SQLite's
  `unixepoch()` does.

**Twins.** `0026_resource_rules` (FB-134) and `0027_functions_js` (FB-93)
reached `main` while FB-133 was in flight; their PostgreSQL twins are in
`migrations/postgres/` (FB-133 merged `main` to write them).

**Pairing.** `every_sqlite_migration_has_a_postgres_twin` (a unit test in the
crate, repeated in the parity test, needs no engine) fails with e.g.
"SQLite migration migrations/system/0026_functions_js.sql has no PostgreSQL
twin: add migrations/postgres/0026_functions_js.sql and register it in
POSTGRES_MIGRATIONS". It also refuses a PostgreSQL migration without a SQLite
twin, a name mismatch, and a PostgreSQL migration older than the baseline.

**Schema parity** compares, per table: columns through the type mapping
(booleans by their `IN (0, 1)` check, JSON by `json_valid`, NOCASE as
`citext`, rowid aliases as identity), nullability (primary-key columns count
as `NOT NULL`), defaults (casts, quotes, parentheses and case normalised; the
two "now as text" defaults are equal), primary keys, unique constraints,
foreign keys (columns, target, `ON UPDATE`/`ON DELETE`; every PostgreSQL FK
must be `DEFERRABLE INITIALLY IMMEDIATE`), named indexes (uniqueness, columns
or expressions, partial predicate), the **number** of check constraints
(SQLite's checks have no names, and PostgreSQL rewrites `BETWEEN`/`IN`, so
"by name" became "by count"; pure `json_valid` checks are excluded because
`jsonb` enforces them), trigger names, row counts, and the rows of every
table with at most 50 rows (seed data; time columns excluded, JSON compared
as values) plus the recorded versions and names. Known differences are
listed in the test: `_forgebase_vector_fts` (compared for existence), FTS5's
shadow tables (SQLite only) and `_rowid`.

**Shared edits outside Owns** (all in `forgebase-db`): `PgDatabase::migrate`
and `migration_status` (the planned one), `testing.rs` (role `search_path`,
strict migrate), `lib.rs` (doc of `Database::migrate`), and
`tests/backends.rs` (the PostgreSQL test database is migrated now).

**Not done here.** The pre-migration snapshot (`pg_basebackup`, managed mode)
needs the engine's binaries, which `forgebase-db` does not have: `migrate`
logs a warning and returns `backup: None` (FB-150, with FB-132's engine
handle). Text columns use the database's default collation; the bundled
engine's `builtin` C.UTF-8 sorts as SQLite does, an external server with a
libc locale may not (FB-148 may refuse or warn).

## Addendum: FB-130, the engine build pipeline and the relocation spike (2026-11-11)

Built as §2.2–§2.4 and ADR-0029 §1.3 describe, to the archive format of the
FB-132 addendum. Files: `.github/workflows/postgres-engine.yml`,
`deploy/postgres-engine/` (shared by CI and local runs; its `README.md`
explains each script), `engine.lock`, `THIRD_PARTY_LICENSES/postgres/`.

**Pinned** (all in `engine.lock` with upstream URL and SHA-256; each
download is checked before it is unpacked): PostgreSQL 18.6 (ftp.postgresql.org;
the SHA-256 matches the upstream `.sha256`), pgvector 0.8.7, pg_cron 1.6.8,
pgmq 1.13.1 (GitHub tag tarballs), OpenSSL 3.5.9 (LTS), zlib 1.4, lz4
1.11.1, zstd 1.5.7, libedit 20260512-3.1; from the Alpine 3.20 build image:
musl 1.2.5 (the shipped loader) and ncurses 6.4 (static, through libedit).

**Measured, aarch64-apple-darwin** (built and tested on this machine; the
estimates of §2.2 and §3.2 are replaced by these):

| | Estimate | Measured |
|---|---|---|
| Archive (`tar` + zstd level 19) | 10–14 MB | **7,661,698 bytes (7.3 MiB)** |
| Unpacked | 30–35 MB | **30,097,121 bytes (28.7 MiB) in 938 files** (32 MiB on disk with 4 KiB blocks) |
| Programs (`bin/`, 14) | 14 MB | 15.4 MiB (`postgres` 9.0 MiB) |
| Libraries and modules (`lib/`, 46) | 6 MB + 5 MB OpenSSL | 10.8 MiB (libcrypto 4.5, libssl 0.9, libpq 0.3, pgvector 0.18, pg_cron 0.09) |
| `share/` | 2 MB | 2.4 MiB (extension SQL and control files 0.7, time zones 0.3 in 598 files, licences) |
| Idle RSS after start, all extensions created | — | **about 73 MiB (74,416 and 74,720 KiB in two runs, 41,888 KiB in a third; the postmaster alone 25 MiB)** (sum over postmaster and its 9 children, which counts shared memory once per process; `shared_buffers = 128MB`) |

The other targets are pending CI; the workflow writes the same three
numbers into each job summary. Size per target is expected to be close to
these (Linux adds the 0.7 MB musl loader, zlib and libedit are linked in
statically; Windows adds the zlib, lz4 and zstd DLLs).

**Verified locally (aarch64-apple-darwin).**
- `build-unix.sh` builds from the pinned sources end to end in about 5
  minutes on 8 jobs, and a second clean build in a different work
  directory gives the same archive bytes: **SHA-256 `c5a23a423b28d934a41688d0b3947260fa50d32cea2af087c132c7d39ced5396` both times, now pinned in `engine.lock`**.
- `engine.py extract` (the checks of `forgebase_pgengine::archive::extract`)
  extracts it elsewhere, and `smoke.sh` passes: every program runs;
  `initdb`; start with `shared_preload_libraries = 'pg_stat_statements,pg_cron'`
  and the ADR-0029 §1.3 settings; `CREATE EXTENSION` for vector, pg_trgm,
  pgmq, pg_cron and the 12 contrib modules; an HNSW query that uses the
  index; pg_trgm, pgcrypto and unaccent; `pgmq.send`/`read`; a pg_cron job
  that fired after 3 s; `ssl_library = OpenSSL`; `pg_dump`/`pg_restore`;
  `pg_basebackup`, `pg_verifybackup` and a hot standby that replays and
  answers a vector query; stop with no FATAL or PANIC lines.
- The FB-132 crate accepts it: with `FORGEBASE_PG_ENGINE_ARCHIVE` and
  `--features bundled-postgres`, `build.rs` accepts it against the
  committed `engine.lock` (and refuses a different file), and `embedded_engine_is_extracted_and_runs` passes (extracted by the
  Rust extractor, started with pg_cron preloaded, bootstrap and roles); with
  `FORGEBASE_PG_ENGINE_DIR` pointing at the extracted engine, 6 of the other
  7 integration tests pass. The seventh,
  `engine_from_an_archive_is_extracted_once_and_old_minors_are_removed`,
  packs only `bin/postgres` and `bin/initdb` from the engine directory, which
  works for Homebrew (absolute library paths) but not for a relocatable
  engine whose `libssl` sits in `lib/`; the test should pack `bin lib share`.
- macOS relocation works: the binaries name `@loader_path/../lib/…`, depend
  only on `/usr/lib` (libSystem, libz, libedit) otherwise, and are ad-hoc
  signed again after `install_name_tool` (`codesign --verify` passes); the
  compiled prefix `/forgebase-postgres` does not exist on the host, so the
  run proves PostgreSQL resolves `share/` and `lib/` relative to itself.

**Only CI can verify** (the workflow does each; nothing here is proven yet):
- **Linux musl-loader relocation**, on the runner (Ubuntu 24.04) and in
  `distros.sh`: Debian 12, Ubuntu 22.04 (glibc 2.35), UBI 9 (glibc 2.34),
  Alpine 3.20 and `distroless/static-debian12` (no C library at all), as an
  unprivileged user, for x86_64 and aarch64. No Linux engine was built or run
  on this machine (no Linux host or Docker daemon).
- The in-place `PT_INTERP` rewrite on real binaries (`engine.py extract`
  mirrors `interp.rs`; both were tested only on synthetic ELF headers), the
  relink pass that gives the shipped executables the 255-byte placeholder,
  and `RUNPATH=$ORIGIN/../lib` (the build fails when any of these is wrong).
- x86_64-apple-darwin (native `macos-15-intel` runner; this machine has no
  Rosetta 2) and Windows (meson + MSVC, `Makefile.win`): `build-windows.ps1`
  and `smoke-windows.ps1` were written without a Windows machine (and
  without PowerShell, so not even parsed here) and are the likeliest to
  need a fix on the first run (dependency discovery for
  OpenSSL, zlib, lz4 and zstd in meson; pg_cron's `Makefile.win` links
  `libintl.lib`, which the script overrides).

**Relocation decision, as far as the evidence goes.** Keep §2.3: musl
loader shipped in `lib/`, placeholder interpreter, `$ORIGIN` RUNPATH. It is
**unproven on Linux** until the first workflow run; the decision becomes
final when every row of the distro matrix passes on both architectures. If
any glibc row fails, switch to the documented fallback (glibc 2.28,
manylinux_2_28 image) for that architecture and record which row failed.
macOS relocation is proven for arm64.

**Differs from the text above.**
- *OpenSSL is shared* (`lib/libssl`, `lib/libcrypto`), not static: static
  linking would put three copies of OpenSSL into one server process
  (`postgres`, `libpq` loaded by `libpqwalreceiver`, `pgcrypto`), whose
  global state can split when symbols interpose. zlib, lz4, zstd and (Linux)
  libedit are static. macOS uses the OS zlib and libedit (stable system
  libraries); Windows ships DLLs next to the executables.
- *RPATH*: Linux keeps PostgreSQL's rpath switch on with
  `rpathdir=$ORIGIN/../lib`, which works from `bin/` and from `lib/`,
  instead of `--disable-rpath` plus hand-written flags.
- *Placeholder interpreter*: the build links everything with musl's normal
  loader (it runs `zic` and `pg_config`), then relinks the shipped
  executables with `-Wl,--dynamic-linker=/forgebase-pg-loader-xxx…` (255
  bytes).
- *Programs*: also `pg_waldump` (run by `pg_verifybackup`), `pg_resetwal`
  and `pg_dumpall` (run by `pg_upgrade`, FB-154). pg_cron is shipped
  (ADR-0029 supersedes §1's "not shipped").
- *Build path*: the build runs through `/tmp/forgebase-engine-<target>`, a
  link to the real work directory, because `postgres` embeds pg_config's
  configure arguments and flags; without it the archive bytes depend on the
  work directory.
- *Windows* `psql` has no line editing (no libedit for MSVC).
- *Extraction time*: the FB-132 extractor calls `sync_all` on each of the
  938 files; on this machine's external SSD (macOS `F_FULLFSYNC`) the
  first extraction took about 4 minutes. One `fsync` of the directory tree
  after writing, or `fsync` without `F_FULLFSYNC`, would cut that (FB-132
  follow-up; 598 of the files are time-zone data).

**For the release workflow (FB-149).** Download the engine release named by
`engine.lock` `engine.release` (`gh release download engine-pg18.6-r1 -p
'forgebase-pg18.6-r1-<target>.tar.zst'`), set
`FORGEBASE_PG_ENGINE_ARCHIVE` to it and build with the `bundled-postgres`
feature (the `cross` builds need the variable and the file passed into the
container), plus a `-lite` build without it. Before that, replace the
`[archives.*]` tables of `engine.lock` with the release's
`engine.lock.archives` asset: CI-built archives differ in bytes from this
machine's (other toolchain), so the local aarch64-apple-darwin hash only
serves local builds until then.

**For FB-160 (Apache AGE).** `contents.extensions` in `engine.lock` drives
the build; `deploy/postgres-engine/extensions/README.md` describes the hook
(patches, extra make arguments, `post-install.sh` for `lib/plugins/`, or a
`build.sh` that replaces the PGXS build) and what else AGE needs (source pin,
licence and `NOTICE`, `graph_cypher` flag, smoke checks).

## Addendum (FB-138): key-value and RESP tenancy and namespace rules

Written 2026-11-11 by FB-138. Closes `rls-coverage.md` G6, G7 and G8 for
the built-in KV engine, its REST API and the RESP listener (and the
optional external-Redis backend). Backend-neutral: the only SQL is the key
binding column and the validation reads below, valid on both engines.

**Tenant binding of keys (G6).** `_forgebase_auth_api_keys.tenant`
(migration 0028 pair, NULL = unbound). `POST /api/auth/tokens` and
`POST /api/auth/service-accounts/{id}/keys` take an optional `tenant`
(`AuthService::create_pat_with` / `create_api_key_with` with
`KeyOptions::with_tenant`). The binding is validated with the conditions
of policy step 2 for the key's **owner**, at creation and at every use
(`AuthService::authenticate_key_bound`, one or two reads): the
environment exists, its project is in the owner's home realm, and the
owner is an active member of the project's organization, owns the
personal project, or holds an allow entry naming them on the environment
or project; administrators and admin service accounts only need the
tenant to exist. A key whose owner left the organization is refused like
a revoked one (`401`). Unknown and foreign tenants give the same `403` at
creation, so creation is no existence oracle. Over HTTP the auth layer
selects the bound tenant (it sets `X-ForgeBase-Tenant`, so the tenancy
layer runs step 2 as for any request) and refuses another one with
`403 tenant_mismatch`; `/api/auth/me` shows `tenant`. RESP sessions run
in the key's home realm (they used `default` before) and bound tenant;
`still_valid` re-checks the binding every 30 s. `authenticate_key` keeps
its signature and drops the binding: realtime in-band authentication and
functions' `ctx.auth.verify` use it and do not apply bindings yet (FB-136,
FB-142).

**Scopes (G6).** A KV scope is `(tenant, namespace)`:
- `t:<tenant>` for a caller with a tenant;
- `shared:<realm>` for namespaces named `shared.<name>`: realm-wide, the
  same keys with or without a tenant (the realm's tenants share them by
  design). Reached with `engine:kv_<action>:shared.<name>` or the plain key;
- `default` (the pre-FB-138 shared scope, now global per instance) for
  privileged callers without a tenant only, so admin tools and functions'
  system context keep their data;
- anything else is `400 tenant_required` (RESP: `-NOPERM tenant_required`),
  checked after authorisation so a denied caller learns nothing.
`fn.<name>` namespaces (functions' private data, edge-functions.md §2)
are refused to non-privileged callers; functions reach them as the system
context in the caller's tenant, unchanged. `GET /api/v1/kv/_namespaces`
lists the caller's tenant (or `default`) plus the realm's `shared.*`.

**Namespace permissions and rules (G7).** Every command asks
`Authorizer::authorize_resource(ctx, KvRead|KvWrite|KvAdmin,
kv_namespace <ns>, op)` once per needed pair (`KvService::command_needs`,
public so the external backend applies the same mapping): `KEYS`, `SCAN`,
`DBSIZE` and channel listings are `list`; other reads and subscriptions
`view`; `DEL`, `UNLINK`, `GETDEL`, `FLUSHDB` `delete`; `PUBLISH` `create`;
writes that may add a key are `create` for a key that does not exist yet
and `update` for one that does (checked under the store lock; the
external backend asks both); other writes `update`. Listing namespaces and
statistics keep the plain `authorize` (only the plain key grants them).
KV registers a `kv_namespace` schema with **no item fields**: rules are
namespace-level (`@request` only), a rule naming `key` is refused when
saved, and a `Filter` decision can never be needed (it would be a denial).

**Per-tenant statistics and budget (G8).** The store keeps keys, memory,
hits, misses, expiries and evictions per scope tenant and a per-tenant LRU
index. `GET /api/v1/kv/_info` (`kv_admin`) answers `scope: tenant` with
the figures of the caller's tenant when a tenant is selected, and `scope:
engine` (every tenant) only for a privileged caller without one;
`KvStats` gained `scope` and `tenant`. `[kv] tenant_max_bytes`
(`FORGEBASE_KV_TENANT_MAX_BYTES`, default `0` = none) caps one scope
tenant; above it that tenant's own least recently used keys are evicted
by the eviction policy (`noeviction` refuses that tenant's growing writes
with `OOM`), never another tenant's. The global `max_memory_bytes`
eviction still picks across tenants; per-tenant budgets are the defence.

**Breaking.** Non-privileged callers without a tenant (users, non-admin
service accounts, unbound keys over RESP) lose the shared `default`
scope: they select a tenant, bind their key, or move the data to a
`shared.<name>` namespace. Existing keys in `default` stay readable by
administrators (dashboard KV studio) for copying.

**For FB-145 (KV on PostgreSQL).** The persisted row keeps the scope
tenant string (`t:<id>`, `shared:<realm>`, `default`) as today; per-tenant
accounting is in memory and rebuilt at load. If KV storage moves to a
PostgreSQL table with native RLS, `shared:<realm>` rows need a realm
column or prefix match, and `default` rows are privileged-only. The
binding column is plain `text` on both engines. **For the dashboard KV
studio (FB-100).** Show `scope`/`tenant` from `_info`, list `shared.*`
next to tenant namespaces, offer `kv_namespace` rules with `@request`
only (no item fields), and offer a tenant when creating keys and PATs.

## Addendum: FB-143, query compiler, records, lists and SQL editor on PostgreSQL (2026-11-11)

Built as §7.4 and §7.5 describe for `forgebase-query` and the data core's
records, lists and SQL editor. What the next packages need to know, and where
the code differs from the text above.

**Files.** `crates/forgebase-query/src/{compile.rs,sql.rs,lib.rs}`;
`crates/forgebase-data/src/{list.rs,records.rs,sql.rs,changes.rs}`; the data
tests `tests/{common/mod.rs,scenarios.rs,security.rs,send_futures.rs}`;
`forgebase-api/src/v1/schemas.rs` (OpenAPI). `guard.rs` needed no change.
Shared edits are listed at the end.

**Compiler: typed operands.** `Compiler::scalar_typed` returns the SQL and
a `ScalarType` (`Text`, `Integer`, `Real`, `Bool`, `Json`). SQLite is
dynamically typed, so its output is byte for byte what it was (the existing
tests pin it), except `!~`, now `NOT (coalesce(v, '') LIKE p ESCAPE '\')`
on both engines (same meaning). On PostgreSQL:
- **JSON paths are `jsonb`**: `nullif((expr)::jsonb->'a'->0->'b',
  'null'::jsonb)`. *Differs from §7.5 and FB-131's `json_extract`*
  (`->>`, text): a text extraction sorts `'10'` before `'9'` and cannot be
  compared with a number parameter. `jsonb` compares and sorts numbers as
  numbers and strings as strings; `nullif` turns the `'null'::jsonb` that
  `->` returns for JSON `null` into SQL NULL, like a missing key, as
  `json_extract` does. The `::jsonb` cast is a no-op on `jsonb` columns and
  accepts JSON kept in `text` (the collection DDL still declares `_data` as
  `TEXT`, see below). Expression indexes built through `Compiler::scalar`
  (FB-144's index manager does this) get the same expression, so they match;
  the `((_data->'metadata'->>'brand'))` example of §7.5 is superseded.
- **Constants against a JSON value** are `jsonb` values: `CAST(? AS jsonb)`
  with the constant's JSON text bound (`5`, `"x"`, `true`), or
  `'…'::jsonb` in index definitions. Literals keep their kind, so `= true`
  matches JSON `true` (SQLite: `json_extract` gives 1 and `1 = 1`).
- **Other constants** are parameters the server types from the column; a
  fraction against an integer or boolean column is
  `CAST(? AS double precision)` (an `int8` parameter would refuse 2.5).
- **Array items** iterate as `jsonb`:
  `jsonb_array_elements(CASE WHEN jsonb_typeof((c)::jsonb) = 'array' THEN
  (c)::jsonb END) AS j1(value)` (a non-array or NULL yields no rows instead of
  an error); select, file and relation values (strings) use
  `Dialect::json_array_elements` (text).
- **`~`** is `Dialect::like_ci` (`ILIKE … ESCAPE '\'`) over text: JSON values
  through `#>> '{}'`, numbers and booleans through `CAST(… AS text)`;
  `:lower` and `:length` convert the same way.
- **Path to path**: a JSON value against anything else compares as `jsonb`
  (`to_jsonb(other)`), text against a number as text.
- **Sort keys** use `Dialect::order_nulls` (`ASC NULLS FIRST`,
  `DESC NULLS LAST`), SQLite's order, which keyset cursors rely on.
- Differences that remain (documented, asserted per engine where tested):
  across JSON types `jsonb` orders strings before numbers before booleans,
  SQLite orders numbers before text; JSON `true` does not equal `1`;
  a non-numeric text constant against a number column is a 400 (the server
  cannot bind `'abc'` as `bigint`; SQLite returns no rows); `ILIKE` folds
  what the database's ctype folds (Unicode with the bundled engine's builtin
  `C.UTF-8`, ASCII with libc `C`, which the test engine uses).

**SQL tokenizer (`forgebase_query::sql`).** `SqlFlavor` (`Sqlite`,
`Postgres`; `SqlFlavor::of(dialect)`), `tokenize_for`,
`split_statements_for`, `analyze_statement_for`; the old functions are the
SQLite flavour (FB-144's view checks in `collections.rs` still call them).
PostgreSQL lexing: `$tag$…$tag$` and `$$…$$` strings (a `DollarString`
token), `E'…'` escapes, nested `/* */` comments (must close), `[`, `` ` ``,
`:` and `@` are operators, `$1` and `?N` are parameters; `BEGIN ATOMIC …
END` routine bodies do not split. Classification: `SELECT`, `VALUES`,
`WITH`, `TABLE`, `SHOW`, `EXPLAIN` read; `INSERT`, `UPDATE`, `DELETE`,
`MERGE`, `TRUNCATE`, `CALL` write; `CREATE`, `ALTER`, `DROP`, `COMMENT`
and a top-level `SELECT … INTO` schema; `ANALYZE`, `REINDEX`, `REFRESH`
maintenance. Refused (`statement_not_allowed`), §7.5's list plus:
- transaction control (`BEGIN`, `START`, `COMMIT`, `END`, `ROLLBACK`,
  `ABORT`, `SAVEPOINT`, `RELEASE`, `PREPARE`);
- **every** `SET`, `RESET` and `DISCARD`, not only the role forms: the
  writer is one long-lived connection, so a session setting would change every
  later ForgeBase statement;
- `COPY` (all forms), `ALTER SYSTEM`, `LOAD`, `LISTEN`/`NOTIFY`/`UNLISTEN`,
  `VACUUM`, `CHECKPOINT`, `CLUSTER`, `DO` (anonymous blocks would hide the
  function list), `GRANT`, `REVOKE`, `REASSIGN`, `SECURITY LABEL`,
  `EXECUTE`/`DEALLOCATE`, cursors, `LOCK`, `IMPORT FOREIGN SCHEMA`,
  `EXPLAIN ANALYZE` (it runs the statement), `PRAGMA`, `ATTACH`;
- creating, altering or dropping extensions, roles/users/groups, databases,
  tablespaces, schemas, subscriptions, publications, servers, foreign data
  wrappers, event triggers, languages, default privileges, access methods,
  transforms, large objects; temporary objects; `LANGUAGE c` / `internal`;
  `SECURITY DEFINER`; `OWNER TO`;
- calls (also quoted or schema-qualified, also inside dollar-quoted routine
  bodies) of `pg_read_file`, `pg_read_binary_file`, `pg_ls_*`,
  `pg_stat_file`, `pg_file_*`, `lo_import`, `lo_export`,
  `pg_terminate_backend`, `pg_cancel_backend`, `pg_reload_conf`,
  `pg_rotate_logfile`, `pg_promote`, `pg_switch_wal`, `pg_backup_*`,
  replication and logical-decoding functions, `dblink*`, every
  `set_config` (not only `forgebase.*`), `pg_advisory*`/`pg_try_advisory*`
  (session locks on the shared writer), `pg_sleep*` (the single writer),
  and the functions that run a query given as text (`query_to_xml*`,
  `cursor_to_xml*`, `ts_stat`), which would hide everything else.
`touches_system_tables` on PostgreSQL: `_forgebase*` names, names qualified by
`forgebase.`, `pg_catalog.`, `information_schema.` or `pg_toast.`, and
`pg_*` relation names (not calls). As §7.5 says, this list is defence in
depth; the boundary is the editor's role (FB-153).

**SQL editor service.** Reads run on the read pool in `READ ONLY`
transactions (the reader role in managed mode): a write the classifier cannot
see, such as a `SELECT` of a function that inserts, fails with
`statement_not_read_only` and writes nothing (tested). `TABLE` reads are
wrapped with the row limit like `SELECT`. Every editor write transaction on
PostgreSQL starts with `SET LOCAL search_path = public, forgebase`, so
unqualified tables, views and functions the editor creates land in `public`,
never in ForgeBase's schema, and ForgeBase's own statements in the same
transaction still resolve (tested). `last_insert_id` is `None` on
PostgreSQL (`RETURNING` returns the key; the result then carries rows).
`list_tables` and `describe_table` use `Dialect::introspect_tables`,
`introspect_columns` and `introspect_indexes` on PostgreSQL, so only the
schemas on ForgeBase's search path (`forgebase`, `public`;
`current_schemas(false)`) are visible, never `pg_catalog` or
`information_schema` (tested); an exact name wins over a case-insensitive
match (quoted names are case-sensitive there); `is_system_table` still runs
first. The SQLite path is unchanged (`sqlite_master`). OpenAPI:
`SqlStatementBody.show_secrets`, `SqlBatchBody.show_secrets` and
`SqlResultSchema.secrets_masked` are documented.

**Lists and aggregates.** Full-text search goes through
`Dialect::fulltext_match("t", "_forgebase_fts_<index>", ?)`: on PostgreSQL
that name is the `tsvector` column FB-144 creates, so FB-144 must keep it (or
change `list.rs` with it); the query text is passed to `websearch_to_tsquery`
as the user's words (it never fails), on SQLite each word is quoted for FTS5
as before. Keyset cursors use `Dialect::null_safe_eq`; a JSON sort key's
cursor value is the JSON text the previous page returned, which binds back as
`jsonb`. Aggregates on PostgreSQL: `sum`/`avg` over JSON values sum the JSON
numbers (`numeric`), a whole sum reads as an integer as on SQLite; `min`/`max`
over JSON use `jsonb` order (`(array_agg(x ORDER BY x) FILTER (WHERE x IS NOT
NULL))[1]`; `jsonb` has no `min`); integer sums are `CAST(sum(x) AS bigint)`
(PostgreSQL sums `bigint` as `numeric`); JSON group keys come back as JSON;
groups order `NULLS FIRST`.

**Records, versions, change snapshots.** The multi-value relation pull on
delete uses `Dialect::json_array_elements`, `json_array_agg`,
`json_array_length` and `null_safe_ne`. The one-row derived tables that run
access rules over a stored row (`snapshot_visible` for record history,
`view_snapshot` for realtime) cast every column to its table type
(`records::column_sql_type`: `Dialect::column_type`, so `jsonb`,
`smallint`, `bigint`, `double precision` on PostgreSQL and the SQLite types
as before), as FB-135 asked. Inserts, updates, expand and versions needed no
change: `forgebase-db` binds text into `jsonb` and integers into `smallint`.

**Tests.** `forgebase-data`'s `tests/common` gives `test_db()` (=
`forgebase_db::testing::test_backend()`), `reopen(&TestDb)`,
`is_postgres`, `skip_on_postgres(db, why)`; `open_db(dir)` stays, SQLite
only, for `schema_changes.rs` until FB-144 moves it. New on both backends:
`semantics_shared_by_both_engines` (§7.4: NULL order with offset and cursor
pages ascending and descending, typed JSON comparison and sort with cursors,
JSON null, booleans, array items, `~`, JSON round trip, aggregate result
types, a NOCASE/`citext` column, quoted identifiers, introspection limits) and
`rules_over_stored_rows_see_typed_columns`; scenario 2 runs PostgreSQL SQL
(identity keys, a PL/pgSQL trigger, `RETURNING`) on PostgreSQL; the editor
restrictions test runs the PostgreSQL deny list there. Skipped on PostgreSQL
until FB-144 ports the index manager: scenarios 5 and 6 and the index half of
`names_and_index_definitions_cannot_inject` (they call
`skip_on_postgres` with the reason). Identity sequences after convert
(FB-151) and commit-ordered cursors (FB-146) are not in these crates.
`forgebase-api` and `forgebase-server` test helpers open SQLite whatever
`FORGEBASE_TEST_BACKEND` says (FB-148 owns them), so their suites ran on
SQLite under both settings.

**Shared edits outside Owns** (minimal; FB-144 owns the files):
`forgebase-data/src/ddl.rs` (`STRICT` only on SQLite;
`verify_collections` reads columns through `Dialect::introspect_columns`;
`object_exists(d, tx, name)` asks `pg_class` on PostgreSQL),
`collections.rs` and `indexes.rs` (the `object_exists` call sites, and
`schema_history`'s `CAST(?1 AS TEXT) IS NULL OR collection_id = ?1`:
PostgreSQL cannot type `?1 IS NULL OR c = ?1`, SQLSTATE 42P18, and
`forgebase-db`'s parameter learning did not rescue it on that read),
`forgebase-data/Cargo.toml` (dev-dependency `forgebase-db` with
`testing`), `forgebase-api/src/v1/schemas.rs` (OpenAPI).

**For FB-144.** Collection DDL still declares `_data TEXT` (make it
`d.column_type(Json)`, `jsonb`) and `_version INTEGER` (`int4` on
PostgreSQL; `bigint` matches the type mapping), and creates tables without a
schema, so they land in `forgebase` (first on the search path): name
`public`. Build JSON-path index keys through `Compiler::scalar` (they then
match the `jsonb` filters) and keep the full-text column name above. Views
(`infer_view_fields`), index listing and stats, `explain` and the
recommendations still read `PRAGMA`/`sqlite_master`. Un-skip the tests
listed above and switch `schema_changes.rs` to `common::test_db()`. Any
`?N IS NULL OR …` pattern needs a `CAST`.

**For FB-152 (native RLS).** Access predicates compile through the same
`Compiler`, so a policy generated from a rule can reuse
`Compiler::filter` with the PostgreSQL dialect; its JSON paths are `jsonb`
expressions and its constants `CAST(? AS jsonb)` or column-typed parameters
(inline them as literals, `inline_literals`, for `CREATE POLICY`). The
derived-table checks (history of deleted records, realtime snapshots) run
rules outside any table, so RLS on the tables does not replace them.

**For FB-153 (SQL editor roles).** The editor's reads use the read pool and
its writes the writer: give the editor its own role (no `SELECT` on secret
columns, no `pg_read_server_files`/`pg_execute_server_program`, `CREATE` on
`public` only) and keep `SET LOCAL search_path = public, forgebase` (or set
it on that role). The deny list stays as defence in depth; it refuses every
`SET`, `DO` and `GRANT`, so role and privilege changes must go through an
admin API.

## Addendum: FB-144, collections, schema changes, indexes and full text on PostgreSQL (2026-11-11)

Built as §7.4–§7.5 describe, in `crates/forgebase-data/src/{ddl.rs,
collections.rs,indexes.rs,catalog.rs}` and `crates/forgebase-schema`. What
the next packages need to know, and where the code differs from the text
above.

**DDL.** Collection tables use the §7.4 type mapping through
`Dialect::column_type`: `id`, `created`, `updated`, `_tenant` are `text`,
`_version` `bigint`, `_data` and JSON fields `jsonb` (no `CHECK`, `jsonb` is
always valid; SQLite keeps `TEXT CHECK (json_valid(…))`), booleans
`smallint CHECK (x IN (0, 1))`; no `STRICT`. Single relations are `REFERENCES
public."target"(id) ON DELETE … DEFERRABLE INITIALLY IMMEDIATE`. Every
statement that creates, drops or alters a collection object names schema
`public` (`ddl::rel`), because schema `forgebase` comes first on the search
path (FB-133): `CREATE TABLE public."posts"`, `DROP INDEX IF EXISTS
public."idx"` (an unqualified `DROP INDEX` would hit a system index of the
same name first). Reads keep unqualified names; they resolve to `public`
because collection names never start with `_forgebase`.

**Catalog introspection** (`catalog::pg`) reads `pg_catalog` for objects in
schema `public` through `to_regclass('public."name"')`, never through the
search path: columns (`PRAGMA table_info` shape), dependent views (a
recursive walk of `pg_depend`/`pg_rewrite`, optionally from given columns),
single-column `CHECK`/foreign-key constraints, index names and one index's
row. `object_exists` (name taken?) checks both schemas on the search path,
so a collection or index can never shadow a system object or be shadowed.

**Schema changes** are `ALTER TABLE` statements in the existing
`SchemaChange` transaction; the SQLite rebuild (`_forgebase_tmp_…`, row copy,
`legacy_alter_table`) is untouched. Renames are `RENAME TO` / `RENAME
COLUMN`; added fields `ADD COLUMN`; dropped fields `DROP COLUMN`; a field
whose column signature changes (storage class, check, foreign key) gets, in
order: its single-column `CHECK` and foreign-key constraints dropped (found in
`pg_constraint`), `ALTER COLUMN … TYPE … USING <conversion>` when the storage
class changes, the new `CHECK`, and the new foreign key (checked against the
existing rows at once; a missing target is `foreign_key_violation`, the code
SQLite's commit-time check gives). Conversions keep SQLite's `STRICT` copy
semantics: a value that does not fit fails the statement
(`incompatible_data`) rather than being rounded: text and reals become
integers through `numeric` and only when integral (`'2.5'` is refused,
`3.0` becomes 3); integral reals become text as `3.0`; text becomes `jsonb`
by a cast that rejects invalid JSON; numbers become JSON with `to_jsonb`.
*Differs:* JSON turned into text is PostgreSQL's `jsonb` text
(`{"x": [1, 2]}`, with spaces) where SQLite keeps the stored compact text;
the documents are equal. Managed indexes (automatic and manual) that use a
changed field are dropped before and created again from their definitions
after (an expression may not fit the new type); other indexes, and raw-SQL
indexes and triggers, stay as they are (no rebuild drops them).

**Views** are `CREATE VIEW public."v" WITH (security_invoker = true) AS …`.
PostgreSQL binds views to columns, so where SQLite lets a change break a view
and the verification step catches it, PostgreSQL needs explicit handling:
a type change on a column views read drops those views (and the views
reading them, transitively), changes the column and creates them again from
`pg_get_viewdef` with their `reloptions` (so `security_invoker` stays);
a view that no longer compiles is `schema_conflict`. Dropping a column, a
table or a view that a view reads is `schema_conflict` up front (found in
`pg_depend`), the same code SQLite's verification gives. Changing a view
collection's query does the same drop-and-restore for views reading it.
*Note for FB-152:* recreated views lose grants, policies and `INSTEAD OF`
triggers attached to them; the regeneration hook must run after the column
changes, inside the same transaction (`update_collection`, after
`alter_table`, before `verify_collections`). View fields are inferred from
`pg_attribute` types: integer types → integer number, `double
precision`/`numeric`/`real` → number, `text`/`varchar`/`citext` → text,
`boolean` → bool, `uuid`/date and time types/network types → text, anything
else → JSON.

**Verification** runs as before, after every change, inside the transaction:
columns from the catalog (`pg_attribute` on PostgreSQL), compared ignoring
case, plus `SELECT * … LIMIT 0` for views.

**Identifier length.** Derived names go through `Dialect::fit_ident` (63
bytes on PostgreSQL, a 10-hex FNV-1a suffix when longer; no change on
SQLite): automatic index names `auto_<collection id>_<field id>_u|r` and
`auto_<collection id>_tenant` (ids may be 64 characters), and full-text
index objects `_forgebase_fts_<index>` (index names may use all 63 bytes).
The fitted name is the one stored in `_forgebase_indexes`, so lookups by
name match the engine. Index names are now refused when longer than 63
bytes or starting with `pg_` (both backends).

**Full text.** *Differs from §7.4:* not a generated `tsvector` column but a
GIN expression index, `CREATE INDEX "_forgebase_fts_<index>" ON public."t"
USING gin ((to_tsvector('simple', _forgebase_unaccent(coalesce((k1)::text,
'') || ' ' || …))))`, matched with the same expression `@@
websearch_to_tsquery('simple', _forgebase_unaccent($n))`
(`indexes::fulltext_match`, which list queries call). Why: a generated
column is a real table column, so `SELECT t.*` would put it into every record
(`row_to_record` keeps unknown columns), every change snapshot and every
record version; adding it rewrites the table under an exclusive lock; and
PostgreSQL refuses to change the type of a column a generated column uses.
The expression index has none of these, needs no triggers or sync, follows
column renames and is dropped with its table. `FB-131`'s
`Dialect::fulltext_*` PostgreSQL methods (generated column) are not used by
the data core. Ranking (`fulltext_rank`, lower is better) is not used by
list queries today; FB-166 adds it with `ts_rank` over the same expression.

**Index manager.** B-tree, unique (per tenant on tenant-scoped
collections), partial, expression and JSON-path indexes map directly; every
key is parenthesised on PostgreSQL (a parenthesised column is still a column
key). `list_indexes` reads `pg_index` for tables in `public` (excluding
`_forgebase_…` objects) plus the managed rows; `get_index` reports table,
unique, partial, columns (`<expr>` for expression keys), the
`pg_get_indexdef` DDL, origin `engine` for primary-key and unique-constraint
indexes, and statistics: `rows_estimate` and `stat` from `pg_class.reltuples`
(absent until `ANALYZE` or `CREATE INDEX` has counted), `size_bytes` from
`pg_relation_size`, and a new `IndexStats::scans` from
`pg_stat_user_indexes.idx_scan` (PostgreSQL only; not serialised when absent,
so SQLite output is unchanged). `rebuild_index` is `REINDEX INDEX
public."i"` (not `CONCURRENTLY`, which cannot run in a transaction; it blocks
writes to the table while it runs) then `ANALYZE` of the table; `analyze` is
`ANALYZE public."t"` or `ANALYZE`. Plans: `explain_list` and
`recommend_indexes` read `EXPLAIN (FORMAT JSON)` themselves
(`QueryPlan::from_pg_json`), because a full scan on PostgreSQL is a `Seq
Scan` *or* an `Index Scan` without an `Index Cond` (the planner walks the
primary key in `ORDER BY id` order and filters every row), which the
`forgebase-db` step text cannot tell from a lookup. Recommendation probes
bind `'0'` instead of NULL on PostgreSQL: the planner folds `x = NULL` to a
constant and the plan would show no scan.

**Name rules** (`forgebase-schema`): `validate_postgres_collection_name`
(`pg_` is reserved: `pg_catalog` is searched before every schema, so a
collection named like a catalog relation would be shadowed),
`validate_postgres_field_name` (`tableoid`, `xmin`, `cmin`, `xmax`, `cmax`,
`ctid`: PostgreSQL's system columns; `oid` was already reserved),
`POSTGRES_MAX_IDENT`, and `Collection::validate_postgres_names(previous)`,
which checks only names that are new relative to `previous`. The data core
checks every name on PostgreSQL and new names on SQLite, so a SQLite project
cannot pick a name that would block `forgebase db convert` (FB-151 should
run `validate_postgres_names(None)` on every collection before converting),
while names it already has keep working.

**Tests.** `crates/forgebase-data/tests/schema_changes.rs` calls
`forgebase_db::testing::test_backend()` directly (FB-143 switches the shared
`tests/common` helper) and runs on both backends: the three original tests
(with backend-specific assertions where the mechanism differs: column type
`bigint` vs `INTEGER`, `ALTER COLUMN … TYPE` vs the `_forgebase_tmp_`
rebuild in the history, `effective_pragmas` SQLite only) and six new ones
(type conversions, views over changed columns and `security_invoker`,
fitted names and accent-insensitive full text, name rules, the index manager
with statistics, plans and recommendations, relation changes). One minimal
edit outside the owned files: `list.rs` calls `indexes::fulltext_match`
instead of building the FTS5 `MATCH` itself.

## Addendum: FB-148, configuration, CLI and server wiring (2026-11-11)

Built as §4.3, §6.1–§6.3 and ADR-0029 §4.1 and §6 describe. What the next
packages need, and where the code differs from the text above.

**Configuration** (`forgebase-core/src/config.rs`). `DatabaseConfig` gained
`backend: DatabaseBackend` (`Postgres` | `Sqlite`) and `postgres:
PostgresConfig { mode, url: Option<DatabaseUrl>, pool_size (16, 1–64),
listen, engine_dir, native_rls, settings: BTreeMap<String, String>,
extensions: PostgresExtensions { vector, graph, queue, cron, trigram } }`.
The default backend is a compile-time constant: `DEFAULT_BACKEND` is
`Postgres` when `forgebase-core` is built with `bundled-postgres` (enabled by
`forgebase-cli/bundled-postgres` → `forgebase-server/bundled-postgres`, which
also embeds the engine), `Sqlite` otherwise (the `-lite` build;
`forgebase --version` says `lite`). The test and development builds are
`-lite`, so every existing suite keeps SQLite unless it asks otherwise.
`DatabaseUrl` redacts its password in `Debug` and is never serialised.
`[database.postgres.settings]` accepts text, numbers and booleans (stored as
text; `on`/`off` for booleans); names must be PostgreSQL parameter names and
values one line; the engine's deny list is applied at start
(`Engine::prepare`). Environment: `FORGEBASE_DATABASE_BACKEND`,
`FORGEBASE_DATABASE_URL` (recorded as a secret by name; the file's `url` is
reported as source `file`), `FORGEBASE_DATABASE_POSTGRES_{MODE, POOL_SIZE,
LISTEN, ENGINE_DIR, NATIVE_RLS}` and
`FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_{VECTOR, GRAPH, QUEUE, CRON, TRIGRAM}`.
*Differs:* the unknown-variable guard now ignores `FORGEBASE_PG_ENGINE_DIR`,
`FORGEBASE_PG_ENGINE_ARCHIVE`, `FORGEBASE_PG_ENGINE_LOCK` and every
`FORGEBASE_TEST_*` variable: before, a `forgebase` binary spawned by a test
running with `FORGEBASE_TEST_BACKEND` refused to start. The SQLite keys keep
their `FORGEBASE_DB_*` names.

**Registry** (`features.rs`). The `database` module now lists
`database.backend`, `database.postgres.{mode, url, pool_size, listen,
engine_dir, native_rls, settings}` and `database.postgres.extensions.*`; the
SQLite keys stay under `server`. Editable from the dashboard: `pool_size`,
`native_rls` and the five extension switches. Read-only: `backend` and `mode`
(§6.1: they need `db convert`), `listen` (opens a network listener),
`engine_dir` (ForgeBase runs the programs in it, so an administrator could run
code as the server's OS user) and `settings` (file only, as the deny list is
the engine's). `url` is a secret. New `SettingType::Map` (`"map"`) for
`settings`; `set_keys` writes nested sections (`[database.postgres.extensions]`)
and `keys_of` in the tests walks them. The module's docs link is
`docs/deployment.md#35-database-postgresql-or-sqlite`.

**Backend marker and resolution** (`forgebase_server::database`).
`resolve(config, explicit, layout)`: the marker, else what is on disk
(`forgebase.db` → SQLite, `postgres/data/PG_VERSION` → managed), else the
configuration. `explicit` is "`database.backend` or `database.postgres.mode`
came from the file, the environment or a flag" (`backend_is_explicit`); a
configuration built in code (`forgebase_server::start(&Config)`) counts as
explicit. A data directory keeps its backend when the configuration only has
the default; an explicit contradiction is `ServerError::BackendMismatch`
(`backend_mismatch`, 409) with the §6.3 message plus the `db convert` hint.
This reconciles §6.3 ("refuse a mismatch") with ADR-0029 §6 ("the marker
wins"): existing SQLite projects never switch on their own, and an edited
config never opens an empty PostgreSQL next to a full SQLite project. The
marker is written atomically by `init` and by the first start of an empty
directory (and by `db convert`, FB-151: `database::write_marker`).

**Server** (`forgebase-server/src/lib.rs`). After the lock,
`database::open_owned(config, explicit, layout, Create::Yes)` starts the
bundled engine (`Engine::prepare(Layout::primary(data_dir), engine_config)`:
`engine_dir` → `EngineSource::Dir`, `listen`, `settings`, `extensions.cron`
→ pg_cron), then opens `Database` with `url(Role::Server)` and
`read_pool_size = pool_size`; an external server uses `FORGEBASE_DATABASE_URL`
(`create_if_missing` creates its database). A failing startup closes the
pools, stops the engine and only then releases the lock. Shutdown closes the
pools, then `EngineHandle::stop()` (fast shutdown), then releases the lock.
`RunningServer::{backend, engine, db}` are new. *Differs:* the final
`CHECKPOINT` runs on SQLite only (`database::checkpoint`): `forgebase_server`
has no `pg_checkpoint` membership, so `Database::checkpoint` fails on the
bundled engine (`permission denied to execute CHECKPOINT`); the engine's fast
shutdown checkpoints anyway. FB-150 (backups) should either grant
`pg_checkpoint` to `forgebase_server` in the FB-132 bootstrap or run its
checkpoint as `forgebase_super`.

**One-off commands** (`database::open_attached(config, explicit, layout,
Access::{Read, Write, Super})`). SQLite opens the file (read-only for
`Read`). The bundled engine: when the lock is free the command takes it and
starts the engine for its duration (`PrimaryDatabase::close` stops it and
releases the lock); when a server holds the lock, it reads
`postgres/data/postmaster.pid` (pid alive, status `ready`, socket directory,
port; waits up to 10 s for a starting server) and logs in through the socket
with the role's password from `roles.secret` (`forgebase_reader`,
`forgebase_server` or `forgebase_super`). `PrimaryDatabase` derefs to
`Database`. Used by `status`, `doctor` (`Super`, for `amcheck`), `migrate
--status`, `admin`, `export`/`import` and `db`; `init` and `migrate` use
`open_owned` under their own lock. *Differs:* §6.2 has `init` start and stop
the engine once; it now runs the migrations and the administrator step on one
open database (`admin::init_admin` takes the `Database`), so the engine starts
once per `init`.

**CLI.** `forgebase init [DIR] --database postgres|sqlite` and
`--database-url URL` (conflicting flags); on a terminal without either, the
ADR-0029 §6 prompt (`[1]` is the build's default). An existing data directory
or an explicit `backend` in an existing `forgebase.toml` decides without
asking, and a flag that contradicts either is refused. The new
`forgebase.toml` gets `backend = "…"` (and `[database.postgres] mode =
"external"`); the URL goes to `DIR/.env` (single-quoted, 0600; a different
URL already there is refused). Root: `PostgreSQL cannot run as root: run as an
unprivileged user, or use --database sqlite`. A build without an engine and
without `engine_dir`/`FORGEBASE_PG_ENGINE_DIR` answers with
`database::NO_BUNDLED_ENGINE`. `status` shows the backend, the cluster or
server, the socket and who started the engine, and the SQLite notice of
ADR-0029 §6 (only while PostgreSQL is the default). `doctor` adds `backend`,
`engine` (`archive::verify_extracted` for extracted engines, `postgres
--version`, major against `PG_VERSION`, `MIN_MAJOR`), `cluster` (mode 0700),
`roles`, `postmaster` (left behind) and runs `amcheck` plus the foreign-key
check as `forgebase_super`; the WAL-size warning is SQLite only.
`forgebase db status [--json]` never starts the engine. `forgebase db psql
[--read-only | --superuser] [ARGS…]` runs the engine's `psql` over the Unix
socket only (human decision 2026-11-11), with a 0600 passfile in
`<data_dir>/tmp/` (removed afterwards; never argv or `PGPASSWORD`), ignoring
Ctrl-C itself so psql gets it; external servers and SQLite are refused with a
hint. One-off commands filter `forgebase::postgres` out of their log (the
pg_cron launcher logs `database "forgebase" does not exist` on a first start,
before the bootstrap creates it); a start failure still carries the engine's
last lines.

**API** (`forgebase-api`). `AppState::with_database_runtime(DatabaseRuntime {
mode, location, extensions, engine })`, where `engine` is a closure returning
`EngineReport { state, version, pid, restarts, last_error, socket_dir, tcp,
warnings }` (no `forgebase-pgengine` dependency in the API crate).
`/api/system/info` `database` gained `backend`, `mode`, `server_version`,
`connections`, `engine` and `features` (per feature: `engine: "portable"`,
`mode`, `extension`, `available`, `installed`, `state` = `off` |
`installed` | `available` | `unavailable`, from `pg_available_extensions` and
`pg_extension`; `{}` on SQLite); `path` is the SQLite file, the cluster
directory or the external URL without password. `/health/ready` fails the
`database` check while the engine is `restarting` or `failed` (with its last
error); its body keeps the Phase 1 contract shape (`status`, `checks`,
`errors`), so the backend is reported by `/api/system/info` only (the
dashboard's backend-info slot reads `database.backend` and
`database.features` there).

**Bench.** `forgebase_bench::Rig` and the queue/jobs bench follow
`FORGEBASE_TEST_BACKEND` (`postgres` starts the bundled engine through
`open_owned`, from `FORGEBASE_PG_ENGINE_DIR` in a `-lite` build);
`run-load.sh` takes an optional `postgres|sqlite` argument for `init`.

**Engine fixes (FB-132 follow-ups).** Extraction no longer calls `sync_all`
per file (on macOS that is `F_FULLFSYNC`, a full flush of the drive's cache,
938 times). The whole tree is made durable once, before the rename that
publishes it: Linux `syncfs` of the file system; macOS a plain `fsync` of
every file and directory, then one `F_FULLFSYNC`; other platforms keep the
per-file sync. The rename is followed by a sync of the parent directory, and
a directory left without a matching `forgebase-engine.sha256` is replaced
instead of kept. Measured with the real FB-130 archive
(`forgebase-pg18.6-r1-aarch64-apple-darwin.tar.zst`, 938 files, debug build,
external SSD): first extraction **143.6 s before, 2.6 s after**; finding it
again 0.09 s (`examples/extract_engine.rs`). With the archive embedded
(`--features bundled-postgres`), `forgebase init` on a fresh directory
started the engine in 5.2 s (extraction about 1.8 s, then `initdb`, start and
bootstrap) and finished in 7.5 s with the migrations; a later start of the
same cluster takes about 0.1 s. The archive test packs `bin lib
share`, so it passes with a relocatable engine.

**Not done here / for the next packages.**
- `serve` on PostgreSQL stops at the first unported service query until
  FB-143–FB-147 merge (on this branch: `forgebase-data/src/ddl.rs` asks
  `sqlite_master`); the full-server test
  `crates/forgebase-server/tests/database_fb148.rs::the_server_runs_on_the_bundled_engine`
  is `#[ignore]` for that reason and should be switched on by god or FB-149
  after the merge.
- FB-149: release builds enable `forgebase-cli/bundled-postgres` (with
  `FORGEBASE_PG_ENGINE_ARCHIVE`); the `-lite` archive builds without it. CI's
  PostgreSQL job runs these suites with `FORGEBASE_TEST_BACKEND=postgres` and
  `FORGEBASE_PG_ENGINE_DIR`.
- FB-150/FB-151: open the data directory through `database::open_owned` /
  `open_attached` (they handle the marker and the engine), write the marker
  with `database::write_marker`, and take the `pg_checkpoint` point above.
  `forgebase backup` still opens `forgebase.db` directly.
- FB-155/FB-102: the new `database.*` settings (type `map` included) and the
  `database.features` and `engine` objects above.
- External servers: the text-collation check FB-133 suggested (a libc locale
  may sort differently from the bundled engine's builtin C.UTF-8) is not done.

## Addendum (FB-140): object storage tenancy, object rules and the sealed signing key

Written 2026-11-11 by FB-140. Closes `rls-coverage.md` G1, G14 and G15 and
the storage part of G11 in `forgebase-storage`. Backend-neutral: new SQL is
valid on both engines (numbered `?N` placeholders, `Dialect::insert_ignore`,
`Dialect::column_type` casts); migration pair 0029 (`storage_rls`).

**Bucket ACLs (G1).** An ACL entry for a prefix covers the keys under it
and never the bucket: get, update, ACL get/set, usage, delete (also
`force`) and the bucket listing match only entries with the empty prefix.
A user with `admin` on `users/alice/` keeps writing there and administers
nothing.

**Authorization order.** Privileged callers; the bucket owner; visibility
(reads); ACL entries; then `Authorizer::authorize_resource(ctx,
Storage<need>, bucket <name>, op)` (FB-134): the permission
`engine:storage_<need>:<bucket>` or the plain key, else the bucket's
resource rule for the operation. Operations: object listing and version
listing `list`; get, head, signed GET, copy source and restore source
`view`; put, upload, copy target and restore `create` when the key holds
no live object at install time, `update` otherwise; delete and move
source `delete`. Bucket get and listing ask `view` / `list`; admin needs
are decided by permissions only. `create_bucket` asks
`authorize_resource(StorageAdmin, bucket <name>, create)`, so
`engine:storage_admin:<name>` lets one create that name. Anonymous callers
reach the rule step (a rule may open a bucket to them). Statistics and the
reconciler keep the plain `authorize`.

**Object rules (G14).** Storage registers its own `bucket` schema
(`forgebase_storage::object_schema`): `key`, `created_by` (principal key
as stored), `owner` (the creator's id, as `@request.auth.id` gives it),
`owner_collection` (a user creator's auth collection), `size`,
`content_type`, `folders` (JSON array of the key's folder names, as
Supabase's `storage.foldername`; `folders.0` is the first). They are
columns of `_forgebase_storage_objects` (0029 adds `owner`,
`owner_collection`, `folders` and fills existing rows with the values the
service writes), so a `Decision::Filter` compiles with
`ResourceSchema::compile` straight into the object SQL over alias `o`.
Reads add it to `WHERE` (hidden objects are `404`; folder prefixes of a
delimiter listing come only from visible objects). Writes that add a
version get both the `create` and `update` decisions up front
(`WriteCheck`); an early check refuses before any byte is received when
the current state's decision denies or the version being replaced fails
the `update` filter; the decision for the state found **inside the
install transaction** is applied there: the replaced version must pass
(`USING`) and the new one, as a one-row derived table of its fields
(`WITH CHECK`), must pass too, else `403` and nothing is written.
Multipart and resumable uploads are checked when they complete. Deletes
check the version removed (or the latest). Owner-only pattern:
`owner = @request.auth.id && owner_collection = @request.auth.collection`
(the second term keeps users of two auth collections with the same id
apart); per-user folders: `folders.0 = @request.auth.id`.

**Tenant buckets (G14).** `_forgebase_storage_buckets.tenant` (NULL =
instance-wide, every bucket before FB-140). A caller with a tenant creates
buckets of that tenant, stored as `<tenant>/<name>` (bucket names never
hold `/`, so names are unique per tenant and the stored name is never
addressable: every name is validated before lookup). A name resolves to
the caller's tenant bucket, else the instance-wide bucket of that name;
another tenant's bucket is unreachable, and so are its upload sessions. A
tenant may not create a name an instance-wide bucket holds (`409`); an
instance-wide bucket created later is hidden in a tenant that has its own.
Listings show the tenant's buckets plus the instance-wide ones; the wire
`Bucket` gained `tenant`; objects and uploads keep showing the bare name.
`GET /api/v1/storage/stats` gained `scope` (`tenant`, `instance` for
non-privileged callers without a tenant, `engine` for privileged callers
without one) and `tenant`; stored bytes are the distinct blobs the scope
references. Resource permissions and rules name the bare bucket name, so
`engine:storage_read:files` and the rules of `files` apply to every
tenant's `files` (rules are super-admin, instance-level settings, as FB-134
decided for engine names).

**Signing key (G11).** The 32-byte key lives in
`_forgebase_storage_settings` row `signing_key_sealed`, sealed with the
auth keyring (XChaCha20-Poly1305, AAD `forgebase/storage/url-signing-key/v1`).
At start a plain `signing_key` row from before FB-140 is sealed
**unchanged** and deleted in one transaction, so URLs issued before the
upgrade stay valid until they expire. A master key that does not open the
sealed key fails the start with a message naming the row to delete;
nothing is overwritten. `StorageService::open` takes a `Keyring` (server:
the data directory's master key, as billing and queues).

**Signed URLs (G15).** Checked when issued, never when used (as S3 and
Supabase), documented in `signed.rs`, ADR-0018 §7 and `docs/security.md`.
New URLs carry `v=2`, `t` (bucket tenant) and `by` (the uploader, recorded
as `created_by`; format 1 recorded `signed-url`), each value length-prefixed
in the signed message; format-1 URLs still verify. Signing a PUT is
refused when the rule for the key's state could fail for an upload the URL
accepts (a rule on `size`, or on `content_type` without `content_type`
pinned). `POST /api/v1/storage/maintenance/rotate-signing-key`
(administrators, `StorageService::rotate_signing_key`) replaces the key and
revokes every URL at once.

**Breaking.** A prefix ACL entry no longer shows its bucket in listings or
opens bucket metadata. Rules that read `owner`/`folders` see the new
columns only (0029 fills existing rows). `StorageService::open` gained the
keyring argument; `SignedParams` gained `format`, `tenant`, `signer`;
`StorageStats` gained `scope`, `tenant`; `Bucket` gained `tenant`.
`DataService::authorizer()` added in `forgebase-data` (one accessor).

**For FB-147 (storage on PostgreSQL).** The new SQL is portable; check on
PostgreSQL: the `CAST(?n AS jsonb)` of `folders` in the new-row check, JSON
paths (`o."folders"->>0`) on the `jsonb` column, `trim_array` in the 0029
twin (PG 14+), and the SQLite twin's `json_remove(…, '$[#-1]')`. The
signing key row is text hex on both engines. Native RLS stays out of scope
(engine tables are API-only, rls-coverage §3.4); if storage objects are
ever exposed, the rule fields are real columns, so policies can reuse them.
**For the dashboard storage studio (FB-100).** Show `tenant` on buckets and
`scope` on stats; send `X-ForgeBase-Tenant` when creating buckets; offer
`bucket` resource rules with the fields above (`folders.0`, `owner`); a
"rotate signing key" action for administrators; expect `404` for objects a
rule hides.

## Addendum (FB-139): vector collection rules and graph label rules

Written 2026-11-11 by FB-139. Closes `rls-coverage.md` G9 (vector) and G10
(graph) on the FB-134 framework. Backend-neutral: no migration; the new SQL
(a derived items table, a loose index scan over labels, `IN` lists,
`coalesce`) is valid on both engines, and rules compile through
`ResourceSchema::compile` with the backend's `Dialect`.

**Data core helpers** (`forgebase-data`, small and listed):
`DataService::authorize_resource(ctx, action, &ResourceRef) ->
Result<Option<Predicate>>` (the system is allowed; `Some(p)` restricts
items), `DataService::register_resource_schema`, and
`DataService::unreadable_fields(ctx, collection)` (hidden fields plus
field rules, lower case; empty for privileged callers).

**Vector (G9).** Every call on one collection asks `authorize_resource`
for `vector_collection <name>` with the operation of the call: `search` and
`hybrid` are `list`; `get_item`, `get_collection`, `list_fields`,
`wait_ready` are `view`; an upsert is `create` for a new item and `update`
for an existing one; deletions are `delete`; administration
(`create_collection` `create`, `delete_collection` `delete`, fields and
`rebuild` `update`, `recall` `view`) and collection `stats` need the
permission or an unconditional rule (a restricting rule cannot apply to
them, so it is refused). Listing collections and `/vector/stats` keep the
plain key and leave out collections the caller may not read at all. The
engine registers `item_schema()` (`id`, `metadata`, the kind's default).
A rule's predicate is ANDed with the caller's filter into every statement
that reads items (`store::ITEMS` is the items table with `_data` exposed
again as `metadata`): the ANN pre-filter key set, exact scans, the
post-filter verification and the FTS5 keyword ranking, so a rule is a
**pre-filter**, never applied after `k`. Writes check the stored row
(update) and the written row (create, update) inside the transaction;
deletes affect only matching items (others are ignored like unknown ids;
`DELETE .../items/{id}` gives 404). Collection definitions count the items
the caller's rule shows. The HTTP layer checks the collection before
reading a body (upsert: `create` or `update`; `create_collection` refuses
anonymous callers only, since the name is in the body).

`source_collection` (new optional field of `NewCollection`, stored in the
collection options, shown on `VectorCollection`; creation only, validated
to exist): items are keyed by record id. For every caller but the system,
each hit is re-checked with `DataService::get_record` as the caller (View
rule, tenant; any failure but a database error hides the item), metadata
keys that name fields the caller may not read are removed, and a caller's
filter on such a key is refused as `unknown_field` (no oracle). Searches
refill `k` by asking for 4× more candidates up to `MAX_K`; hybrid searches
re-check within their `candidates` pool. The `text` and the vectors are
returned as stored: do not put hidden record content in `text`.

`tenant_required`: callers with a tenant use it; privileged callers
without one keep the shared `''` scope; anyone else gets `400
tenant_required` after authorisation. Edge functions call the same service
methods as the caller (`ctx.vector`), so they get every check above;
`ctx.admin.vector` (the system) bypasses rules and source re-checks.

**Graph (G10).** A node's resource is its label (`graph_label`), an edge's
its type (`graph_edge_type`); rule fields are the kinds' defaults, which
are the tables' own columns. Reads (`list` for listings, neighbours,
traversal, shortest path and stats; `view` for single nodes and edges)
build a `GraphAccess`: the tenant's distinct labels and edge types (a
recursive loose index scan, at most `MAX_RULE_NAMES` = 1000 each, else
`400 graph_too_many_names`) each get a decision, compiled into one
condition (`label IN (allowed…) OR (label = ? AND <predicate>) …`; denied
names are absent, so labels created meanwhile stay hidden). An edge is
visible when its type is and both of its nodes are. The traversal's
recursive step joins the next node and applies both conditions, so a walk
never crosses a hidden edge or enters a hidden node; shortest-path hops do
the same; neighbours, listings and counts filter in their statements.
Privileged callers and callers whose every decision is `Allow` get the
statements unchanged. A caller for whom nothing at all is allowed gets
`403` (on an empty graph, the decision for the name `*` decides). Writes
ask the row's label or type: `create` (the new row must match a
restricting rule), `update` (the stored row, and the changed row under the
rule of its possibly new label), `delete` (and deleting a node needs the
`delete` rule of every edge touching it, else `403`). A refused stored row
is `404` when the caller may not view it, `403` otherwise; edges can only
be created between nodes the caller may view. `tenant_required` as for
vector; `stats` keeps the plain key and counts only what the rules show.

**For FB-162 / FB-163.** `VectorService::item_rule(ctx, action, name, op)`
returns `ItemRule { predicate }`; `ItemRule::compile(&mut SqlBuilder,
alias)` gives the condition over a row exposing `id` and `metadata`.
`GraphService::access(ctx, action, op)` returns `GraphAccess`;
`is_unrestricted()` says whether any label or edge-type rule applies,
`node_sql`/`edge_sql` give the conditions (numbered placeholders, safe to
repeat in one statement), and `nodes`/`edges` are the per-kind `Rules`.

**For FB-147 (PostgreSQL port).** Hybrid search on `tsvector` must AND the
same `Filter` (rule plus caller filter) into the keyword query, and the
pgvector path must use the rule as a pre-filter (`item_rule`). The graph
walk on PostgreSQL must keep the per-step node join and the edge condition;
`json_each` in the existing statements is the only SQLite-only construct
left in these crates. **For FB-160 (AGE).** A derived AGE graph must be
read through `GraphAccess` (or not at all for restricted callers), since
openCypher queries do not see these conditions until FB-163. **For the
dashboard vector and graph studios.** Offer `vector_collection` rules over
`id`/`metadata.*` and `graph_label`/`graph_edge_type` rules over
`properties.*`; show `source_collection` on a collection; administrators
see everything, so preview a rule with `POST
/api/v1/policies/resources/{kind}/{name}/check`.

**Breaking.** Non-privileged callers without a tenant lose the shared
vector scope and the shared `''` graph (`tenant_required`); a `Filter`
decision for graph data is now applied instead of refused; an explicit
deny of `engine:vector_read:<name>` now hides that collection from
listings. `NewCollection` and `VectorCollection` gained a public field
(`source_collection`), so struct literals add it; `store` functions of
the vector crate take the dialect and an optional filter (crate-private).

## Addendum (FB-136): billing, credits and realtime scoping

Written 2026-11-11 by FB-136. Closes `rls-coverage.md` G18 (billing and
credits) and G17 (realtime channels), and the FB-138 follow-up for
realtime's in-band key authentication. Backend-neutral: the only new SQL is
the realm filter below, checked on SQLite (the server suite) and on
PostgreSQL 18.3 (a prepared statement over the same columns); no migration.

**Realm-level grants stay in their realm (G18).** `billing:read` /
`billing:write` (and `usage:write` for spending) held at realm level now
reach a subject only when it belongs to the caller's realm
(`ctx.realm_id`): an organization whose `_forgebase_orgs.realm_id` is that
realm, a user whose auth collection that realm names, a service account
of that realm (users and service accounts default to `default`, unknown
organizations belong to no realm). The rule is
`forgebase_policy::Snapshot::realm_of_subject` (new, public) and applies
to every check: reads and management of a subject (`can_read`,
`can_manage` in billing; reads and spends in credits), the operator
actions that used to be subject-less (billing `can_operate(ctx, subject)`
for tax exemption; mark-paid, void and credit notes look the invoice up
first and answer `404` for another realm's invoice, `403` still comes first
for a caller who is no operator at all; credits `can_mint(ctx, subject)` for
top-ups, grants, consumption refunds and reversals). A refused fallback is
a policy denial at step `tenant` (`403`), as before for a non-member.
Organization-level grants are unchanged (policy step 2 already ties an
organization to the caller's realm).

**"All customers" lists are realm-filtered (G18).** `can_read_all` returns
the realm the read is limited to (`None` for privileged callers, who still
see every realm). Every instance-wide read applies
`forgebase_billing::service::realm_clause` (an `AND` over the subject
column, the realm as the last parameter): customers, subscriptions,
invoices, payments, refunds and credit notes without a subject, store
purchases, `admin/orders` (its `total` too), `admin/usage-meters`, the
dunning queue, and every figure of `/billing/stats` (MRR, status counts,
churn, revenue, refunds through their invoice, failed payments). The SQL
(`subject_in_realm_sql`) mirrors `realm_of_subject` with
`_forgebase_orgs`, `_forgebase_service_accounts.realm` and
`_forgebase_realm_identity.collection` (unique), using only `||`, `substr`,
`length`, `COALESCE` and scalar subqueries, valid on both engines; with
paging the filter runs before `LIMIT`/`OFFSET`.

**Channel namespaces (G17).** `forgebase_realtime::channel::resolve(ctx,
name)` decides where a channel lives:
- a caller with a tenant (bound key, selected `X-ForgeBase-Tenant`, a
  function running for a tenant, the system with a tenant) is in
  `t:<tenant>:`: `chat` is routed as `t:<tenant>:chat`; the explicit
  `t:<tenant>:chat` names the same channel and is refused
  (`403 tenant_mismatch`) to non-privileged callers of any other tenant or
  of none; privileged callers may address any tenant's channel explicitly;
- `global:<name>` is shared by every tenant; subscribing follows the
  channel's permission or rule, publishing and joining presence need a
  privileged caller (`403 global_channel`);
- `jobs:<id>` (`SYSTEM_PREFIXES`, the job queue's own channels) is never
  namespaced; `JobChannelAuthorizer` keeps deciding who reads it (FB-142);
- callers without a tenant keep plain names (as before).
Inside a tenant name, a reserved prefix (`t:`, `global:`, `jobs:`) is
`400 invalid_channel`. Messages and presence frames carry the name the
client used; rooms, the in-process bus and the cross-instance fan-out use
the routed name (a rolling upgrade mixing versions would route tenant
callers' channels differently for the overlap).

**Channel rules (G17).** Channels are FB-134 resources of the new kind
`realtime_channel` (`forgebase_data::ResourceKind::Channel`; operations
`view` = subscribe and presence listing, `create` = publish and presence
join; no item fields, `@request` only, so a rule is a yes/no). The two
actions joined `perm::RESOURCE_ACTIONS`: `engine:channel_subscribe:<name>`
and `engine:channel_publish:<name>` grant one channel, the plain keys keep
meaning every channel (so existing grants are unchanged).
`PolicyAuthorizer::authorize_channel` now asks `authorize_resource`; the
name it sees is the tenant's own name for the channel
(`Channel::rule_name`, without `t:<tenant>:`), so a rule is written once
per channel and tells tenants apart with `@request.tenant`, e.g.
`PUT /api/v1/policies/resources/realtime_channel/lobby {"rules": {"view":
"@request.tenant != ''", "create": null}}`. Anonymous callers can now be
admitted by a rule (`""`), as for other resources; before, only a
permission could admit anyone. Rules are re-checked on every delivered
message, as channel decisions were.

**In-band keys keep their binding (FB-138).** Realtime's own
authentication (`?access_token=` on the WebSocket/SSE upgrade, the
`{"type":"auth"}` refresh, the periodic re-validation) uses
`authenticate_key_bound`: a bound key's connection runs in its tenant (the
binding was validated for the key's owner by the auth service), and in the
identity's home realm (`access_context_for`; it was always `default`
before). A refresh with a key bound to another tenant is
`403 tenant_mismatch` (the connection's namespace is fixed), and a
re-validation that finds the key's tenant changed or its owner gone closes
the connection (`4001`).

**Breaking.** Tenant-bound callers no longer meet callers of other tenants
or tenant-less callers on plain channel names: cross-tenant broadcasts move
to `global:` (published by the server or an administrator). A non-privileged
caller publishing on `global:` is refused. Realm-level billing operators of
one realm lose access to other realms' subjects and see only their realm in
lists and statistics.

**Edits outside FB-136's packages.** `forgebase-data/src/access.rs`
(`ResourceKind::Channel`), `forgebase-policy/src/perm.rs` (two resource
actions), `forgebase-policy/src/lib.rs` (`authorize_channel`),
`forgebase-policy/src/snapshot.rs` (`realm_of_subject`),
`forgebase-billing/src/{admin,stats,store}.rs` (the realm filter on their
instance-wide reads). The OpenAPI description of
`/api/v1/policies/resources/{kind}` still lists seven kinds (unchanged to
keep the generated SDKs as they are); the kind list in the error message
and the API itself include `realtime_channel`.

**For FB-145 (billing and credits on PostgreSQL).** The realm filter is
already portable; the surrounding `?N IS NULL OR subject = ?N` patterns
need FB-143's `CAST(?N AS TEXT) IS NULL`. `realm_clause` always appends
its parameter last, after `LIMIT`/`OFFSET` parameters, so numbering stays
stable. **For FB-146 (realtime on PostgreSQL).** No SQL changed in
realtime; routed channel names are memory and fan-out only. **For FB-152
(native RLS).** Channels stay API-only; `realtime_channel` rules are not
compiled into PostgreSQL policies. **For the dashboard.** Realtime studio:
show and accept channel names as clients use them (the tenant prefix is
implicit), offer `global:` for broadcasts (administrators only) and
`realtime_channel` rules with `@request` only. Billing studio: a realm
operator's lists and statistics are of their realm; a privileged caller
still sees every realm.

## Addendum (FB-142): jobs RLS and internal invocation contexts

Written 2026-11-11 by FB-142. Closes `rls-coverage.md` G3 and G4 and the
FB-142 part of G5 for the job queue (`forgebase-jobs`), `function.invoke`
and record hooks (`forgebase-functions`). Backend-neutral: the new SQL is a
migration pair and statements valid on both engines (`COALESCE`, `LIKE`
with constant prefixes, `IS NULL`; no `?n IS NULL` parameter tests).

**Tenant (G3).** Migration 0030 pair (`job_tenant`): `_forgebase_jobs.tenant`,
`_forgebase_jobs.context`, `_forgebase_job_schedules.tenant`,
`_forgebase_job_schedules.context`, `_forgebase_job_workflows.tenant`, index
`_forgebase_jobs_by_tenant (tenant, created_at)`, and the idempotency index
widened to `(queue, COALESCE(tenant, ''), idempotency_key)`; on PostgreSQL
`context` is `jsonb`. A job takes the enqueuer's `ctx.tenant` (`NULL` = none); a schedule
firing takes the schedule's tenant and context; child jobs and saga
compensations take their parent's (or the compensated step's) tenant. Job,
Schedule and Workflow JSON gain `tenant`; `context` never leaves the server.

**Visibility** (`forgebase_jobs::JobAccess`, `Scope`). Privileged callers
see every job. Others see jobs of their own tenant only (`tenant IS NULL`
without one), and of those only the ones they created (or that a schedule
they created fired) unless they hold `engine:read_jobs:all` (a plain key:
`engine:read_jobs` does not grant it, `engine:*` and `*` do). Applied to
list, stats, get, events, attempts, checkpoint, result, cancel, retry,
delete, schedules (list, get, update, delete) and workflows (list, get,
cancel); an invisible id answers `404` like a missing one. Idempotency keys
are per tenant (another tenant's key never matches; another creator's in
the same tenant is `409 idempotency_key_in_use`). The server answers the key
with `PolicyService::has(ctx, key, ctx.org_id)` (which also runs policy
steps 1 and 2) through `JobService::attach_permissions`.

**Job channels.** `JobChannelAuthorizer` now also requires visibility for a
non-privileged subscriber of `jobs:<id>`. `authorize_channel` is
synchronous, so it reads owners remembered by the shared `JobAccess`
(enqueued, published, read or listed since start; at most 100 000). A
subscriber to a job not seen since a restart is refused until it reads or
lists the job: clients (the dashboard) list or `GET` before subscribing.
The server creates the authorizer first (`open_auth`) and hands its
`access()` to the `JobService` (`attach_access`).

**Kinds (G4).** `JobRegistry::register` now makes a kind **reserved**
(system and privileged callers only); `register_public` and
`register_guarded(kind, handler, guard)` (`EnqueueGuard::admit(ctx,
payload)`) open it. `start_pool` installs the kinds
(`JobService::install_kinds`). Kinds with the prefixes `function.`,
`storage.`, `billing.`, `credits.`, `entitlements.`, `customize.`,
`forgebase.` and `system.` are reserved even when no handler registered
them (a module switched off cannot be handed jobs for later). Every other
unregistered kind stays public (external workers). Enqueue, schedule
creation, a schedule payload change, workflow steps and compensations, and
`spawn_children` all ask `JobAccess::admit`. A non-privileged worker claims
only public kinds of its own tenant (`tenant IS NULL` without one).

**`function.invoke` (G4, G5).** Guarded: a non-privileged enqueuer must pass
the function's HTTP invoke check (`public`, `authenticated`, or `permission` =
`engine:invoke_function`) at enqueue; unknown and forbidden functions give
the same `403`. The job records the enqueuer (`RecordedContext`: principal,
realm, organization, tenant, scopes, impersonator; no roles: the policy
engine resolves them again) and runs **as it**
(`JobContext::enqueuer`, `FunctionService::invoke_as`): before the run the
enqueuer must still pass the invoke check and hold `engine:enqueue_job` or
`engine:manage_jobs` (which re-validates its tenant), and a user must still
exist (its record gives `@request.auth.<field>`); otherwise the job fails for
good. A changed schedule payload is recorded with the updater's context.
**Privileged enqueuers do not lend their rights**: jobs of administrators,
admin service accounts and the system run as the function's own identity
`service:function.<name>` in the recorded realm, organization and tenant
(the job's tenant for system jobs); only `service_role` reaches `ctx.admin`.

**Hooks (G5 follow-up).** Hooks pass an explicit context
(`invocation_context`): a before-hook acts as the writer, except that a
privileged writer's hook acts as the function's identity in the writer's
realm, organization and tenant; an after-hook acts as the function's
identity in the tenant recorded with the change (`Event.tenant`), so it can
use that tenant's tenant-scoped data under the collection rules.
`FunctionService::invoke` (no context: the function's identity, no tenant)
stays for tests.

**`ctx.auth.verify`.** Uses `authenticate_key_bound` (FB-138): a bound key's
tenant is reported in the answer's `tenant` (it was `null`).

**Breaking.**
- Non-privileged callers no longer see other callers' jobs (or other
  tenants'): grant `engine:read_jobs:all` (seeded Owner `*` and Developer
  `engine:*` hold it). Jobs, schedules and workflows from before 0030 have no
  tenant: visible to callers without a tenant (their creator) and to
  administrators.
- Embedders' handlers registered with `register` become reserved: use
  `register_public` for kinds HTTP callers may enqueue.
- Kinds under the module prefixes above are refused to non-privileged
  callers even without a handler.
- `function.invoke` jobs of non-privileged callers act as the caller (rules
  and tenancy apply), not as `service:function.<name>`; before-hooks of
  administrators' writes no longer act as the administrator.

**For FB-146 (jobs on PostgreSQL).** `context` is `jsonb` on PostgreSQL and
`TEXT CHECK (json_valid(context))` on SQLite; the service reads it as text
(`Row::opt_text`), like `payload`. The scope filter is `tenant = ?n` /
`tenant IS NULL` plus `created_by = ?m OR schedule_id IN (SELECT ...)`;
the claim filter adds `kind NOT IN (...)` and `NOT (kind LIKE 'p%' OR ...)`
(PostgreSQL `LIKE` is case-sensitive, SQLite's is not for ASCII; both only
exclude more). The idempotency lookup matches `COALESCE(tenant, '') = ?3`,
the expression of the unique index on both engines.
`rowid` ordering in list and workflow reads is unchanged (FB-146's).
**For FB-147 (functions SQL).** No new functions tables; the execution log
does not record the context a run used (a possible `actor` column).
**For the dashboard jobs and functions studios.** Show `tenant` on jobs,
schedules and workflows; list or `GET` a job before subscribing to its
channel; explain `engine:read_jobs:all` in the roles editor; reserved kinds
answer `403` (offer only public kinds and `function.invoke` of functions the
user may invoke).

## Addendum (FB-141): queue tenancy, resource permissions and record views

Written 2026-11-11 by FB-141. Closes `rls-coverage.md` G2 and G16 in
`forgebase-queue`. Backend-neutral: new SQL uses numbered `?N`
placeholders and constructs valid on both engines; migration pair 0031
(`queue_rls`).

**Tenant-scoped names (G16).** `_forgebase_queue_topics.tenant`,
`_forgebase_queue_subscriptions.tenant` and `_forgebase_queue_snapshots.tenant`
(NULL = instance-wide, every resource before FB-141). A caller with a
tenant creates resources of that tenant, stored as `<tenant>/<name>` (queue
names never hold `/`, and every name is validated before lookup, so a
stored name is never addressable), exactly as FB-140 did for buckets. A
name resolves to the caller's tenant resource, else the instance-wide one;
another tenant's topic, subscription or snapshot is unreachable. A tenant
may not create a name an instance-wide resource holds (`409`). A
subscription's topic and dead-letter topic are resolved in the creator's
scope (a tenant may subscribe to an instance-wide topic). Listings show
the tenant's resources plus the instance-wide ones it does not shadow;
topic metrics count only the subscriptions of the caller's scope. The wire
`Topic`, `Subscription` and `Snapshot` gained `tenant`; names (topics,
subscriptions, dead-letter topics, messages' `topic`, dead letters, push
bodies and headers) are always the bare name. Record-source topics are
still created by privileged callers only; one created in a tenant relays
only that tenant's changes.

**Permissions (G16).** Every call asks `Authorizer::authorize_resource`
(FB-134) about the topic or subscription it acts on:

| Call | Action, kind, operation |
|---|---|
| create topic | `queue_admin`, topic, `create` (so `engine:queue_admin:<name>` lets one create that name) |
| get topic, topic metrics | `queue_read`, topic, `view` |
| browse messages | `queue_read`, topic, `list` |
| update / delete topic | `queue_admin`, topic, `update` / `delete` |
| publish (REST, `publish_in_tx`, `ctx.queues.publish`) | `queue_publish`, topic, `create` |
| create subscription | `queue_admin`, subscription, `create`; plus `queue_read` on the topic (`list`) and `queue_publish` on the dead-letter topic (`create`) |
| get subscription, metrics | `queue_read`, subscription, `view` |
| dead letters | `queue_read`, subscription, `list` |
| update / delete subscription, seek | `queue_admin`, subscription, `update` / `delete` / `update` |
| pull, WebSocket and SSE streams | `queue_consume`, subscription, `list` (streams re-check every round) |
| ack, nack, modify deadline | `queue_consume`, subscription, `update` |
| snapshots | on their subscription: create `queue_admin`/`create`, get `queue_read`/`view`, delete `queue_admin`/`delete` |

Listings (topics, subscriptions, snapshots) return everything in scope for
holders of the plain `engine:queue_read`, else the resources the caller
may `list` (resource keys, ownership, rules); with none of those the plain
`403`/`401` stands. **Ownership**: a subscription's creator
(`created_by`, the principal key) is passed as `ResourceRef::owner`, so the
owner holds every permission on it (consume, read, administer) without a
grant; an explicit deny still wins. Admin actions stay permission-only
(FB-134). Queue resource rules read `@request` only (no item fields; the
FB-134 default schema), so a `Filter` decision is a denial. HTTP: a call
on a path resource is checked before its body is read; create calls (the
name is in the body) are checked by the service once the body parses,
and a body that does not parse is answered after the plain check, so
anonymous and forbidden callers still get `401`/`403` first.
`QueueService::authorize_topic` and `authorize_subscription` are the
public pre-checks.

**Record-source subscriptions (G2).** Evaluated at **delivery**, like
realtime. The relay no longer builds messages as the system: a
record-change message stores the changed row in
`_forgebase_queue_messages.source_row` (`{collection_id, tenant,
snapshot}` with the outbox row snapshot) and `data.record` is null at
rest. A subscription records its creator's context in
`_forgebase_queue_subscriptions.owner_context` (principal key, realm,
organization, tenant, key scopes). Each claim (pull, stream, push)
resolves the owner **as they are now** (an administrator or service
account must still exist and be enabled, with its current admin role; a
user record must still exist, its fields are the `@request.auth.*`
attributes) and fills `data.record` through `DataService::view_snapshot`
as that owner: tenant clause, current View rule, hidden fields and field
rules. A message the owner may not see (another tenant, the rule, a
deleted collection, a deleted owner) is withheld: its delivery row is
dropped (`forgebase_queue_withheld_total`), and the claim continues with
the next ones. Redeliveries are evaluated again. Dead letters store the
owner's view; a copy on a dead-letter topic keeps `source_row`, so its
subscribers get their own view. Browsing a topic evaluates each record as
the reader and leaves out the rows they may not see (a page may hold fewer
items than `limit`). Subscriptions from before FB-141 (no
`owner_context`) act as their `created_by` in the default realm.
Record-change messages from before FB-141 (full system view in `data`)
are marked `source_row = 'legacy'` by the migration and are delivered and
shown only to privileged owners and readers.

**Edge functions.** `ctx.queues.publish` goes through
`QueueService::publish` with the caller's context (FB-93), so it gets the
same resource checks and resolves names in the caller's tenant. Push to a
function still invokes it with its own identity (FB-142 owns recorded
invocation contexts).

**Breaking.** Non-privileged consumers of record-source topics get the
owner's view instead of every field of every row: hidden fields, other
tenants' rows and rows outside the owner's View rule are gone, and
messages can be withheld. Creating a subscription needs `queue_read` on
its topic (and `queue_publish` on a dead-letter topic). Tenant callers
create tenant topics (callers with a tenant used to share one global
namespace). `Topic`, `Subscription`, `Snapshot` gained `tenant`;
`QueueService::authorize` keeps the plain check.

**For FB-146 (queues on PostgreSQL, pgmq per ADR-0029).** Keep the stored
name scheme or replace it with a `(tenant, name)` key: pgmq queue names
are identifiers (`pgmq.q_<name>`, 47 characters), so map a stored name to
a hashed pgmq queue name and keep `tenant` in ForgeBase's own tables. The
owner view must stay in ForgeBase (it runs the data core's rules), not in
pgmq: keep `source_row` with the message (pgmq message JSON can carry it)
and fill `record` at read time. New SQL to check on PostgreSQL:
`(tenant IS NULL OR tenant = ?1)` with a NULL parameter,
`(?2 IS NULL OR topic = ?2)`, and the 0031 twin's `UPDATE ... WHERE topic
IN (SELECT ...)`. Resource placement (ADR-0029 §3.2): `tenant` on topics
and subscriptions is the placement key; instance-wide ones stay on
control. **For the dashboard queues studio.** Show `tenant` on topics,
subscriptions and snapshots and send `X-ForgeBase-Tenant` when creating
them; show the owner (`created_by`) of a subscription; offer
`queue_topic` / `queue_subscription` resource rules with `@request` only;
explain that record-source messages show the owner's (or, when browsing,
your own) view and that withheld messages do not appear.

## Addendum (FB-137): realm-scoped administrators

Written 2026-11-11 by FB-137. Closes `rls-coverage.md` G19: every
administrator was a cross-realm superuser, and impersonation never checked
the target's realm. Backend-neutral: no migration; the new SQL is plain
`IN`/`NOT IN` lists and sub-selects, valid on both engines.

**Who is a realm administrator.** A user, or a service account without the
`admin` role, that holds the permission key `identity:admin` in its **home
realm** at realm level: through a realm binding (`PUT
/api/v1/realm-bindings` with a realm role that allows `identity:admin`, or
`*`) or a realm-level override. Org memberships and teams do not count.
Auth cannot call the policy engine (policy depends on auth), so
`forgebase_auth::RealmAdminCheck` (`Fn(&AccessContext) -> bool`) is
installed with `AuthService::set_realm_admin_check`; the router
(`forgebase_api::router_with`, next to the realm creator) wires it to
`PolicyService::has(ctx, IDENTITY_ADMIN, None)` with the caller's context in
its home realm. Policy step 2 refuses cross-realm access, so a binding in
another realm makes nobody an administrator there, and a suspended account
holds nothing. Without the check (the engine used on its own) only
platform administrators administer.

**Scope.** `AuthService::admin_scope(caller) -> AdminScope`:
`Platform` for administrators and admin service accounts (unchanged),
`Realm(home)` for a realm administrator, `403` otherwise. An impersonation
session never administers, whoever it impersonates. Helpers in
`service/mod.rs`: `require_realm_admin(caller, realm)`,
`require_admin_of(caller, subject)` (users and non-admin service accounts
of the realm; platform admins and admin service accounts are platform
targets), and `realm_users_sql` / `realm_principals_sql`, which turn a
realm into conditions over collection or principal columns (users of
`default` are the auth collections no other realm names, as in
`RealmDirectory::realm_of_collection`). `require_admin` keeps meaning
"platform administrator".

**What a realm administrator reaches.**
- Users (`/api/auth/users/{collection}/{id}` detail, sessions, revoke,
  sign-in history, and the lifecycle calls: status, suspend, unsuspend,
  deactivate, reactivate, delete, anonymise, export): users of their realm.
  The realm check runs before the existence check, so another realm's ids
  give `403` whether or not they exist.
- Service accounts and keys: create `service`-role accounts in their realm
  only (`X-ForgeBase-Realm` defaults to it; `admin` role `403`); get, list,
  delete and create keys for their realm's non-admin accounts (others are
  `404`); `GET /api/auth/api-keys` and `DELETE /api/auth/api-keys/{id}`
  cover their realm's users' PATs and accounts' keys (others `404`).
  FB-138 tenant bindings are unchanged: `create_api_key_with` validates the
  binding for the key's owner as before.
- Realm identity: `GET /api/auth/realms` lists their realm only; `GET`/`PUT
  /api/auth/realms/{realm}` and the FIX-4 endpoints under it (consents,
  OIDC key ids, social providers, SMS) and `POST /api/auth/keys/oidc/rotate`
  for their realm. Setting a realm's user pool up for the first time
  (`PUT` on a realm without identity, `POST /api/auth/realms`) stays
  platform-only.
- Audit: `GET /api/auth/audit` returns the rows whose `realm` column
  (0014) is their realm: events about their users, and subject-less events
  of requests that selected their realm. Rows with no realm (requests
  without `X-ForgeBase-Realm`, platform events) are platform-only.
- Impersonation: users of their realm only (same rules: interactive
  session, MFA when enrolled, reason, TTL), never another realm
  administrator (`cannot_impersonate_admin`). The session's `impersonator`
  is the realm administrator's principal `user:<collection>/<id>`
  (platform administrators keep their id). They list and end
  impersonations of their realm's users only.

**Platform-only, unchanged:** administrators (`/api/auth/admins`), the
auth signing key rotation, realm creation, IdP client apps
(`/api/auth/clients`, still `require_admin`), and everything
outside `/api/auth`. The first-run setup (FB-90) creates a platform
administrator as before. `GET /api/auth/me` gains `admin_scope`
(`"platform"` or `{"realm": "<id>"}`, absent for non-administrators) for
the dashboard (FB-102).

**PostgreSQL (FB-145).** The new SQL keeps the existing `?N IS NULL OR`
pattern and adds `col IN (?n, …)`, `col NOT IN (…)` and
`owner_id IN (SELECT id FROM _forgebase_service_accounts WHERE realm = ?n
AND role <> 'admin')`; `revoke_key` keeps its `?6 OR …` boolean parameter
(an integer on SQLite; FB-145 ports it with the rest of the file).
Tests: `crates/forgebase-server/tests/security_fb137.rs`.

## Addendum (FB-147): KV, graph, vector, storage and functions on PostgreSQL

Written 2026-11-11 by FB-147. The five crates run on both backends: each
`tests/common` helper uses `forgebase_db::testing::test_backend()`, and the
suites of `forgebase-kv`, `-graph`, `-vector`, `-storage` and `-functions`
pass with `FORGEBASE_TEST_BACKEND=sqlite` and `=postgres` (PostgreSQL 18.6
with pgvector). No migration.

**What was SQLite-only, and what it became.**
- *Id lists*: every `IN (SELECT value FROM json_each(?))` goes through
  `Dialect::in_json_list` (graph, vector ids). Vector item keys are integers,
  so a local helper casts the elements to `bigint` on PostgreSQL
  (`key = ANY (SELECT CAST(jsonb_array_elements_text(…) AS bigint))`): the key
  stays a plain bigint comparison.
- *Graph walk*: the PostgreSQL form has **one** reference to the recursive
  term. The steps (forward, backward, undirected) are a `CROSS JOIN LATERAL
  (… UNION …)` inside it; the row cap is a `LIMIT` on `SELECT … FROM walk`
  **outside** the CTE (rows are produced on demand, so it stops the walk);
  `UNION` (not `ALL`) with the `w.depth < ?3` bound is the cycle guard. The
  per-step node join and the edge condition of FB-139 are unchanged, so label
  rules stay pre-filters. The SQLite text is kept as it was (it is measured).
  The anchor is `CAST(?2 AS text)`.
- *Property filters*: `n.properties -> ?key = CAST(?json AS jsonb)` on
  PostgreSQL, so `true` is not `1`; the SQLite `json_extract` form is kept.
- *Merge patch*: `json_patch` has no PostgreSQL twin. Node and edge property
  patches (RFC 7396) are applied in Rust inside the write transaction and the
  result is written back, on both backends.
- *Optional parameters*: `(CAST(?n AS text) IS NULL OR col = ?n)` in the node
  and edge lists and the walk (FB-143's rule: PostgreSQL cannot type a bare
  `?n IS NULL`).
- *Vector hybrid search*: the keyword side reads `_forgebase_vector_fts`
  (FB-133): `f.fts @@ websearch_to_tsquery('simple', _forgebase_unaccent(?))`,
  ranked by `ts_rank` (higher is better; `bm25` is lower-is-better, so the sign
  is handled per backend), joined to the items on `rowid`. The caller's rule and
  filter stay ANDed into the keyword query as pre-filters (FB-139). The query
  text for PostgreSQL is built by `match_query`: quoted words joined by `or` or
  a space; FTS5 operators stay literal. Ranks differ from bm25 (documented in
  §7.4). The vector index itself (USearch over canonical `bytea`) is the
  portable path on both backends, as the plan says; pgvector as the PostgreSQL
  engine is FB-162 (`VectorService::item_rule(…).compile(b, alias)` is the
  pre-filter hook it needs).
- *Storage*: `ON CONFLICT … DO UPDATE SET refcount = refcount + 1` was
  ambiguous on PostgreSQL (the target table is named); `MAX(a, b)` (scalar)
  became a `CASE`. The rest was already portable (`INSERT … ON CONFLICT`,
  `substr`, FB-140's jsonb folders and `CAST(?n AS jsonb)` in the rule check).
- *Functions*: the version-size query uses `octet_length` on PostgreSQL
  (`CAST(source AS BLOB)` is SQLite-only). The rest of the SQL (cursors,
  secrets, versions) was portable.
- *KV*: no SQL change; the flush upsert already uses the scope key
  `(tenant, namespace, key)` and ran unchanged.

**Found by FB-142, fixed here.**
1. *`scenario_18` (WASM `kv incr`, status 7)* was a host bug, not a test bug:
   FB-138 refuses `fn.<name>` to non-privileged callers and says functions reach
   it "as the system context in the caller's tenant", which the JavaScript host
   already did. The WASM host called KV as the function's own non-privileged
   identity. It now uses the system context with the caller's tenant; the test
   is unchanged.
2. *`run_after_hooks` double delivery*: the cursor was read, advanced and stored
   with no lock, so a manual call racing the 250 ms task re-read the old cursor.
   A `tokio::sync::Mutex` is held for the whole pass; a second caller waits and
   starts from the stored cursor. Test
   `concurrent_after_hook_passes_deliver_each_event_once` (three racing passes,
   12 events: 36 deliveries before, 12 now). Delivery is still at-least-once
   across a crash between the hook and the cursor write.

**Deviations.**
- `storage_quota_exceeded_is_429_and_deletes_give_it_back` returns early on
  PostgreSQL: the usage counters are in `forgebase-entitlements` (error
  `operator is not unique: unknown + unknown`), which FB-145 ports.
- The index-name assertion of the 10 000-edge graph test is backend specific:
  SQLite must use `_out`/`_in`; PostgreSQL chooses among the three edge indexes
  by statistics, so the test asserts an index plan with no sequential scan.
- Server suites use their own `security_common` harness, outside this
  package's files; they were run unchanged.

**For FB-152 (native RLS).** Graph and vector rules are SQL conditions over
real columns (`GraphAccess::node_sql/edge_sql`, `ItemRule::compile`); a native
policy can reuse them. The keyword query joins `_forgebase_vector_items`
through the `ITEMS` subselect, so a policy on the items table applies to it.
**For FB-160/FB-161 (AGE and the extension layer).** The canonical graph store
stays `_forgebase_graph_*`; read a derived AGE graph through
`GraphService::access` (FB-139). The lateral walk is the reference result an
AGE traversal must match, including `UNION` de-duplication and the depth bound.
**For FB-163 (openCypher).** Property filters compare typed jsonb, so Cypher
`WHERE n.p = true` should compile the same way; merge patches are applied in
Rust, not SQL.

## Addendum (FB-146): jobs, queues and realtime on PostgreSQL

Written 2026-11-11 by FB-146. `forgebase-jobs`, `forgebase-queue` and
`forgebase-realtime` run on both engines; their crate suites run on the
backend `FORGEBASE_TEST_BACKEND` selects (§7.7). Also closes the FB-141 /
FB-142 follow-up: named-queue settings and workers are tenant-scoped.
Migration pair 0035 (`job_queue_tenant`).

**`rowid` tie-breaks.** Job and workflow reads order by
`Dialect::row_order_col()`: SQLite's `rowid`, the `_rowid bigint GENERATED
BY DEFAULT AS IDENTITY` column on PostgreSQL. `_forgebase_jobs` had it from
the baseline; the PostgreSQL half of 0035 adds it to
`_forgebase_job_workflows` (workflow lists break `created_at` ties on it), and
`postgres::ROWID_TABLES` lists both, so the parity test and `db convert`
(FB-151, which copies `rowid` into `_rowid` and `setval`s it) cover it.
Transaction helpers get the column from `Fx::row_order` (the per-transaction
effects value already threaded through every write path), so no SQL names
`rowid` any more. The orders kept: claim `priority DESC, run_at, _rowid`;
lists `created_at DESC, _rowid DESC`; workflow jobs and parents `created_at,
_rowid`; compensations `finished_at DESC, _rowid DESC` (newest step first).

**Delivery history** (`_forgebase_queue_deliveries.history`, text JSON) is
built by `forgebase-queue/src/history.rs` from the dialect's JSON array
functions: `json_array_append`, `json_set_last` (nested for outcome and
error), `json_array_length`. Two pieces have no dialect method and are
spelled per engine there: dropping the oldest of 25 entries
(`json_remove(history, '$[0]')` / `history::jsonb - 0`) and building the
entry (`json_object` / `jsonb_build_object` with `CAST(?n AS bigint|text)`,
since PostgreSQL cannot type a bare parameter inside a variadic call). The
`jsonb` result is stored into the `text` column by PostgreSQL's assignment
cast (its text form has spaces after `:` and `,`; readers parse it).

**`last_insert_id` → `RETURNING seq`** for queue messages (the published
message's `seq`). No other caller in these crates used it.

**Other PostgreSQL fixes.** `INSERT OR IGNORE` on job dependencies →
`INSERT … ON CONFLICT DO NOTHING` (both engines); `CASE WHEN … THEN NULL
ELSE ?n END` into `bigint` columns casts the parameter (`CAST(?n AS BIGINT)`;
PostgreSQL resolves an untyped `CASE` to `text`); the queue's
`?2 IS NULL OR topic = ?2` and the nack's `CASE WHEN ?4 IS NULL` cast the
parameter to `TEXT` (42P18); arithmetic on a bare parameter
(`?4 - 120000`, `?1 - 3600000` in the queue rate windows and pruning) casts
it to `BIGINT` (PostgreSQL types `unknown - integer` as `int4`, too small
for epoch milliseconds); the scalar two-argument `max(attempts - 1, 0)`
(job and delivery release) → `CASE`; the rate upsert qualifies its target column
(`_forgebase_queue_rates.count + excluded.count`; PostgreSQL calls a bare
`count` ambiguous). The FB-141 SQL FB-141 asked to check runs on
PostgreSQL: `(tenant IS NULL OR tenant = ?1)` with a NULL parameter types
from the column, and the 0031 twin's `UPDATE … WHERE topic IN (SELECT …)`
is in the migrated test template. Realtime had no SQLite-only SQL. The
claim-plan guard (`explain_claim_plan`) runs `EXPLAIN (COSTS OFF)` with
sequential scans off on PostgreSQL and fails on a `Sort` node, as it fails
on `USE TEMP B-TREE` on SQLite. One fix outside these crates: the quota
check in `forgebase-entitlements/src/usage.rs` (`?5 + ?6 <= 0`, "operator
is not unique: unknown + unknown") casts its parameters; queue metering
goes through it. That crate is FB-145's; the change is three casts.

**Commit-ordered cursors and the single writer (ADR-0011, ADR-0022).** The
queue's subscription cursors and the realtime dispatcher read rows with
`seq > cursor` and never look back; that is correct only while sequence
order is commit order, which holds because there is one writer connection
(§7.3). Two tests per crate pin it (`tests/single_writer.rs` in
forgebase-queue and forgebase-realtime), on both backends:

1. `pool_stats().writer.max == 1`.
2. Eight concurrent publishers (or record writers) commit while a cursor
   tails the table; the cursor must read every committed `seq` once, in
   order. A transaction that took a lower `seq` and committed after a higher
   one would be skipped and fail the test.
3. PostgreSQL: while they run, a sampler reads `pg_stat_activity` for
   sessions of the database holding a transaction id (`backend_xid IS NOT
   NULL`) other than ForgeBase's writer (`pg_backend_pid()` of the writer
   connection). Each is classified (`tests/common/writers.rs`): allowed are
   exactly the two writers of the FB-158 addendum: pg_cron running
   `SELECT forgebase.fire_schedule(…)` and nothing else (backend type
   `pg_cron`, or a client named `pg_cron` when it runs jobs over libpq),
   and logical-replication apply, parallel-apply and table-sync workers
   during a tenant move (ADR-0029 §1.7, §3.8). Autovacuum workers are
   ignored (ANALYZE writes `pg_statistic`, never ForgeBase rows). Anything
   else fails the test. `a_second_writer_is_detected` opens a second handle
   on the same database and checks that its open write transaction is
   reported. On SQLite every connection takes the file's write lock before
   it allocates a row id, so commit order equals sequence order with any
   number of connections; the pool check is what applies there.

**Tenant-scoped queue settings and workers (FB-141/FB-142 follow-up).**
Before, `_forgebase_job_queues` and `_forgebase_job_workers` were one
instance-wide namespace: any `read_jobs` holder listed every tenant's
settings, job counts and workers, any `manage_jobs` holder changed or
deleted another tenant's settings (pausing `emails` paused it for every
tenant), and any `work_jobs` holder took another tenant's worker id over.
Now both tables have `tenant` and `created_by` (migration 0035; NULL on
older rows):

- Settings are per tenant: a caller configures the queues of its own tenant
  (`ctx.tenant`; none: the tenantless queues). Tenant settings are stored as
  `<tenant>/<queue>` (queue names never hold `/`), so two tenants configure
  the same queue name. They govern only that tenant's jobs: enqueue
  defaults, pause, concurrency and rate limits match a job by the key
  `CASE WHEN tenant IS NULL THEN queue ELSE tenant || '/' || queue END`
  (`queues::job_queue_key`). An administrator acting without a tenant
  configures the tenantless queues only; to change a tenant's queue it acts
  in that tenant (`X-ForgeBase-Tenant`).
- Visibility follows FB-142's job scope: privileged callers see every row;
  `engine:read_jobs:all` holders the rows of their tenant; others the rows
  of their tenant that they created. Queue job counts include only jobs the
  caller may see. Changing or deleting settings the caller cannot see
  (same tenant, another creator) answers 403. `QueueInfo` gained `tenant`
  and `created_by`.
- Workers belong to the tenant and principal that registered them first.
  Lists use the same scope. Registering an id the caller cannot see answers
  `409 worker_id_taken`; a claim or heartbeat under such an id leaves the
  row alone (it never creates or touches a row the caller cannot see).
  `Worker` gained `tenant` and `created_by`.

Tests: `crates/forgebase-server/tests/security_fb146.rs` (both fail before
FB-146): `queue_settings_belong_to_a_tenant_and_govern_only_its_jobs`,
`workers_belong_to_a_tenant_and_their_ids_cannot_be_taken_over`.

**Test harnesses.** The three crates' `tests/common` open the test database
through `forgebase_db::testing` (dev-dependency with feature `testing`):
SQLite in a temp directory as before, or a database copied from the shared
PostgreSQL template. `Store::open` reopens the same database, so the
restart tests (crashed pool, restart survival, push secret) run on both.

**Breaking.** Queue settings without a tenant no longer limit tenant jobs,
and tenant callers no longer see or change tenantless settings; settings
saved before FB-146 stay tenantless and keep governing tenantless jobs.
Non-privileged callers without `engine:read_jobs:all` see only queue
settings and workers they created (older rows have no creator: privileged
callers only). Reusing another tenant's or user's worker id answers 409.
`QueueInfo` and `Worker` gain fields (regenerate OpenAPI/SDK fixtures).

**For FB-164 (pgmq deliveries).** Keep `seq` semantics: pgmq's `msg_id`
is a sequence too and has the same commit-order property only under the
single writer; the single-writer tests read `_forgebase_queue_messages`
and must move to whatever table the cursor tails then. Delivery history
can stay in ForgeBase's table (`history.rs`), or go into the pgmq message
JSON with the same entry shape. Tenant settings keys (`<tenant>/<name>`)
are FB-141's scheme; hash them to pgmq identifiers. **For FB-165 (pg_cron
schedules).** `forgebase.fire_schedule()` must insert jobs exactly as
`insert_job` does, including the FB-146 queue defaults lookup by
`<tenant>/<queue>` (`default_priority`, `default_timeout_ms` of the
schedule's tenant) and `_rowid` (identity default). pg_cron's session is
allowed by the census only while it runs `SELECT forgebase.fire_schedule(…)`
alone; the command must be exactly that. **For FB-157 (concurrent
writers).** The census and cursor tests fail by design once a second writer
exists; they are the gate to replace with per-set fences.

## Addendum: FB-145 — auth, policy, customize, credits, entitlements and billing on PostgreSQL

No new migration. The six crates' SQL now runs unchanged in meaning on both backends.

- **Upserts.** `INSERT OR IGNORE` became `INSERT ... ON CONFLICT DO NOTHING`; `INSERT OR REPLACE` (suspensions, account status) became `ON CONFLICT (pk) DO UPDATE SET` for every column. In `DO UPDATE` the target's own columns are qualified with the table name (`next`, `attempts`, `product_id`, ...): PostgreSQL calls an unqualified one ambiguous.
- **Scalar `MAX(a, b)` / `MIN(a, b)`** exist only in SQLite. They are now `CASE WHEN ... END` (`updated_at` bumps, cursors, `synced_version`, pack `remaining`, lockout `locked_until`), with `CAST(?N AS BIGINT)` on the parameter.
- **Typed parameters.** PostgreSQL cannot type a parameter that only appears in `?N IS NULL` or in arithmetic (`?5 + ?6`, `?4 - ?1`). Every `?N IS [NOT] NULL OR col = ?N` filter is `CAST(?N AS TEXT|BIGINT) IS NULL OR ...`. Integer booleans bound as parameters (`?6 OR`, `NOT ?3`) are `CAST(?N AS BIGINT) = 1|0`.
- **`col IS ?N`** (a NULL-safe compare against a parameter) is `IS NOT DISTINCT FROM ?N`, which SQLite 3.39+ also accepts.
- **JSON.** `json_each(scopes)` in the OIDC logout path goes through `Dialect::json_array_elements`. The backend already encodes text parameters into `jsonb` columns, so no write needed a cast.
- **Credits ledger.** The entry `seq` comes from `INSERT ... RETURNING seq` instead of `last_insert_id`, which is `None` on PostgreSQL.
- **Audit filter.** `substr(event, 1, length(?1))` casts `?1` to text.
- **Test harnesses.** All six crates' `tests/common` open the database with `forgebase_db::testing::test_backend()`, so `FORGEBASE_TEST_BACKEND=sqlite|postgres` selects the backend (dev-dependency `forgebase-db` with feature `testing`). Auth tests that restart a service reopen the same database through `TestDb::options()`. Tests that peeked at SQL specifics now use the dialect: `introspect_tables()` for the plaintext-secret scan, `CAST(jsonb_column AS TEXT) LIKE`, `ON CONFLICT DO NOTHING`. The append-only ledger test rolls back after each refused statement (PostgreSQL aborts the transaction). The social-login test that reads the SQLite file skips on PostgreSQL.
- **Not ported / depends on others.** Seven tests that run the job queue (customize email deliveries x4, credits maintenance expiry, entitlements alert delivery, and the auth secret-dump test that needed the `forgebase-db` statement fix) pass only with FB-146's `forgebase-jobs` and `forgebase-db` changes (`rowid` ordering, statement preparation). They were verified green on a throwaway merge of `agent/worker-fb146` and fail on this branch alone until FB-146 merges. The entitlements quota `UPDATE` in `usage.rs` is identical to FB-146's version (same text, to merge without a conflict).

## Addendum: FB-150 — backups, restore and snapshots on PostgreSQL

Built as §5 describes. Single data set only: a data directory with
`postgres/nodes/<name>/` (replicas, shard sets) is refused by backup and
restore with a message naming FB-179 and its future `forgebase backup
--cluster`.

**Backup.**
- Managed: `pg_basebackup -D <backup>/postgres -Fp -X stream --checkpoint=fast
  --manifest-checksums=SHA256` as `forgebase_super` over the Unix socket
  (`--checkpoint=fast` makes the extra `CHECKPOINT` unnecessary). Online, no
  lock. Every file under `postgres/` is hashed into `MANIFEST.json` (kind
  `postgres`), next to PostgreSQL's own `backup_manifest`.
- `roles.secret` is copied into the backup (path `roles.secret`): the
  restored cluster holds the scram hashes of those passwords, so without the
  file the bundled engine could not log in. Treat a backup as secret as the
  data directory. The master key rule is unchanged.
- External server: `pg_dump -Fc --no-owner -n forgebase -n public` into
  `postgres.dump` (kind `dump`). Collection tables live in `public`.
  Needs `pg_dump` from `engine_dir`/`FORGEBASE_PG_ENGINE_DIR` or `PATH`.
- Manifest: format 2 for PostgreSQL (format 1 stays for SQLite, so older
  builds refuse a PostgreSQL backup with "unsupported manifest format 2"),
  new `engine {kind, mode, version, major, method}` (also written for SQLite;
  absent = SQLite), `database_only`, `note`. `FileKind` gains `postgres` and
  `dump`.
- `BackupOptions.pg: Option<PgSource>` (tools directory, socket, port,
  `forgebase_super` password, or an external URL) is required on PostgreSQL;
  `BackupService::with_pg`. Passwords reach the tools only through a private
  `PGPASSFILE` (removed afterwards), never argv or the environment.
  `forgebase-backup` does not depend on the engine crate or the server.

**Verify.** Hashes and the file list as before, then `pg_verifybackup
--exit-on-error postgres/` and a `PG_VERSION` vs manifest major check (dump:
`pg_restore --list`). `verify_with(dir, VerifyOptions{pg, tmp})` passes the
tools; plain `verify(dir)` without them passes hashes only and warns that
`pg_verifybackup` did not run. The CLI, `restore` and `doctor --deep` always
pass them when an engine directory is found.

**Restore.** Offline and journalled as before; the items are `postgres/data`
and `postgres/roles.secret` (plus storage, indexes, key) instead of
`forgebase.db*`; the old cluster goes to `backups/pre-restore-*/postgres/`.
Refused: a different PostgreSQL major than the running engine, a live
postmaster on the target, a SQLite target for a PostgreSQL backup and the
reverse (message points at `db convert`, FB-151), a marker that contradicts
the backup, several data sets (FB-179). WAL replay happens on the first
start. The CLI writes the backend marker after the swap. A logical backup is
loaded with `pg_restore --clean --if-exists --single-transaction` first
(needs `--force`), then files are swapped.

**Pre-migration snapshots.** `forgebase-db` gets the trait
`PreMigrationSnapshot` and `Database::set_pre_migration`; PostgreSQL
`migrate` calls it before applying anything to a database with applied
migrations, and fails the migration if the snapshot fails. The server
(`database::open_owned/open_attached`) installs
`forgebase_backup::pre_migration::PgPreMigration` on managed databases, so
the server start and `forgebase migrate` both snapshot into
`backups/pre-migrate-v<N>-<time>/`: a database-only physical backup
(`database_only`, kind `pre-migration`). `forgebase restore <dir> --force`
puts the database back and leaves storage, indexes and keys alone. External
servers get a warning, as before. The hook does not touch `Database`
(migrate holds the only writer connection): `backup::create_physical`.

**Other.** `GRANT pg_checkpoint TO forgebase_server` in the FB-132 bootstrap;
`database::checkpoint` now runs on PostgreSQL (no-op on external). `forgebase
db dump [FILE] [--plain] [--schema-only]`: `pg_dump` of the whole database
without owners and privileges, 0600, works while the server runs. `doctor`:
newest snapshot's engine against the project's backend and PostgreSQL major,
`--deep` verifies with `pg_verifybackup`, pre-migration snapshot count,
leftover `.pgpass-*`/`*.partial` files in `backups/`. `backup list` shows the
engine. The server's `/system/backups` snapshots work on PostgreSQL
(`BackupService::with_pg`).

**For FB-151 (`db convert`).** Convert should take a fresh snapshot with
`BackupOptions{pg}` first; restore refuses cross-backend and names
`db convert`. Write the marker after the data (`database::write_marker`).
**For FB-179.** Plug in at `pg.rs::ensure_single_set` (the refusal),
`backup.rs::copy_database` (one `basebackup` per set into
`postgres/` → `sets/<name>/`) and `restore.rs::items_of/target_path`;
the manifest `engine` and format 2 can grow a `sets` list; `basebackup`
takes any `PgSource` endpoint, so a replica source is another `Managed`
endpoint; replicas need `needs_rebuild` after a restore.

**Tests.** `forgebase-backup/tests/postgres.rs` (6, real clusters: round
trip, damage vs hashes and `pg_verifybackup`, swap/crash/rollback, refusals
incl. FB-179, snapshots and the migrate hook, external logical round trip),
`forgebase-cli/tests/backup_e2e.rs` (3, both backends), unit tests in
`pg.rs`.

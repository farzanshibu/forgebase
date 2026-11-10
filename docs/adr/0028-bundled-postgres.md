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
runs `ALTER ROLE <session_user> IN DATABASE <db> SET search_path =
forgebase, public` (an ordinary role may change its own defaults; scoped to
the one database so an operator's other databases are untouched), sets it on
the current session and closes the idle read-pool connections opened before
the change. Tested with an ordinary, non-superuser role that owns its
database (the external-server case): it migrates, owns the tables, and its
new sessions find the system tables unqualified. *Test engine:* copies of a
template database do not inherit per-database settings, so
`forgebase_db::testing` sets the search_path on its role
(`ALTER ROLE CURRENT_USER SET …`, the stand-in for FB-132's roles) and now
fails a test when migration fails instead of leaving the database empty.

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

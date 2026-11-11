# Apache AGE in the engine build (FB-160)

Hook directory for `build-unix.sh` (see `../extensions/README.md`): Apache AGE
1.8.0 for PostgreSQL 18, from the ASF release tarball pinned as
`[sources.age]` in `engine.lock`, built with PGXS on macOS and Linux (musl).
Windows archives do not have it (no MSVC build upstream, apache/age#2502) and
say so with `features.graph_cypher = false` in their manifest.

| File | What it does |
|---|---|
| `patches/*.patch` | unreleased upstream fixes, applied in name order with `patch -p1` (ADR-0029 §9a, decision 1 B) |
| `make-args` | `BISON=` the first GNU Bison 3 found (Homebrew on macOS, `bison` on Alpine); the build stops with a message when there is none |
| `post-install.sh` | checks every patch against `[[sources.age.patches]]` in `engine.lock` (same files, same order, same SHA-256), checks the licence and NOTICE against upstream, ships the NOTICE, and links `lib/plugins/age.<so\|dylib>` to `../age.<so\|dylib>` |

## Patches

| File | Upstream | What it fixes |
|---|---|---|
| `0001-pr2475-detach-delete-rls-segfault.patch` | [#2475](https://github.com/apache/age/pull/2475), commit `2e4b4ad9`, verbatim | backend crash (SIGSEGV) on `DETACH DELETE` when an edge label has an RLS policy that calls a SQL function |
| `0002-pr2561-rls-and-select-acl-in-traversals.patch` | [#2561](https://github.com/apache/age/pull/2561), commit `3eeb20bd`, verbatim | variable-length and shortest-path traversals read label tables with a raw heap scan that skipped RLS and `SELECT` privileges; adds `age.enforce_rls_in_traversal` (default on, superuser-only) |
| `0003-pr2578-utility-commands-without-age.patch` | [#2578](https://github.com/apache/age/pull/2578), commit `6e073cf4`, backported | with AGE loaded, `TRUNCATE` in a database without the extension failed reading AGE's catalog; 1.8.0 has only the `TRUNCATE` branch, so the backport keeps that hunk and the upstream tests |

Each patch is dropped when an AGE release for PG18 contains it: delete the
file and its `[[sources.age.patches]]` table, bump `[sources.age]`.

Changing a patch: edit the file, put its new SHA-256 in `engine.lock`
(`shasum -a 256 deploy/postgres-engine/age/patches/*.patch`), and say in the
ADR-0029 addendum what changed and that the diff was reviewed.

## Bison

AGE's Cypher grammar is a GLR grammar built with `-Wno-error=conflicts-sr`,
which needs GNU Bison 3. macOS ships Bison 2.3: `brew install bison` (keg-only;
`make-args` finds it under `/opt/homebrew/opt/bison` or `/usr/local/opt/bison`).
The Alpine build image installs `bison` 3.8 (`build-linux.sh`). Flex and Perl
are the ones PostgreSQL's own build uses.

## How ForgeBase loads it

`CREATE EXTENSION age` runs as `forgebase_super` (the extension's functions
use `$libdir/age`). `forgebase_server` and `forgebase_reader` load it per
session with `session_preload_libraries = '$libdir/plugins/age'`, the only
directory a non-superuser may preload from (FB-161). `lib/plugins/age.*` is a
link to `lib/age.*`: PostgreSQL recognises an already loaded library by
inode, so both names are one loaded module; a copy would be loaded a second
time and install AGE's hooks twice in the same backend.

`smoke.sh` checks all of it from the archive: `CREATE EXTENSION age`,
`create_graph`, `MATCH` and a variable-length `MATCH` as a non-superuser that
preloads `$libdir/plugins/age`, one check per patch, and the same `MATCH`es on
a hot standby.

# Extension hook

`build-unix.sh` builds every name in `engine.lock` `contents.extensions`, in
order, after PostgreSQL and contrib. By default an extension is a PGXS build
against the staged engine (`make PG_CONFIG=<staged pg_config> OPTFLAGS=""`,
then `make install`) from the tarball pinned as `[sources.<name>]`. A
directory named after the extension, `deploy/postgres-engine/<name>/` or
`deploy/postgres-engine/extensions/<name>/` (the first wins), changes that:

| File | Effect |
|---|---|
| `patches/*.patch` | applied in name order with `patch -p1` before building (once per source tree) |
| `make-args` | extra `make` arguments, one per line |
| `post-install.sh` | runs after `make install`; env `STAGED` (staged install root), `OS`, `TARGET`, `DLL` (`so`/`dylib`), `EXT_DIR` (source tree) |
| `build.sh` | replaces the default build; same env plus `PG_CONFIG`, `JOBS` |

Apache AGE (FB-160) needs: `[sources.age]` in `engine.lock` (ASF dist
tarball, SHA-256 recorded after checking the ASF SHA-512), `age` appended to
`contents.extensions`, `deploy/postgres-engine/age/` (FB-160 owns it)
with its patches and a `post-install.sh` that copies `age.$DLL` into
`$STAGED/lib/plugins/`, `bison` and `flex` (already in the Alpine package
list and on macOS), a licence file `THIRD_PARTY_LICENSES/postgres/age.txt`
(plus AGE's `NOTICE`), the manifest flags (`build-unix.sh` already writes `graph_cypher = true`
and `age = <version>` when `age` is in `contents.extensions`; the Windows
build never builds AGE and keeps `false`),
and the Cypher checks in `smoke.sh`.

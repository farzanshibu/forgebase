# Licence notes for `forgebase-pgengine` (FB-132)

`deny.toml` needs **no change**: every crate this package adds to
`Cargo.lock` is under a licence already on its allow list. Resolved offline
from the local registry cache on 2026-11-11.

| Crate | Version | Licence (Cargo metadata) | Why |
|---|---|---|---|
| `tar` | 0.4.46 | MIT OR Apache-2.0 | engine archive reader and writer |
| `filetime` | 0.2.29 | MIT/Apache-2.0 | dependency of `tar` |
| `zstd` | 0.13.3 | MIT | archive compression |
| `zstd-safe` | 7.2.4 | MIT OR Apache-2.0 | dependency of `zstd` |
| `zstd-sys` | 2.0.16+zstd.1.5.7 | MIT/Apache-2.0 (bundles the zstd C library, BSD-3-Clause OR GPL-2.0, used under BSD-3-Clause) | dependency of `zstd` |
| `jobserver` | 0.1.35 | MIT OR Apache-2.0 | build dependency of `zstd-sys` (through `cc`) |

Already in the tree before this package: `object` 0.40.0 (`PT_INTERP`
relocation), `rustix` 1.1.6 (signals, euid), `sha2`, `serde_json`, `sqlx`,
`tokio`, `toml` (build script).

`tar` is used with `default-features = false` (no `xattr`), `zstd` with
`default-features = false`, `object` with `read_core`, `elf` and `std` only
(no compression features, so no `ruzstd` or `flate2`).

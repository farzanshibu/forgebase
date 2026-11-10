# Licences of the bundled PostgreSQL engine

Every engine archive carries these files as `share/licenses/` (ADR-0028
§2.4, ADR-0029 §1.3). Each component in `engine.lock` (`[sources.*]` and
`[bundled.*]`) names its file here with `licence_file`, and
`deploy/postgres-engine/engine.py licences` fails the build when one is
missing. Texts are copied verbatim from the pinned upstream sources.

| Component | Licence | File | Shipped on |
|---|---|---|---|
| PostgreSQL 18.6 (server, client programs, contrib) | PostgreSQL License | `postgresql.txt` | all |
| pgvector 0.8.7 | PostgreSQL License | `pgvector.txt` | all |
| pg_cron 1.6.8 | PostgreSQL License | `pg_cron.txt` | all |
| pgmq 1.13.1 | PostgreSQL License | `pgmq.txt` | all |
| OpenSSL 3.5.9 | Apache-2.0 (no NOTICE file upstream) | `openssl.txt` | all |
| lz4 1.11.1 (library) | BSD-2-Clause | `lz4.txt` | all |
| zstd 1.5.7 | BSD-3-Clause (dual BSD/GPLv2; used under BSD) | `zstd.txt` | all |
| zlib 1.4 | Zlib | `zlib.txt` | Linux, Windows (macOS uses the system zlib) |
| libedit 20260512-3.1 | BSD-3-Clause | `libedit.txt` | Linux (macOS uses the system libedit; Windows psql has none) |
| musl 1.2.5 (loader and C library) | MIT | `musl.txt` | Linux |
| ncurses 6.4 (static, through libedit) | X11 (MIT-style) | `ncurses.txt` | Linux |

No GPL component is shipped: readline, ICU, LLVM, Perl, Python and Tcl are
configured out.

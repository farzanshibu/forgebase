# syntax=docker/dockerfile:1
#
# ForgeBase image: one binary with the admin dashboard embedded, and (when an
# engine archive is given) the bundled PostgreSQL engine embedded in it.
#
#   # Standard image: PostgreSQL by default. The engine archive comes from the
#   # engine release named by engine.lock (engine.release); the release workflow
#   # downloads it into engine-dl/ before building:
#   gh release download "$(python3 deploy/postgres-engine/engine.py get engine.release)" \
#     -p 'forgebase-pg*-x86_64-unknown-linux-musl.tar.zst' -p engine.lock.archives -D engine-dl
#   (replace the [archives.*] tables of engine.lock with engine.lock.archives, as the
#    release workflow does)
#   docker build --build-arg ENGINE_ARCHIVE=engine-dl/forgebase-pg18.6-r1-x86_64-unknown-linux-musl.tar.zst \
#     -t forgebase .
#
#   # -lite image (no ENGINE_ARCHIVE): SQLite only, nothing embedded.
#   docker build -t forgebase:lite .
#
#   docker run --rm -p 127.0.0.1:8090:8090 -v forgebase-data:/data \
#     --shm-size=256m -e FORGEBASE_HOST=0.0.0.0 forgebase
# The image sets FORGEBASE_HOST=0.0.0.0 itself (the binary defaults to loopback).
#
# PostgreSQL needs shared memory: Docker's default /dev/shm is 64 MB, which
# parallel queries and larger shared_buffers outgrow ("could not resize shared
# memory segment"). Run with --shm-size=256m (compose: shm_size). The data
# volume holds the engine, its data directory and its socket; PostgreSQL
# refuses to run as root, so the image runs as the unprivileged user 10001.

ARG ENGINE_ARCHIVE=""

# --- 1. Dashboard: build dashboard/dist (embedded into the binary) -----------
FROM node:22-bookworm-slim AS dashboard
WORKDIR /src/dashboard
COPY dashboard/package.json dashboard/package-lock.json ./
RUN npm ci
COPY dashboard/ ./
RUN npm run build

# --- 2. Rust: build the release binary --------------------------------------
# Alpine, so the host target is musl: x86_64-unknown-linux-musl on the amd64
# build, the target of the engine archive passed in (engine.lock
# [archives."x86_64-unknown-linux-musl"]). A static binary runs on the Debian
# runtime below.
FROM rust:1.99-alpine AS server
RUN apk add --no-cache build-base musl-dev cmake perl linux-headers
WORKDIR /src
COPY . .
# Put the freshly built dashboard where rust-embed reads it (dashboard/dist).
COPY --from=dashboard /src/dashboard/dist ./dashboard/dist
ARG ENGINE_ARCHIVE
# crt-static for the binary. An explicit --target keeps RUSTFLAGS off build
# scripts and proc-macros: a static musl host cannot load a proc-macro
# ("cannot produce proc-macro ... does not support these crate types").
ENV RUSTFLAGS="-C target-feature=+crt-static"
# With ENGINE_ARCHIVE (a path inside the build context): the standard build,
# the archive embedded and checked against engine.lock. Without it: -lite.
RUN set -eu; \
    target="$(rustc -vV | sed -n 's/^host: //p')"; \
    if [ -n "$ENGINE_ARCHIVE" ]; then \
      test -f "$ENGINE_ARCHIVE" || { echo "ENGINE_ARCHIVE $ENGINE_ARCHIVE is not in the build context" >&2; exit 1; }; \
      FORGEBASE_PG_ENGINE_ARCHIVE="/src/$ENGINE_ARCHIVE" \
        cargo build --release --locked --target "$target" -p forgebase-cli --features bundled-postgres; \
    else \
      cargo build --release --locked --target "$target" -p forgebase-cli; \
    fi; \
    cp "target/$target/release/forgebase" /forgebase

# --- 3. Runtime: slim image, non-root user, /data volume --------------------
FROM debian:bookworm-slim AS runtime
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl ca-certificates \
 && rm -rf /var/lib/apt/lists/* \
 && groupadd --system --gid 10001 forgebase \
 && useradd --system --uid 10001 --gid forgebase --home-dir /data --shell /usr/sbin/nologin forgebase \
 && mkdir -p /data \
 && chown forgebase:forgebase /data

COPY --from=server /forgebase /usr/local/bin/forgebase

ENV FORGEBASE_DATA_DIR=/data \
    FORGEBASE_HOST=0.0.0.0 \
    FORGEBASE_PORT=8090 \
    FORGEBASE_LOG_FORMAT=json

USER forgebase
WORKDIR /data
VOLUME ["/data"]
EXPOSE 8090

HEALTHCHECK --interval=30s --timeout=5s --start-period=20s --retries=3 \
  CMD curl -fsS http://127.0.0.1:8090/health || exit 1

STOPSIGNAL SIGTERM
ENTRYPOINT ["/usr/local/bin/forgebase"]
CMD ["serve"]

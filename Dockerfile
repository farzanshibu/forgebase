# syntax=docker/dockerfile:1
#
# ForgeBase Phase 1 image: one static-ish binary with the admin dashboard embedded.
#   docker build -t forgebase .
#   docker run --rm -p 127.0.0.1:8090:8090 -v forgebase-data:/data \
#     -e FORGEBASE_HOST=0.0.0.0 forgebase
# The image sets FORGEBASE_HOST=0.0.0.0 itself (the binary defaults to loopback).

# --- 1. Dashboard: build dashboard/dist (embedded into the binary) -----------
FROM node:22-bookworm-slim AS dashboard
WORKDIR /src/dashboard
COPY dashboard/package.json dashboard/package-lock.json ./
RUN npm ci
COPY dashboard/ ./
RUN npm run build

# --- 2. Rust: build the release binary --------------------------------------
FROM rust:1.99-bookworm AS server
WORKDIR /src
COPY . .
# Put the freshly built dashboard where rust-embed reads it (dashboard/dist).
COPY --from=dashboard /src/dashboard/dist ./dashboard/dist
RUN cargo build --release --locked -p forgebase-cli \
 && cp target/release/forgebase /forgebase

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

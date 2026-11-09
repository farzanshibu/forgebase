# Deploying ForgeBase

ForgeBase runs as one process with its data in one directory. You can run the
binary directly, run the Docker image, or run it as a systemd service. All three
read the same configuration and use the same data layout.

Before you expose it to a network, read [security.md](security.md). The short
version: put a reverse proxy with TLS in front, keep the default loopback bind,
and do not publish the Redis port.

## 1. Choose how to run it

| Method | Use when | Section |
|---|---|---|
| Release binary | You manage the host yourself, or you want the smallest moving parts. | [§2.1](#21-release-binary) |
| Docker image | You run containers. Compose file included for a single node. | [§2.2](#22-docker) |
| systemd service | Linux host, no containers. Hardened unit file included. | [§2.3](#23-systemd) |

ForgeBase is single-node. There is no clustering in this release.

## 2. Install

### 2.1 Release binary

Release archives are built by `.github/workflows/release.yml` for these targets:

| Target | Archive |
|---|---|
| `x86_64-unknown-linux-musl` | `.tar.gz` |
| `aarch64-unknown-linux-musl` | `.tar.gz` |
| `x86_64-apple-darwin` | `.tar.gz` |
| `aarch64-apple-darwin` | `.tar.gz` |
| `x86_64-pc-windows-msvc` | `.zip` |

Each release has a `SHA256SUMS` file. Verify the archive before you run it:

```sh
sha256sum -c SHA256SUMS --ignore-missing     # Linux
shasum -a 256 -c SHA256SUMS --ignore-missing # macOS
```

To build from source, you need Rust 1.97 or newer (`rust-version` in
`Cargo.toml`), Node.js 22, and npm. The dashboard is compiled into the binary,
so build it first:

```sh
cd dashboard && npm ci && npm run build && cd ..
cargo build --release --locked -p forgebase-cli
./target/release/forgebase version
```

Without `dashboard/dist`, the build still succeeds, but `/admin/` shows a "not built" page.

Create a project and start it:

```sh
forgebase init ./my-project     # writes forgebase.toml, creates the data dir, migrates
cd ./my-project
forgebase serve                 # http://127.0.0.1:8090
```

`init` does not overwrite an existing `forgebase.toml`. `serve` applies pending
migrations at startup. Stop it with Ctrl-C or SIGTERM. SIGTERM drains requests
for `shutdown_timeout_secs` and then checkpoints the WAL.

Create the first administrator before you use the dashboard:

```sh
forgebase admin create --email you@example.com --password-stdin < password.txt
forgebase admin list
```

Use `--password-stdin` so the password stays out of shell history and the process
list. `admin reset-password` sets a new password and ends that admin's sessions.

### 2.2 Docker

The [`Dockerfile`](../Dockerfile) has three stages: the dashboard build, the release
binary build, and a slim runtime image. The runtime image runs as uid `10001`,
keeps data in `/data`, sets `FORGEBASE_HOST=0.0.0.0` and `FORGEBASE_LOG_FORMAT=json`,
and has a health check on `GET /health`.

```sh
docker compose up --build
```

[`docker-compose.yml`](../docker-compose.yml) publishes the port on loopback
only (`127.0.0.1:8090`) and stores data in the `forgebase-data` volume. It also
sets the container read-only, with `tmpfs` for `/tmp`, `no-new-privileges`, and
all capabilities dropped.

Without Compose:

```sh
docker build -t forgebase .
docker run --rm -p 127.0.0.1:8090:8090 -v forgebase-data:/data forgebase
```

Set configuration with environment variables (§3.3) or mount a `forgebase.toml`
and point `FORGEBASE_CONFIG` at it.

### 2.3 systemd

[`deploy/systemd/forgebase.service`](../deploy/systemd/forgebase.service) runs
`forgebase serve` as a dedicated user. The install steps are in the file header:

```sh
useradd --system --home-dir /var/lib/forgebase --shell /usr/sbin/nologin forgebase
install -m 0755 forgebase /usr/local/bin/forgebase
install -m 0644 deploy/systemd/forgebase.service /etc/systemd/system/
# optional overrides, one FORGEBASE_* per line:
install -d /etc/forgebase && $EDITOR /etc/forgebase/forgebase.env
systemctl daemon-reload && systemctl enable --now forgebase
```

The unit sets `FORGEBASE_DATA_DIR=/var/lib/forgebase`, `FORGEBASE_LOG_FORMAT=json`,
and `KillMode=mixed` with a 45 s stop timeout. It keeps the process in a hardened
sandbox: `ProtectSystem=strict` with only `/var/lib/forgebase` writable, no capabilities,
a system-call filter, and `RestrictAddressFamilies=AF_UNIX AF_INET AF_INET6`.
`StateDirectory=` creates `/var/lib/forgebase` with mode `0700`.

Read logs with `journalctl -u forgebase`.

## 3. Configuration

### 3.1 Precedence

Highest first:

1. Command-line flags
2. `FORGEBASE_*` environment variables
3. The TOML file
4. Built-in defaults

The file is `--config`, else `FORGEBASE_CONFIG`, else `./forgebase.toml` if it
exists. A named file that does not exist is an error. **Unknown keys in the file
and unknown `FORGEBASE_*` variables are errors**, so a typo stops startup
instead of being ignored. Values are range-checked.

[`forgebase.example.toml`](../forgebase.example.toml) lists every key with its
default and its environment variable. It does not yet include a `[billing]`
section; see §7.

### 3.2 Flags

Global flags work with every subcommand:

| Flag | Config key | Env var |
|---|---|---|
| `--config <FILE>` | (file path) | `FORGEBASE_CONFIG` |
| `--data-dir <DIR>` | `data_dir` | `FORGEBASE_DATA_DIR` |
| `--host <HOST>` | `server.host` | `FORGEBASE_HOST` |
| `--port <PORT>` | `server.port` | `FORGEBASE_PORT` |
| `--log-level <FILTER>` | `log.level` | `FORGEBASE_LOG_LEVEL` |
| `--log-format <pretty\|json>` | `log.format` | `FORGEBASE_LOG_FORMAT` |
| `--resp` | `resp.enabled` | `FORGEBASE_RESP_ENABLED` |
| `--resp-host <HOST>` | `resp.host` | `FORGEBASE_RESP_HOST` |
| `--resp-port <PORT>` | `resp.port` | `FORGEBASE_RESP_PORT` |
| `--billing` | `billing.enabled` | `FORGEBASE_BILLING_ENABLED` |

`--resp` and `--billing` are switches that turn the feature on. To turn it off,
set the config key or environment variable to `false`.

### 3.3 Environment variables

Every variable below is read by the binary. Any other `FORGEBASE_*` variable
stops startup with an error.

| Variable | Purpose |
|---|---|
| `FORGEBASE_CONFIG` | Path to the TOML file. |
| `FORGEBASE_DATA_DIR` | Data directory. |
| `FORGEBASE_HOST`, `FORGEBASE_PORT` | HTTP bind address and port. |
| `FORGEBASE_REQUEST_TIMEOUT_SECS` | Per-request timeout. |
| `FORGEBASE_SHUTDOWN_TIMEOUT_SECS` | Grace period for in-flight requests on shutdown. |
| `FORGEBASE_DB_SYNCHRONOUS` | `full` or `normal`. |
| `FORGEBASE_DB_BUSY_TIMEOUT_MS` | Lock wait for other processes. |
| `FORGEBASE_DB_READ_POOL_SIZE` | Read connections. |
| `FORGEBASE_LOG_LEVEL`, `FORGEBASE_LOG_FORMAT` | Logging. |
| `FORGEBASE_RESP_ENABLED`, `FORGEBASE_RESP_HOST`, `FORGEBASE_RESP_PORT` | Redis protocol listener. |
| `FORGEBASE_RESP_MAX_CONNECTIONS`, `FORGEBASE_RESP_MAX_BULK_BYTES`, `FORGEBASE_RESP_IDLE_TIMEOUT_SECS` | Redis protocol limits. |
| `FORGEBASE_BILLING_ENABLED` | Mount the billing API. |
| `FORGEBASE_STORAGE_S3_ACCESS_KEY`, `FORGEBASE_STORAGE_S3_SECRET_KEY`, `FORGEBASE_STORAGE_S3_SESSION_TOKEN` | S3 credentials, intended to be read from the environment only. **Known bug:** the CLI's environment parser rejects these names, so every `forgebase` command fails while they are set. See the note below. |

`FORGEBASE_TEST_REDIS_URL` is used by the test suite. The same parser rejects
it, so do not set it on a host that runs ForgeBase.

Storage settings other than the S3 credentials (driver, limits, endpoint) are
set in the TOML file only.

**Known issue (S3 credentials).** `ConfigOverrides::from_env`
(`crates/forgebase-core/src/config.rs`) rejects any `FORGEBASE_*` name it does
not list, and `forgebase-cli` passes it the whole process environment. The
server reads the S3 keys directly (`crates/forgebase-server/src/lib.rs`), so
the S3 driver cannot be used with keys in the environment until the parser
allows those names. Until that is fixed, use `driver = "local"`.

### 3.4 `.env` files

If `./.env` exists in the working directory, `forgebase` loads it at startup.
Variables already in the process environment are never overridden. Use this for
development. Do not ship a `.env` with secrets; `.dockerignore` excludes it from
the image.

## 4. Data directory

```text
<data_dir>/                       created 0700
├── forgebase.db (+ -wal, -shm)   all structured data; created 0600
├── storage/                      object blobs (when storage is used)
├── auth/master.key               root key for auth secrets; created on first start
├── indexes/                      derived indexes; safe to delete, rebuilt later
├── backups/                      pre-migration snapshots
├── tmp/                          staging; safe to delete when stopped
└── .forgebase.lock               held by the running server
```

Back up with `forgebase backup` (online, consistent; it covers `forgebase.db`,
`storage/` and `indexes/`). `auth/master.key` is **not** copied unless you pass
`--include-master-key`: losing it makes the stored secrets unreadable, so keep
one copy of it apart from the backups. Schedule snapshots with cron or a
systemd timer, for example `forgebase backup --keep-last 14 --max-age-days 30`.
Restore with `forgebase restore latest --force` on the stopped data directory.
See [database.md §5](database.md#5-backups-and-rollback).

## 5. TLS

The binary serves plain HTTP. Terminate TLS in a reverse proxy (nginx, Caddy,
a cloud load balancer) and forward to ForgeBase on loopback:

```text
internet ──TLS──► reverse proxy ──HTTP──► 127.0.0.1:8090 (forgebase)
```

Keep `server.host` at `127.0.0.1`. Then only the proxy on the same host can
reach it. If the proxy is on another host, set `FORGEBASE_HOST` to that
interface and restrict it with a firewall.

The client IP ForgeBase uses for rate limits is the socket address. It does not
read `X-Forwarded-For` until an `[auth]` section can set `trust_forwarded_for`
(see [security.md §6](security.md#6-open-items)). Behind a proxy, all clients
currently share the proxy's rate-limit bucket.

## 6. Upgrades

Migrations run automatically on `serve`. Before you upgrade:

1. Read the release notes for the new version.
2. Check the current migration state with the **old** binary:
   `forgebase migrate --status`.
3. Stop the server (SIGTERM, or `systemctl stop forgebase`).
4. Take a snapshot: `forgebase backup --label pre-upgrade` (works online or
   stopped), and make sure `auth/master.key` is stored safely.
5. Run `forgebase doctor` on the stopped data directory.
6. Replace the binary, or pull the new image.
7. Start the server. Pending migrations run. Before the first pending migration
   the server writes `backups/pre-migrate-v<N>-<time>.db`.
8. Check `GET /health/ready` and `forgebase status`.

**Rollback.** A database migrated by a newer binary is refused by an older one.
To roll back:

1. Stop the server.
2. Restore the snapshot you took (`forgebase restore <id> --force`), or copy
   `backups/pre-migrate-v<N>-<time>.db` over `forgebase.db` and delete
   `forgebase.db-wal` and `forgebase.db-shm`.
3. Start the previous binary.

Migrations are forward-only, so the snapshot is the only rollback path.

## 7. Configuration keys

Keys in the TOML file, with defaults. Ranges are the accepted values.

| Key | Default | Range or values |
|---|---|---|
| `data_dir` | `"forgebase-data"` | Relative paths resolve against the config file's directory. |
| `server.host` | `"127.0.0.1"` | IP address. |
| `server.port` | `8090` | `0` picks a free port. |
| `server.request_timeout_secs` | `30` | `1`–`3600` |
| `server.shutdown_timeout_secs` | `30` | `1`–`3600` |
| `database.synchronous` | `"full"` | `full`, `normal` |
| `database.busy_timeout_ms` | `5000` | up to `600000` |
| `database.read_pool_size` | `8` | `1`–`64` |
| `log.level` | `"info"` | filter such as `info` or `forgebase=debug`; not empty |
| `log.format` | `"pretty"` | `pretty`, `json` |
| `resp.enabled` | `false` | Redis protocol listener. |
| `resp.host` | `"127.0.0.1"` | |
| `resp.port` | `6380` | `0` picks a free port. |
| `resp.max_connections` | `1024` | `1`–`1000000` |
| `resp.max_bulk_bytes` | `16777216` | `1`–`536870912` |
| `resp.idle_timeout_secs` | `300` | `0` = never. Subscribed connections are exempt. |
| `storage.driver` | `"local"` | `local`, `s3` |
| `storage.max_object_bytes` | `5368709120` (5 GiB) | |
| `storage.upload_ttl_secs` | `86400` | |
| `storage.reconcile_grace_secs` | `3600` | |
| `storage.s3_endpoint`, `s3_region`, `s3_bucket`, `s3_prefix`, `s3_part_size` | unset / `""` / `8388608` | S3 only. Credentials come from the environment (§3.3). |
| `billing.enabled` | `false` | Mounts `/api/v1/billing/*` and the provider webhooks. |

Example `[billing]` section, which the example file does not yet include:

```toml
[billing]
enabled = false   # FORGEBASE_BILLING_ENABLED
```

Billing is off by default. Live payment-provider keys are refused; only
test-mode keys are accepted (see [security.md §3](security.md#3-trust-boundaries-and-controls)).

## 8. Release checklist

- [ ] `forgebase doctor` passes on the data directory.
- [ ] `auth/master.key` is backed up separately from the data backups.
- [ ] `forgebase backup` runs on a schedule and `forgebase backup verify latest` passes.
- [ ] `FORGEBASE_HOST` is loopback, or only the proxy can reach the port.
- [ ] TLS terminates at the proxy.
- [ ] `log.format` is `json` for log collection.
- [ ] `resp.enabled` is `false`, or the Redis port is on loopback.
- [ ] `/health` and `/health/ready` return `200`.

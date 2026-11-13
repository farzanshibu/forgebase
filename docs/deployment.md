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

The dashboard needs an administrator account. [§2.4](#24-first-run) lists
the ways to create the first one. To add more, or to recover one, use the
command line:

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

The standard image initialises PostgreSQL by default: the first start unpacks
the bundled engine under `/data/postgres/` (the container already runs as the
unprivileged user PostgreSQL needs) and the data volume holds the engine, the
cluster and its Unix socket. PostgreSQL uses POSIX shared memory and Docker's
default `/dev/shm` is 64 MB, so the compose file sets `shm_size` (see its
comment). To keep SQLite in a container, use the `-lite` image or set
`FORGEBASE_DATABASE_BACKEND=sqlite`; a volume that already holds a SQLite
project keeps it either way (§3.5).

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

### 2.4 First run

A server with no administrator account is in **setup mode**. There are three
ways to create the first administrator.

**1. During `forgebase init` (on a terminal).** After it creates the project,
`init` asks `Create the administrator account now? [Y/n]`, then for the email
and the password twice. The password is not shown as you type it (on Linux and
macOS; on Windows use option 2 or 3).

**2. During `forgebase init` (scripts, containers, CI).** Pass the email as a
flag and the password on standard input:

```sh
forgebase init ./my-project --admin-email you@example.com --admin-password-stdin < password.txt
```

`init` checks the email and password before it writes anything. If an
administrator already exists, `init` keeps it and does not create another one,
so you can run the same command again safely. Without a terminal and without
these flags, `init` does not create an administrator. Use `--no-admin` to skip
the question on purpose.

**3. From the browser, with the setup link.** When no administrator exists,
`forgebase serve` prints a one-time link to standard output:

```text
First run: create the administrator at http://127.0.0.1:8090/admin/setup?token=<token>
```

Open the link and fill in the form. The administrator is created and signed
in. The link uses the address the server is bound to. Behind a reverse proxy,
replace the host and port with your public address and keep the path and the
`token` part.

The token:

- is 256 random bits, kept only in the server's memory, and compared in
  constant time;
- works once: it stops working as soon as any administrator exists, including
  one created with `forgebase admin create`;
- is replaced by a new one each time the server restarts in setup mode;
- is printed to standard output only, never to the log (the log goes to
  standard error).

Note: systemd (`journalctl`) and Docker (`docker logs`) record standard output
as well as standard error. On those hosts the link also lands in the journal or
the container log until you use it. If other people can read that log, create
the administrator with option 2 or with `forgebase admin create` before the
first `serve`.

The setup endpoints need no sign-in:

| Method and path | Request | Response |
|---|---|---|
| `GET /api/setup/status` | | `200 {"setup_required": true}` while no administrator exists |
| `POST /api/setup/admin` | `{"token", "email", "password"}`; optional `"cookie": false` for bearer tokens | `201`, the same body and session cookie as `POST /api/auth/admins/login` |

`POST /api/setup/admin` returns these errors: `403 invalid_setup_token`,
`409 setup_complete` once any administrator exists, and
`400 validation_failed`. Passwords need at least 10 characters, the same rule
as `forgebase admin create`. Each client address gets 10 attempts per minute;
after that the endpoint returns `429` with `Retry-After`.

`forgebase status` shows the state: `setup: pending (no administrator)` or
`setup: complete`. With `--json`, the field is `setup_required`.

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
default and its environment variable.

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

`--resp` and `--billing` are switches that turn the feature on (billing is
already on by default). To turn billing or RESP off, set the config key or
environment variable to `false`.

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
| `FORGEBASE_PUBLIC_URL` | Address people and apps reach the server at, without a path; links the server hands out use it (§7). |
| `FORGEBASE_OPS_ACCESS` | Who may read `/metrics` and `/api/system/*`: `loopback`, `admin` or `public` (§7). |
| `FORGEBASE_AUTH_TRUST_FORWARDED_FOR` | Take the client address from `X-Forwarded-For` (behind your own proxy only). |
| `FORGEBASE_AUTH_ALLOW_PRIVATE_OUTBOUND` | Let OAuth/OIDC, SMS and Turnstile requests reach private addresses. |
| `FORGEBASE_DATABASE_BACKEND` | `postgres` or `sqlite` (§3.5). |
| `FORGEBASE_DATABASE_URL` | External PostgreSQL server, with its password (a secret: environment or `.env` only). |
| `FORGEBASE_DATABASE_POSTGRES_MODE`, `FORGEBASE_DATABASE_POSTGRES_POOL_SIZE`, `FORGEBASE_DATABASE_POSTGRES_LISTEN`, `FORGEBASE_DATABASE_POSTGRES_ENGINE_DIR`, `FORGEBASE_DATABASE_POSTGRES_NATIVE_RLS` | `[database.postgres]` keys (§7). |
| `FORGEBASE_DATABASE_POSTGRES_EXTENSIONS_VECTOR`, `_GRAPH`, `_QUEUE`, `_CRON`, `_TRIGRAM` | `auto`, `on` or `off` per accelerator (§7). |
| `FORGEBASE_PG_ENGINE_DIR` | Development builds without the embedded engine: an installed PostgreSQL 17+ to run as the bundled engine. Not configuration; ignored by the config loader. |
| `FORGEBASE_DB_SYNCHRONOUS` | `full` or `normal` (SQLite). |
| `FORGEBASE_DB_BUSY_TIMEOUT_MS` | Lock wait for other processes. |
| `FORGEBASE_DB_READ_POOL_SIZE` | Read connections. |
| `FORGEBASE_LOG_LEVEL`, `FORGEBASE_LOG_FORMAT` | Logging. |
| `FORGEBASE_RESP_ENABLED`, `FORGEBASE_RESP_HOST`, `FORGEBASE_RESP_PORT` | Redis protocol listener. |
| `FORGEBASE_RESP_MAX_CONNECTIONS`, `FORGEBASE_RESP_MAX_BULK_BYTES`, `FORGEBASE_RESP_IDLE_TIMEOUT_SECS` | Redis protocol limits. |
| `FORGEBASE_RESP_TLS` | Redis protocol over TLS with the `[tls]` certificate (§5). |
| `FORGEBASE_TLS_MODE`, `FORGEBASE_TLS_CERT_FILE`, `FORGEBASE_TLS_KEY_FILE`, `FORGEBASE_TLS_RELOAD_INTERVAL_SECS`, `FORGEBASE_TLS_MIN_VERSION`, `FORGEBASE_TLS_HSTS_MAX_AGE_SECS`, `FORGEBASE_TLS_HSTS_INCLUDE_SUBDOMAINS`, `FORGEBASE_TLS_REDIRECT_HTTP`, `FORGEBASE_TLS_HTTP_PORT` | Native TLS (§5, §7). |
| `FORGEBASE_TLS_ACME_DOMAINS`, `FORGEBASE_TLS_ACME_EMAIL`, `FORGEBASE_TLS_ACME_DIRECTORY_URL`, `FORGEBASE_TLS_ACME_CHALLENGES`, `FORGEBASE_TLS_ACME_EAB_KID`, `FORGEBASE_TLS_ACME_CA_FILE` | ACME (§5); lists are comma-separated. |
| `FORGEBASE_TLS_ACME_EAB_HMAC_KEY` | ACME external account binding key (a secret: environment or `.env`). |
| `FORGEBASE_BILLING_ENABLED` | Mount the billing API. |
| `FORGEBASE_STORAGE_ENABLED`, `FORGEBASE_FUNCTIONS_ENABLED`, `FORGEBASE_QUEUE_ENABLED`, `FORGEBASE_REALTIME_ENABLED`, `FORGEBASE_VECTOR_ENABLED`, `FORGEBASE_GRAPH_ENABLED`, `FORGEBASE_IDENTITY_ENABLED`, `FORGEBASE_ACCESS_ENABLED` | Switch a module on or off (default on; ADR-0026, [architecture.md §6](architecture.md#6-feature-flags)). |
| `FORGEBASE_REALTIME_MAX_CONNECTIONS_PER_PRINCIPAL` | Most realtime connections one user or key may hold (default 32). |
| `FORGEBASE_STORAGE_S3_ACCESS_KEY`, `FORGEBASE_STORAGE_S3_SECRET_KEY`, `FORGEBASE_STORAGE_S3_SESSION_TOKEN` | S3 credentials, read from the environment only (the storage driver reads them; the config loader only notes that they are set). |

`FORGEBASE_TEST_*` variables (`FORGEBASE_TEST_BACKEND`, `FORGEBASE_TEST_REDIS_URL`,
`FORGEBASE_TEST_PG_*`) belong to the test suite; the parser ignores them. Do not
set them on a host that runs ForgeBase.

Storage settings other than the S3 credentials and `enabled` (driver, limits,
endpoint) are set in the TOML file only. The S3 credential names were rejected
by the environment parser before FB-91; they are accepted now.

### 3.4 `.env` files

If `./.env` exists in the working directory, `forgebase` loads it at startup.
Variables already in the process environment are never overridden. Use this for
development. Do not ship a `.env` with secrets; `.dockerignore` excludes it from
the image.

### 3.5 Database: PostgreSQL or SQLite

ForgeBase keeps its data in PostgreSQL or in SQLite (ADR-0028, ADR-0029 §6).

| | PostgreSQL, bundled (`managed`) | PostgreSQL, your server (`external`) | SQLite |
|---|---|---|---|
| Choose it with | `forgebase init --database postgres` | `forgebase init --database-url postgres://…` | `forgebase init --database sqlite` |
| Default | standard release | | `-lite` release (`forgebase --version` says `lite`) |
| Where the data lives | `<data_dir>/postgres/data` | your server | `<data_dir>/forgebase.db` |
| Who runs the engine | `forgebase serve` starts it before opening the database and stops it (fast shutdown) when it stops | you | in-process |

**Choosing.** On a terminal, `forgebase init` asks
`Database: [1] PostgreSQL (default, bundled server) [2] SQLite (one file)`;
scripts pass `--database` or `--database-url`. `init` writes
`[database] backend` into the new `forgebase.toml` and the backend marker
`<data_dir>/forgebase.backend` (`postgres-managed`, `postgres-external` or
`sqlite`). `--database-url` writes the URL to the project's `.env` as
`FORGEBASE_DATABASE_URL` (mode 0600), never to `forgebase.toml`; run
`forgebase` from the project directory so `.env` is read. A `-lite` build
answers `--database postgres` with `this build has no bundled PostgreSQL; use
--database-url, --database sqlite or the standard release`. As root, `init`
stops before it writes anything: `PostgreSQL cannot run as root: run as an
unprivileged user, or use --database sqlite`; it never falls back to SQLite on
its own.

**The marker wins.** At every start the server compares the configuration
with the marker. A data directory keeps its engine when the configuration
only has the default, so an existing SQLite project (with `forgebase.db` and no
marker) stays SQLite after an upgrade. A setting that contradicts it stops
startup: `database.backend is postgres, but the data directory … holds a
sqlite database; run forgebase db convert --to postgres, or set [database]
backend = "sqlite"`. An empty data directory takes the configuration.
`forgebase status` and `forgebase doctor` show a one-line notice on SQLite
projects while PostgreSQL is the default.

**The bundled engine.** The first start extracts the engine embedded in the
binary into `<data_dir>/postgres/engine/` (about 30 MB, a few seconds), runs
`initdb` (builtin C.UTF-8 locale, SCRAM passwords, data checksums), creates
the ForgeBase roles with random passwords in `postgres/roles.secret` (0600),
and listens on a Unix socket in `postgres/run/` only. TCP logins are off;
`[database.postgres] listen = "127.0.0.1:5433"` turns on loopback TCP (other
hosts need TLS, which this version does not set up). PostgreSQL refuses to run
as root: run ForgeBase as an unprivileged user (the Docker image does). If the
postmaster dies, ForgeBase restarts it with backoff; while it is down requests
get `503 database_unavailable` and `/health/ready` fails the `database` check.
Engine log lines go to the server's log with target `forgebase::postgres`.

**Commands on a PostgreSQL project.** `status`, `doctor`, `migrate --status`,
`admin`, `export` and `db` log in to the running server's engine through its
socket; with no server running they start the engine under the data-directory
lock for the command and stop it afterwards (one to three seconds).

| Command | What it does |
|---|---|
| `forgebase db status [--json]` | Backend, cluster directory, engine directory, whether the engine runs (pid, socket, port), TCP logins, and with a running engine the database size, connections, schema version and the extensions (available, installed). Never starts the engine. |
| `forgebase db psql [--read-only \| --superuser] [PSQL_ARGS…]` | The engine's `psql` through the Unix socket, as `forgebase_server` (reads and writes every ForgeBase table, bypasses row-level security, not a superuser), `forgebase_reader` or `forgebase_super`. Example: `forgebase db psql -c 'select 1'`. The password goes through a temporary 0600 passfile, never the command line. External servers: use `psql "$FORGEBASE_DATABASE_URL"`. |
| `forgebase doctor` | Adds the engine (files against the archive manifest, `postgres --version`), the cluster (`PG_VERSION` against the engine, mode 0700), `roles.secret`, a postmaster left behind, and `amcheck` (`bt_index_check` on every B-tree index) plus the foreign-key check as the superuser. |

`/api/system/info` reports `database.backend`, `mode`, `server_version`,
`connections`, the bundled `engine` (`state`, `version`, `pid`, `restarts`,
`socket_dir`) and `features` (per accelerator: `engine` (`portable` until its
package switches it over), `mode`, `extension`, `available`, `installed`,
`state`). `/health/ready` keeps its shape and fails the `database` check while the
bundled engine is restarting or has failed.

## 4. Data directory

```text
<data_dir>/                       created 0700
├── forgebase.backend             "postgres-managed", "postgres-external" or "sqlite" (§3.5)
├── forgebase.db (+ -wal, -shm)   SQLite only: all structured data; created 0600
├── postgres/                     bundled PostgreSQL only: engine/, data/ (0700), run/ (socket), roles.secret (0600)
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

Two setups are supported; pick one per server (ADR-0033):

- **Behind a reverse proxy** (the default, `[tls] mode = "off"`): the binary
  serves plain HTTP and a proxy (nginx, Caddy, a cloud load balancer, a
  Kubernetes ingress) terminates TLS. Read on below.
- **Native TLS** (`[tls] mode = "files"` or `"acme"`): the binary serves
  HTTPS itself, with your certificate files or with a certificate it gets and
  renews from Let's Encrypt or another ACME authority. See
  [Native TLS](#native-tls-certificate-files) further down.

#### Behind a reverse proxy

The binary serves plain HTTP. Terminate TLS in a reverse proxy (nginx, Caddy,
a cloud load balancer) and forward to ForgeBase on loopback:

```text
internet ──TLS──► reverse proxy ──HTTP──► 127.0.0.1:8090 (forgebase)
```

Keep `server.host` at `127.0.0.1`. Then only the proxy on the same host can
reach it. If the proxy is on another host, set `FORGEBASE_HOST` to that
interface and restrict it with a firewall.

The client IP ForgeBase uses for rate limits is the socket address. Behind a
proxy that is the proxy's address, so all clients share one rate-limit bucket.
Set `[auth] trust_forwarded_for = true` when, and only when, every request
reaches ForgeBase through a proxy you run that **appends** the real client
address to `X-Forwarded-For` (nginx `proxy_add_x_forwarded_for`, Caddy and
Traefik by default). ForgeBase then uses the **last** entry, the one your proxy
wrote; entries a client sent itself are ignored.

`/metrics` and `/api/system/*` follow `server.ops_access` (default
`loopback`): an administrator's credentials work from anywhere; without
credentials only a client on the same host that connects directly (no
`Forwarded`, `X-Forwarded-For` or `X-Real-IP` header) is admitted, such as a
Prometheus scraping `127.0.0.1:8090`. A proxy that does not add any forwarding
header would make every remote client look local: configure it to send
`X-Forwarded-For`, or set `ops_access = "admin"`. A scraper elsewhere uses an
admin service-account API key with the `system:read` scope as a bearer token.

#### Native TLS: certificate files

```toml
[server]
host = "0.0.0.0"
port = 443

[tls]
mode = "files"
cert_file = "/etc/forgebase/tls/fullchain.pem"   # chain, leaf first
key_file = "/etc/forgebase/tls/privkey.pem"      # PKCS#8, PKCS#1 or SEC1; RSA, ECDSA or Ed25519
# reload_interval_secs = 30   # 0 = only on SIGHUP
# min_version = "1.2"         # or "1.3"
# hsts_max_age_secs = 31536000
# redirect_http = true        # plain HTTP on http_port (80) redirects to HTTPS
```

```text
internet ──TLS──► 0.0.0.0:443 (forgebase)
internet ──HTTP─► 0.0.0.0:80  (forgebase: 308 to https://, when redirect_http = true)
```

- The files are read and checked before anything else starts. A missing
  file, a file without a certificate, or a key that does not belong to the
  certificate stops the server with an error naming both files
  (`tls_key_mismatch`, `tls_file_unreadable`, ...). Make the key readable by
  the server's user only.
- **Renewing the files needs no restart.** The server compares the files'
  contents every `reload_interval_secs` (default 30) and on `SIGHUP`
  (`systemctl reload` with `ExecReload=/bin/kill -HUP $MAINPID`, or
  `kill -HUP <pid>`), and serves the new pair from the next handshake on.
  Open connections are not dropped. A changed pair that does not load (for
  example the key written before the certificate) is logged and reported in
  `/api/system/info` under `tls.last_error`; the old certificate stays until
  a good pair is in place. This works with certbot's `--deploy-hook`, a
  Kubernetes Secret mounted as files (cert-manager), or any tool that
  rewrites the files.
- ALPN offers HTTP/2 and HTTP/1.1. WebSockets (realtime) use HTTP/1.1.
- HSTS (`hsts_max_age_secs`) is off by default. Turn it on once HTTPS works;
  browsers then refuse plain HTTP for that long.

#### Native TLS: ACME (Let's Encrypt)

```toml
[server]
host = "0.0.0.0"
port = 443                  # the CA connects to port 443 (tls-alpn-01)
public_url = "https://forgebase.example.com"

[tls]
mode = "acme"
# redirect_http = true      # also redirect http:// to https://

[tls.acme]
domains = ["forgebase.example.com"]
email = "ops@example.com"
# directory_url = "letsencrypt-staging"   # try the staging CA first
# challenges = ["tls-alpn-01", "http-01"]
```

- **Setting `mode = "acme"` accepts the certificate authority's terms of
  service** (for Let's Encrypt, the Subscriber Agreement); the account is
  created on the first start with `email` as its contact.
- Every name in `domains` must resolve to this server. Wildcards are not
  supported (they need the `dns-01` challenge); use files mode for them.
- Challenges: `tls-alpn-01` (default) is answered on the HTTPS port, which
  the CA reaches as port **443**. `http-01` is answered by a plain HTTP
  listener on `[tls] http_port` (default 80), which the CA reaches as port
  **80**; that listener also redirects other requests to HTTPS. Ports below
  1024 need `CAP_NET_BIND_SERVICE` on Linux
  (`AmbientCapabilities=CAP_NET_BIND_SERVICE` in the systemd unit) or a port
  mapping (`-p 443:8443` with `port = 8443`, and `public_url` set so redirects
  name the public port).
- The first certificate is requested in the background: the server starts
  at once, and HTTPS handshakes fail until it is issued (usually seconds;
  watch the log or `tls` in `/api/system/info`). It is renewed at about two
  thirds of its lifetime (day 54 to 60 for Let's Encrypt's 90 days). A
  failed request is retried after 2 minutes, doubling up to 6 hours, while
  the current certificate keeps being served.
- Other authorities: `directory_url` takes any RFC 8555 directory (ZeroSSL,
  Google Trust Services, your own step-ca). For one that needs external
  account binding set `eab_kid` and the secret
  `FORGEBASE_TLS_ACME_EAB_HMAC_KEY`. For a private authority whose own HTTPS
  endpoint uses a private root, set `ca_file` to that root (PEM).
- The account key and certificate are stored under `<data_dir>/tls/acme/`,
  sealed with the data directory's master key (`<data_dir>/auth/master.key`).
  `status.json` next to them holds the expiry and last error for
  `forgebase doctor`. `forgebase backup` snapshots do not include `tls/`:
  after a restore the server simply requests a new certificate (mind the
  CA's rate limits when restoring often; a file-level copy of the whole data
  directory keeps it).

#### Redis protocol over TLS

`[resp] tls = true` serves the RESP listener with the same certificate
(`rediss://`; `redis-cli --tls`). It needs `[tls] mode = "files"` or
`"acme"`. Renewals and reloads apply to it too.

#### Several servers

ACME runs on one server per domain: each server keeps its ACME state in its
own data directory, and a CA's validation request may reach a server that
did not ask for it. With more than one ForgeBase server behind a load
balancer (Kubernetes replicas), either terminate TLS at the load balancer
(`mode = "off"`), or use `mode = "files"` with a certificate the platform
renews (cert-manager writing a Secret mounted into every pod): each server
reloads it on its own. PostgreSQL replicas and shards (ADR-0029) are not
affected: `forgebase node` serves no HTTP.

#### Client addresses and operational endpoints with native TLS

Clients connect directly, so the socket address is the client's: keep
`[auth] trust_forwarded_for = false` (the server warns when it is on with
native TLS; only a TLS pass-through or re-encrypting proxy that appends
`X-Forwarded-For` justifies it). `ops_access = "loopback"` keeps its meaning:
a Prometheus on the same host scraping `https://127.0.0.1:443/metrics`
directly (with `tls_config.server_name` set to the certificate's domain) is
admitted without credentials; everyone else needs an administrator's
credentials.

Check the certificate with `forgebase doctor` (a warning under 14 days, a
failure when expired) or the `forgebase_tls_certificate_expiry_seconds`
metric.

### 5.1 Metrics (`/metrics`)

`GET /metrics` serves Prometheus text format 0.0.4 (`Cache-Control: no-store`).
Access follows `server.ops_access` (§5, [security.md §5](security.md#5-admin-and-operational-access)):
by default administrators from anywhere, and a scraper on the same host that
connects directly to `127.0.0.1` without a forwarding header. A scraper
elsewhere sends an admin service-account API key with the `system:read`
scope as `Authorization: Bearer …` (Prometheus `authorization.credentials`). Every
label comes from a fixed list (method, route template, status, state); there
is no per-user or per-record label.

Scrape every 15 s or so; each scrape refreshes the gauges below.

| Metric | Type | Labels |
|---|---|---|
| `forgebase_http_requests_total` | counter | `method`, `route`, `status` |
| `forgebase_http_request_duration_seconds` | histogram | `method`, `route` |
| `forgebase_http_requests_in_flight` | gauge | none |
| `forgebase_build_info` | gauge | `version`, `os`, `arch` |
| `forgebase_uptime_seconds` | gauge | none |
| `forgebase_db_size_bytes`, `forgebase_db_wal_size_bytes`, `forgebase_db_schema_version` | gauge | none |
| `process_resident_memory_bytes` | gauge | none |
| `process_cpu_seconds_total` | counter | none |
| `forgebase_db_query_duration_seconds` | histogram | `mode` = `read` or `write` |
| `forgebase_db_pool_connections_in_use` | gauge | `pool` = `writer` or `reader` |
| `forgebase_db_pool_connections_idle` | gauge | `pool` = `writer` or `reader` |
| `forgebase_jobs` | gauge | `status` = `queued`, `running`, `succeeded`, `dead`, `cancelled` |
| `forgebase_storage_bytes` | gauge | `kind` = `logical` (current objects) or `stored` (after deduplication) |
| `forgebase_storage_objects` | gauge | `kind` = `current` or `versions` |
| `forgebase_vector_indexes` | gauge | `state` = `ready` or `building` |
| `forgebase_vector_indexed_vectors`, `forgebase_vector_index_memory_bytes` | gauge | none |
| `forgebase_kv_keys`, `forgebase_kv_memory_bytes` | gauge | none |
| `forgebase_queue_deliveries` | gauge | `state` = `ready`, `delayed`, `in_flight` (only when `[queue]` is enabled) |
| `forgebase_queue_messages` | gauge | none (only when `[queue]` is enabled) |
| `forgebase_tls_certificate_expiry_seconds` | gauge | none (only with native TLS; negative once expired) |
| `forgebase_tls_renewals_total`, `forgebase_tls_renewal_failures_total` | counter | none (ACME mode) |
| `forgebase_tls_reloads_total`, `forgebase_tls_reload_failures_total` | counter | none (files mode) |
| `forgebase_tls_handshake_failures_total` | counter | `listener` = `https` or `resp` |

Notes:

* `process_*` come from the operating system. A platform that cannot report
  one omits it, and the dashboard overview then shows "not reported".
* The `forgebase_db_query_duration_seconds` histogram times each statement
  inside the database layer (queueing for a pool connection is not included).
  Buckets run from 1 ms to 10 s, like the HTTP histogram.
* Subsystem gauges are read when scraped, each with a two-second cut-off. If a
  read fails the gauge keeps its last value and the error is logged at debug.
* Series appear once the subsystem has something to report. Counters and
  histograms (HTTP, database, functions, RESP) appear after their first event.

## 6. Upgrades

Migrations run automatically on `serve`. Before you upgrade:

1. Read the release notes for the new version. Moving from 0.2 to 0.3: read
   [Upgrading to 0.3](release-notes/0.3.md#upgrading-to-03). PostgreSQL is the
   default for new projects; an existing SQLite project stays SQLite until you
   run `forgebase db convert --to postgres`.
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
| `server.public_url` | unset | Absolute `http://` or `https://` URL with no path, such as `https://forgebase.example.com`; one trailing `/` is dropped. Links the server hands out (the setup link) start with it; unset means the bound address. |
| `server.ops_access` | `"loopback"` | `loopback` (administrators, plus direct same-host callers without forwarding headers), `admin`, `public`. Applies to `/metrics` and `/api/system/*`; `/health` and `/health/ready` stay open. |
| `database.backend` | `"postgres"` (`"sqlite"` in `-lite` builds) | `postgres`, `sqlite`. Set by `init`; the data directory's marker wins over the default (§3.5). Not editable in the dashboard. |
| `database.postgres.mode` | `"managed"` | `managed` (bundled engine), `external` (`FORGEBASE_DATABASE_URL`). Not editable in the dashboard. |
| `database.postgres.url` | unset | External only; a secret: set `FORGEBASE_DATABASE_URL`. |
| `database.postgres.pool_size` | `16` | `1`–`64` read connections; one writer. |
| `database.postgres.listen` | `""` | `""` (Unix socket only) or a loopback `host:port`. |
| `database.postgres.engine_dir` | `""` | An installed PostgreSQL 17+ instead of the bundled engine. |
| `database.postgres.native_rls` | `"logins"` | `off`, `logins`, `enforce` (FB-152). |
| `database.postgres.settings` | `{}` | Extra `postgresql.conf` parameters; ForgeBase's own (listening, preload libraries, replication, anything that runs a program) are refused at start. File only. |
| `database.postgres.extensions.vector`, `graph`, `queue`, `cron`, `trigram` | `"auto"` | `auto`, `on`, `off` (ADR-0029 §1.1). `cron = "off"` keeps pg_cron out of `shared_preload_libraries`. |
| `database.synchronous` | `"full"` | `full`, `normal` (SQLite) |
| `database.busy_timeout_ms` | `5000` | up to `600000` (SQLite) |
| `database.read_pool_size` | `8` | `1`–`64` (SQLite) |
| `log.level` | `"info"` | filter such as `info` or `forgebase=debug`; not empty |
| `log.format` | `"pretty"` | `pretty`, `json` |
| `resp.enabled` | `false` | Redis protocol listener. |
| `resp.host` | `"127.0.0.1"` | |
| `resp.port` | `6380` | `0` picks a free port. |
| `resp.max_connections` | `1024` | `1`–`1000000` |
| `resp.max_bulk_bytes` | `16777216` | `1`–`536870912` |
| `resp.idle_timeout_secs` | `300` | `0` = never. Subscribed connections are exempt. |
| `resp.tls` | `false` | RESP over TLS with the `[tls]` certificate; needs `tls.mode` `files` or `acme`. |
| `tls.mode` | `"off"` | `off` (plain HTTP behind a proxy), `files`, `acme` (§5). |
| `tls.cert_file`, `tls.key_file` | `""` | Files mode: PEM chain and key; required with `files`. Relative paths resolve against the config file's directory. |
| `tls.reload_interval_secs` | `30` | `0`–`86400`; `0` = reload on `SIGHUP` only. |
| `tls.min_version` | `"1.2"` | `1.2`, `1.3`. |
| `tls.hsts_max_age_secs` | `0` | `0` (no header)–`63072000`. |
| `tls.hsts_include_subdomains` | `false` | |
| `tls.redirect_http` | `false` | Plain HTTP listener that redirects to HTTPS (always on with `http-01`). |
| `tls.http_port` | `80` | Port of that listener on `server.host`; must differ from `server.port`. |
| `tls.acme.domains` | `[]` | DNS names, no wildcards; required with `acme`. |
| `tls.acme.email` | `""` | Contact address for the CA. |
| `tls.acme.directory_url` | Let's Encrypt production | `letsencrypt`, `letsencrypt-staging` or an `https://` RFC 8555 directory. |
| `tls.acme.challenges` | `["tls-alpn-01"]` | `tls-alpn-01`, `http-01`, in order of preference. |
| `tls.acme.eab_kid` | `""` | External account binding key id; the HMAC key is `FORGEBASE_TLS_ACME_EAB_HMAC_KEY`. |
| `tls.acme.ca_file` | `""` | PEM root for a private CA's own HTTPS endpoint. |
| `storage.driver` | `"local"` | `local`, `s3` |
| `storage.max_object_bytes` | `5368709120` (5 GiB) | |
| `storage.upload_ttl_secs` | `86400` | |
| `storage.reconcile_grace_secs` | `3600` | |
| `storage.s3_endpoint`, `s3_region`, `s3_bucket`, `s3_prefix`, `s3_part_size` | unset / `""` / `8388608` | S3 only. Credentials come from the environment (§3.3). |
| `realtime.max_connections_per_principal` | `32` | `1`–`1000000`. Most simultaneous realtime (WebSocket) connections for one user or API key; the next one gets `429 too_many_connections`. Raise it for a shared service identity that fans out many subscribers. |
| `billing.enabled` | `true` | Mounts `/api/v1/billing/*` and the provider webhooks. |
| `storage.enabled`, `functions.enabled`, `queue.enabled`, `realtime.enabled`, `vector.enabled`, `graph.enabled`, `identity.enabled`, `access.enabled` | `true` | Switch a module off: its routes answer `404 feature_disabled` and its workers do not start ([architecture.md §6](architecture.md#6-feature-flags), ADR-0026). |
| `auth.trust_forwarded_for` | `false` | Client address from the last `X-Forwarded-For` entry. Only behind your own proxy (§5). |
| `auth.allow_private_outbound` | `false` | The auth service's outbound HTTPS (OAuth/OIDC providers, SMS gateways, Turnstile) refuses loopback, private, link-local and metadata addresses unless this is `true` (an identity provider on your own network). LDAP and S3 endpoints are not affected. |

Example `[billing]` section (turns billing off):

```toml
[billing]
enabled = false   # FORGEBASE_BILLING_ENABLED (default: true)
```

Billing is on by default (a fresh server serves `/api/v1/billing/*` with no provider keys; add test-mode keys at runtime). Set `billing.enabled = false` or `FORGEBASE_BILLING_ENABLED=false` to turn it off. Live payment-provider keys are refused; only
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

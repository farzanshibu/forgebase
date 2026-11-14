//! The emulator: an in-process server on its own data directory, with an
//! administrator and an API key created for the developer.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use forgebase_core::DataLayout;
use forgebase_core::config::telemetry_config::{ObservabilityOverrides, OtlpOverrides};
use forgebase_core::config::{BUNDLED_POSTGRES, DEFAULT_BACKEND, DatabaseBackend, PostgresMode, PostgresOverrides};
use forgebase_server::RunningServer;
use forgebase_server::database::{self, BackendMarker};
use serde_json::json;

use super::http::Api;
use super::state::{self, Creds};
use crate::GlobalArgs;
use crate::probe;

/// Email of the generated administrator.
const ADMIN_EMAIL: &str = "dev@forgebase.local";

/// How to start the emulator.
#[derive(Debug, Clone)]
pub struct Settings {
    /// `sqlite` or `postgres`; `None` keeps a persisted directory's engine, else the build's default.
    pub database: Option<String>,
    /// Keep the data here instead of in a directory deleted on exit.
    pub persist: Option<PathBuf>,
}

/// A running emulator.
pub struct Emulator {
    server: RunningServer,
    /// Base URL.
    pub url: String,
    /// `host:port` to dial.
    pub addr: String,
    /// Administrator and API key.
    pub creds: Creds,
    /// The directory that holds the data and the config.
    pub root: PathBuf,
    /// Whether `root` is deleted on exit.
    pub ephemeral: bool,
    /// `sqlite` or `postgres`.
    pub database: &'static str,
}

fn engine_of(marker: BackendMarker) -> anyhow::Result<&'static str> {
    Ok(match marker {
        BackendMarker::Sqlite => "sqlite",
        BackendMarker::PostgresManaged => "postgres",
        BackendMarker::PostgresExternal => {
            bail!("the dev emulator runs its own database; this directory uses an external PostgreSQL")
        }
    })
}

fn ephemeral_root() -> anyhow::Result<PathBuf> {
    let tag = forgebase_auth::crypto::random_token(6).map_err(|e| anyhow::anyhow!("{e}"))?;
    let tag: String = tag.chars().filter(char::is_ascii_alphanumeric).collect();
    let root = std::env::temp_dir().join(format!("fbdev-{tag}"));
    std::fs::create_dir_all(&root).with_context(|| format!("cannot create {}", root.display()))?;
    Ok(root)
}

impl Emulator {
    /// Starts the server and bootstraps the administrator and the API key.
    /// The caller installs the shutdown signal handlers first.
    pub async fn start(g: &GlobalArgs, s: &Settings) -> anyhow::Result<Self> {
        let (root, ephemeral) = match &s.persist {
            Some(p) => {
                std::fs::create_dir_all(p).with_context(|| format!("cannot create {}", p.display()))?;
                (std::fs::canonicalize(p)?, false)
            }
            None => (ephemeral_root()?, true),
        };
        match Self::boot(g, s, &root, ephemeral).await {
            Ok(e) => Ok(e),
            Err(e) => {
                if ephemeral {
                    let _ = std::fs::remove_dir_all(&root);
                }
                Err(e)
            }
        }
    }

    async fn boot(g: &GlobalArgs, s: &Settings, root: &Path, ephemeral: bool) -> anyhow::Result<Self> {
        let data_dir = root.join("data");
        let config_file = root.join("forgebase.toml");
        if !config_file.exists() {
            std::fs::write(
                &config_file,
                "# ForgeBase dev emulator: settings come from flags and defaults.\n",
            )?;
        }
        let on_disk = database::existing(&DataLayout::new(&data_dir))?;
        let wanted = match s.database.as_deref() {
            Some("sqlite") => Some(BackendMarker::Sqlite),
            Some("postgres") => Some(BackendMarker::PostgresManaged),
            Some(other) => bail!("--database is sqlite or postgres, not {other:?}"),
            None => None,
        };
        let mut cli = g.overrides();
        cli.config_file = Some(config_file);
        cli.data_dir = Some(data_dir.clone());
        let marker = match (wanted, on_disk) {
            (Some(w), Some(d)) if w != d => bail!(
                "{} already holds {}; the dev emulator does not change the database engine (drop --database, or use another --persist directory)",
                root.display(),
                engine_of(d)?
            ),
            (Some(w), _) => w,
            (None, Some(d)) => d,
            // Nothing says which engine: the environment may (FORGEBASE_DATABASE_BACKEND), else the build's default.
            (None, None) => {
                let probe = g.load_with(cli.clone())?;
                if crate::commands::explicit_backend(&probe) {
                    match (probe.config.database.backend, probe.config.database.postgres.mode) {
                        (DatabaseBackend::Sqlite, _) => BackendMarker::Sqlite,
                        (DatabaseBackend::Postgres, PostgresMode::Managed) => BackendMarker::PostgresManaged,
                        (DatabaseBackend::Postgres, PostgresMode::External) => {
                            bail!(
                                "the dev emulator runs its own database: unset the external PostgreSQL settings, or pass --database"
                            )
                        }
                    }
                } else {
                    match DEFAULT_BACKEND {
                        DatabaseBackend::Sqlite => BackendMarker::Sqlite,
                        DatabaseBackend::Postgres => BackendMarker::PostgresManaged,
                    }
                }
            }
        };
        let database = engine_of(marker)?;

        if marker == BackendMarker::Sqlite {
            cli.database_backend = Some(DatabaseBackend::Sqlite);
        } else {
            cli.database_backend = Some(DatabaseBackend::Postgres);
            cli.postgres = Some(PostgresOverrides {
                mode: Some(PostgresMode::Managed),
                ..PostgresOverrides::default()
            });
        }
        let mut loaded = g.load_with(cli.clone())?;
        // Trace ids for `functions invoke --trace` and the function logs (ADR-0035) need trace context to be
        // on, which the server ties to the OTLP switch. Turn it on without exporting anything, unless the
        // developer configured an exporter of their own (then their collector gets the dev server's traces).
        if !loaded.config.observability.otlp.enabled {
            cli.observability = Some(ObservabilityOverrides {
                otlp: Some(OtlpOverrides {
                    enabled: Some(true),
                    endpoint: Some("http://127.0.0.1:4318".into()),
                    export_traces: Some(false),
                    export_logs: Some(false),
                    ..OtlpOverrides::default()
                }),
                ..ObservabilityOverrides::default()
            });
            loaded = g.load_with(cli)?;
        }
        let level = g
            .log_level
            .clone()
            .unwrap_or_else(|| "warn,forgebase::postgres=off,sqlx::query=error,forgebase_db::slow_log=error".into());
        forgebase_observability::init_telemetry(
            &level,
            g.log_format.unwrap_or(forgebase_core::LogFormat::Pretty),
            &loaded.config.observability,
            loaded.config.auth.trust_forwarded_for,
        )?;
        if marker == BackendMarker::PostgresManaged
            && !BUNDLED_POSTGRES
            && !database::bundled_engine_available(&loaded.config.database)
        {
            bail!("{}", database::NO_BUNDLED_ENGINE);
        }
        let server = forgebase_server::start_loaded(&loaded)
            .await
            .map_err(crate::commands::running_hint)?;
        let local = server.local_addr();
        let addr = probe::dial_addr(&local.ip().to_string(), local.port());
        let url = format!("http://{addr}");
        let mut em = Self {
            server,
            url,
            addr,
            creds: Creds::default(),
            root: root.to_path_buf(),
            ephemeral,
            database,
        };
        if let Err(e) = em.bootstrap().await {
            let _ = em.server.shutdown().await;
            return Err(e);
        }
        Ok(em)
    }

    /// Creates (or signs in) the administrator and makes sure the API key exists.
    async fn bootstrap(&mut self) -> anyhow::Result<()> {
        let api = Api::new(&self.addr, None);
        let saved = if self.ephemeral {
            None
        } else {
            state::read_credentials(&self.root)?
        };
        let (status, st) = api.call("GET", "/api/setup/status", None).await?;
        if status != 200 {
            bail!("the dev server does not answer /api/setup/status (HTTP {status})");
        }
        let mut creds = saved.clone().unwrap_or_else(|| Creds {
            admin_email: ADMIN_EMAIL.into(),
            admin_password: random_password().unwrap_or_default(),
            api_key: None,
        });
        if st["setup_required"].as_bool() == Some(true) {
            let token = self
                .server
                .setup_token()
                .ok_or_else(|| anyhow::anyhow!("first-run setup is open but the server issued no setup token"))?
                .to_owned();
            api.ok(
                "POST",
                "/api/setup/admin",
                Some(&json!({"token": token, "email": creds.admin_email, "password": creds.admin_password, "cookie": false})),
            )
            .await
            .context("creating the dev administrator")?;
            creds.api_key = None;
        } else if saved.is_none() {
            bail!(
                "{} holds an administrator, but no {}: restart with a fresh --persist directory, or delete {}",
                self.root.display(),
                state::CREDENTIALS_FILE,
                self.root.display()
            );
        }
        self.creds = creds;
        let admin = Api::new(&self.addr, Some(self.admin_token().await?));
        if self.creds.api_key.is_none() {
            let sa = admin
                .ok(
                    "POST",
                    "/api/auth/service-accounts",
                    Some(
                        &json!({"name": "forgebase-dev", "description": "Created by `forgebase dev`", "role": "admin"}),
                    ),
                )
                .await
                .context("creating the dev service account")?;
            let id = sa["id"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("the service account answer has no id"))?;
            let key = admin
                .ok(
                    "POST",
                    &format!("/api/auth/service-accounts/{id}/keys"),
                    Some(&json!({"name": "dev", "scopes": ["*"]})),
                )
                .await
                .context("creating the dev API key")?;
            self.creds.api_key = Some(
                key["key"]
                    .as_str()
                    .ok_or_else(|| anyhow::anyhow!("the API key answer has no key"))?
                    .to_owned(),
            );
        }
        if !self.ephemeral {
            state::write_credentials(&self.root, &self.creds)?;
        }
        Ok(())
    }

    /// A fresh administrator access token (they expire; each apply signs in again).
    pub async fn admin_token(&self) -> anyhow::Result<String> {
        let v = Api::new(&self.addr, None)
            .ok(
                "POST",
                "/api/auth/admins/login",
                Some(&json!({"email": self.creds.admin_email, "password": self.creds.admin_password})),
            )
            .await
            .context("signing in as the dev administrator")?;
        v["access_token"]
            .as_str()
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("the sign-in answer has no access token"))
    }

    /// A client with the dev API key.
    pub fn api(&self) -> Api {
        Api::new(&self.addr, self.creds.api_key.clone())
    }

    /// Stops the server (draining requests, closing the database and the bundled engine) and deletes an ephemeral data directory.
    pub async fn stop(self) -> anyhow::Result<()> {
        let result = self.server.shutdown().await;
        if self.ephemeral
            && let Err(e) = std::fs::remove_dir_all(&self.root)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            eprintln!("warning: cannot delete {}: {e}", self.root.display());
        }
        result.map_err(Into::into)
    }
}

fn random_password() -> anyhow::Result<String> {
    let t = forgebase_auth::crypto::random_token(18).map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(t)
}

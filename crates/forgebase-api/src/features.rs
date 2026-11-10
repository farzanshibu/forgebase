//! Feature registry and configuration API (FB-91, ADR-0026).
//!
//! * [`gate`] answers `404 feature_disabled` for every route of a module
//!   that is switched off (`[<module>] enabled = false`), before
//!   authentication, so a disabled module looks unmounted.
//! * `GET /api/system/features` lists every module with its typed settings,
//!   their values and where each value came from.
//! * `PATCH /api/system/config` validates a set of settings and writes them
//!   to the config file in one atomic, comment-preserving replace.
//!
//! Both endpoints are for administrators only, whatever `[server]
//! ops_access` says: they reveal and change the deployment.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::Json;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::http::header::{CACHE_CONTROL, HeaderValue};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use forgebase_core::features::{self, FeatureDef, PatchError};
use forgebase_core::{Config, ConfigOrigin, ConfigSources};
use forgebase_data::{AccessContext, Principal};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::error::ApiError;
use crate::http::ApiJson;
use crate::principal::Access;

/// The running configuration and the layers it came from: the state of the
/// two endpoints.
#[derive(Debug)]
pub struct SystemConfig {
    running: Config,
    origin: ConfigOrigin,
    startup_sources: ConfigSources,
    restart_pending: AtomicBool,
    write: tokio::sync::Mutex<()>,
}

impl SystemConfig {
    /// State for a server running `running`, loaded from `origin`.
    pub fn new(running: Config, origin: ConfigOrigin, sources: ConfigSources) -> Self {
        Self {
            running,
            origin,
            startup_sources: sources,
            restart_pending: AtomicBool::new(false),
            write: tokio::sync::Mutex::new(()),
        }
    }

    /// The configuration the server started with.
    pub fn running(&self) -> &Config {
        &self.running
    }

    /// The file `PATCH` writes (the loaded file, else `forgebase.toml` next
    /// to the data directory).
    pub fn target_file(&self) -> PathBuf {
        self.origin.target_file(&self.running.data_dir)
    }

    /// The registry with the configuration as the file says now (values the
    /// server will use after a restart).
    pub fn view(&self) -> SystemFeatures {
        let file = self.target_file();
        let (config, sources, error) = match self.origin.resolve(&file) {
            Ok((c, s)) => (c, s, None),
            Err(e) => (self.running.clone(), self.startup_sources.clone(), Some(e.to_string())),
        };
        let restart_pending =
            self.restart_pending.load(Ordering::Relaxed) || error.is_some() || config != self.running;
        SystemFeatures {
            config_file: file.display().to_string(),
            restart_pending,
            config_error: error,
            features: feature_views(&config, &sources),
        }
    }
}

/// `GET /api/system/features`.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SystemFeatures {
    /// Absolute path of the config file (`PATCH` creates it when missing).
    pub config_file: String,
    /// The file differs from what the server runs: restart to apply it.
    pub restart_pending: bool,
    /// Set when the config file cannot be read; values are then the running ones.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
    /// Every module, in display order.
    pub features: Vec<SystemFeature>,
}

/// One module.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SystemFeature {
    /// Stable id (`storage`).
    pub id: String,
    /// Display name.
    pub name: String,
    /// One line on what it does.
    pub summary: String,
    /// No switch: the module is part of every server.
    pub always_on: bool,
    /// `[<section>] enabled` switches it.
    pub can_disable: bool,
    /// Whether it is on (after a restart, when `restart_pending`).
    pub enabled: bool,
    /// Dashboard route of its studio.
    pub studio_path: Option<String>,
    /// Documentation, relative to the repository root.
    pub docs: String,
    /// The switch first (when `can_disable`), then the module's settings.
    pub settings: Vec<SystemSetting>,
}

/// One setting.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SystemSetting {
    /// Dotted key (`storage.max_object_bytes`), as in `forgebase.toml`.
    pub key: String,
    /// Short label.
    pub label: String,
    /// One or two sentences; names the environment variable and flag.
    pub help: String,
    /// `bool`, `int`, `string`, `enum`, `duration`, `list` or `url`.
    #[serde(rename = "type")]
    pub ty: String,
    /// Unit of an integer: `bytes`, `seconds`, `ms`, `days`.
    pub unit: Option<String>,
    /// Allowed values of an `enum`; `null` otherwise.
    pub options: Option<Vec<String>>,
    /// Current value; `null` for a secret.
    #[schema(value_type = Object)]
    pub value: JsonValue,
    /// Built-in default; `null` for a secret.
    #[schema(value_type = Object)]
    pub default: JsonValue,
    /// `default`, `file`, `env` or `flag`.
    pub source: String,
    /// Whether `PATCH /api/system/config` may change it (never for `env`,
    /// `flag` or secrets).
    pub editable: bool,
    /// A change takes effect after a restart.
    pub restart_required: bool,
    /// Never listed with a value nor written by the API.
    pub secret: bool,
}

/// `PATCH /api/system/config` body.
#[derive(Debug, Clone, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct SystemConfigPatch {
    /// Dotted keys from the registry and their new values.
    #[schema(value_type = Object)]
    pub set: serde_json::Map<String, JsonValue>,
}

/// `PATCH /api/system/config` answer.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct SystemConfigPatched {
    /// The file that was written.
    pub written: String,
    /// The change needs a restart.
    pub restart_required: bool,
    /// Keys that took effect without a restart (none in this version).
    pub applied: Vec<String>,
    /// The registry after the change.
    pub features: Vec<SystemFeature>,
}

/// The registry for `config`, with `sources`.
pub fn feature_views(config: &Config, sources: &ConfigSources) -> Vec<SystemFeature> {
    let defaults = features::default_values();
    features::FEATURES
        .iter()
        .map(|f| SystemFeature {
            id: f.id.to_owned(),
            name: f.name.to_owned(),
            summary: f.summary.to_owned(),
            always_on: !f.can_disable,
            can_disable: f.can_disable,
            enabled: f.enabled(config),
            studio_path: f.studio_path.map(str::to_owned),
            docs: f.docs.to_owned(),
            settings: f
                .settings
                .iter()
                .map(|s| SystemSetting {
                    key: s.key.to_owned(),
                    label: s.label.to_owned(),
                    help: s.help.to_owned(),
                    ty: s.ty.as_str().to_owned(),
                    unit: s.unit.map(str::to_owned),
                    options: (!s.options.is_empty()).then(|| s.options.iter().map(|o| (*o).to_owned()).collect()),
                    value: features::setting_value(config, s.key).unwrap_or(JsonValue::Null),
                    default: defaults.get(s.key).cloned().unwrap_or(JsonValue::Null),
                    source: features::source_of(s, sources).as_str().to_owned(),
                    editable: features::editable(s, sources),
                    restart_required: s.restart_required,
                    secret: s.secret,
                })
                .collect(),
        })
        .collect()
}

/// The disabled modules and their route patterns, for [`gate`].
#[derive(Debug, Clone)]
pub struct DisabledFeatures(Arc<Vec<&'static FeatureDef>>);

impl DisabledFeatures {
    /// The modules that are off in `config`.
    pub fn of(config: &Config) -> Self {
        Self(Arc::new(features::disabled(config)))
    }

    /// Whether no module with routes is off.
    pub fn is_empty(&self) -> bool {
        self.0.iter().all(|f| f.routes.is_empty())
    }

    /// The disabled module that owns `path`.
    pub fn owner(&self, path: &str) -> Option<&'static FeatureDef> {
        features::owner_of(&self.0, path)
    }
}

/// Middleware: `404 feature_disabled` for the routes of a disabled module.
pub async fn gate(State(off): State<DisabledFeatures>, req: Request, next: Next) -> Response {
    match off.owner(req.uri().path()) {
        Some(f) => feature_disabled(f).into_response(),
        None => next.run(req).await,
    }
}

/// The `feature_disabled` error of `f`.
pub fn feature_disabled(f: &FeatureDef) -> ApiError {
    ApiError::new(StatusCode::NOT_FOUND, "feature_disabled", f.disabled_message())
}

fn require_admin(ctx: &AccessContext) -> Result<(), ApiError> {
    if ctx.is_privileged() {
        Ok(())
    } else if ctx.principal == Principal::Anonymous {
        Err(ApiError::new(
            StatusCode::UNAUTHORIZED,
            "unauthenticated",
            "the feature and configuration API needs an administrator's credentials",
        ))
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the feature and configuration API is for administrators",
        ))
    }
}

fn unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "config_unavailable",
        "this server was started without a configuration",
    )
}

fn no_store(mut res: Response) -> Response {
    res.headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    res
}

/// `GET /api/system/features`.
pub async fn get_features(
    State(sys): State<Option<Arc<SystemConfig>>>,
    Access(ctx): Access,
) -> Result<Response, ApiError> {
    require_admin(&ctx)?;
    let sys = sys.ok_or_else(unavailable)?;
    let view = tokio::task::spawn_blocking(move || sys.view())
        .await
        .map_err(|_| ApiError::internal())?;
    Ok(no_store(Json(view).into_response()))
}

/// `PATCH /api/system/config`.
pub async fn patch_config(
    State(sys): State<Option<Arc<SystemConfig>>>,
    Access(ctx): Access,
    ApiJson(body): ApiJson<SystemConfigPatch>,
) -> Result<Response, ApiError> {
    require_admin(&ctx)?;
    let sys = sys.ok_or_else(unavailable)?;
    // One writer at a time: validation reads the file the write replaces.
    let _write = sys.write.lock().await;
    let task = sys.clone();
    let result = tokio::task::spawn_blocking(move || {
        features::apply_patch(&task.origin, &task.running.data_dir, &body.set).map(|out| (out, task.view()))
    })
    .await
    .map_err(|_| ApiError::internal())?;
    match result {
        Ok((out, view)) => {
            if out.restart_required {
                sys.restart_pending.store(true, Ordering::Relaxed);
            }
            tracing::info!(
                actor = %ctx.actor(),
                file = %out.written.display(),
                keys = ?out.keys,
                "configuration written; restart to apply"
            );
            let body = SystemConfigPatched {
                written: out.written.display().to_string(),
                restart_required: out.restart_required,
                applied: Vec::new(),
                features: view.features,
            };
            Ok(no_store(Json(body).into_response()))
        }
        Err(PatchError::Invalid(d)) => Err(ApiError::new(
            StatusCode::BAD_REQUEST,
            "validation_failed",
            "one or more settings are invalid; nothing was written",
        )
        .with_details(serde_json::to_value(d).unwrap_or_default())),
        Err(PatchError::Overridden(d)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "setting_overridden",
            "one or more settings are set by an environment variable or a flag; nothing was written",
        )
        .with_details(serde_json::to_value(d).unwrap_or_default())),
        Err(PatchError::Config(e)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "config_invalid",
            format!("the config file cannot be read, fix it by hand first: {e}"),
        )),
        Err(e @ PatchError::Write { .. }) => {
            tracing::error!(error = %e, "configuration not written");
            Err(ApiError::new(
                StatusCode::INTERNAL_SERVER_ERROR,
                "config_write_failed",
                "the config file could not be written",
            ))
        }
    }
}

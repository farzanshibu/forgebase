//! The capability matrix (ADR-0034): per feature, on this server, the
//! engine it runs on (a PostgreSQL extension as accelerator, the portable
//! engine, or the backend's own machinery), its state and its limits.
//!
//! Served as `GET /api/system/capabilities` and as `database.capabilities`
//! of `GET /api/system/info` (both behind the ops gate, like the rest of
//! `/api/system/*`). The accelerated features read the extension report of
//! ADR-0029 §1.1 (`database.features`): the one the server's extension
//! setup kept, or, in a process that did not run it, a detection that
//! changes nothing. `docs/feature-parity.md` is the cross-backend table.
//!
//! `state`:
//!
//! | Value | Meaning |
//! |---|---|
//! | `active` | on its PostgreSQL extension |
//! | `portable` | on the portable engine: SQLite, or the extension is not usable here (`reason`) |
//! | `off` | on the portable engine because `[database.postgres.extensions]` says `off` |
//! | `native` | the feature has no accelerator; it runs on the backend's own machinery |
//! | `unavailable` | not on this backend (`reason`) |

use std::collections::BTreeMap;

use axum::Json;
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use forgebase_db::{BackendKind, ExtensionReport, Feature, FeatureState};
use serde::Serialize;
use serde_json::{Value, json};
use utoipa::ToSchema;

use crate::AppState;
use crate::error::ApiError;

/// Every feature of this server.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Capabilities {
    /// `sqlite` or `postgres`.
    pub backend: &'static str,
    /// One entry per feature, in a fixed order.
    pub features: Vec<Capability>,
}

/// One feature on this server.
#[derive(Debug, Clone, PartialEq, Serialize, ToSchema)]
pub struct Capability {
    /// Stable name: `vector`, `graph`, `opencypher`, `fulltext`, `fuzzy`,
    /// `queue`, `cron`, `kv`, `realtime`, `read_replicas`, `sharding`.
    pub feature: &'static str,
    /// Human name.
    pub label: &'static str,
    /// What it runs on: an extension and its version (`pgvector 0.8.7`,
    /// `Apache AGE 1.8.0`, `pg_trgm 1.6`) or a description of the
    /// portable or built-in engine.
    pub engine: String,
    /// Whether `engine` is a PostgreSQL extension (an accelerator).
    pub accelerated: bool,
    /// `active`, `portable`, `off`, `native` or `unavailable` (see the
    /// module documentation).
    pub state: &'static str,
    /// `[database.postgres.extensions]` mode (`auto`, `on`, `off`) of an
    /// accelerated feature on PostgreSQL.
    pub mode: Option<&'static str>,
    /// The accelerator's PostgreSQL extension, for accelerated features.
    pub extension: Option<&'static str>,
    /// Why the feature is not on its accelerator although it could be, or
    /// why it is unavailable.
    pub reason: Option<String>,
    /// Limits of the engine in use (names and values).
    #[schema(value_type = Object)]
    pub limits: BTreeMap<&'static str, Value>,
}

/// The extension report of ADR-0029 §1.1: the one the server's setup kept,
/// or a detection with the configured modes (no changes made). `None` on
/// SQLite, or when detection fails.
pub(crate) async fn extension_report(state: &AppState) -> Option<ExtensionReport> {
    let db = state.db();
    if db.kind() != BackendKind::Postgres {
        return None;
    }
    if let Some(r) = db.extensions() {
        return Some((*r).clone());
    }
    let mut modes = forgebase_core::config::PostgresExtensions::default();
    for (feature, _, mode) in &state.database_runtime().extensions {
        let Ok(mode) = mode.parse::<forgebase_core::config::ExtensionMode>() else {
            continue;
        };
        match Feature::from_name(feature) {
            Some(Feature::Vector) => modes.vector = mode,
            Some(Feature::Graph) => modes.graph = mode,
            Some(Feature::Queue) => modes.queue = mode,
            Some(Feature::Cron) => modes.cron = mode,
            Some(Feature::Trigram) => modes.trigram = mode,
            None => {}
        }
    }
    match db.detect_extensions(&modes).await {
        Ok(r) => Some(r),
        Err(e) => {
            tracing::debug!(error = %e, "cannot detect PostgreSQL extensions");
            None
        }
    }
}

/// The matrix of this server.
pub(crate) async fn matrix(state: &AppState) -> Capabilities {
    let report = extension_report(state).await;
    build(state.db().kind(), report.as_ref())
}

/// `GET /api/system/capabilities`.
pub async fn get_capabilities(State(state): State<AppState>) -> Result<Response, ApiError> {
    Ok(crate::routes::no_store(Json(matrix(&state).await).into_response()))
}

fn limits(pairs: &[(&'static str, Value)]) -> BTreeMap<&'static str, Value> {
    pairs.iter().cloned().collect()
}

/// An accelerated feature: its extension when the report says it is
/// active, else `portable`, with the report's mode and reason.
fn accelerated(
    backend: BackendKind,
    report: Option<&ExtensionReport>,
    feature: Feature,
    name: &'static str,
    label: &'static str,
    portable: &str,
    limits_of: impl Fn(bool) -> BTreeMap<&'static str, Value>,
) -> Capability {
    let base = |engine: String, active: bool, state: &'static str, mode, reason| Capability {
        feature: name,
        label,
        engine,
        accelerated: active,
        state,
        mode,
        extension: Some(feature.extension()),
        reason,
        limits: limits_of(active),
    };
    if backend != BackendKind::Postgres {
        return base(
            portable.to_owned(),
            false,
            "portable",
            None,
            Some("SQLite has no extensions: the portable engine runs".into()),
        );
    }
    match report.and_then(|r| r.get(feature)) {
        Some(s) => {
            let active = s.state == FeatureState::Active;
            base(
                if active { s.engine.clone() } else { portable.to_owned() },
                active,
                s.state.as_str(),
                Some(s.mode.as_str()),
                s.reason.clone(),
            )
        }
        None => base(
            portable.to_owned(),
            false,
            "portable",
            None,
            Some("the extension status could not be read".into()),
        ),
    }
}

fn native(name: &'static str, label: &'static str, engine: &str, limits: BTreeMap<&'static str, Value>) -> Capability {
    Capability {
        feature: name,
        label,
        engine: engine.to_owned(),
        accelerated: false,
        state: "native",
        mode: None,
        extension: None,
        reason: None,
        limits,
    }
}

fn postgres_only(backend: BackendKind, name: &'static str, label: &'static str, engine: &str) -> Capability {
    let pg = backend == BackendKind::Postgres;
    Capability {
        feature: name,
        label,
        engine: if pg { engine.to_owned() } else { "none".to_owned() },
        accelerated: false,
        state: if pg { "native" } else { "unavailable" },
        mode: None,
        extension: None,
        reason: (!pg).then(|| "PostgreSQL only (ADR-0029)".to_owned()),
        limits: BTreeMap::new(),
    }
}

/// The matrix for `backend` with extension report `report` (pure: the
/// tests build it for every case).
pub fn build(backend: BackendKind, report: Option<&ExtensionReport>) -> Capabilities {
    use forgebase_data::query::{cypher, trigram};
    let pg = backend == BackendKind::Postgres;
    let features = vec![
        accelerated(
            backend,
            report,
            Feature::Vector,
            "vector",
            "Vector search",
            "portable (canonical vectors in the database, USearch HNSW index files)",
            |_| BTreeMap::new(),
        ),
        native(
            "graph",
            "Graph traversal (REST)",
            "recursive CTE over the graph tables",
            limits(&[
                ("max_depth", json!(forgebase_graph::MAX_DEPTH)),
                ("max_results", json!(forgebase_graph::MAX_LIMIT)),
                ("max_shortest_path_nodes", json!(forgebase_graph::MAX_VISITED)),
            ]),
        ),
        accelerated(
            backend,
            report,
            Feature::Graph,
            "opencypher",
            "openCypher (read-only)",
            "portable (an openCypher subset compiled to SQL over the graph tables)",
            |age| {
                let mut l = limits(&[
                    ("max_rows", json!(forgebase_graph::MAX_LIMIT)),
                    ("max_timeout_ms", json!(forgebase_graph::MAX_TIMEOUT_MS)),
                    ("max_query_bytes", json!(forgebase_graph::MAX_QUERY_BYTES)),
                ]);
                if !age {
                    l.insert("max_path_depth", json!(cypher::MAX_PATH_DEPTH));
                    l.insert(
                        "max_paths_per_relationship",
                        json!(forgebase_graph::portable::MAX_WALK_ROWS),
                    );
                    l.insert("max_patterns", json!(cypher::MAX_ELEMENTS));
                }
                l
            },
        ),
        native(
            "fulltext",
            "Full-text search",
            if pg {
                "tsvector + GIN (plainto_tsquery)"
            } else {
                "FTS5 (unicode61)"
            },
            BTreeMap::new(),
        ),
        accelerated(
            backend,
            report,
            Feature::Trigram,
            "fuzzy",
            "Fuzzy search (trigram indexes)",
            if pg {
                "portable (GIN over forgebase._forgebase_trgm)"
            } else {
                "portable (trigram tables kept by triggers)"
            },
            |ext| {
                let mut l = limits(&[("default_threshold", json!(trigram::DEFAULT_THRESHOLD))]);
                if !pg && !ext {
                    l.insert("max_chars", json!(trigram::MAX_CHARS));
                }
                l
            },
        ),
        accelerated(
            backend,
            report,
            Feature::Queue,
            "queue",
            "Queues (subscription deliveries)",
            "portable (delivery tables)",
            |_| BTreeMap::new(),
        ),
        accelerated(
            backend,
            report,
            Feature::Cron,
            "cron",
            "Scheduled jobs",
            "portable (the server's scheduler tick)",
            |_| BTreeMap::new(),
        ),
        native(
            "kv",
            "Key-value (Redis protocol)",
            "in memory, written behind to the database",
            BTreeMap::new(),
        ),
        native(
            "realtime",
            "Realtime",
            "change outbox written in each data change's transaction",
            BTreeMap::new(),
        ),
        postgres_only(
            backend,
            "read_replicas",
            "Read replicas",
            "streaming replicas (managed or external)",
        ),
        postgres_only(
            backend,
            "sharding",
            "Tenant sharding",
            "control database plus shard sets",
        ),
    ];
    Capabilities {
        backend: backend.as_str(),
        features,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forgebase_core::config::ExtensionMode;
    use forgebase_db::FeatureStatus;

    fn status(feature: Feature, state: FeatureState, mode: ExtensionMode) -> FeatureStatus {
        FeatureStatus {
            feature,
            extension: feature.extension(),
            mode,
            engine: if state == FeatureState::Active {
                format!("{} 1.0", feature.label())
            } else {
                "portable".into()
            },
            state,
            available: Some("1.0".into()),
            installed: (state == FeatureState::Active).then(|| "1.0".into()),
            schema: None,
            schema_version: 0,
            reason: (state == FeatureState::Portable).then(|| "not usable".into()),
        }
    }

    fn get<'a>(c: &'a Capabilities, name: &str) -> &'a Capability {
        c.features.iter().find(|f| f.feature == name).unwrap()
    }

    #[test]
    fn sqlite_runs_everything_portable_or_native() {
        let c = build(BackendKind::Sqlite, None);
        assert_eq!(c.backend, "sqlite");
        for name in ["vector", "opencypher", "fuzzy", "queue", "cron"] {
            let f = get(&c, name);
            assert_eq!((f.state, f.accelerated, f.mode), ("portable", false, None), "{name}");
            assert!(f.engine.starts_with("portable"), "{name}: {}", f.engine);
        }
        for name in ["graph", "fulltext", "kv", "realtime"] {
            assert_eq!(get(&c, name).state, "native", "{name}");
        }
        assert_eq!(get(&c, "fulltext").engine, "FTS5 (unicode61)");
        assert_eq!(get(&c, "read_replicas").state, "unavailable");
        assert_eq!(get(&c, "fuzzy").limits["max_chars"], json!(4096));
        assert_eq!(get(&c, "opencypher").limits["max_path_depth"], json!(10));
    }

    #[test]
    fn postgres_follows_the_extension_report() {
        let report = ExtensionReport {
            features: vec![
                status(Feature::Vector, FeatureState::Active, ExtensionMode::Auto),
                status(Feature::Graph, FeatureState::Off, ExtensionMode::Off),
                status(Feature::Queue, FeatureState::Portable, ExtensionMode::Auto),
                status(Feature::Cron, FeatureState::Active, ExtensionMode::On),
                status(Feature::Trigram, FeatureState::Off, ExtensionMode::Off),
            ],
        };
        let c = build(BackendKind::Postgres, Some(&report));
        let v = get(&c, "vector");
        assert_eq!(
            (v.state, v.accelerated, v.engine.as_str(), v.mode),
            ("active", true, "pgvector 1.0", Some("auto"))
        );
        let g = get(&c, "opencypher");
        assert_eq!((g.state, g.accelerated, g.mode), ("off", false, Some("off")));
        assert!(g.engine.starts_with("portable"));
        assert!(g.limits.contains_key("max_paths_per_relationship"));
        let q = get(&c, "queue");
        assert_eq!((q.state, q.reason.as_deref()), ("portable", Some("not usable")));
        let f = get(&c, "fuzzy");
        assert_eq!(f.state, "off");
        assert_eq!(f.engine, "portable (GIN over forgebase._forgebase_trgm)");
        assert!(!f.limits.contains_key("max_chars"));
        assert_eq!(get(&c, "read_replicas").state, "native");
        assert_eq!(get(&c, "fulltext").engine, "tsvector + GIN (plainto_tsquery)");
        // AGE active: the AGE engine's limits.
        let report = ExtensionReport {
            features: vec![status(Feature::Graph, FeatureState::Active, ExtensionMode::Auto)],
        };
        let c = build(BackendKind::Postgres, Some(&report));
        let g = get(&c, "opencypher");
        assert_eq!((g.state, g.engine.as_str()), ("active", "Apache AGE 1.0"));
        assert!(!g.limits.contains_key("max_path_depth"));
        // No report: portable with a reason.
        let c = build(BackendKind::Postgres, None);
        assert_eq!(get(&c, "vector").state, "portable");
        assert!(get(&c, "vector").reason.is_some());
    }
}

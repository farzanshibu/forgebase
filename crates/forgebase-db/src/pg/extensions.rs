//! PostgreSQL extensions as accelerators (ADR-0029 §1.1, FB-161).
//!
//! Each accelerated [`Feature`] keeps its portable engine and may run on a
//! PostgreSQL extension instead. [`Database::setup_extensions`] runs after
//! the migrations, once per start:
//!
//! 1. **Detect** what the server has: `pg_available_extensions` (control
//!    files and installed versions), `shared_preload_libraries`,
//!    `session_preload_libraries` and `cron.database_name`.
//! 2. **Decide** per feature from its mode (`auto`, `on`, `off`,
//!    `[database.postgres.extensions]`) and, for the features that store
//!    data ([`Feature::stateful`]: vector, queue), the engine the database
//!    was set up with (`_forgebase_meta` `ext.<feature>.engine`). A stateful
//!    feature never changes engine because an extension appeared or went
//!    away: that is a conversion job (`vector.convert`, `queue.convert`).
//! 3. **Create** the extensions in use: `CREATE EXTENSION IF NOT EXISTS`,
//!    the relocatable ones `WITH SCHEMA forgebase` (where the baseline keeps
//!    `citext` and `unaccent`). Extensions only a superuser may create
//!    (pgvector, pg_cron, AGE) go through the administrator URL
//!    (`forgebase_super` on the bundled engine, [`ExtensionSetup::admin_url`]);
//!    the others (pgmq, pg_trgm) through the server's own role, which then
//!    owns them (pgmq's queues are tables the server creates).
//! 4. **Grant** the server and reader roles the extension schemas they use
//!    (`pgmq`, `cron`, `ag_catalog`), every start (idempotent).
//! 5. **Versioned DDL**: each feature's PostgreSQL-only DDL ([`EXTENSION_DDL`])
//!    is applied in version order on the writer, each step in one
//!    schema-change transaction that also records `ext.<feature>.schema =
//!    N` in `_forgebase_meta`.
//!
//! A feature in mode `on` whose extension cannot be used refuses the start
//! ([`ExtensionError::Refused`], all problems in one message). In `auto` it
//! runs portable and says why ([`FeatureStatus::reason`]).
//!
//! The result is kept on the [`Database`]: [`Database::feature_active`] is
//! how the accelerator packages ask (cheap, no query), and
//! [`Database::info`] and `/api/system/info` report it.
//!
//! [`Database`]: crate::Database
//! [`Database::setup_extensions`]: crate::Database::setup_extensions
//! [`Database::feature_active`]: crate::Database::feature_active
//! [`Database::info`]: crate::Database::info

use std::collections::BTreeMap;
use std::fmt;

use forgebase_core::config::{ExtensionMode, PostgresExtensions};
use forgebase_core::{ErrorCode, ErrorKind};
use serde::Serialize;
use sqlx::{AssertSqlSafe, Connection};

use super::PgDatabase;
use crate::backend::{Backend, Statement, TxMode, Value};
use crate::dialect::Dialect;
use crate::{DbError, SecretString};

/// Schema of the relocatable extensions (pgvector, pg_trgm), next to the
/// baseline's `citext` and `unaccent`.
pub const EXTENSION_SCHEMA: &str = "forgebase";

/// A feature that can run on a PostgreSQL extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Vector search on pgvector (FB-162).
    Vector,
    /// The derived openCypher graph on Apache AGE (FB-163).
    Graph,
    /// Subscription deliveries on pgmq (FB-164).
    Queue,
    /// Schedules on pg_cron (FB-165).
    Cron,
    /// Trigram indexes and fuzzy search on pg_trgm (FB-166).
    Trigram,
}

impl Feature {
    /// Every feature, in the order of `[database.postgres.extensions]`.
    pub const ALL: [Feature; 5] = [
        Feature::Vector,
        Feature::Graph,
        Feature::Queue,
        Feature::Cron,
        Feature::Trigram,
    ];

    /// The configuration key and API name (`vector`, `graph`, …).
    pub fn name(self) -> &'static str {
        match self {
            Feature::Vector => "vector",
            Feature::Graph => "graph",
            Feature::Queue => "queue",
            Feature::Cron => "cron",
            Feature::Trigram => "trigram",
        }
    }

    /// The PostgreSQL extension (`CREATE EXTENSION <name>`).
    pub fn extension(self) -> &'static str {
        match self {
            Feature::Vector => "vector",
            Feature::Graph => "age",
            Feature::Queue => "pgmq",
            Feature::Cron => "pg_cron",
            Feature::Trigram => "pg_trgm",
        }
    }

    /// The extension's product name, as `engine` shows it (`pgvector 0.8.7`).
    pub fn label(self) -> &'static str {
        match self {
            Feature::Vector => "pgvector",
            Feature::Graph => "Apache AGE",
            Feature::Queue => "pgmq",
            Feature::Cron => "pg_cron",
            Feature::Trigram => "pg_trgm",
        }
    }

    /// Whether the feature keeps canonical data in the extension's storage,
    /// so switching engines moves data (a conversion job). The others hold
    /// only derived state (an index kind, the derived graph, `cron.job`
    /// rows synced from schedules) and follow their mode on every start.
    pub fn stateful(self) -> bool {
        matches!(self, Feature::Vector | Feature::Queue)
    }

    /// Whether only a superuser may create the extension (not trusted, and
    /// not `superuser = false` in its control file).
    pub fn needs_superuser(self) -> bool {
        matches!(self, Feature::Vector | Feature::Graph | Feature::Cron)
    }

    /// The schema `CREATE EXTENSION` puts it in: [`EXTENSION_SCHEMA`] for
    /// the relocatable ones; `None` for those with a fixed schema (`pgmq`,
    /// `ag_catalog`, pg_cron's `cron`).
    pub fn target_schema(self) -> Option<&'static str> {
        match self {
            Feature::Vector | Feature::Trigram => Some(EXTENSION_SCHEMA),
            Feature::Graph | Feature::Queue | Feature::Cron => None,
        }
    }

    /// The configured mode.
    pub fn mode(self, modes: &PostgresExtensions) -> ExtensionMode {
        match self {
            Feature::Vector => modes.vector,
            Feature::Graph => modes.graph,
            Feature::Queue => modes.queue,
            Feature::Cron => modes.cron,
            Feature::Trigram => modes.trigram,
        }
    }

    /// `_forgebase_meta` key of the applied extension DDL version.
    pub fn schema_key(self) -> String {
        format!("ext.{}.schema", self.name())
    }

    /// `_forgebase_meta` key of the engine a stateful feature stores its
    /// data with (`extension` or `portable`).
    pub fn engine_key(self) -> String {
        format!("ext.{}.engine", self.name())
    }

    /// The feature named `name` (`vector`, …).
    pub fn from_name(name: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.name() == name)
    }
}

impl fmt::Display for Feature {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// What a feature runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureState {
    /// On its extension.
    Active,
    /// On the portable engine although the mode allows the extension
    /// (`auto` without a usable extension, or data stored portably).
    Portable,
    /// On the portable engine because the mode is `off`.
    Off,
}

impl FeatureState {
    /// `active`, `portable` or `off`.
    pub fn as_str(self) -> &'static str {
        match self {
            FeatureState::Active => "active",
            FeatureState::Portable => "portable",
            FeatureState::Off => "off",
        }
    }
}

/// The engine of one feature after setup (or detection).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FeatureStatus {
    /// The feature.
    pub feature: Feature,
    /// The PostgreSQL extension.
    pub extension: &'static str,
    /// The configured mode (`auto`, `on`, `off`).
    pub mode: ExtensionMode,
    /// `pgvector 0.8.7` (label and installed version) when active, else
    /// `portable`.
    pub engine: String,
    /// What it runs on.
    pub state: FeatureState,
    /// Version the server can install (`pg_available_extensions`), if any.
    pub available: Option<String>,
    /// Installed version in this database, if any.
    pub installed: Option<String>,
    /// Schema of the installed extension.
    pub schema: Option<String>,
    /// Applied extension DDL version (`ext.<feature>.schema`; 0 = none).
    pub schema_version: u32,
    /// Why the feature is portable although its mode allows the extension.
    pub reason: Option<String>,
}

impl FeatureStatus {
    /// Whether the feature runs on its extension.
    pub fn is_active(&self) -> bool {
        self.state == FeatureState::Active
    }
}

/// Every feature's status, in [`Feature::ALL`] order.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ExtensionReport {
    /// One entry per feature (empty on SQLite).
    pub features: Vec<FeatureStatus>,
}

impl ExtensionReport {
    /// The status of `feature`.
    pub fn get(&self, feature: Feature) -> Option<&FeatureStatus> {
        self.features.iter().find(|s| s.feature == feature)
    }

    /// Whether `feature` runs on its extension.
    pub fn is_active(&self, feature: Feature) -> bool {
        self.get(feature).is_some_and(FeatureStatus::is_active)
    }
}

/// Inputs of [`Database::setup_extensions`](crate::Database::setup_extensions).
#[derive(Debug, Clone, Default)]
pub struct ExtensionSetup {
    /// `[database.postgres.extensions]`.
    pub modes: PostgresExtensions,
    /// A superuser's URL for the extensions only a superuser may create
    /// (the bundled engine's `forgebase_super`). `None`: the database's own
    /// role creates them (an external server's configured role).
    pub admin_url: Option<SecretString>,
    /// A read-only role to grant the extension schemas to as well (the
    /// bundled engine's `forgebase_reader`).
    pub reader_role: Option<String>,
}

/// One step of an accelerator's PostgreSQL-only DDL. `{schema}` in `sql`
/// is replaced by the extension's schema (quoted).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExtensionDdl {
    /// The feature.
    pub feature: Feature,
    /// Version this step brings `ext.<feature>.schema` to (1, 2, … per feature).
    pub version: u32,
    /// The statements (simple protocol: several are allowed).
    pub sql: &'static str,
}

/// The versioned DDL of every accelerator (ADR-0029 §1.1). Append steps
/// with the next version of their feature; never change a released step.
pub const EXTENSION_DDL: &[ExtensionDdl] = &[
    // ADR-0029 §1.4: the canonical vector in a pgvector column (no fixed
    // dimension); the portable bytea column stays NULL on accelerated
    // databases.
    ExtensionDdl {
        feature: Feature::Vector,
        version: 1,
        sql: "ALTER TABLE _forgebase_vector_values ADD COLUMN IF NOT EXISTS vec {schema}.vector, \
              ALTER COLUMN vector DROP NOT NULL",
    },
];

/// The DDL steps of `feature`, in version order.
pub fn ddl_steps(feature: Feature) -> impl Iterator<Item = &'static ExtensionDdl> {
    EXTENSION_DDL.iter().filter(move |d| d.feature == feature)
}

/// Why setting up the extensions failed.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    /// A feature's mode or its stored data needs an extension that cannot
    /// be used, or a mode contradicts the stored data. One entry per
    /// feature.
    #[error("PostgreSQL extensions: {}", .0.join("; "))]
    Refused(Vec<String>),
    /// A database error.
    #[error(transparent)]
    Db(#[from] DbError),
}

impl ErrorCode for ExtensionError {
    fn code(&self) -> &'static str {
        match self {
            ExtensionError::Refused(_) => "extension_unavailable",
            ExtensionError::Db(e) => e.code(),
        }
    }

    fn kind(&self) -> ErrorKind {
        match self {
            ExtensionError::Refused(_) => ErrorKind::Unavailable,
            ExtensionError::Db(e) => e.kind(),
        }
    }
}

/// The engine a stateful feature's data is stored with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Pin {
    Extension,
    Portable,
}

impl Pin {
    fn as_str(self) -> &'static str {
        match self {
            Pin::Extension => "extension",
            Pin::Portable => "portable",
        }
    }

    fn parse(s: &str) -> Option<Pin> {
        match s {
            "extension" => Some(Pin::Extension),
            "portable" => Some(Pin::Portable),
            _ => None,
        }
    }
}

/// What the server offers for one extension.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Offer {
    /// `pg_available_extensions.default_version` (a control file exists).
    pub(crate) available: Option<String>,
    /// Installed version in this database.
    pub(crate) installed: Option<String>,
    /// Schema of the installed extension.
    pub(crate) schema: Option<String>,
}

/// Server facts the decisions depend on.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Probe {
    pub(crate) offers: BTreeMap<String, Offer>,
    pub(crate) database: String,
    /// The role may read superuser-only settings (`pg_read_all_settings`,
    /// which the bundled engine grants `forgebase_server`). Without it the
    /// preload lists below read as empty.
    pub(crate) settings_visible: bool,
    pub(crate) shared_preload: String,
    pub(crate) session_preload: String,
    pub(crate) cron_database: Option<String>,
    /// A setting of AGE (`age.*`) exists: the library is loaded in this session.
    pub(crate) age_settings: bool,
    pub(crate) role: String,
}

fn listed(list: &str, module: &str) -> bool {
    list.split(',').map(str::trim).any(|m| {
        let m = m.trim_matches(|c| c == '"' || c == '\'');
        let base = m.rsplit('/').next().unwrap_or(m);
        base == module || base.strip_suffix(".so").is_some_and(|b| b == module)
    })
}

impl Probe {
    fn offer(&self, f: Feature) -> Offer {
        self.offers.get(f.extension()).cloned().unwrap_or_default()
    }

    /// `Err(reason)` when the extension cannot be used in this database,
    /// installed or not.
    pub(crate) fn usable(&self, f: Feature) -> Result<(), String> {
        let offer = self.offer(f);
        if offer.available.is_none() && offer.installed.is_none() {
            return Err(format!(
                "the server has no {} extension (not in pg_available_extensions)",
                f.extension()
            ));
        }
        match f {
            // Installing pg_cron needs it preloaded in its database, so an
            // installed one passed that check.
            Feature::Cron if !self.settings_visible => {
                if offer.installed.is_none() {
                    return Err(format!(
                        "role {:?} cannot read shared_preload_libraries (grant it pg_read_all_settings), so \
                         pg_cron's preload cannot be checked; or install pg_cron as a superuser",
                        self.role
                    ));
                }
            }
            Feature::Cron => {
                if !listed(&self.shared_preload, "pg_cron") {
                    return Err("pg_cron is not in shared_preload_libraries".into());
                }
                match self.cron_database.as_deref() {
                    Some(db) if db == self.database => {}
                    Some(db) => {
                        return Err(format!(
                            "pg_cron runs in database {db:?} (cron.database_name), not in {:?}",
                            self.database
                        ));
                    }
                    None => return Err("cron.database_name is not set".into()),
                }
            }
            Feature::Graph => {
                let loaded =
                    self.age_settings || listed(&self.session_preload, "age") || listed(&self.shared_preload, "age");
                if !loaded && !self.settings_visible {
                    return Err(format!(
                        "AGE does not appear loaded in ForgeBase's sessions, and role {:?} cannot read \
                         session_preload_libraries (grant it pg_read_all_settings)",
                        self.role
                    ));
                }
                if !loaded {
                    return Err(format!(
                        "AGE is not loaded in ForgeBase's sessions: set session_preload_libraries = \
                         '$libdir/plugins/age' for role {:?}",
                        self.role
                    ));
                }
            }
            Feature::Vector | Feature::Queue | Feature::Trigram => {}
        }
        Ok(())
    }
}

/// The outcome for one feature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Decision {
    /// Use the extension (create it when missing).
    Use,
    /// Portable, with the reason; `pin` records `portable` for a stateful
    /// feature that had no engine yet.
    Portable { reason: String, pin: bool },
    /// A stateful feature pinned to its portable engine in mode `auto`
    /// whose extension is usable: stays portable (`reason`), but the
    /// extension is created and its DDL applied, so `<feature>.convert` can
    /// move the data (FB-162).
    Prepare { reason: String },
    /// Mode `off`; `pin` as above.
    Off { pin: bool },
    /// Refuse the start.
    Refuse(String),
}

/// The decision for `f` in `mode`, given whether its extension is usable
/// and the engine its data is stored with.
pub(crate) fn decide(f: Feature, mode: ExtensionMode, usable: Result<(), String>, pin: Option<Pin>) -> Decision {
    let unpinned = f.stateful() && pin.is_none();
    let (ext, name) = (f.extension(), f.name());
    match (mode, pin) {
        (ExtensionMode::Off, Some(Pin::Extension)) => Decision::Refuse(format!(
            "{name} = \"off\", but this database keeps its {name} data in {ext}; run {name}.convert to the \
             portable engine first, or set {name} = \"auto\""
        )),
        (ExtensionMode::Off, _) => Decision::Off { pin: unpinned },
        (ExtensionMode::On, Some(Pin::Portable)) => Decision::Refuse(format!(
            "{name} = \"on\", but this database stores its {name} data on the portable engine; start with \
             {name} = \"auto\" and run {name}.convert"
        )),
        // Without a usable extension the reason is the same as at the first
        // start (FB-164): a restart reports the status it reported then.
        (ExtensionMode::Auto, Some(Pin::Portable)) => match usable {
            Ok(()) => Decision::Prepare {
                reason: format!(
                    "this database stores its {name} data on the portable engine; {name}.convert moves it to {ext}"
                ),
            },
            Err(why) => Decision::Portable { reason: why, pin: false },
        },
        (_, Some(Pin::Extension)) => match usable {
            Ok(()) => Decision::Use,
            Err(why) => Decision::Refuse(format!("this database keeps its {name} data in {ext}, but {why}")),
        },
        (ExtensionMode::On, None) => match usable {
            Ok(()) => Decision::Use,
            Err(why) => Decision::Refuse(format!("{name} = \"on\", but {why}")),
        },
        (ExtensionMode::Auto, None) => match usable {
            Ok(()) => Decision::Use,
            Err(reason) => Decision::Portable { reason, pin: unpinned },
        },
    }
}

fn status(
    f: Feature,
    mode: ExtensionMode,
    offer: &Offer,
    state: FeatureState,
    reason: Option<String>,
    schema_version: u32,
) -> FeatureStatus {
    let engine = match (state, &offer.installed) {
        (FeatureState::Active, Some(v)) => format!("{} {v}", f.label()),
        (FeatureState::Active, None) => f.label().to_owned(),
        _ => "portable".to_owned(),
    };
    FeatureStatus {
        feature: f,
        extension: f.extension(),
        mode,
        engine,
        state,
        available: offer.available.clone(),
        installed: offer.installed.clone(),
        schema: offer.schema.clone(),
        schema_version,
        reason,
    }
}

/// `ext.*` rows of `_forgebase_meta`: pins and applied DDL versions.
#[derive(Debug, Default)]
struct Recorded {
    pins: BTreeMap<Feature, Pin>,
    versions: BTreeMap<Feature, u32>,
}

impl Recorded {
    fn pin(&self, f: Feature) -> Option<Pin> {
        if f.stateful() { self.pins.get(&f).copied() } else { None }
    }

    fn version(&self, f: Feature) -> u32 {
        self.versions.get(&f).copied().unwrap_or(0)
    }
}

const NAMES: &str = "'vector', 'age', 'pgmq', 'pg_cron', 'pg_trgm'";

impl PgDatabase {
    async fn probe_extensions(&self) -> Result<Probe, DbError> {
        // Casts to text: the result types are all built in (no type lookups).
        let offers = self
            .read(&Statement::new(
                format!(
                    "SELECT coalesce(a.name, e.extname)::text, a.default_version::text, e.extversion::text, \
                     n.nspname::text \
                     FROM pg_available_extensions a \
                     FULL JOIN pg_extension e ON e.extname = a.name \
                     LEFT JOIN pg_namespace n ON n.oid = e.extnamespace \
                     WHERE coalesce(a.name, e.extname) IN ({NAMES})"
                ),
                vec![],
            ))
            .await?;
        let settings = self
            .read(&Statement::new(
                // pg_settings hides superuser-only rows instead of failing.
                "SELECT current_database()::text, \
                 (SELECT setting FROM pg_settings WHERE name = 'shared_preload_libraries'), \
                 (SELECT setting FROM pg_settings WHERE name = 'session_preload_libraries'), \
                 (SELECT setting FROM pg_settings WHERE name = 'cron.database_name'), \
                 current_user::text, \
                 pg_has_role(current_user, 'pg_read_all_settings', 'USAGE'), \
                 EXISTS (SELECT 1 FROM pg_settings WHERE name LIKE 'age.%')",
                vec![],
            ))
            .await?;
        let mut probe = Probe::default();
        for row in &offers.rows {
            let text = |i: usize| row.get(i).and_then(Value::as_str).map(str::to_owned);
            // FULL JOIN: an installed extension whose control file is gone
            // still has a row (installed, not available).
            let Some(name) = text(0) else {
                continue;
            };
            probe.offers.insert(
                name,
                Offer {
                    available: text(1),
                    installed: text(2),
                    schema: text(3),
                },
            );
        }
        if let Some(row) = settings.rows.first() {
            let text = |i: usize| row.get(i).and_then(Value::as_str).map(str::to_owned);
            probe.database = text(0).unwrap_or_default();
            probe.shared_preload = text(1).unwrap_or_default();
            probe.session_preload = text(2).unwrap_or_default();
            probe.cron_database = text(3).filter(|s| !s.is_empty());
            let flag = |i: usize| row.get(i).and_then(Value::as_i64) == Some(1);
            probe.settings_visible = flag(5);
            probe.age_settings = flag(6);
            probe.role = text(4).unwrap_or_default();
        }
        Ok(probe)
    }

    async fn recorded(&self) -> Result<Recorded, DbError> {
        let rows = self
            .read(&Statement::new(
                "SELECT key, value FROM _forgebase_meta WHERE key LIKE 'ext.%'",
                vec![],
            ))
            .await?;
        let mut out = Recorded::default();
        for row in &rows.rows {
            let (Some(key), Some(value)) = (row.first().and_then(Value::as_str), row.get(1).and_then(Value::as_str))
            else {
                continue;
            };
            let Some((name, what)) = key.strip_prefix("ext.").and_then(|k| k.split_once('.')) else {
                continue;
            };
            let Some(f) = Feature::from_name(name) else { continue };
            match what {
                "engine" => {
                    if let Some(p) = Pin::parse(value) {
                        out.pins.insert(f, p);
                    }
                }
                "schema" => {
                    if let Ok(v) = value.parse() {
                        out.versions.insert(f, v);
                    }
                }
                _ => {}
            }
        }
        Ok(out)
    }

    /// Runs `sql` on the administrator connection (opened on first use) or,
    /// without an administrator URL, on the writer.
    async fn run_admin(
        &self,
        admin_url: Option<&SecretString>,
        admin: &mut Option<sqlx::PgConnection>,
        sql: String,
    ) -> Result<(), String> {
        let Some(url) = admin_url else {
            return self.run_writer(sql).await;
        };
        if admin.is_none() {
            let opts = super::base_options(url).map_err(|e| e.to_string())?;
            let conn = sqlx::PgConnection::connect_with(&opts).await.map_err(|e| {
                format!(
                    "cannot log in as the administrator: {}",
                    super::error::classify(e, self.location())
                )
            })?;
            *admin = Some(conn);
        }
        let Some(conn) = admin.as_mut() else {
            return Err("no administrator connection".into());
        };
        sqlx::raw_sql(AssertSqlSafe(sql))
            .execute(&mut *conn)
            .await
            .map(|_| ())
            .map_err(|e| super::error::classify(e, self.location()).to_string())
    }

    /// Runs `sql` on the writer in one schema-change transaction.
    async fn run_writer(&self, sql: String) -> Result<(), String> {
        let run = async {
            let mut tx = self.begin(TxMode::SchemaChange).await?;
            tx.execute(&Statement::raw(sql)).await?;
            tx.commit().await
        };
        run.await.map_err(|e: DbError| e.to_string())
    }

    /// Grants on the extension schemas that are not ForgeBase's own.
    fn grants(f: Feature, server: &str, reader: Option<&str>) -> Option<String> {
        let q = |r: &str| super::PG.quote_ident(r);
        let (server, reader) = (q(server), reader.map(q));
        match f {
            Feature::Queue => {
                let mut sql = format!(
                    "GRANT USAGE, CREATE ON SCHEMA pgmq TO {server}; \
                     GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA pgmq TO {server}; \
                     GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA pgmq TO {server}; \
                     GRANT EXECUTE ON ALL FUNCTIONS IN SCHEMA pgmq TO {server};"
                );
                if let Some(reader) = reader {
                    sql.push_str(&format!(
                        " GRANT USAGE ON SCHEMA pgmq TO {reader}; \
                         GRANT SELECT ON ALL TABLES IN SCHEMA pgmq TO {reader}; \
                         ALTER DEFAULT PRIVILEGES FOR ROLE {server} IN SCHEMA pgmq GRANT SELECT ON TABLES TO {reader};"
                    ));
                }
                Some(sql)
            }
            // cron.schedule and friends are PUBLIC; the schema is not.
            Feature::Cron => Some(format!("GRANT USAGE ON SCHEMA cron TO {server};")),
            Feature::Graph => Some(match reader {
                Some(reader) => format!(
                    "GRANT USAGE ON SCHEMA ag_catalog TO {server}, {reader}; \
                     GRANT SELECT ON ALL TABLES IN SCHEMA ag_catalog TO {reader};"
                ),
                None => format!("GRANT USAGE ON SCHEMA ag_catalog TO {server};"),
            }),
            // In the forgebase schema, which both roles already use.
            Feature::Vector | Feature::Trigram => None,
        }
    }

    /// Applies the pending [`EXTENSION_DDL`] steps of `f`; returns the
    /// version reached.
    async fn apply_ddl(&self, f: Feature, from: u32, schema: &str) -> Result<u32, DbError> {
        let mut version = from;
        for step in ddl_steps(f).filter(|s| s.version > from) {
            let sql = step.sql.replace("{schema}", &super::PG.quote_ident(schema));
            let mut tx = self.begin(TxMode::SchemaChange).await?;
            tx.execute(&Statement::raw(sql)).await?;
            tx.execute(&meta_upsert(&f.schema_key(), &step.version.to_string()))
                .await?;
            tx.commit().await?;
            tracing::info!(feature = f.name(), version = step.version, "applied extension DDL");
            version = step.version;
        }
        Ok(version)
    }

    async fn write_pin(&self, f: Feature, pin: Pin) -> Result<(), DbError> {
        let mut tx = self.begin(TxMode::Normal).await?;
        tx.execute(&meta_upsert(&f.engine_key(), pin.as_str())).await?;
        tx.commit().await
    }

    /// See [`Database::setup_extensions`](crate::Database::setup_extensions).
    pub(crate) async fn setup_extensions(&self, setup: &ExtensionSetup) -> Result<ExtensionReport, ExtensionError> {
        let mut probe = self.probe_extensions().await?;
        let recorded = self.recorded().await?;
        let mut admin: Option<sqlx::PgConnection> = None;
        let mut problems = Vec::new();
        let mut decisions = Vec::new();
        let mut created = false;
        for f in Feature::ALL {
            let mode = f.mode(&setup.modes);
            let mut decision = decide(f, mode, probe.usable(f), recorded.pin(f));
            if matches!(decision, Decision::Use | Decision::Prepare { .. }) && probe.offer(f).installed.is_none() {
                let schema = f
                    .target_schema()
                    .map(|s| format!(" WITH SCHEMA {}", super::PG.quote_ident(s)))
                    .unwrap_or_default();
                let sql = format!(
                    "CREATE EXTENSION IF NOT EXISTS {}{schema}",
                    super::PG.quote_ident(f.extension())
                );
                let done = if f.needs_superuser() {
                    self.run_admin(setup.admin_url.as_ref(), &mut admin, sql).await
                } else {
                    self.run_writer(sql).await
                };
                match done {
                    Ok(()) => {
                        created = true;
                        tracing::info!(extension = f.extension(), "created PostgreSQL extension");
                    }
                    Err(e) => {
                        let why = format!("CREATE EXTENSION {} failed: {e}", f.extension());
                        decision = decide(f, mode, Err(why), recorded.pin(f));
                    }
                }
            }
            if let Decision::Refuse(why) = &decision {
                problems.push(why.clone());
            }
            decisions.push((f, mode, decision));
        }
        if created {
            self.bump_schema_generation();
            probe = self.probe_extensions().await?;
        }
        let mut features = Vec::with_capacity(decisions.len());
        for (f, mode, decision) in decisions {
            let offer = probe.offer(f);
            let version = recorded.version(f);
            let st = match decision {
                Decision::Use => {
                    if let Some(sql) = Self::grants(f, &probe.role, setup.reader_role.as_deref())
                        && let Err(e) = self.run_admin(setup.admin_url.as_ref(), &mut admin, sql).await
                    {
                        problems.push(format!("granting the {} schema failed: {e}", f.extension()));
                    }
                    let schema = offer.schema.clone().unwrap_or_else(|| EXTENSION_SCHEMA.to_owned());
                    let version = self.apply_ddl(f, version, &schema).await?;
                    if f.stateful() && recorded.pin(f).is_none() {
                        self.write_pin(f, Pin::Extension).await?;
                    }
                    status(f, mode, &offer, FeatureState::Active, None, version)
                }
                Decision::Prepare { reason } => {
                    if let Some(sql) = Self::grants(f, &probe.role, setup.reader_role.as_deref())
                        && let Err(e) = self.run_admin(setup.admin_url.as_ref(), &mut admin, sql).await
                    {
                        problems.push(format!("granting the {} schema failed: {e}", f.extension()));
                    }
                    let schema = offer.schema.clone().unwrap_or_else(|| EXTENSION_SCHEMA.to_owned());
                    let version = self.apply_ddl(f, version, &schema).await?;
                    status(f, mode, &offer, FeatureState::Portable, Some(reason), version)
                }
                Decision::Portable { reason, pin } => {
                    if pin {
                        self.write_pin(f, Pin::Portable).await?;
                    }
                    if mode == ExtensionMode::Auto {
                        tracing::info!(feature = f.name(), %reason, "feature runs on its portable engine");
                    }
                    status(f, mode, &offer, FeatureState::Portable, Some(reason), version)
                }
                Decision::Off { pin } => {
                    if pin {
                        self.write_pin(f, Pin::Portable).await?;
                    }
                    status(f, mode, &offer, FeatureState::Off, None, version)
                }
                Decision::Refuse(why) => status(f, mode, &offer, FeatureState::Portable, Some(why), version),
            };
            features.push(st);
        }
        if let Some(conn) = admin {
            let _ = conn.close().await;
        }
        if !problems.is_empty() {
            return Err(ExtensionError::Refused(problems));
        }
        let report = ExtensionReport { features };
        self.store_extensions(report.clone());
        Ok(report)
    }

    /// See [`Database::detect_extensions`](crate::Database::detect_extensions).
    pub(crate) async fn detect_extensions(&self, modes: &PostgresExtensions) -> Result<ExtensionReport, DbError> {
        let probe = self.probe_extensions().await?;
        let recorded = self.recorded().await?;
        let features = Feature::ALL
            .into_iter()
            .map(|f| {
                let mode = f.mode(modes);
                let offer = probe.offer(f);
                let version = recorded.version(f);
                match decide(f, mode, probe.usable(f), recorded.pin(f)) {
                    Decision::Use if offer.installed.is_some() => {
                        status(f, mode, &offer, FeatureState::Active, None, version)
                    }
                    Decision::Use => status(
                        f,
                        mode,
                        &offer,
                        FeatureState::Portable,
                        Some(format!(
                            "{} is not installed yet (the server installs it at start)",
                            f.extension()
                        )),
                        version,
                    ),
                    Decision::Portable { reason, .. } | Decision::Prepare { reason } | Decision::Refuse(reason) => {
                        status(f, mode, &offer, FeatureState::Portable, Some(reason), version)
                    }
                    Decision::Off { .. } => status(f, mode, &offer, FeatureState::Off, None, version),
                }
            })
            .collect();
        Ok(ExtensionReport { features })
    }
}

fn meta_upsert(key: &str, value: &str) -> Statement {
    Statement::new(
        "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3) \
         ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
        vec![key.into(), value.into(), forgebase_core::now_rfc3339().into()],
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const AUTO: ExtensionMode = ExtensionMode::Auto;
    const ON: ExtensionMode = ExtensionMode::On;
    const OFF: ExtensionMode = ExtensionMode::Off;

    fn missing() -> Result<(), String> {
        Err("missing".into())
    }

    #[test]
    fn names_follow_the_configuration() {
        let modes = PostgresExtensions::default();
        let listed: Vec<(&str, &str)> = modes.list().iter().map(|(f, e, _)| (*f, *e)).collect();
        let ours: Vec<(&str, &str)> = Feature::ALL.iter().map(|f| (f.name(), f.extension())).collect();
        assert_eq!(listed, ours);
        assert_eq!(Feature::from_name("queue"), Some(Feature::Queue));
        assert_eq!(Feature::Vector.schema_key(), "ext.vector.schema");
        assert_eq!(Feature::Queue.engine_key(), "ext.queue.engine");
        assert_eq!(NAMES.matches('\'').count(), 10);
        for f in Feature::ALL {
            assert!(NAMES.contains(&format!("'{}'", f.extension())));
        }
    }

    #[test]
    fn ddl_versions_count_up_from_one_per_feature() {
        for f in Feature::ALL {
            for (i, step) in ddl_steps(f).enumerate() {
                assert_eq!(step.version as usize, i + 1, "{f} step {i}");
            }
        }
    }

    #[test]
    fn auto_uses_what_is_there_and_falls_back() {
        assert_eq!(decide(Feature::Trigram, AUTO, Ok(()), None), Decision::Use);
        assert_eq!(
            decide(Feature::Trigram, AUTO, missing(), None),
            Decision::Portable {
                reason: "missing".into(),
                pin: false
            }
        );
        // A stateful feature records that its data is portable.
        assert_eq!(
            decide(Feature::Vector, AUTO, missing(), None),
            Decision::Portable {
                reason: "missing".into(),
                pin: true
            }
        );
    }

    #[test]
    fn on_refuses_without_the_extension() {
        assert_eq!(decide(Feature::Cron, ON, Ok(()), None), Decision::Use);
        assert!(
            matches!(decide(Feature::Cron, ON, missing(), None), Decision::Refuse(m) if m.contains("cron = \"on\""))
        );
    }

    #[test]
    fn off_is_portable_and_pins_stateful_features() {
        assert_eq!(decide(Feature::Queue, OFF, Ok(()), None), Decision::Off { pin: true });
        assert_eq!(decide(Feature::Graph, OFF, Ok(()), None), Decision::Off { pin: false });
    }

    #[test]
    fn stateful_features_never_switch_engine_on_their_own() {
        // Stored portably: an extension that appeared changes nothing in auto
        // (it is only prepared for <feature>.convert, FB-162).
        assert!(matches!(
            decide(Feature::Vector, AUTO, Ok(()), Some(Pin::Portable)),
            Decision::Prepare { reason } if reason.contains("vector.convert")
        ));
        assert!(matches!(
            decide(Feature::Vector, ON, Ok(()), Some(Pin::Portable)),
            Decision::Refuse(_)
        ));
        // Without the extension a restart gives the first start's reason.
        assert_eq!(
            decide(Feature::Queue, AUTO, missing(), Some(Pin::Portable)),
            Decision::Portable {
                reason: "missing".into(),
                pin: false
            }
        );
        assert_eq!(
            decide(Feature::Queue, AUTO, missing(), None),
            Decision::Portable {
                reason: "missing".into(),
                pin: true
            }
        );
        // Stored in the extension: it must stay usable, whatever the mode.
        assert_eq!(
            decide(Feature::Queue, AUTO, Ok(()), Some(Pin::Extension)),
            Decision::Use
        );
        assert!(matches!(
            decide(Feature::Queue, AUTO, missing(), Some(Pin::Extension)),
            Decision::Refuse(_)
        ));
        assert!(
            matches!(decide(Feature::Queue, OFF, Ok(()), Some(Pin::Extension)), Decision::Refuse(m) if m.contains("queue.convert"))
        );
    }

    #[test]
    fn probe_checks_preloads_and_the_cron_database() {
        let mut p = Probe {
            database: "forgebase".into(),
            role: "forgebase_server".into(),
            settings_visible: true,
            ..Probe::default()
        };
        assert!(p.usable(Feature::Vector).is_err());
        for name in ["vector", "pg_cron", "age"] {
            p.offers.insert(
                name.into(),
                Offer {
                    available: Some("1".into()),
                    ..Offer::default()
                },
            );
        }
        assert_eq!(p.usable(Feature::Vector), Ok(()));
        assert!(
            p.usable(Feature::Cron)
                .unwrap_err()
                .contains("shared_preload_libraries")
        );
        p.shared_preload = "pg_stat_statements, pg_cron".into();
        assert!(p.usable(Feature::Cron).unwrap_err().contains("cron.database_name"));
        p.cron_database = Some("postgres".into());
        assert!(p.usable(Feature::Cron).unwrap_err().contains("\"postgres\""));
        p.cron_database = Some("forgebase".into());
        assert_eq!(p.usable(Feature::Cron), Ok(()));
        assert!(p.usable(Feature::Graph).unwrap_err().contains("forgebase_server"));
        p.session_preload = "'$libdir/plugins/age'".into();
        assert_eq!(p.usable(Feature::Graph), Ok(()));

        // A role that cannot read the preload lists: pg_cron counts when
        // installed, AGE when one of its settings exists.
        let mut hidden = Probe {
            settings_visible: false,
            ..p.clone()
        };
        hidden.shared_preload.clear();
        hidden.session_preload.clear();
        assert!(
            hidden
                .usable(Feature::Cron)
                .unwrap_err()
                .contains("pg_read_all_settings")
        );
        assert!(
            hidden
                .usable(Feature::Graph)
                .unwrap_err()
                .contains("pg_read_all_settings")
        );
        hidden.offers.insert(
            "pg_cron".into(),
            Offer {
                installed: Some("1.6".into()),
                ..Offer::default()
            },
        );
        assert_eq!(hidden.usable(Feature::Cron), Ok(()));
        hidden.age_settings = true;
        assert_eq!(hidden.usable(Feature::Graph), Ok(()));
    }

    #[test]
    fn preload_lists_match_module_names() {
        assert!(listed("pg_stat_statements,pg_cron", "pg_cron"));
        assert!(listed("$libdir/plugins/age", "age"));
        assert!(listed("\"$libdir/plugins/age.so\"", "age"));
        assert!(!listed("pg_cronx", "pg_cron"));
        assert!(!listed("", "age"));
    }

    #[test]
    fn engine_text_names_the_extension_version() {
        let offer = Offer {
            available: Some("0.8.7".into()),
            installed: Some("0.8.7".into()),
            schema: Some("forgebase".into()),
        };
        let s = status(Feature::Vector, AUTO, &offer, FeatureState::Active, None, 1);
        assert_eq!(s.engine, "pgvector 0.8.7");
        let s = status(Feature::Vector, OFF, &offer, FeatureState::Off, None, 1);
        assert_eq!(s.engine, "portable");
        let json = serde_json::to_value(&s).unwrap();
        assert_eq!(json["state"], "off");
        assert_eq!(json["mode"], "off");
        assert_eq!(json["feature"], "vector");
    }
}

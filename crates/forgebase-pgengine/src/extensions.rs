//! The engine's part of the extension layer (ADR-0029 §1.1, §1.2, §1.5;
//! FB-161).
//!
//! * **pg_cron** is preloaded through `shared_preload_libraries` when the
//!   engine ships it ([`crate::Engine::prepare`], `cron` mode).
//! * **Apache AGE** is *not* preloaded cluster-wide: in AGE 1.8.0 that breaks
//!   `TRUNCATE`, `VACUUM FULL` and `CLUSTER` in databases without AGE
//!   (apache/age#2520). ForgeBase loads it per role instead,
//!   `ALTER ROLE forgebase_server SET session_preload_libraries =
//!   '$libdir/plugins/age'` (and the same for `forgebase_reader`), from
//!   `$libdir/plugins/` so the non-superuser roles may load it. The setting
//!   is reset when the engine has no AGE or `graph = "off"`: a role setting
//!   naming a missing library would make every login of that role fail.
//! * `forgebase_server` is granted `pg_read_all_settings`: the preload
//!   lists the extension setup checks are superuser-only settings.
//! * `search_path` keeps `forgebase, public`: the openCypher path sets
//!   `ag_catalog` per transaction (`SET LOCAL`, ADR-0029 §1.5).
//!
//! Creating the extensions, their grants and their DDL happen in the
//! `forgebase` database after the migrations (`forgebase_db`
//! `Database::setup_extensions`, as `forgebase_super`).

use sqlx::{AssertSqlSafe, Connection, PgConnection};

use crate::cluster;
use crate::config::{EngineConfig, ExtensionMode};
use crate::discover::EngineDir;
use crate::error::{EngineError, Result};
use crate::secret::{Role, RoleSecrets};

/// `session_preload_libraries` value of the server and reader roles when
/// the engine ships AGE.
pub const AGE_PRELOAD: &str = "$libdir/plugins/age";

impl EngineDir {
    /// Whether a module exists under `$libdir/plugins/` (where
    /// non-superusers may load it from).
    pub fn has_plugin(&self, name: &str) -> bool {
        self.pkglibdirs.iter().any(|d| {
            ["so", "dylib", "dll"]
                .iter()
                .any(|ext| d.join("plugins").join(format!("{name}.{ext}")).is_file())
        })
    }
}

/// Whether the roles preload AGE, per `graph` mode and the engine's files.
/// `warnings` gets a line when `auto` finds no AGE.
///
/// # Errors
/// `graph = "on"` with an engine that has no AGE plugin (before anything
/// starts).
pub(crate) fn age_preload(engine: &EngineDir, cfg: &EngineConfig, warnings: &mut Vec<String>) -> Result<bool> {
    let has_age = engine.has_plugin("age");
    match (cfg.graph, has_age) {
        (ExtensionMode::Off, _) => Ok(false),
        (_, true) => Ok(true),
        (ExtensionMode::On, false) => Err(EngineError::Config(format!(
            "extensions.graph = \"on\", but the engine in {} has no Apache AGE module ({AGE_PRELOAD})",
            engine.root.display()
        ))),
        (ExtensionMode::Auto, false) => {
            let w = "Apache AGE is not in this engine; the openCypher endpoint is unavailable".to_owned();
            tracing::info!(target: "forgebase::postgres", engine = %engine.root.display(), "{w}");
            warnings.push(w);
            Ok(false)
        }
    }
}

/// The role settings for `age`: set or reset `session_preload_libraries`
/// of the server and reader roles; `pg_read_all_settings` for the server.
pub(crate) fn role_settings_sql(age: bool) -> String {
    let (server, reader) = (Role::Server.name(), Role::Reader.name());
    // The server reads the preload lists to decide which extensions it can
    // use (superuser-only settings otherwise).
    let read_settings = format!("GRANT pg_read_all_settings TO {server}; ");
    let preload = if age {
        format!(
            "ALTER ROLE {server} SET session_preload_libraries = '{AGE_PRELOAD}'; \
             ALTER ROLE {reader} SET session_preload_libraries = '{AGE_PRELOAD}';"
        )
    } else {
        format!(
            "ALTER ROLE {server} RESET session_preload_libraries; \
             ALTER ROLE {reader} RESET session_preload_libraries;"
        )
    };
    read_settings + &preload
}

/// Applies [`role_settings_sql`] as `forgebase_super` (a superuser-only
/// parameter). Runs on every start of a primary, after the bootstrap; new
/// sessions pick it up.
pub(crate) async fn apply_role_settings(ep: &crate::process::Endpoint, secrets: &RoleSecrets, age: bool) -> Result<()> {
    let pw = secrets
        .password(Role::Super)
        .ok_or_else(|| EngineError::Config("roles.secret has no forgebase_super password".into()))?;
    let opts = cluster::connect_options(ep, Role::Super.name(), pw, cluster::DATABASE);
    let mut conn = PgConnection::connect_with(&opts)
        .await
        .map_err(|e| EngineError::Sql(format!("connect: {e}")))?;
    let done = sqlx::raw_sql(AssertSqlSafe(role_settings_sql(age)))
        .execute(&mut conn)
        .await
        .map(|_| ())
        .map_err(|e| EngineError::Sql(format!("role settings for Apache AGE: {e}")));
    let _ = conn.close().await;
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn age_is_preloaded_per_role_or_reset() {
        let on = role_settings_sql(true);
        assert!(on.contains("ALTER ROLE forgebase_server SET session_preload_libraries = '$libdir/plugins/age'"));
        assert!(on.contains("ALTER ROLE forgebase_reader SET session_preload_libraries"));
        assert!(on.starts_with("GRANT pg_read_all_settings TO forgebase_server;"));
        let off = role_settings_sql(false);
        assert!(off.contains("ALTER ROLE forgebase_server RESET session_preload_libraries"));
        assert!(off.contains("ALTER ROLE forgebase_reader RESET session_preload_libraries"));
    }
}

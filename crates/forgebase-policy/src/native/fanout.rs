//! Native policies and direct-login roles on every database set (ADR-0029
//! §3.10, FB-173).
//!
//! * **Rule changes** (`put_policy`, `delete_policy`) regenerate the
//!   collection's policies on control and on every shard as one two-phase
//!   transaction ([`PolicyService::commit_native`]): the same generated
//!   statements everywhere (`native::generate` runs once; relation helpers
//!   are named by a hash, so they are equal on every node).
//! * **Schema changes** regenerate inside the data core's two-phase schema
//!   transaction: the data-core hook runs in each shard's part.
//! * **Startup** ([`PolicyService::sync_native_rls`]) brings each set up to
//!   date on its own (each keeps its own fingerprint in its per-set
//!   `_forgebase_meta`): regeneration is idempotent, so no node waits for
//!   another.
//! * **Roles**: `forgebase_rls` and `forgebase_login` are created by
//!   migration 0040 on every node. A direct login (FB-153) is bound to one
//!   tenant: its role is created on the tenant's shard primary
//!   ([`create_login_role`]), and a move recreates it on the target with
//!   the same SCRAM verifier ([`copy_login_role`]).

use forgebase_db::twophase::{self, Participant, TwoPhase};
use forgebase_db::{Backend, Database, DbRouter, Statement, TxMode, Value, WriteTx};

use super::{FINGERPRINT_KEY, Generated, LOGIN_GROUP, apply};
use crate::{PolicyError, PolicyService};
use forgebase_schema::Collection;

impl PolicyService {
    /// The data core's router, when it has shards (FB-173).
    pub(crate) fn sharded_router(&self) -> Option<DbRouter> {
        self.inner
            .data
            .get()?
            .shard_router()
            .filter(|r| r.is_sharded())
            .cloned()
    }

    /// Commits `tx` (control), whose last statements regenerated native
    /// policies with `statements`. Once sharded, the same statements run
    /// on every shard and everything commits as one two-phase transaction;
    /// any failure rolls back everywhere.
    pub(crate) async fn commit_native(&self, tx: Box<dyn WriteTx>, statements: &[String]) -> Result<(), PolicyError> {
        let router = match self.sharded_router() {
            Some(r) if !statements.is_empty() => r,
            _ => {
                tx.commit().await?;
                return Ok(());
            }
        };
        let control = Participant::from(&router.control());
        let shards: Vec<Participant> = router
            .sets()
            .iter()
            .filter(|h| !h.is_control())
            .map(Participant::from)
            .collect();
        for p in std::iter::once(&control).chain(&shards) {
            if let Err(e) = twophase::check_supported(p).await {
                let _ = tx.rollback().await;
                return Err(forgebase_db::DbError::from(e).into());
            }
        }
        let mut two = TwoPhase::new(&control.db, "native policies");
        if let Err(e) = two.prepare(&control, tx).await {
            two.abort().await;
            return Err(e.into());
        }
        for p in &shards {
            let r = async {
                let mut stx = p.db.begin(TxMode::Normal).await?;
                if let Err(e) = apply(&mut *stx, statements).await {
                    let _ = stx.rollback().await;
                    return Err(e);
                }
                two.prepare(p, stx).await
            }
            .await;
            if let Err(e) = r {
                tracing::warn!(set = %p.set_id, error = %e, "native policies failed on a shard; rolled back everywhere");
                two.abort().await;
                return Err(e.into());
            }
        }
        two.commit()
            .await
            .map(|_| ())
            .map_err(|e| forgebase_db::DbError::from(e).into())
    }

    /// Brings one set's native policies to `generated` unless its
    /// fingerprint is `print` already.
    pub(crate) async fn sync_native_on(
        &self,
        node: &dyn Backend,
        generated: &[(Collection, Generated)],
        print: &str,
    ) -> Result<bool, PolicyError> {
        let current = node
            .fetch_all(&Statement::new(
                "SELECT value FROM _forgebase_meta WHERE key = ?1",
                vec![FINGERPRINT_KEY.into()],
            ))
            .await?;
        if current.scalar().and_then(|v| v.as_str()) == Some(print) {
            return Ok(false);
        }
        let mut tx = node.begin(TxMode::Normal).await?;
        let result = async {
            for (_, g) in generated {
                apply(&mut *tx, &g.statements).await?;
            }
            tx.execute(&Statement::new(
                "INSERT INTO _forgebase_meta (key, value, updated_at) VALUES (?1, ?2, ?3)
                 ON CONFLICT (key) DO UPDATE SET value = excluded.value, updated_at = excluded.updated_at",
                vec![
                    FINGERPRINT_KEY.into(),
                    print.to_owned().into(),
                    forgebase_core::now_rfc3339().into(),
                ],
            ))
            .await?;
            Ok::<(), forgebase_db::DbError>(())
        }
        .await;
        match result {
            Ok(()) => tx.commit().await?,
            Err(e) => {
                let _ = tx.rollback().await;
                return Err(e.into());
            }
        }
        Ok(true)
    }

    /// Every shard's primary (not control), when sharded.
    pub(crate) fn shard_databases(&self) -> Vec<(String, Database)> {
        self.sharded_router()
            .map(|r| {
                r.sets()
                    .into_iter()
                    .filter(|h| !h.is_control())
                    .map(|h| (h.id().to_owned(), h.database().clone()))
                    .collect()
            })
            .unwrap_or_default()
    }
}

/// A direct database login (FB-153): a PostgreSQL role on its tenant's
/// shard primary, member of [`LOGIN_GROUP`].
#[derive(Clone, PartialEq, Eq)]
pub struct LoginRole<'a> {
    /// Role name: lower-case letters, digits and `_`, starting with a
    /// letter, at most 63 bytes, not `pg_*` or `forgebase*`.
    pub name: &'a str,
    /// SCRAM-SHA-256 verifier (`SCRAM-SHA-256$<iter>:<salt>$<stored>:<server>`),
    /// stored as the role's password as is: the password never reaches
    /// ForgeBase or the shard in clear text.
    pub verifier: &'a str,
}

impl std::fmt::Debug for LoginRole<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LoginRole").field("name", &self.name).finish_non_exhaustive()
    }
}

fn check_role_name(name: &str) -> Result<(), PolicyError> {
    let ok = !name.is_empty()
        && name.len() <= 63
        && name.as_bytes()[0].is_ascii_lowercase()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && !name.starts_with("pg_")
        && !name.starts_with("forgebase");
    if ok {
        Ok(())
    } else {
        Err(PolicyError::invalid(
            "invalid_login_name",
            "a login name is 1-63 lower-case letters, digits or _, starting with a letter (not pg_ or forgebase)",
        ))
    }
}

fn check_verifier(v: &str) -> Result<(), PolicyError> {
    let ok = v.starts_with("SCRAM-SHA-256$")
        && v.len() <= 512
        && v.bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b':' | b'$' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(PolicyError::invalid(
            "invalid_login_verifier",
            "the password must be given as a SCRAM-SHA-256 verifier",
        ))
    }
}

/// Creates (or updates the verifier of) a direct login's role on `db`, the
/// primary of the login's tenant's shard: `LOGIN`, no other attributes,
/// member of [`LOGIN_GROUP`]. Needs `CREATEROLE` (the server role on
/// managed nodes).
pub async fn create_login_role(db: &Database, role: &LoginRole<'_>) -> Result<(), PolicyError> {
    check_role_name(role.name)?;
    check_verifier(role.verifier)?;
    let exists = !db
        .fetch_all(&Statement::new(
            "SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = ?1",
            vec![role.name.into()],
        ))
        .await?
        .is_empty();
    // Names and verifiers are checked above: plain characters, no quotes.
    let statements = if exists {
        vec![format!("ALTER ROLE \"{}\" LOGIN PASSWORD '{}'", role.name, role.verifier)]
    } else {
        vec![
            format!(
                "CREATE ROLE \"{}\" LOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS \
                 PASSWORD '{}'",
                role.name, role.verifier
            ),
            format!("GRANT {LOGIN_GROUP} TO \"{}\"", role.name),
        ]
    };
    let mut tx = db.begin(TxMode::Normal).await?;
    for s in &statements {
        if let Err(e) = tx.execute(&Statement::raw(s.clone())).await {
            let _ = tx.rollback().await;
            return Err(e.into());
        }
    }
    tx.commit().await?;
    Ok(())
}

/// Drops a direct login's role from `db` (`Ok(false)` if it is not there).
pub async fn drop_login_role(db: &Database, name: &str) -> Result<bool, PolicyError> {
    check_role_name(name)?;
    let exists = !db
        .fetch_all(&Statement::new(
            "SELECT 1 FROM pg_catalog.pg_roles WHERE rolname = ?1",
            vec![name.into()],
        ))
        .await?
        .is_empty();
    if !exists {
        return Ok(false);
    }
    let mut tx = db.begin(TxMode::Normal).await?;
    if let Err(e) = tx.execute(&Statement::raw(format!("DROP ROLE \"{name}\""))).await {
        let _ = tx.rollback().await;
        return Err(e.into());
    }
    tx.commit().await?;
    Ok(true)
}

/// The SCRAM verifier of role `name` on `db` (`pg_authid`: needs a
/// superuser; `None` when the role has none or cannot be read).
pub async fn login_role_verifier(db: &Database, name: &str) -> Result<Option<String>, PolicyError> {
    check_role_name(name)?;
    let rows = db
        .fetch_all(&Statement::new(
            "SELECT rolpassword FROM pg_catalog.pg_authid WHERE rolname = ?1",
            vec![name.into()],
        ))
        .await;
    match rows {
        Ok(r) => Ok(r.scalar().and_then(Value::as_str).map(str::to_owned)),
        Err(forgebase_db::DbError::Statement(_)) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Recreates role `name` from `from` on `to` with the same verifier (a
/// tenant move, ADR-0029 §3.10). `verifier` is used when given (FB-153
/// keeps it in `_forgebase_db_logins`), else it is read from `from`'s
/// `pg_authid`. Returns whether a role was written.
pub async fn copy_login_role(
    from: &Database,
    to: &Database,
    name: &str,
    verifier: Option<&str>,
) -> Result<bool, PolicyError> {
    let verifier = match verifier {
        Some(v) => Some(v.to_owned()),
        None => login_role_verifier(from, name).await?,
    };
    let Some(verifier) = verifier else {
        return Ok(false);
    };
    create_login_role(
        to,
        &LoginRole {
            name,
            verifier: &verifier,
        },
    )
    .await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn login_names_and_verifiers_are_checked() {
        assert!(check_role_name("app_reader").is_ok());
        for bad in ["", "1abc", "Upper", "pg_x", "forgebase_rls", "a\"b", "a b", &"a".repeat(64)] {
            assert!(check_role_name(bad).is_err(), "{bad}");
        }
        assert!(check_verifier("SCRAM-SHA-256$4096:c2FsdA==$c3RvcmVk:c2VydmVy").is_ok());
        for bad in ["md5abc", "SCRAM-SHA-256$4096:x'y", "plain"] {
            assert!(check_verifier(bad).is_err(), "{bad}");
        }
    }
}

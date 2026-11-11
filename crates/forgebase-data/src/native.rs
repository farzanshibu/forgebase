//! Hooks for native PostgreSQL row-level security (FB-152,
//! rls-coverage §3.4). `forgebase-policy` implements them; the data core
//! calls them inside its schema transactions and, in `enforce` mode, runs
//! the statements of non-privileged callers as the RLS role
//! ([`crate::Authorizer::native_session`]).

use std::fmt;

use forgebase_db::{BoxFuture, WriteTx};
use forgebase_schema::Collection;

use crate::DataError;

/// Generates a collection's native policies in the transaction that changes
/// its schema, so the table and its policies never disagree.
pub trait NativePolicies: Send + Sync + fmt::Debug {
    /// Before the DDL of a schema change of `coll` (and before it is
    /// dropped): removes native objects that would block the DDL (the `api`
    /// view reads the table, and PostgreSQL refuses to drop or retype what a
    /// view reads).
    fn before_change<'a>(
        &'a self,
        tx: &'a mut dyn WriteTx,
        coll: &'a Collection,
    ) -> BoxFuture<'a, Result<(), DataError>>;

    /// After the DDL, in the same transaction: (re)generates the policies,
    /// grants and `api` view of `coll` and of the collections whose rules
    /// read it. `catalog` is every collection after the change.
    fn after_change<'a>(
        &'a self,
        tx: &'a mut dyn WriteTx,
        coll: &'a Collection,
        catalog: &'a [Collection],
    ) -> BoxFuture<'a, Result<(), DataError>>;
}

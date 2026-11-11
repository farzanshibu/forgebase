//! `LISTEN` on a dedicated connection (FB-165): how the server learns that
//! pg_cron's `forgebase.fire_schedule()` inserted a job without waiting for
//! the next poll. The notifications themselves carry only a hint (a payload
//! string); the receiver reads the tables.

use sqlx::postgres::PgListener;

use super::{PgDatabase, error};
use crate::DbError;

/// A dedicated connection listening on one channel. sqlx re-connects and
/// re-listens by itself after a lost connection; notifications sent while it
/// was away are lost, so receivers treat a notification as "look again".
pub struct Listener {
    inner: PgListener,
    location: String,
}

impl std::fmt::Debug for Listener {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Listener").field("location", &self.location).finish()
    }
}

impl Listener {
    /// Waits for the next notification and returns its payload.
    ///
    /// # Errors
    /// The connection failed and could not be re-established.
    pub async fn recv(&mut self) -> Result<String, DbError> {
        let n = self
            .inner
            .recv()
            .await
            .map_err(|e| error::classify(e, &self.location))?;
        Ok(n.payload().to_owned())
    }
}

impl PgDatabase {
    pub(crate) async fn listen(&self, channel: &str) -> Result<Listener, DbError> {
        let location = self.inner.location.clone();
        let mut inner = PgListener::connect_with(&self.inner.reader.primary)
            .await
            .map_err(|e| error::classify(e, &location))?;
        inner.listen(channel).await.map_err(|e| error::classify(e, &location))?;
        Ok(Listener { inner, location })
    }
}

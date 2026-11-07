//! In-memory snapshot of collection definitions.

use std::collections::HashMap;

use forgebase_db::{Backend, Statement, Value, WriteTx};
use forgebase_schema::Collection;

use crate::DataError;

/// Collection definitions by id, with a case-insensitive name index.
#[derive(Debug, Default, Clone)]
pub(crate) struct CatalogSnapshot {
    by_id: HashMap<String, Collection>,
    by_name: HashMap<String, String>,
}

const SELECT_DEFINITIONS: &str = "SELECT definition FROM _forgebase_collections ORDER BY name";

impl CatalogSnapshot {
    pub(crate) async fn load(backend: &dyn Backend) -> Result<Self, DataError> {
        let rows = backend.fetch_all(&Statement::new(SELECT_DEFINITIONS, vec![])).await?;
        Self::from_rows(rows.rows)
    }

    /// Loads the catalog as seen inside a write transaction.
    pub(crate) async fn load_tx(tx: &mut dyn WriteTx) -> Result<Self, DataError> {
        let rows = tx.fetch_all(&Statement::new(SELECT_DEFINITIONS, vec![])).await?;
        Self::from_rows(rows.rows)
    }

    fn from_rows(rows: Vec<Vec<Value>>) -> Result<Self, DataError> {
        let mut s = Self::default();
        for row in rows {
            let text = row.first().and_then(Value::as_str).unwrap_or("{}");
            let c: Collection = serde_json::from_str(text)
                .map_err(|e| DataError::Internal(format!("collection definition does not decode: {e}")))?;
            s.insert(c);
        }
        Ok(s)
    }

    pub(crate) fn insert(&mut self, c: Collection) {
        self.by_name.insert(c.name.to_ascii_lowercase(), c.id.clone());
        self.by_id.insert(c.id.clone(), c);
    }

    /// By id, or by name (case-insensitive).
    pub(crate) fn get(&self, name_or_id: &str) -> Option<&Collection> {
        self.by_id.get(name_or_id).or_else(|| {
            self.by_name
                .get(&name_or_id.to_ascii_lowercase())
                .and_then(|id| self.by_id.get(id))
        })
    }

    /// All collections, ordered by name.
    pub(crate) fn all(&self) -> Vec<&Collection> {
        let mut v: Vec<&Collection> = self.by_id.values().collect();
        v.sort_by_key(|c| c.name.to_ascii_lowercase());
        v
    }
}

impl forgebase_query::Catalog for CatalogSnapshot {
    fn collection_by_id(&self, id: &str) -> Option<&Collection> {
        self.by_id.get(id)
    }
}

//! Collection tables and their indexes on the target, made by the data
//! core itself (ADR-0028 §6.4 step 5): the same DDL a collection created on
//! that engine gets, never translated SQL.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use forgebase_data::{AccessContext, DataService, DefaultAuthorizer, IndexDefinition};
use forgebase_db::{Backend, Database, Statement};
use forgebase_schema::{Collection, FieldKind};

use crate::ConvertError;

/// The source's catalog: collections and the managed (manual) indexes.
#[derive(Debug, Clone, Default)]
pub(crate) struct SourceCatalog {
    pub collections: Vec<Collection>,
    pub indexes: Vec<IndexDefinition>,
    /// Automatic index names per collection id.
    pub auto_indexes: BTreeMap<String, BTreeSet<String>>,
}

pub(crate) async fn load(db: &Database) -> Result<SourceCatalog, ConvertError> {
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT definition FROM _forgebase_collections ORDER BY created_at, id",
        ))
        .await?;
    let mut collections = Vec::with_capacity(rows.len());
    for r in &rows.rows {
        let text = r.first().and_then(|v| v.as_str()).unwrap_or("{}");
        collections.push(
            serde_json::from_str::<Collection>(text)
                .map_err(|e| ConvertError::Precondition(format!("a collection definition does not read: {e}")))?,
        );
    }
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT definition FROM _forgebase_indexes WHERE origin = 'manual' ORDER BY name",
        ))
        .await?;
    let mut indexes = Vec::with_capacity(rows.len());
    for r in &rows.rows {
        let text = r.first().and_then(|v| v.as_str()).unwrap_or("{}");
        indexes.push(
            serde_json::from_str::<IndexDefinition>(text)
                .map_err(|e| ConvertError::Precondition(format!("an index definition does not read: {e}")))?,
        );
    }
    Ok(SourceCatalog {
        collections,
        indexes,
        auto_indexes: auto_indexes(db).await?,
    })
}

pub(crate) async fn auto_indexes(db: &Database) -> Result<BTreeMap<String, BTreeSet<String>>, ConvertError> {
    let rows = db
        .fetch_all(&Statement::raw(
            "SELECT collection_id, name FROM _forgebase_indexes WHERE origin = 'auto'",
        ))
        .await?;
    let mut out: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for r in &rows.rows {
        if let (Some(c), Some(n)) = (r[0].as_str(), r[1].as_str()) {
            out.entry(c.to_owned()).or_default().insert(n.to_owned());
        }
    }
    Ok(out)
}

/// What [`build`] made.
#[derive(Debug, Default)]
pub(crate) struct Built {
    pub collections: usize,
    pub indexes: usize,
    /// Relation fields added after their target existed (relation cycles).
    pub deferred_fields: usize,
}

fn relation_targets(c: &Collection) -> BTreeSet<String> {
    c.fields
        .iter()
        .filter_map(|f| match &f.kind {
            FieldKind::Relation { collection_id, .. } if collection_id != &c.id => Some(collection_id.clone()),
            _ => None,
        })
        .collect()
}

/// Creates every collection of `cat` on `target` (relation targets first;
/// a relation cycle is created without the closing fields, which are added
/// once their target exists; views last), then the managed indexes.
pub(crate) async fn build(target: &Database, cat: &SourceCatalog) -> Result<Built, ConvertError> {
    let data = DataService::new(Arc::new(target.clone()), Arc::new(DefaultAuthorizer)).await?;
    let ctx = AccessContext::system();
    let mut built = Built::default();
    let ids: BTreeSet<String> = cat.collections.iter().map(|c| c.id.clone()).collect();
    let mut created: BTreeSet<String> = BTreeSet::new();
    let mut pending: Vec<&Collection> = cat.collections.iter().filter(|c| !c.is_view()).collect();
    let mut later: Vec<&Collection> = Vec::new();
    let fail = |c: &Collection, e: forgebase_data::DataError| {
        ConvertError::Schema(format!("collection {} cannot be created on the target: {e}", c.name))
    };
    while !pending.is_empty() {
        let ready = pending.iter().position(|c| {
            relation_targets(c)
                .iter()
                .all(|t| created.contains(t) || !ids.contains(t))
        });
        let (c, full) = match ready {
            Some(i) => (pending.remove(i), true),
            None => (pending.remove(0), false),
        };
        let mut def = c.clone();
        if !full {
            def.fields.retain(|f| match &f.kind {
                FieldKind::Relation { collection_id, .. } => {
                    collection_id == &c.id || created.contains(collection_id) || !ids.contains(collection_id)
                }
                _ => true,
            });
            built.deferred_fields += c.fields.len() - def.fields.len();
            later.push(c);
        }
        data.create_collection(&ctx, def).await.map_err(|e| fail(c, e))?;
        created.insert(c.id.clone());
        built.collections += 1;
    }
    for c in later {
        data.update_collection(&ctx, &c.id, c.clone())
            .await
            .map_err(|e| fail(c, e))?;
    }
    for c in cat.collections.iter().filter(|c| c.is_view()) {
        data.create_collection(&ctx, c.clone()).await.map_err(|e| {
            ConvertError::Schema(format!(
                "view collection {} cannot be created on the target (its query may use SQL the other engine \
                 does not have; change the query and run the conversion again): {e}",
                c.name
            ))
        })?;
        built.collections += 1;
    }
    for ix in &cat.indexes {
        data.create_index(&ctx, ix.clone())
            .await
            .map_err(|e| ConvertError::Schema(format!("index {} cannot be created on the target: {e}", ix.name)))?;
        built.indexes += 1;
    }
    // Automatic index names are derived (and fitted to 63 bytes on
    // PostgreSQL): a name the target derives differently would leave the
    // catalog naming an index that does not exist.
    let made = auto_indexes(target).await?;
    let mut problems = Vec::new();
    for (coll, names) in &cat.auto_indexes {
        let theirs = made.get(coll).cloned().unwrap_or_default();
        if &theirs != names {
            problems.push(format!(
                "collection {coll}: automatic index names differ ({:?} on the source, {:?} on the target)",
                names, theirs
            ));
        }
    }
    if !problems.is_empty() {
        return Err(ConvertError::Refused(problems));
    }
    Ok(built)
}

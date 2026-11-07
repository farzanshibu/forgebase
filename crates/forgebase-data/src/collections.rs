//! Collections: create, alter and drop as transactional DDL with a schema
//! history (ADR-0003 §4).
//!
//! Every change runs in one write transaction together with its catalog and
//! history rows. Table rebuilds (type changes, dropped fields) suspend
//! foreign-key enforcement for the transaction and check the whole database
//! before commit (SQLite's documented procedure). Any failure — bad data for a
//! new type, a duplicate under a new unique field, a view that would break —
//! rolls everything back: table, catalog and history stay as they were.

use std::collections::{BTreeMap, HashMap};

use forgebase_db::{Statement, TxMode, Value, WriteTx};
use forgebase_query::{StatementKind, analyze_statement, split_statements};
use forgebase_schema::{Collection, CollectionType, Field, FieldKind, generate_id, is_valid_id, now_timestamp};
use serde::Serialize;
use serde_json::Value as Json;

use crate::catalog::CatalogSnapshot;
use crate::ddl::{
    DdlLog, SYSTEM_COLUMNS, TMP_PREFIX, column_signature, column_sql, create_table_sql, object_exists, record_history,
    relation_table, verify_collections,
};
use crate::indexes::{
    IndexDefinition, IndexKind, auto_indexes, create_index_sql, drop_index_sql, fields_used, fts_trigger_sql,
    insert_index_row, managed_indexes, rename_fields,
};
use crate::{AccessContext, Action, DataError, DataService, finish};

/// One applied schema change.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SchemaChange {
    /// Sequence number.
    pub id: i64,
    /// Collection id (none for SQL-editor DDL).
    pub collection_id: Option<String>,
    /// Collection name at the time.
    pub collection: Option<String>,
    /// Collection schema version after the change.
    pub version: Option<i64>,
    /// `create`, `alter`, `drop`, `sql`, `index_create`, `index_drop`.
    pub action: String,
    /// DDL that ran.
    pub statements: Vec<String>,
    /// Definition before.
    pub before: Option<Json>,
    /// Definition after.
    pub after: Option<Json>,
    /// Who.
    pub actor: String,
    /// When.
    pub applied_at: String,
}

impl DataService {
    /// All collections the caller may see the schema of.
    pub async fn list_collections(&self, ctx: &AccessContext) -> Result<Vec<Collection>, DataError> {
        self.authorize(ctx, Action::ReadSchema, None)?;
        Ok(self.catalog().all().into_iter().cloned().collect())
    }

    /// One collection by name or id.
    pub async fn get_collection(&self, ctx: &AccessContext, name_or_id: &str) -> Result<Collection, DataError> {
        let c = self.collection(name_or_id)?;
        self.authorize(ctx, Action::ReadSchema, Some(&c))?;
        Ok(c)
    }

    /// Schema history, newest last (optionally for one collection).
    pub async fn schema_history(
        &self,
        ctx: &AccessContext,
        collection: Option<&str>,
    ) -> Result<Vec<SchemaChange>, DataError> {
        let id = match collection {
            Some(c) => {
                let c = self.collection(c)?;
                self.authorize(ctx, Action::ReadSchema, Some(&c))?;
                Some(c.id)
            }
            None => {
                self.authorize(ctx, Action::ReadSchema, None)?;
                None
            }
        };
        let rows = self
            .inner
            .backend
            .fetch_all(&Statement::new(
                "SELECT id, collection_id, collection, version, action, statements, before, after, actor, applied_at
                 FROM _forgebase_schema_history WHERE ?1 IS NULL OR collection_id = ?1 ORDER BY id",
                vec![id.into()],
            ))
            .await?;
        let text = |v: &Value| v.as_str().map(str::to_owned);
        let json = |v: &Value| v.as_str().and_then(|s| serde_json::from_str(s).ok());
        Ok(rows
            .rows
            .iter()
            .map(|r| SchemaChange {
                id: r[0].as_i64().unwrap_or_default(),
                collection_id: text(&r[1]),
                collection: text(&r[2]),
                version: r[3].as_i64(),
                action: text(&r[4]).unwrap_or_default(),
                statements: r[5]
                    .as_str()
                    .and_then(|s| serde_json::from_str(s).ok())
                    .unwrap_or_default(),
                before: json(&r[6]),
                after: json(&r[7]),
                actor: text(&r[8]).unwrap_or_default(),
                applied_at: text(&r[9]).unwrap_or_default(),
            })
            .collect())
    }

    /// Creates a collection: its table (or view), automatic indexes, catalog
    /// row and history row, atomically.
    pub async fn create_collection(&self, ctx: &AccessContext, mut def: Collection) -> Result<Collection, DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        self.authorize(ctx, Action::ManageSchema, Some(&def))?;
        if (def.system || def.fields.iter().any(|f| f.system)) && !ctx.is_system() {
            return Err(DataError::Forbidden(
                "only ForgeBase creates system collections and fields".into(),
            ));
        }
        if def.id.is_empty() {
            def.id = generate_id();
        } else if !is_valid_id(&def.id) {
            return Err(DataError::invalid(
                "invalid_id",
                "collection id must be 1-64 characters of [A-Za-z0-9_-]",
            ));
        }
        let now = now_timestamp();
        def.version = 1;
        def.created = now.clone();
        def.updated = now;
        def.ensure_auth_fields();
        for f in &mut def.fields {
            if f.id.is_empty() {
                f.id = generate_id();
            }
        }
        def.validate()?;
        let view_sql = match def.kind {
            CollectionType::View => Some(
                self.checked_view_query(ctx, def.view_query.as_deref().unwrap_or(""))
                    .await?,
            ),
            _ => None,
        };

        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            let mut cat = CatalogSnapshot::load_tx(&mut *tx).await?;
            if cat.get(&def.id).is_some() {
                return Err(DataError::AlreadyExists(format!("collection id {}", def.id)));
            }
            if cat.get(&def.name).is_some() || object_exists(&mut *tx, &def.name).await? {
                return Err(DataError::AlreadyExists(format!("collection or table {}", def.name)));
            }
            let mut log = DdlLog::new(&mut *tx);
            if let Some(q) = &view_sql {
                log.run(format!("CREATE VIEW {} AS {q}", d.quote_ident(&def.name))).await?;
                def.fields = infer_view_fields(log.tx, d, &def.name).await?;
            } else {
                log.run(create_table_sql(d, &def.name, &def, &cat)?).await?;
                for ix in auto_indexes(&def) {
                    for sql in create_index_sql(d, &cat, &def, &ix)? {
                        log.run(sql).await?;
                    }
                }
            }
            log.exec(
                "INSERT INTO _forgebase_collections (id, name, type, system, definition, version, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                vec![
                    def.id.clone().into(),
                    def.name.clone().into(),
                    def.kind.as_str().into(),
                    def.system.into(),
                    serde_json::to_string(&def)?.into(),
                    def.version.into(),
                    def.created.clone().into(),
                    def.updated.clone().into(),
                ],
            )
            .await?;
            for ix in auto_indexes(&def) {
                insert_index_row(&mut log, &def, &ix, "auto").await?;
            }
            let statements = std::mem::take(&mut log.statements);
            record_history(&mut *tx, Some(&def), "create", &statements, None, Some(&def), &ctx.actor()).await?;
            cat.insert(def.clone());
            Ok(def.clone())
        }
        .await;
        let created = finish(tx, result).await?;
        self.reload_locked().await?;
        Ok(created)
    }

    /// Replaces a collection's definition (rename, add / rename / change /
    /// drop fields, change options), applying the needed DDL atomically.
    /// Fields are matched by `id`; a field without an id is new.
    pub async fn update_collection(
        &self,
        ctx: &AccessContext,
        name_or_id: &str,
        mut def: Collection,
    ) -> Result<Collection, DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        let d = self.inner.backend.dialect();
        let current = self.collection(name_or_id)?;
        self.authorize(ctx, Action::ManageSchema, Some(&current))?;
        if current.system && !ctx.is_system() {
            return Err(DataError::Forbidden(format!("{} is a system collection", current.name)));
        }
        if def.kind != current.kind {
            return Err(DataError::invalid(
                "type_change",
                "the type of a collection cannot change",
            ));
        }
        def.id = current.id.clone();
        def.system = current.system;
        def.created = current.created.clone();
        def.version = current.version + 1;
        def.updated = now_timestamp();
        for f in &mut def.fields {
            if f.id.is_empty() {
                f.id = generate_id();
            }
        }
        if !ctx.is_system() {
            for old in current.fields.iter().filter(|f| f.system) {
                match def.field_by_id(&old.id) {
                    Some(new) if new.name == old.name && new.kind == old.kind && new.system => {}
                    _ => {
                        return Err(DataError::Forbidden(format!(
                            "system field {} cannot be changed or removed",
                            old.name
                        )));
                    }
                }
            }
            if def
                .fields
                .iter()
                .any(|f| f.system && current.field_by_id(&f.id).is_none_or(|o| !o.system))
            {
                return Err(DataError::Forbidden(
                    "only ForgeBase marks fields as system fields".into(),
                ));
            }
        }
        def.ensure_auth_fields();
        if def.is_view() {
            def.fields.clear();
        }
        def.validate()?;
        let view_sql = match def.kind {
            CollectionType::View => Some(
                self.checked_view_query(ctx, def.view_query.as_deref().unwrap_or(""))
                    .await?,
            ),
            _ => None,
        };

        let mut tx = self.begin(TxMode::SchemaChange).await?;
        let result = async {
            let cat = CatalogSnapshot::load_tx(&mut *tx).await?;
            let old = cat
                .get(&current.id)
                .cloned()
                .ok_or_else(|| DataError::CollectionNotFound(name_or_id.to_owned()))?;
            let renamed = !old.name.eq(&def.name);
            if renamed
                && (cat.get(&def.name).is_some_and(|c| c.id != old.id)
                    || (!old.name.eq_ignore_ascii_case(&def.name) && object_exists(&mut *tx, &def.name).await?))
            {
                return Err(DataError::AlreadyExists(format!("collection or table {}", def.name)));
            }
            let mut log = DdlLog::new(&mut *tx);
            if let Some(q) = &view_sql {
                log.run(format!("DROP VIEW {}", d.quote_ident(&old.name))).await?;
                log.run(format!("CREATE VIEW {} AS {q}", d.quote_ident(&def.name))).await?;
                def.fields = infer_view_fields(log.tx, d, &def.name).await?;
            } else {
                self.alter_table(&mut log, &cat, &old, &def).await?;
            }
            log.exec(
                "UPDATE _forgebase_collections SET name = ?2, definition = ?3, version = ?4, updated_at = ?5 WHERE id = ?1",
                vec![
                    def.id.clone().into(),
                    def.name.clone().into(),
                    serde_json::to_string(&def)?.into(),
                    def.version.into(),
                    def.updated.clone().into(),
                ],
            )
            .await?;
            let statements = std::mem::take(&mut log.statements);
            record_history(&mut *tx, Some(&def), "alter", &statements, Some(&old), Some(&def), &ctx.actor()).await?;
            let mut after = cat.clone();
            after.insert(def.clone());
            verify_collections(d, &mut *tx, &after).await?;
            Ok(def.clone())
        }
        .await;
        let updated = finish(tx, result).await?;
        self.reload_locked().await?;
        Ok(updated)
    }

    /// Applies the table changes between `old` and `new`.
    async fn alter_table(
        &self,
        log: &mut DdlLog<'_>,
        cat: &CatalogSnapshot,
        old: &Collection,
        new: &Collection,
    ) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        let q = |s: &str| d.quote_ident(s);
        let mut table = old.name.clone();
        if old.name != new.name {
            log.run(format!("ALTER TABLE {} RENAME TO {}", q(&old.name), q(&new.name)))
                .await?;
            table = new.name.clone();
        }

        // Field diff by id.
        let old_by_id: HashMap<&str, &Field> = old.fields.iter().map(|f| (f.id.as_str(), f)).collect();
        let dropped: Vec<&Field> = old.fields.iter().filter(|f| new.field_by_id(&f.id).is_none()).collect();
        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        let mut rebuild = !dropped.is_empty();
        let mut added: Vec<&Field> = Vec::new();
        // Renamed relation targets keep their table names; compare signatures
        // with the *new* names so a rename alone does not force a rebuild.
        let mut new_cat = cat.clone();
        new_cat.insert(new.clone());
        for f in &new.fields {
            match old_by_id.get(f.id.as_str()) {
                None => added.push(f),
                Some(o) => {
                    if o.name != f.name {
                        renames.insert(o.name.to_ascii_lowercase(), f.name.clone());
                    }
                    let mut o_renamed = (*o).clone();
                    o_renamed.name = f.name.clone();
                    let old_sig =
                        column_signature(d, &o_renamed, relation_table(&o_renamed, new, &new_cat)?.as_deref());
                    let new_sig = column_signature(d, f, relation_table(f, new, &new_cat)?.as_deref());
                    if old_sig != new_sig {
                        rebuild = true;
                    }
                }
            }
        }

        // Managed indexes: follow renames, refuse indexes on dropped fields.
        let mut manual: Vec<IndexDefinition> = Vec::new();
        let mut old_auto: Vec<IndexDefinition> = Vec::new();
        for (mut def, origin) in managed_indexes(log.tx, &old.id).await? {
            if origin == "auto" {
                old_auto.push(def);
                continue;
            }
            rename_fields(&mut def, &renames)?;
            def.collection = new.name.clone();
            let used = fields_used(&def)?;
            if let Some(f) = dropped.iter().find(|f| used.contains(&f.name.to_ascii_lowercase())) {
                return Err(DataError::conflict(
                    "index_uses_field",
                    format!("index {} uses field {}; drop the index first", def.name, f.name),
                ));
            }
            manual.push(def);
        }

        for (from, to) in &renames {
            let from_name = &old
                .fields
                .iter()
                .find(|f| f.name.eq_ignore_ascii_case(from))
                .map(|f| f.name.clone())
                .unwrap_or_default();
            log.run(format!(
                "ALTER TABLE {} RENAME COLUMN {} TO {}",
                q(&table),
                q(from_name),
                q(to)
            ))
            .await?;
        }

        let desired_auto = auto_indexes(new);
        if rebuild {
            // Objects SQLite drops with the table that ForgeBase does not
            // manage (indexes and triggers created with raw SQL).
            let mut managed_names: Vec<String> = old_auto
                .iter()
                .chain(manual.iter())
                .map(|i| i.name.to_ascii_lowercase())
                .collect();
            managed_names.extend(desired_auto.iter().map(|i| i.name.to_ascii_lowercase()));
            let unmanaged = log
                .tx
                .fetch_all(&Statement::new(
                    "SELECT name, sql FROM sqlite_master WHERE lower(tbl_name) = lower(?1) AND type IN ('index', 'trigger')
                       AND sql IS NOT NULL AND name NOT LIKE '\\_forgebase\\_%' ESCAPE '\\'",
                    vec![table.clone().into()],
                ))
                .await?;
            let keep: Vec<String> = unmanaged
                .rows
                .iter()
                .filter(|r| !managed_names.contains(&r[0].as_str().unwrap_or_default().to_ascii_lowercase()))
                .filter_map(|r| r[1].as_str().map(str::to_owned))
                .collect();

            let tmp = format!("{TMP_PREFIX}{}", new.id);
            log.run(create_table_sql(d, &tmp, new, &new_cat)?).await?;
            let mut cols: Vec<String> = SYSTEM_COLUMNS.iter().map(|c| q(c)).collect();
            let mut src: Vec<String> = cols.clone();
            for f in &new.fields {
                if old_by_id.contains_key(f.id.as_str()) {
                    cols.push(q(&f.name));
                    src.push(q(&f.name)); // already renamed in place
                }
            }
            log.run(format!(
                "INSERT INTO {} (rowid, {}) SELECT rowid, {} FROM {}",
                q(&tmp),
                cols.join(", "),
                src.join(", "),
                q(&table)
            ))
            .await
            .map_err(|e| conversion_error(e, &new.name))?;
            log.run(format!("DROP TABLE {}", q(&table))).await?;
            // Legacy rename: views and triggers that name the table are left
            // untouched (the table is about to exist again under that name).
            log.exec("PRAGMA legacy_alter_table = ON", vec![]).await?;
            let renamed = log
                .run(format!("ALTER TABLE {} RENAME TO {}", q(&tmp), q(&table)))
                .await;
            log.exec("PRAGMA legacy_alter_table = OFF", vec![]).await?;
            renamed?;
            for ix in &desired_auto {
                for sql in create_index_sql(d, &new_cat, new, ix)? {
                    log.run(sql).await.map_err(|e| unique_error(e, &new.name))?;
                }
            }
            for ix in &manual {
                let stmts = match ix.kind {
                    IndexKind::Btree => create_index_sql(d, &new_cat, new, ix)?,
                    IndexKind::Fulltext => fts_trigger_sql(d, &new_cat, new, ix)?,
                };
                for sql in stmts {
                    log.run(sql).await.map_err(|e| unique_error(e, &new.name))?;
                }
            }
            for sql in keep {
                log.run(sql).await?;
            }
        } else {
            for f in &added {
                log.run(format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    q(&table),
                    column_sql(d, f, relation_table(f, new, &new_cat)?.as_deref())
                ))
                .await?;
            }
            for ix in old_auto
                .iter()
                .filter(|o| !desired_auto.iter().any(|n| n.name == o.name))
            {
                for sql in drop_index_sql(d, ix) {
                    log.run(sql).await?;
                }
            }
            for ix in desired_auto
                .iter()
                .filter(|n| !old_auto.iter().any(|o| o.name == n.name))
            {
                for sql in create_index_sql(d, &new_cat, new, ix)? {
                    log.run(sql).await.map_err(|e| unique_error(e, &new.name))?;
                }
            }
        }

        // Catalog rows of managed indexes.
        log.exec(
            "DELETE FROM _forgebase_indexes WHERE collection_id = ?1",
            vec![new.id.clone().into()],
        )
        .await?;
        for ix in &desired_auto {
            insert_index_row(log, new, ix, "auto").await?;
        }
        for ix in &manual {
            insert_index_row(log, new, ix, "manual").await?;
        }
        Ok(())
    }

    /// Drops a collection, its table or view, its managed indexes and record
    /// versions. Refused while other collections have relation fields to it
    /// or views depend on it.
    pub async fn delete_collection(&self, ctx: &AccessContext, name_or_id: &str) -> Result<(), DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        let d = self.inner.backend.dialect();
        let coll = self.collection(name_or_id)?;
        self.authorize(ctx, Action::ManageSchema, Some(&coll))?;
        if coll.system && !ctx.is_system() {
            return Err(DataError::Forbidden(format!("{} is a system collection", coll.name)));
        }
        let mut tx = self.begin(TxMode::SchemaChange).await?;
        let result = async {
            let mut cat = CatalogSnapshot::load_tx(&mut *tx).await?;
            for other in cat.all() {
                if other.id == coll.id {
                    continue;
                }
                if let Some(f) = other
                    .fields
                    .iter()
                    .find(|f| f.kind.relation_target() == Some(coll.id.as_str()))
                {
                    return Err(DataError::conflict(
                        "collection_referenced",
                        format!(
                            "{}.{} is a relation to {}; remove it first",
                            other.name, f.name, coll.name
                        ),
                    ));
                }
            }
            let mut log = DdlLog::new(&mut *tx);
            for (ix, _) in managed_indexes(log.tx, &coll.id).await? {
                if ix.kind == IndexKind::Fulltext {
                    for sql in drop_index_sql(d, &ix) {
                        log.run(sql).await?;
                    }
                }
            }
            let kind = if coll.is_view() { "VIEW" } else { "TABLE" };
            log.run(format!("DROP {kind} {}", d.quote_ident(&coll.name))).await?;
            for sql in [
                "DELETE FROM _forgebase_indexes WHERE collection_id = ?1",
                "DELETE FROM _forgebase_record_versions WHERE collection_id = ?1",
                "DELETE FROM _forgebase_collections WHERE id = ?1",
            ] {
                log.exec(sql, vec![coll.id.clone().into()]).await?;
            }
            let statements = std::mem::take(&mut log.statements);
            record_history(
                &mut *tx,
                Some(&coll),
                "drop",
                &statements,
                Some(&coll),
                None,
                &ctx.actor(),
            )
            .await?;
            cat = CatalogSnapshot::load_tx(&mut *tx).await?;
            verify_collections(d, &mut *tx, &cat).await?;
            Ok(())
        }
        .await;
        finish(tx, result).await?;
        self.reload_locked().await
    }

    /// Validates a view query: one read-only statement, no system tables
    /// (except for the system principal).
    async fn checked_view_query(&self, ctx: &AccessContext, q: &str) -> Result<String, DataError> {
        let parts = split_statements(q)?;
        let [stmt] = parts.as_slice() else {
            return Err(DataError::invalid(
                "invalid_view",
                "a view query is exactly one SELECT statement",
            ));
        };
        let info = analyze_statement(stmt)?;
        if info.kind != StatementKind::Read || !matches!(info.keyword.as_str(), "SELECT" | "WITH" | "VALUES") {
            return Err(DataError::invalid("invalid_view", "a view query must be a SELECT"));
        }
        if info.touches_system_tables && !ctx.is_system() {
            return Err(DataError::Forbidden("views cannot read ForgeBase system tables".into()));
        }
        if self
            .inner
            .backend
            .statement_writes(&Statement::raw(stmt.clone()))
            .await?
        {
            return Err(DataError::invalid("invalid_view", "a view query must not write"));
        }
        Ok(stmt.clone())
    }
}

/// Fields of a view, inferred from its columns. The view must have an `id`
/// column.
async fn infer_view_fields(
    tx: &mut dyn WriteTx,
    d: &dyn forgebase_db::Dialect,
    view: &str,
) -> Result<Vec<Field>, DataError> {
    let info = tx
        .fetch_all(&Statement::raw(format!("PRAGMA table_info({})", d.quote_ident(view))))
        .await?;
    let (Some(ni), Some(ti)) = (info.column("name"), info.column("type")) else {
        return Err(DataError::Internal("PRAGMA table_info has no name/type".into()));
    };
    let mut has_id = false;
    let mut fields = Vec::new();
    for r in &info.rows {
        let name = r[ni].as_str().unwrap_or_default().to_owned();
        let ty = r[ti].as_str().unwrap_or_default().to_ascii_uppercase();
        if name.eq_ignore_ascii_case("id") {
            has_id = true;
            continue;
        }
        if SYSTEM_COLUMNS.contains(&name.to_ascii_lowercase().as_str()) {
            continue;
        }
        let kind = if ty.contains("INT") {
            FieldKind::Number {
                min: None,
                max: None,
                integer: true,
            }
        } else if ty.contains("REAL") || ty.contains("FLOA") || ty.contains("DOUB") || ty.contains("NUM") {
            FieldKind::Number {
                min: None,
                max: None,
                integer: false,
            }
        } else if ty.contains("TEXT") || ty.contains("CHAR") || ty.contains("CLOB") {
            FieldKind::Text {
                min_length: None,
                max_length: None,
            }
        } else {
            FieldKind::Json { max_size: None }
        };
        fields.push(Field {
            id: format!("view_{}", fields.len()),
            ..Field::new(name, kind)
        });
    }
    if !has_id {
        return Err(DataError::invalid(
            "invalid_view",
            "a view collection must select an id column",
        ));
    }
    Ok(fields)
}

fn conversion_error(e: DataError, coll: &str) -> DataError {
    match e {
        DataError::Db(
            forgebase_db::DbError::Constraint { message, .. } | forgebase_db::DbError::Statement(message),
        ) => DataError::conflict(
            "incompatible_data",
            format!("existing records of {coll} do not fit the new field types: {message}"),
        ),
        other => other,
    }
}

fn unique_error(e: DataError, coll: &str) -> DataError {
    match e {
        DataError::Db(forgebase_db::DbError::Constraint {
            kind: forgebase_db::ConstraintKind::Unique,
            ..
        }) => DataError::conflict(
            "duplicate_values",
            format!("existing records of {coll} have duplicate values for a field that is now unique"),
        ),
        other => other,
    }
}

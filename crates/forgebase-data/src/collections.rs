//! Collections: create, alter and drop as transactional DDL with a schema
//! history (ADR-0003 §4).
//!
//! Every change runs in one write transaction together with its catalog and
//! history rows. On SQLite, table rebuilds (type changes, dropped fields)
//! suspend foreign-key enforcement for the transaction and check the whole
//! database before commit (SQLite's documented procedure). On PostgreSQL the
//! same changes are `ALTER TABLE` statements (`ALTER COLUMN … TYPE … USING`,
//! `DROP COLUMN`) in one `SchemaChange` transaction, with foreign keys
//! deferred to the commit (ADR-0028 §7.5). Any failure — bad data for a new
//! type, a duplicate under a new unique field, a view that would break —
//! rolls everything back: table, catalog and history stay as they were.

use std::collections::{BTreeMap, HashMap};

use forgebase_db::{Backend, Statement, TxMode, Value, WriteTx};
use forgebase_query::{StatementKind, analyze_statement, split_statements};
use forgebase_schema::{
    Collection, CollectionType, Field, FieldKind, Storage, generate_id, is_valid_id, now_timestamp,
};
use serde::Serialize;
use serde_json::Value as Json;

use crate::access::{READS_TENANT_DATA_OPTION, TENANT_COLUMN, is_tenant_scoped};
use crate::catalog::CatalogSnapshot;
use crate::catalog::pg::DependentView;
use crate::ddl::{
    DdlLog, SYSTEM_COLUMNS, TMP_PREFIX, column_signature, column_sql, create_table_sql, create_view_sql,
    foreign_key_sql, introspect_collection_columns, is_pg, object_exists, pg_regclass, record_history, rel,
    relation_table, storage_check, storage_type, system_column_names, verify_collections,
};
use crate::indexes::{
    IndexDefinition, IndexKind, auto_indexes, create_index_sql, drop_index_sql, fields_used, fts_trigger_sql,
    insert_index_row, managed_indexes, rename_fields, tenant_index_name,
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
        Ok(self.resolve(ctx, Action::ReadSchema, name_or_id)?.0)
    }

    /// Schema history, newest last (optionally for one collection).
    pub async fn schema_history(
        &self,
        ctx: &AccessContext,
        collection: Option<&str>,
    ) -> Result<Vec<SchemaChange>, DataError> {
        let id = match collection {
            Some(c) => Some(self.resolve(ctx, Action::ReadSchema, c)?.0.id),
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
                 FROM _forgebase_schema_history WHERE CAST(?1 AS TEXT) IS NULL OR collection_id = ?1 ORDER BY id",
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
        // New names must also be valid on PostgreSQL, on both backends, so a
        // SQLite project can always be converted (FB-151).
        def.validate_postgres_names(None)?;
        let view_sql = match def.kind {
            CollectionType::View => Some(
                self.checked_view_query(ctx, def.view_query.as_deref().unwrap_or(""))
                    .await?,
            ),
            _ => None,
        };
        if let Some(q) = &view_sql {
            self.mark_tenant_reads(&mut def, q)?;
        }

        let d = self.inner.backend.dialect();
        let mut tx = self.begin(TxMode::Normal).await?;
        let result = async {
            let mut cat = CatalogSnapshot::load_tx(&mut *tx).await?;
            if cat.get(&def.id).is_some() {
                return Err(DataError::AlreadyExists(format!("collection id {}", def.id)));
            }
            if cat.get(&def.name).is_some() || object_exists(d, &mut *tx, &def.name).await? {
                return Err(DataError::AlreadyExists(format!("collection or table {}", def.name)));
            }
            let mut log = DdlLog::new(&mut *tx);
            if let Some(q) = &view_sql {
                log.run(create_view_sql(d, &def.name, q)).await?;
                def.fields = infer_view_fields(log.tx, d, &def.name).await?;
            } else {
                log.run(create_table_sql(d, &def.name, &def, &cat)?).await?;
                if is_tenant_scoped(&def) {
                    log.run(format!(
                        "CREATE INDEX {} ON {} ({})",
                        d.quote_ident(&tenant_index_name(d, &def)),
                        rel(d, &def.name),
                        d.quote_ident(TENANT_COLUMN)
                    ))
                    .await?;
                }
                for ix in auto_indexes(d, &def) {
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
            for ix in auto_indexes(d, &def) {
                insert_index_row(&mut log, &def, &ix, "auto").await?;
            }
            if let Some(native) = self.native_policies() {
                let mut all: Vec<Collection> = cat.all().into_iter().cloned().collect();
                all.push(def.clone());
                native.after_change(&mut *log.tx, &def, &all).await?;
            }
            let statements = std::mem::take(&mut log.statements);
            record_history(&mut *tx, Some(&def), "create", &statements, None, Some(&def), &ctx.actor()).await?;
            cat.insert(def.clone());
            let all: Vec<Collection> = cat.all().into_iter().cloned().collect();
            let step = ShardStep::Create {
                def: def.clone(),
                view_sql: view_sql.clone(),
                cat: cat.clone(),
                all,
            };
            Ok((def.clone(), step))
        }
        .await;
        let created = self.finish_schema(tx, result, "create collection").await?;
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
        let (current, _) = self.resolve(ctx, Action::ManageSchema, name_or_id)?;
        if current.system && !ctx.is_system() {
            return Err(DataError::Forbidden(format!("{} is a system collection", current.name)));
        }
        if def.kind != current.kind {
            return Err(DataError::invalid(
                "type_change",
                "the type of a collection cannot change",
            ));
        }
        if is_tenant_scoped(&def) != is_tenant_scoped(&current) {
            return Err(DataError::invalid(
                "tenant_scope_immutable",
                "tenant_scoped is set when a collection is created and cannot change",
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
        // On PostgreSQL every name must fit; on SQLite only new names must
        // (stored ones keep working, see `Collection::validate_postgres_names`).
        def.validate_postgres_names((!is_pg(d)).then_some(&current))?;
        let view_sql = match def.kind {
            CollectionType::View => Some(
                self.checked_view_query(ctx, def.view_query.as_deref().unwrap_or(""))
                    .await?,
            ),
            _ => None,
        };
        if let Some(q) = &view_sql {
            self.mark_tenant_reads(&mut def, q)?;
        }

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
                    || (!old.name.eq_ignore_ascii_case(&def.name) && object_exists(d, &mut *tx, &def.name).await?))
            {
                return Err(DataError::AlreadyExists(format!("collection or table {}", def.name)));
            }
            let mut managed: Vec<(IndexDefinition, String)> = Vec::new();
            let mut log = DdlLog::new(&mut *tx);
            if let Some(native) = self.native_policies() {
                native.before_change(&mut *log.tx, &old).await?;
            }
            if let Some(q) = &view_sql {
                // PostgreSQL refuses to drop a view other views read: take
                // them down and put them back (they must still compile).
                let saved = drop_dependent_views(&mut log, d, &old.name, None).await?;
                log.run(format!("DROP VIEW {}", rel(d, &old.name))).await?;
                log.run(create_view_sql(d, &def.name, q)).await?;
                def.fields = infer_view_fields(log.tx, d, &def.name).await?;
                restore_views(&mut log, &saved).await?;
            } else {
                managed = managed_indexes(log.tx, &old.id).await?;
                self.alter_table(&mut log, &cat, &old, &def, &managed, true).await?;
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
            if let Some(native) = self.native_policies() {
                let all: Vec<Collection> = after.all().into_iter().cloned().collect();
                native.after_change(&mut *tx, &def, &all).await?;
            }
            let step = ShardStep::Update {
                old: old.clone(),
                def: def.clone(),
                view_sql: view_sql.clone(),
                cat: cat.clone(),
                after,
                managed,
            };
            Ok((def.clone(), step))
        }
        .await;
        let updated = self.finish_schema(tx, result, "update collection").await?;
        self.reload_locked().await?;
        Ok(updated)
    }

    /// Applies the table changes between `old` and `new`. `managed` are the
    /// collection's managed indexes (control's catalog rows); with
    /// `catalog_rows` (control) their rows are rewritten too, without (a
    /// shard, FB-173) only the DDL runs.
    async fn alter_table(
        &self,
        log: &mut DdlLog<'_>,
        cat: &CatalogSnapshot,
        old: &Collection,
        new: &Collection,
        managed: &[(IndexDefinition, String)],
        catalog_rows: bool,
    ) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        let q = |s: &str| d.quote_ident(s);
        let mut table = old.name.clone();
        if old.name != new.name {
            log.run(format!("ALTER TABLE {} RENAME TO {}", rel(d, &old.name), q(&new.name)))
                .await?;
            table = new.name.clone();
        }

        // Field diff by id.
        let old_by_id: HashMap<&str, &Field> = old.fields.iter().map(|f| (f.id.as_str(), f)).collect();
        let dropped: Vec<&Field> = old.fields.iter().filter(|f| new.field_by_id(&f.id).is_none()).collect();
        let mut renames: BTreeMap<String, String> = BTreeMap::new();
        let mut rebuild = !dropped.is_empty();
        let mut added: Vec<&Field> = Vec::new();
        // Fields whose column changes (type, check or foreign key): the old
        // definition under its new name, and the new one.
        let mut changed: Vec<(Field, &Field)> = Vec::new();
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
                        changed.push((o_renamed, f));
                    }
                }
            }
        }

        // Managed indexes: follow renames, refuse indexes on dropped fields.
        let mut manual: Vec<IndexDefinition> = Vec::new();
        let mut old_auto: Vec<IndexDefinition> = Vec::new();
        for (mut def, origin) in managed.iter().cloned() {
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
                rel(d, &table),
                q(from_name),
                q(to)
            ))
            .await?;
        }

        let desired_auto = auto_indexes(d, new);
        // Automatic indexes the PostgreSQL column changes dropped and that
        // must be created again.
        let mut recreate_auto: Vec<String> = Vec::new();
        if rebuild && is_pg(d) {
            recreate_auto = self
                .alter_columns_pg(log, &new_cat, new, &table, &dropped, &changed, &old_auto, &manual)
                .await?;
        } else if rebuild {
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
            let mut cols: Vec<String> = system_column_names(new).iter().map(|c| q(c)).collect();
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
        }
        if !rebuild || is_pg(d) {
            for f in &added {
                log.run(format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    rel(d, &table),
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
                .filter(|n| !old_auto.iter().any(|o| o.name == n.name) || recreate_auto.contains(&n.name))
            {
                for sql in create_index_sql(d, &new_cat, new, ix)? {
                    log.run(sql).await.map_err(|e| unique_error(e, &new.name))?;
                }
            }
        }

        // Catalog rows of managed indexes.
        if !catalog_rows {
            return Ok(());
        }
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

    /// PostgreSQL: drops and changes columns in place (ADR-0028 §7.5).
    ///
    /// Per changed field: its single-column `CHECK` and foreign-key
    /// constraints are dropped, the column gets its new type through a
    /// conversion that fails on values that do not fit (as SQLite's `STRICT`
    /// copy does), and the new constraints are added (a new foreign key is
    /// checked against the existing rows at once). Views reading a changed
    /// column are dropped first and created again after (PostgreSQL refuses
    /// to change a column a view reads); a dropped column that a view reads
    /// is a `schema_conflict`. Managed indexes using a changed field are
    /// dropped and created again from their definitions. Returns the
    /// automatic indexes the caller must create again.
    #[allow(clippy::too_many_arguments)]
    async fn alter_columns_pg(
        &self,
        log: &mut DdlLog<'_>,
        new_cat: &CatalogSnapshot,
        new: &Collection,
        table: &str,
        dropped: &[&Field],
        changed: &[(Field, &Field)],
        old_auto: &[IndexDefinition],
        manual: &[IndexDefinition],
    ) -> Result<Vec<String>, DataError> {
        let d = self.inner.backend.dialect();
        let q = |s: &str| d.quote_ident(s);
        let t = rel(d, table);
        let regclass = pg_regclass(d, table);

        for f in dropped {
            let readers =
                crate::catalog::pg::dependent_views(log.tx, &regclass, Some(std::slice::from_ref(&f.name))).await?;
            if let Some(v) = readers.first() {
                return Err(view_conflict(&v.name, &format!("it reads {table}.{}", f.name)));
            }
        }

        let changed_names: Vec<String> = changed.iter().map(|(_, f)| f.name.clone()).collect();
        let lower: Vec<String> = changed_names.iter().map(|n| n.to_ascii_lowercase()).collect();
        let saved = if changed.is_empty() {
            Vec::new()
        } else {
            drop_dependent_views(log, d, table, Some(&changed_names)).await?
        };

        // Managed indexes on changed fields are built again from their
        // definitions (an expression may not fit the new type).
        let uses_changed = |ix: &IndexDefinition| -> Result<bool, DataError> {
            Ok(fields_used(ix)?.iter().any(|u| lower.contains(u)))
        };
        let mut recreate_auto = Vec::new();
        for ix in old_auto {
            if uses_changed(ix)? {
                for sql in drop_index_sql(d, ix) {
                    log.run(sql).await?;
                }
                recreate_auto.push(ix.name.clone());
            }
        }
        let mut rebuild_manual = Vec::new();
        for ix in manual {
            if uses_changed(ix)? {
                for sql in drop_index_sql(d, ix) {
                    log.run(sql).await?;
                }
                rebuild_manual.push(ix);
            }
        }

        for f in dropped {
            log.run(format!("ALTER TABLE {t} DROP COLUMN {}", q(&f.name))).await?;
        }
        for (old_f, new_f) in changed {
            let col = q(&new_f.name);
            for con in crate::catalog::pg::column_constraints(log.tx, &regclass, &new_f.name).await? {
                log.run(format!("ALTER TABLE {t} DROP CONSTRAINT {}", q(&con))).await?;
            }
            let (from, to) = (old_f.kind.storage(), new_f.kind.storage());
            if from != to {
                log.run(format!(
                    "ALTER TABLE {t} ALTER COLUMN {col} TYPE {} USING {}",
                    storage_type(d, to),
                    pg_convert(&col, from, to)
                ))
                .await
                .map_err(|e| conversion_error(e, &new.name))?;
            }
            if let Some(check) = storage_check(d, to, &col) {
                log.run(format!("ALTER TABLE {t} ADD {check}"))
                    .await
                    .map_err(|e| conversion_error(e, &new.name))?;
            }
            if let Some(fk) = foreign_key_sql(d, new_f, relation_table(new_f, new, new_cat)?.as_deref()) {
                // A value with no target fails here as `foreign_key_violation`,
                // the code SQLite's commit-time check gives.
                log.run(format!("ALTER TABLE {t} ADD FOREIGN KEY ({col}) {fk}")).await?;
            }
        }

        for ix in rebuild_manual {
            for sql in create_index_sql(d, new_cat, new, ix)? {
                log.run(sql).await.map_err(|e| unique_error(e, &new.name))?;
            }
        }
        restore_views(log, &saved).await?;
        Ok(recreate_auto)
    }

    /// Drops a collection, its table or view, its managed indexes and record
    /// versions. Refused while other collections have relation fields to it
    /// or views depend on it.
    pub async fn delete_collection(&self, ctx: &AccessContext, name_or_id: &str) -> Result<(), DataError> {
        let _guard = self.inner.schema_lock.lock().await;
        let d = self.inner.backend.dialect();
        let (coll, _) = self.resolve(ctx, Action::ManageSchema, name_or_id)?;
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
            if let Some(native) = self.native_policies() {
                native.before_change(&mut *log.tx, &coll).await?;
            }
            if is_pg(d) {
                // PostgreSQL refuses to drop what a view reads; SQLite drops
                // it and the verification below finds the broken view.
                let readers = crate::catalog::pg::dependent_views(log.tx, &pg_regclass(d, &coll.name), None).await?;
                if let Some(v) = readers.first() {
                    return Err(view_conflict(&v.name, &format!("it reads {}", coll.name)));
                }
            }
            let managed = managed_indexes(log.tx, &coll.id).await?;
            for (ix, _) in &managed {
                if ix.kind == IndexKind::Fulltext {
                    for sql in drop_index_sql(d, ix) {
                        log.run(sql).await?;
                    }
                }
            }
            let kind = if coll.is_view() { "VIEW" } else { "TABLE" };
            log.run(format!("DROP {kind} {}", rel(d, &coll.name))).await?;
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
            let step = ShardStep::Delete {
                coll: coll.clone(),
                after: cat,
                managed,
            };
            Ok(((), step))
        }
        .await;
        self.finish_schema(tx, result, "delete collection").await?;
        self.reload_locked().await
    }

    /// Validates a view query: one read-only statement, no system tables
    /// (except for the system principal).
    /// Sets the computed `reads_tenant_data` option of a view: whether its
    /// query names a tenant-scoped collection (such views are closed to
    /// callers who are not privileged; ADR-0008 §4).
    fn mark_tenant_reads(&self, def: &mut Collection, query: &str) -> Result<(), DataError> {
        let scoped: Vec<String> = self
            .catalog()
            .all()
            .into_iter()
            .filter(|c| is_tenant_scoped(c))
            .map(|c| c.name.to_ascii_lowercase())
            .collect();
        let reads = forgebase_query::sql::tokenize(query)?.iter().any(|t| {
            matches!(
                t.kind,
                forgebase_query::sql::TokenKind::Word | forgebase_query::sql::TokenKind::QuotedIdent
            ) && scoped.contains(&t.text.to_ascii_lowercase())
        });
        def.options.insert(READS_TENANT_DATA_OPTION.into(), Json::Bool(reads));
        Ok(())
    }

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
    let info = tx.fetch_all(&introspect_collection_columns(d, view)).await?;
    let (Some(ni), Some(ti)) = (info.column("name"), info.column("type")) else {
        return Err(DataError::Internal("column introspection has no name/type".into()));
    };
    let pg = is_pg(d);
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
        let kind = if pg && ty.starts_with("BOOL") {
            // PostgreSQL booleans decode as 0/1, like ForgeBase's bool fields.
            FieldKind::Bool
        } else if pg
            && ["UUID", "DATE", "TIME", "INTERVAL", "INET", "CIDR", "MACADDR"]
                .iter()
                .any(|t| ty.contains(t))
        {
            // Read back as text (ADR-0028 §7.3, decoding).
            FieldKind::Text {
                min_length: None,
                max_length: None,
            }
        } else if ty.contains("INT") {
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

/// Drops the views that read `table` (or, with `columns`, those columns of
/// it), with the views reading them, so the table can change; returns them
/// in creation order for [`restore_views`]. PostgreSQL only; nothing on
/// SQLite, whose views name columns lazily (and whose rebuild keeps them).
async fn drop_dependent_views(
    log: &mut DdlLog<'_>,
    d: &dyn forgebase_db::Dialect,
    table: &str,
    columns: Option<&[String]>,
) -> Result<Vec<DependentView>, DataError> {
    if !is_pg(d) {
        return Ok(Vec::new());
    }
    let views = crate::catalog::pg::dependent_views(log.tx, &pg_regclass(d, table), columns).await?;
    for v in views.iter().rev() {
        log.run(format!("DROP VIEW {}", v.qualified)).await?;
    }
    Ok(views)
}

/// Creates views [`drop_dependent_views`] took down. A view that no longer
/// compiles is a `schema_conflict` (the transaction then rolls back).
async fn restore_views(log: &mut DdlLog<'_>, views: &[DependentView]) -> Result<(), DataError> {
    for v in views {
        log.run(v.create_sql()).await.map_err(|e| match e {
            DataError::Db(db) => view_conflict(&v.name, &db.to_string()),
            other => other,
        })?;
    }
    Ok(())
}

/// The error for a change that would break view `view`.
fn view_conflict(view: &str, why: &str) -> DataError {
    DataError::conflict(
        "schema_conflict",
        format!("this change would break collection {view:?} ({why}); change collections through the collections API"),
    )
}

/// PostgreSQL conversion of column `col` between storage classes, for
/// `ALTER COLUMN … TYPE … USING`. It keeps SQLite's `STRICT` copy semantics:
/// a value that does not fit fails the statement (`incompatible_data`)
/// instead of being rounded or dropped: integral text and integral reals
/// become integers, a fraction is refused; numbers become text in SQLite's
/// form (`3.0` for an integral real); JSON is checked by the `jsonb` cast.
fn pg_convert(col: &str, from: Storage, to: Storage) -> String {
    use Storage::{Bool, Integer, Json, Real, Text};
    // Text (and JSON as text) to an exact integer: through numeric, refusing
    // fractions with an error that names the value.
    let integral = |src: &str, ty: &str| {
        format!(
            "CASE WHEN ({src})::numeric = trunc(({src})::numeric) THEN ({src})::numeric::{ty} \
             ELSE ('not an integer: ' || ({src})::text)::{ty} END"
        )
    };
    match (from, to) {
        (Real, Text) => format!(
            "CASE WHEN {col} = trunc({col}) AND abs({col}) < 1e15 THEN trunc({col})::bigint::text || '.0' ELSE {col}::text END"
        ),
        (_, Text) => format!("{col}::text"),
        (Text | Json | Real, Integer) => integral(&format!("{col}::text"), "bigint"),
        (Text | Json | Real, Bool) => integral(&format!("{col}::text"), "smallint"),
        (Bool, Integer) => format!("{col}::bigint"),
        (Integer, Bool) => format!("{col}::smallint"),
        (Json, Real) => format!("({col}::text)::double precision"),
        (_, Real) => format!("{col}::double precision"),
        (Text, Json) => format!("{col}::jsonb"),
        (_, Json) => format!("to_jsonb({col})"),
        (Integer, Integer) | (Bool, Bool) => col.to_owned(),
    }
}

/// The schema work a shard repeats after control did it (ADR-0029 §3.7,
/// FB-173). Every node runs the DDL against its own catalog (constraint and
/// view names are read from the node); catalog rows, history and index rows
/// stay on control.
#[derive(Debug)]
pub(crate) enum ShardStep {
    Create {
        def: Collection,
        view_sql: Option<String>,
        cat: CatalogSnapshot,
        all: Vec<Collection>,
    },
    Update {
        old: Collection,
        def: Collection,
        view_sql: Option<String>,
        cat: CatalogSnapshot,
        after: CatalogSnapshot,
        managed: Vec<(IndexDefinition, String)>,
    },
    Delete {
        coll: Collection,
        after: CatalogSnapshot,
        managed: Vec<(IndexDefinition, String)>,
    },
}

impl DataService {
    /// Makes schema changes run on every set of `router` (FB-173): once it
    /// has shards, create, update and delete collection commit on control
    /// and on every shard as one two-phase transaction. Once; `false` if a
    /// router was attached already. Without shards nothing changes.
    pub fn attach_shards(&self, router: forgebase_db::DbRouter) -> bool {
        self.inner.shards.set(router).is_ok()
    }

    /// The attached router (FB-173), if any.
    pub fn shard_router(&self) -> Option<&forgebase_db::DbRouter> {
        self.inner.shards.get()
    }

    /// The router, when it has shards to fan out to.
    fn sharded(&self) -> Option<&forgebase_db::DbRouter> {
        self.inner.shards.get().filter(|r| r.is_sharded())
    }

    /// Commits a schema change: on one set, `tx` as before; on a sharded
    /// cluster, `tx` (control) and the same step on every shard as one
    /// two-phase transaction. Any failure rolls back everywhere.
    pub(crate) async fn finish_schema<T>(
        &self,
        tx: Box<dyn WriteTx>,
        result: Result<(T, ShardStep), DataError>,
        purpose: &str,
    ) -> Result<T, DataError> {
        let (value, step) = match result {
            Ok(v) => v,
            Err(e) => return finish(tx, Err(e)).await,
        };
        let Some(router) = self.sharded() else {
            return finish(tx, Ok(value)).await;
        };
        use forgebase_db::twophase::{self, Participant, TwoPhase};
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
        let mut two = TwoPhase::new(&control.db, purpose);
        if let Err(e) = two.prepare(&control, tx).await {
            two.abort().await;
            return Err(e.into());
        }
        for p in &shards {
            let mut stx = match p.db.begin(TxMode::SchemaChange).await {
                Ok(t) => t,
                Err(e) => {
                    two.abort().await;
                    return Err(e.into());
                }
            };
            if let Err(e) = self.apply_on_shard(&mut *stx, &step).await {
                tracing::warn!(set = %p.set_id, error = %e, "schema change failed on a shard; rolled back everywhere");
                let _ = stx.rollback().await;
                two.abort().await;
                return Err(e);
            }
            if let Err(e) = two.prepare(p, stx).await {
                two.abort().await;
                return Err(e.into());
            }
        }
        match two.commit().await {
            Ok(_) => Ok(value),
            Err(e) => Err(forgebase_db::DbError::from(e).into()),
        }
    }

    /// Runs `step` in a shard's transaction.
    async fn apply_on_shard(&self, tx: &mut dyn WriteTx, step: &ShardStep) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        match step {
            ShardStep::Create { def, view_sql, cat, all } => {
                let mut log = DdlLog::new(tx);
                if let Some(q) = view_sql {
                    log.run(create_view_sql(d, &def.name, q)).await?;
                } else {
                    log.run(create_table_sql(d, &def.name, def, cat)?).await?;
                    self.create_table_indexes(&mut log, cat, def, &[]).await?;
                }
                if let Some(native) = self.native_policies() {
                    native.after_change(&mut *log.tx, def, all).await?;
                }
            }
            ShardStep::Update {
                old,
                def,
                view_sql,
                cat,
                after,
                managed,
            } => {
                let mut log = DdlLog::new(tx);
                if let Some(native) = self.native_policies() {
                    native.before_change(&mut *log.tx, old).await?;
                }
                if let Some(q) = view_sql {
                    let saved = drop_dependent_views(&mut log, d, &old.name, None).await?;
                    log.run(format!("DROP VIEW {}", rel(d, &old.name))).await?;
                    log.run(create_view_sql(d, &def.name, q)).await?;
                    restore_views(&mut log, &saved).await?;
                } else {
                    self.alter_table(&mut log, cat, old, def, managed, false).await?;
                }
                verify_collections(d, &mut *log.tx, after).await?;
                if let Some(native) = self.native_policies() {
                    let all: Vec<Collection> = after.all().into_iter().cloned().collect();
                    native.after_change(&mut *log.tx, def, &all).await?;
                }
            }
            ShardStep::Delete { coll, after, managed } => {
                let mut log = DdlLog::new(tx);
                if let Some(native) = self.native_policies() {
                    native.before_change(&mut *log.tx, coll).await?;
                }
                let readers = crate::catalog::pg::dependent_views(log.tx, &pg_regclass(d, &coll.name), None).await?;
                if let Some(v) = readers.first() {
                    return Err(view_conflict(&v.name, &format!("it reads {}", coll.name)));
                }
                for (ix, _) in managed {
                    if ix.kind == IndexKind::Fulltext {
                        for sql in drop_index_sql(d, ix) {
                            log.run(sql).await?;
                        }
                    }
                }
                let kind = if coll.is_view() { "VIEW" } else { "TABLE" };
                log.run(format!("DROP {kind} IF EXISTS {}", rel(d, &coll.name))).await?;
                verify_collections(d, &mut *log.tx, after).await?;
            }
        }
        Ok(())
    }

    /// The tenant index, the automatic indexes and `manual` (managed
    /// indexes from control's rows) of a new table.
    async fn create_table_indexes(
        &self,
        log: &mut DdlLog<'_>,
        cat: &CatalogSnapshot,
        def: &Collection,
        manual: &[IndexDefinition],
    ) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        if is_tenant_scoped(def) {
            log.run(format!(
                "CREATE INDEX {} ON {} ({})",
                d.quote_ident(&tenant_index_name(d, def)),
                rel(d, &def.name),
                d.quote_ident(TENANT_COLUMN)
            ))
            .await?;
        }
        for ix in auto_indexes(d, def).iter().chain(manual) {
            for sql in create_index_sql(d, cat, def, ix)? {
                log.run(sql).await?;
            }
        }
        Ok(())
    }

    /// Creates every collection of control's catalog on `target`, a new,
    /// migrated shard (ADR-0029 §3.7: "a new shard replays the catalog"):
    /// tables (relations to other collections added once every table
    /// exists, so cycles work), tenant, automatic and managed indexes,
    /// views (definitions read back from control) and native policies, in
    /// one transaction. PostgreSQL only.
    pub async fn replay_catalog(&self, target: &forgebase_db::Database) -> Result<(), DataError> {
        let d = self.inner.backend.dialect();
        if !is_pg(d) || target.kind() != forgebase_db::BackendKind::Postgres {
            return Err(DataError::invalid(
                "postgres_required",
                "only PostgreSQL databases can be shards",
            ));
        }
        let _guard = self.inner.schema_lock.lock().await;
        let cat = CatalogSnapshot::load(self.inner.backend.as_ref()).await?;
        let mut all: Vec<Collection> = cat.all().into_iter().cloned().collect();
        all.sort_by(|a, b| a.created.cmp(&b.created).then(a.id.cmp(&b.id)));
        // Managed (manual) indexes and view definitions, from control.
        let rows = self
            .inner
            .backend
            .fetch_all(&Statement::raw(
                "SELECT collection_id, definition, origin FROM _forgebase_indexes ORDER BY name",
            ))
            .await?;
        let mut manual: HashMap<String, Vec<IndexDefinition>> = HashMap::new();
        for r in &rows.rows {
            if r.get(2).and_then(Value::as_str) == Some("manual") {
                let def: IndexDefinition = serde_json::from_str(r.get(1).and_then(Value::as_str).unwrap_or("{}"))?;
                manual
                    .entry(r.first().and_then(Value::as_str).unwrap_or_default().to_owned())
                    .or_default()
                    .push(def);
            }
        }
        let mut tx = target.begin(TxMode::SchemaChange).await?;
        let result = async {
            let mut log = DdlLog::new(&mut *tx);
            let mut later: Vec<(&Collection, &Field)> = Vec::new();
            for c in all.iter().filter(|c| !c.is_view()) {
                // Relations to other collections come after every table exists.
                let mut first = c.clone();
                first.fields.retain(|f| match f.kind.relation_target() {
                    Some(t) if t != c.id => false,
                    _ => true,
                });
                later.extend(
                    c.fields
                        .iter()
                        .filter(|f| f.kind.relation_target().is_some_and(|t| t != c.id))
                        .map(|f| (c, f)),
                );
                log.run(create_table_sql(d, &c.name, &first, &cat)?).await?;
            }
            for (c, f) in later {
                log.run(format!(
                    "ALTER TABLE {} ADD COLUMN {}",
                    rel(d, &c.name),
                    column_sql(d, f, relation_table(f, c, &cat)?.as_deref())
                ))
                .await?;
            }
            for c in all.iter().filter(|c| !c.is_view()) {
                let m = manual.get(&c.id).cloned().unwrap_or_default();
                self.create_table_indexes(&mut log, &cat, c, &m).await?;
            }
            for c in all.iter().filter(|c| c.is_view()) {
                let def = self
                    .inner
                    .backend
                    .fetch_all(&Statement::new(
                        "SELECT pg_catalog.pg_get_viewdef(pg_catalog.to_regclass(CAST(?1 AS text)), true)",
                        vec![pg_regclass(d, &c.name).into()],
                    ))
                    .await?;
                let q = def
                    .scalar()
                    .and_then(Value::as_str)
                    .map(|s| s.trim().trim_end_matches(';').to_owned())
                    .ok_or_else(|| DataError::Internal(format!("view {} has no definition on control", c.name)))?;
                log.run(create_view_sql(d, &c.name, &q)).await?;
            }
            verify_collections(d, &mut *log.tx, &cat).await?;
            if let Some(native) = self.native_policies() {
                for c in &all {
                    native.after_change(&mut *log.tx, c, &all).await?;
                }
            }
            Ok(())
        }
        .await;
        finish(tx, result).await
    }
}

impl forgebase_db::shards::CatalogReplay for DataService {
    fn replay<'a>(
        &'a self,
        target: &'a forgebase_db::Database,
    ) -> forgebase_db::BoxFuture<'a, Result<(), forgebase_db::DbError>> {
        Box::pin(async move {
            self.replay_catalog(target).await.map_err(|e| match e {
                DataError::Db(e) => e,
                other => forgebase_db::DbError::Statement(other.to_string()),
            })
        })
    }
}

/// Per-set state of an index built with the index manager on a sharded
/// cluster (`_forgebase_index_builds`, FB-173).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct IndexBuild {
    /// The shard.
    pub set_id: String,
    /// `building`, `ready` or `failed`.
    pub state: String,
    /// Why it failed.
    pub error: Option<String>,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// `CREATE [UNIQUE] INDEX x …` → `… INDEX CONCURRENTLY IF NOT EXISTS x …`,
/// `DROP INDEX x` → `DROP INDEX CONCURRENTLY IF EXISTS x`; anything else as is.
pub(crate) fn concurrently(sql: &str) -> String {
    let t = sql.trim_start();
    for (prefix, with) in [
        ("CREATE UNIQUE INDEX ", "CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS "),
        ("CREATE INDEX ", "CREATE INDEX CONCURRENTLY IF NOT EXISTS "),
        ("DROP INDEX ", "DROP INDEX CONCURRENTLY IF EXISTS "),
    ] {
        if let Some(rest) = t.strip_prefix(prefix) {
            let rest = rest
                .strip_prefix("IF NOT EXISTS ")
                .or_else(|| rest.strip_prefix("IF EXISTS "))
                .unwrap_or(rest);
            return format!("{with}{rest}");
        }
    }
    sql.to_owned()
}

impl DataService {
    async fn set_build_state(&self, index: &str, set: &str, state: &str, error: Option<&str>) -> Result<(), DataError> {
        let mut tx = self.begin(TxMode::Normal).await?;
        let r = tx
            .execute(&Statement::new(
                "INSERT INTO _forgebase_index_builds (index_name, set_id, state, error, updated_at) VALUES (?1, ?2, ?3, ?4, ?5)
                 ON CONFLICT (index_name, set_id) DO UPDATE SET state = excluded.state, error = excluded.error,
                 updated_at = excluded.updated_at",
                vec![
                    index.into(),
                    set.into(),
                    state.into(),
                    error.map(str::to_owned).into(),
                    forgebase_core::now_rfc3339().into(),
                ],
            ))
            .await;
        finish(tx, r.map(|_| ()).map_err(DataError::from)).await
    }

    /// After control committed a new managed index: builds it on every
    /// shard with `CREATE INDEX CONCURRENTLY` (ADR-0029 §3.7), one shard at
    /// a time, recording `building` → `ready` or `failed` per set. A
    /// failure on a shard does not undo the index elsewhere; it is
    /// reported by [`DataService::index_builds`]. No-op without shards.
    pub(crate) async fn build_index_on_shards(&self, coll: &Collection, def: &IndexDefinition) {
        let Some(router) = self.sharded() else {
            return;
        };
        let d = self.inner.backend.dialect();
        let cat = self.catalog();
        let statements = match create_index_sql(d, &cat, coll, def) {
            Ok(s) => s.iter().map(|s| concurrently(s)).collect::<Vec<_>>(),
            Err(e) => {
                tracing::warn!(index = %def.name, error = %e, "cannot render the index for the shards");
                return;
            }
        };
        let shards: Vec<_> = router.sets().into_iter().filter(|h| !h.is_control()).collect();
        for h in &shards {
            if let Err(e) = self.set_build_state(&def.name, h.id(), "building", None).await {
                tracing::warn!(index = %def.name, set = %h.id(), error = %e, "cannot record the index build");
            }
        }
        for h in &shards {
            let outcome = h.database().run_autocommit(&statements).await;
            let (state, error) = match &outcome {
                Ok(()) => ("ready", None),
                Err(e) => {
                    // A failed concurrent build leaves an invalid index behind.
                    let drop = drop_index_sql(d, def).iter().map(|s| concurrently(s)).collect::<Vec<_>>();
                    let _ = h.database().run_autocommit(&drop).await;
                    tracing::warn!(index = %def.name, set = %h.id(), error = %e, "index build failed on a shard");
                    ("failed", Some(e.to_string()))
                }
            };
            if let Err(e) = self.set_build_state(&def.name, h.id(), state, error.as_deref()).await {
                tracing::warn!(index = %def.name, set = %h.id(), error = %e, "cannot record the index build");
            }
        }
    }

    /// After control dropped a managed index: drops it on every shard
    /// (`DROP INDEX CONCURRENTLY IF EXISTS`). Its build rows went with the
    /// catalog row.
    pub(crate) async fn drop_index_on_shards(&self, def: &IndexDefinition) {
        let Some(router) = self.sharded() else {
            return;
        };
        let d = self.inner.backend.dialect();
        let statements: Vec<String> = drop_index_sql(d, def).iter().map(|s| concurrently(s)).collect();
        for h in router.sets().into_iter().filter(|h| !h.is_control()) {
            if let Err(e) = h.database().run_autocommit(&statements).await {
                tracing::warn!(index = %def.name, set = %h.id(), error = %e, "could not drop the index on a shard");
            }
        }
    }

    /// Per-set build state of managed index `name` (empty unless it was
    /// built on shards with the index manager).
    pub async fn index_builds(&self, ctx: &AccessContext, name: &str) -> Result<Vec<IndexBuild>, DataError> {
        let rows = self
            .inner
            .backend
            .fetch_all(&Statement::new(
                "SELECT i.collection_id, b.set_id, b.state, b.error, b.updated_at FROM _forgebase_indexes i
                 LEFT JOIN _forgebase_index_builds b ON b.index_name = i.name WHERE i.name = ?1 ORDER BY b.set_id",
                vec![name.into()],
            ))
            .await?;
        let Some(first) = rows.rows.first() else {
            return Err(DataError::IndexNotFound(name.to_owned()));
        };
        let coll = self.collection(first.first().and_then(Value::as_str).unwrap_or_default())?;
        self.authorize(ctx, Action::ManageIndexes, Some(&coll))?;
        Ok(rows
            .rows
            .iter()
            .filter_map(|r| {
                let text = |i: usize| r.get(i).and_then(Value::as_str).map(str::to_owned);
                Some(IndexBuild {
                    set_id: text(1)?,
                    state: text(2).unwrap_or_default(),
                    error: text(3),
                    updated_at: text(4).unwrap_or_default(),
                })
            })
            .collect())
    }
}

#[cfg(test)]
mod fanout_tests {
    use super::concurrently;

    #[test]
    fn index_statements_become_concurrent() {
        assert_eq!(
            concurrently("CREATE UNIQUE INDEX \"i\" ON public.\"t\" (\"a\")"),
            "CREATE UNIQUE INDEX CONCURRENTLY IF NOT EXISTS \"i\" ON public.\"t\" (\"a\")"
        );
        assert_eq!(
            concurrently("CREATE INDEX IF NOT EXISTS i ON t (a)"),
            "CREATE INDEX CONCURRENTLY IF NOT EXISTS i ON t (a)"
        );
        assert_eq!(concurrently("DROP INDEX public.\"i\""), "DROP INDEX CONCURRENTLY IF EXISTS public.\"i\"");
        assert_eq!(concurrently("ANALYZE t"), "ANALYZE t");
    }
}

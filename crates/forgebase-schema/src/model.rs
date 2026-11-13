//! Collection and field definitions.

use std::collections::HashSet;

use forgebase_core::{ErrorCode, ErrorKind};
use serde::{Deserialize, Serialize};
use serde_json::Value as Json;

/// System columns every collection table has, in this order. They cannot be
/// used as field names.
pub const SYSTEM_FIELDS: &[&str] = &["id", "created", "updated", "_version"];

/// Kind of collection (SPEC §4.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollectionType {
    /// Ordinary records (also used for JSON document collections).
    #[default]
    Base,
    /// User accounts: a base collection with the auth system fields. Only the
    /// schema lives here; authentication logic is FB-20.
    Auth,
    /// Read-only records defined by a SQL `SELECT`.
    View,
}

impl CollectionType {
    /// Name stored in the catalog.
    pub fn as_str(self) -> &'static str {
        match self {
            CollectionType::Base => "base",
            CollectionType::Auth => "auth",
            CollectionType::View => "view",
        }
    }
}

/// How undeclared keys in a record are treated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SchemaMode {
    /// Only declared fields are accepted.
    #[default]
    Strict,
    /// Declared fields are validated; any other top-level keys are kept as a
    /// JSON document (dynamic schema) and can be filtered and indexed by path.
    Flexible,
}

/// Element type of an [`FieldKind::Array`] field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrayItems {
    /// Strings.
    Text,
    /// Numbers.
    Number,
    /// Booleans.
    Bool,
    /// Any JSON value.
    #[default]
    Json,
}

fn one() -> u32 {
    1
}
fn eight() -> u32 {
    8
}

/// Type and type-specific options of a field.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum FieldKind {
    /// UTF-8 text.
    Text {
        /// Minimum length in characters.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_length: Option<u32>,
        /// Maximum length in characters (default 1 000 000).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_length: Option<u32>,
    },
    /// A finite number.
    Number {
        /// Minimum value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<f64>,
        /// Maximum value.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<f64>,
        /// Only integers (stored as INTEGER).
        #[serde(default)]
        integer: bool,
    },
    /// `true` / `false`.
    Bool,
    /// A date-time, stored as a UTC timestamp `YYYY-MM-DDTHH:MM:SS.mmmZ`.
    Date,
    /// An e-mail address.
    Email,
    /// An `http`/`https` URL.
    Url,
    /// One (or up to `max_select`) of a fixed list of values (an enum).
    Select {
        /// Allowed values.
        values: Vec<String>,
        /// Values that may be chosen at once; above 1 the field holds an array.
        #[serde(default = "one")]
        max_select: u32,
    },
    /// Any JSON value (objects and arrays can be filtered by path).
    Json {
        /// Maximum serialised size in bytes (default 1 MiB).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_size: Option<u32>,
    },
    /// A JSON array of typed items.
    Array {
        /// Item type.
        #[serde(default)]
        items: ArrayItems,
        /// Minimum number of items.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min_items: Option<u32>,
        /// Maximum number of items.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_items: Option<u32>,
    },
    /// Reference(s) to records of another collection.
    Relation {
        /// Target collection id.
        collection_id: String,
        /// Records that may be referenced; above 1 the field holds an array.
        #[serde(default = "one")]
        max_select: u32,
        /// Delete this record when the referenced record is deleted. Without it,
        /// a single reference is cleared (or the delete is refused when the
        /// field is required) and multiple references drop the deleted id.
        #[serde(default)]
        cascade_delete: bool,
    },
    /// Reference(s) to stored files (names only; blobs are Phase 4).
    File {
        /// Files that may be referenced; above 1 the field holds an array.
        #[serde(default = "one")]
        max_select: u32,
    },
    /// A secret: write-only, stored hashed, never returned or filterable.
    Password {
        /// Minimum length in characters (default 8).
        #[serde(default = "eight")]
        min_length: u32,
    },
    /// A point `{ "lon": .., "lat": .. }` in WGS 84 degrees.
    Geo,
    /// A fixed-length vector of numbers (stored data only; search is Phase 6).
    Vector {
        /// Number of dimensions (1 to 16 000).
        dimensions: u32,
    },
}

/// How a field is stored in its column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Storage {
    /// Text.
    Text,
    /// Integer.
    Integer,
    /// Float.
    Real,
    /// 0 / 1.
    Bool,
    /// JSON text.
    Json,
}

impl FieldKind {
    /// Type name (`text`, `number`, ...).
    pub fn type_name(&self) -> &'static str {
        match self {
            FieldKind::Text { .. } => "text",
            FieldKind::Number { .. } => "number",
            FieldKind::Bool => "bool",
            FieldKind::Date => "date",
            FieldKind::Email => "email",
            FieldKind::Url => "url",
            FieldKind::Select { .. } => "select",
            FieldKind::Json { .. } => "json",
            FieldKind::Array { .. } => "array",
            FieldKind::Relation { .. } => "relation",
            FieldKind::File { .. } => "file",
            FieldKind::Password { .. } => "password",
            FieldKind::Geo => "geo",
            FieldKind::Vector { .. } => "vector",
        }
    }

    /// Column storage.
    pub fn storage(&self) -> Storage {
        match self {
            FieldKind::Text { .. }
            | FieldKind::Date
            | FieldKind::Email
            | FieldKind::Url
            | FieldKind::Password { .. } => Storage::Text,
            FieldKind::Number { integer: true, .. } => Storage::Integer,
            FieldKind::Number { .. } => Storage::Real,
            FieldKind::Bool => Storage::Bool,
            FieldKind::Select { max_select, .. }
            | FieldKind::Relation { max_select, .. }
            | FieldKind::File { max_select, .. } => {
                if *max_select > 1 {
                    Storage::Json
                } else {
                    Storage::Text
                }
            }
            FieldKind::Json { .. } | FieldKind::Array { .. } | FieldKind::Geo | FieldKind::Vector { .. } => {
                Storage::Json
            }
        }
    }

    /// Whether the field holds several values (a JSON array the filter
    /// language iterates with the `?=` family of operators).
    pub fn is_multi(&self) -> bool {
        match self {
            FieldKind::Select { max_select, .. }
            | FieldKind::Relation { max_select, .. }
            | FieldKind::File { max_select, .. } => *max_select > 1,
            FieldKind::Array { .. } => true,
            _ => false,
        }
    }

    /// Whether the field can appear in filters, sorts and indexes.
    pub fn is_queryable(&self) -> bool {
        !matches!(self, FieldKind::Password { .. } | FieldKind::Vector { .. })
    }

    /// Target collection of a relation field.
    pub fn relation_target(&self) -> Option<&str> {
        match self {
            FieldKind::Relation { collection_id, .. } => Some(collection_id),
            _ => None,
        }
    }
}

/// A field of a collection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Field {
    /// Stable id; renames keep it. Generated when empty.
    #[serde(default)]
    pub id: String,
    /// Column name.
    pub name: String,
    /// Type and options.
    #[serde(flatten)]
    pub kind: FieldKind,
    /// Must be present and non-empty.
    #[serde(default)]
    pub required: bool,
    /// Values must be unique across the collection (enforced by a unique index).
    #[serde(default)]
    pub unique: bool,
    /// Omitted from records returned to non-administrators.
    #[serde(default)]
    pub hidden: bool,
    /// Created by ForgeBase; cannot be removed or changed through the API.
    #[serde(default)]
    pub system: bool,
    /// Value used on create when the field is missing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Json>,
}

/// Keys every field definition may have, besides its type's options.
const FIELD_KEYS: &[&str] = &[
    "id", "name", "type", "required", "unique", "hidden", "system", "default",
];

/// The option keys of field type `type_name` (`None` for an unknown type).
pub fn field_option_keys(type_name: &str) -> Option<&'static [&'static str]> {
    Some(match type_name {
        "text" => &["min_length", "max_length"],
        "number" => &["min", "max", "integer"],
        "bool" | "date" | "email" | "url" | "geo" => &[],
        "select" => &["values", "max_select"],
        "json" => &["max_size"],
        "array" => &["items", "min_items", "max_items"],
        "relation" => &["collection_id", "max_select", "cascade_delete"],
        "file" => &["max_select"],
        "password" => &["min_length"],
        "vector" => &["dimensions"],
        _ => return None,
    })
}

/// The keys of a field definition (as JSON) that are neither a field key nor
/// an option of its type. Deserialising a [`Field`] ignores them, so the API
/// refuses them instead: a typo or an `options` object would otherwise be
/// dropped without a word (type options sit beside `type`).
pub fn unknown_field_keys(field: &Json) -> Vec<String> {
    let Some(map) = field.as_object() else {
        return Vec::new();
    };
    let options = map
        .get("type")
        .and_then(Json::as_str)
        .and_then(field_option_keys)
        .unwrap_or(&[]);
    map.keys()
        .filter(|k| !FIELD_KEYS.contains(&k.as_str()) && !options.contains(&k.as_str()))
        .cloned()
        .collect()
}

impl Field {
    /// A field with default options.
    pub fn new(name: impl Into<String>, kind: FieldKind) -> Self {
        Self {
            id: String::new(),
            name: name.into(),
            kind,
            required: false,
            unique: false,
            hidden: false,
            system: false,
            default: None,
        }
    }

    /// Same field, required.
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }

    /// Same field, unique.
    pub fn unique(mut self) -> Self {
        self.unique = true;
        self
    }

    /// Same field, with a default value.
    pub fn with_default(mut self, value: Json) -> Self {
        self.default = Some(value);
        self
    }
}

/// A collection definition.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Collection {
    /// Stable id (generated when empty).
    #[serde(default)]
    pub id: String,
    /// Name; also the table name.
    pub name: String,
    /// Kind.
    #[serde(rename = "type", default)]
    pub kind: CollectionType,
    /// Created by ForgeBase; protected from deletion and renaming.
    #[serde(default)]
    pub system: bool,
    /// Strict (declared fields only) or flexible (JSON documents).
    #[serde(default)]
    pub schema_mode: SchemaMode,
    /// Keep every version of every record.
    #[serde(default)]
    pub versioned: bool,
    /// Fields, in order.
    #[serde(default)]
    pub fields: Vec<Field>,
    /// `SELECT` of a view collection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub view_query: Option<String>,
    /// Free-form options owned by other packages (e.g. auth settings, FB-20).
    #[serde(default)]
    pub options: serde_json::Map<String, Json>,
    /// Schema version, incremented by every change.
    #[serde(default)]
    pub version: i64,
    /// Creation time.
    #[serde(default)]
    pub created: String,
    /// Last schema change.
    #[serde(default)]
    pub updated: String,
}

impl Collection {
    /// A base collection with no fields.
    pub fn base(name: impl Into<String>) -> Self {
        Self {
            id: String::new(),
            name: name.into(),
            kind: CollectionType::Base,
            system: false,
            schema_mode: SchemaMode::Strict,
            versioned: false,
            fields: Vec::new(),
            view_query: None,
            options: serde_json::Map::new(),
            version: 0,
            created: String::new(),
            updated: String::new(),
        }
    }

    /// A flexible (document) collection with no fields.
    pub fn documents(name: impl Into<String>) -> Self {
        let mut c = Self::base(name);
        c.schema_mode = SchemaMode::Flexible;
        c
    }

    /// Same collection with another field.
    pub fn with_field(mut self, field: Field) -> Self {
        self.fields.push(field);
        self
    }

    /// Field by name (case-insensitive, as SQL column names are).
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// Field by id.
    pub fn field_by_id(&self, id: &str) -> Option<&Field> {
        self.fields.iter().find(|f| f.id == id)
    }

    /// Whether this is a view collection.
    pub fn is_view(&self) -> bool {
        self.kind == CollectionType::View
    }

    /// Adds the system fields of auth collections that are missing, and marks
    /// them as system fields. Idempotent.
    pub fn ensure_auth_fields(&mut self) {
        if self.kind != CollectionType::Auth {
            return;
        }
        let wanted = [
            Field::new("email", FieldKind::Email).required().unique(),
            Field::new("email_visibility", FieldKind::Bool).with_default(Json::Bool(false)),
            Field::new("verified", FieldKind::Bool).with_default(Json::Bool(false)),
            Field::new("password", FieldKind::Password { min_length: 8 }).required(),
            Field {
                hidden: true,
                ..Field::new(
                    "token_key",
                    FieldKind::Text {
                        min_length: None,
                        max_length: Some(255),
                    },
                )
            },
        ];
        for mut f in wanted {
            if let Some(existing) = self.fields.iter_mut().find(|e| e.name.eq_ignore_ascii_case(&f.name)) {
                existing.system = true;
            } else {
                f.system = true;
                self.fields.push(f);
            }
        }
    }

    /// Checks the names PostgreSQL refuses ([`validate_postgres_collection_name`],
    /// [`validate_postgres_field_name`]) that are new relative to `previous`
    /// (every name when `previous` is `None`).
    ///
    /// The data service runs it with `None` on PostgreSQL, and against the
    /// stored definition on SQLite, so a SQLite project cannot pick a new
    /// name that would block `forgebase db convert` while names it already
    /// has keep working. A field counts as new when its id is new or its name
    /// changed.
    pub fn validate_postgres_names(&self, previous: Option<&Collection>) -> Result<(), SchemaError> {
        if previous.is_none_or(|p| !p.name.eq_ignore_ascii_case(&self.name)) {
            validate_postgres_collection_name(&self.name)?;
        }
        for f in &self.fields {
            let known = previous
                .and_then(|p| p.field_by_id(&f.id))
                .is_some_and(|old| old.name.eq_ignore_ascii_case(&f.name));
            if !known {
                validate_postgres_field_name(&f.name)?;
            }
        }
        Ok(())
    }

    /// Checks the definition on its own (names, options, defaults). Relation
    /// targets are checked by the data service, which knows the catalog.
    pub fn validate(&self) -> Result<(), SchemaError> {
        validate_collection_name(&self.name, self.system)?;
        if self.is_view() {
            let q = self.view_query.as_deref().unwrap_or("").trim();
            if q.is_empty() {
                return Err(SchemaError::InvalidView("a view collection needs a view_query".into()));
            }
            if !self.fields.is_empty() {
                return Err(SchemaError::InvalidView(
                    "view fields are derived from the query; do not declare them".into(),
                ));
            }
        } else if self.view_query.is_some() {
            return Err(SchemaError::InvalidView(
                "only view collections have a view_query".into(),
            ));
        }
        if self.fields.len() > 500 {
            return Err(SchemaError::TooManyFields);
        }
        let mut names = HashSet::new();
        let mut ids = HashSet::new();
        for f in &self.fields {
            validate_field_name(&f.name)?;
            if !names.insert(f.name.to_ascii_lowercase()) {
                return Err(SchemaError::DuplicateField(f.name.clone()));
            }
            if !f.id.is_empty() && !ids.insert(f.id.clone()) {
                return Err(SchemaError::DuplicateField(f.id.clone()));
            }
            validate_field_options(f)?;
        }
        Ok(())
    }
}

/// Whether `name` is a system column or otherwise reserved.
pub fn is_reserved_field_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    SYSTEM_FIELDS.contains(&lower.as_str())
        || lower == "expand"
        || lower.starts_with('_')
        || lower == "rowid"
        || lower == "oid"
}

/// Collection names: `[A-Za-z][A-Za-z0-9_]{0,62}`; system collections may
/// also start with `_`. Names starting with `_forgebase_` or `sqlite_` are
/// always reserved.
pub fn validate_collection_name(name: &str, system: bool) -> Result<(), SchemaError> {
    let lower = name.to_ascii_lowercase();
    if lower.starts_with("_forgebase") || lower.starts_with("sqlite_") {
        return Err(SchemaError::ReservedName(name.to_owned()));
    }
    let body = if system {
        name.strip_prefix('_').unwrap_or(name)
    } else {
        name
    };
    if !is_name(body) || name.len() > 63 {
        return Err(SchemaError::InvalidName(name.to_owned()));
    }
    Ok(())
}

/// Field names: `[A-Za-z][A-Za-z0-9_]{0,62}`, not reserved.
pub fn validate_field_name(name: &str) -> Result<(), SchemaError> {
    if is_reserved_field_name(name) {
        return Err(SchemaError::ReservedName(name.to_owned()));
    }
    if !is_name(name) || name.len() > 63 {
        return Err(SchemaError::InvalidName(name.to_owned()));
    }
    Ok(())
}

/// Longest identifier PostgreSQL keeps, in bytes (`NAMEDATALEN - 1`).
/// Collection and field names already fit (63 ASCII characters).
pub const POSTGRES_MAX_IDENT: usize = 63;

/// Column names PostgreSQL gives every table (`oid` is already reserved by
/// [`is_reserved_field_name`]). A field with one of these names cannot be
/// created on PostgreSQL ("column name conflicts with a system column name").
pub const POSTGRES_SYSTEM_COLUMNS: &[&str] = &["tableoid", "xmin", "cmin", "xmax", "cmax", "ctid"];

/// PostgreSQL's extra rule for collection names: `pg_` is reserved.
///
/// `pg_catalog` is searched before every schema on the search path, so a
/// collection named like a catalog relation (`pg_class`, `pg_stats`) would be
/// shadowed by it in every unqualified statement; PostgreSQL also reserves the
/// prefix for system schemas and roles. The prefix is refused whole rather than
/// per catalog relation, because new releases add relations.
pub fn validate_postgres_collection_name(name: &str) -> Result<(), SchemaError> {
    if name.to_ascii_lowercase().starts_with("pg_") || name.len() > POSTGRES_MAX_IDENT {
        return Err(SchemaError::ReservedName(name.to_owned()));
    }
    Ok(())
}

/// PostgreSQL's extra rule for field names: its system column names.
pub fn validate_postgres_field_name(name: &str) -> Result<(), SchemaError> {
    let lower = name.to_ascii_lowercase();
    if POSTGRES_SYSTEM_COLUMNS.contains(&lower.as_str()) || name.len() > POSTGRES_MAX_IDENT {
        return Err(SchemaError::ReservedName(name.to_owned()));
    }
    Ok(())
}

fn is_name(s: &str) -> bool {
    let mut c = s.chars();
    matches!(c.next(), Some(first) if first.is_ascii_alphabetic())
        && c.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
}

fn validate_field_options(f: &Field) -> Result<(), SchemaError> {
    let bad = |m: &str| {
        Err(SchemaError::InvalidOption {
            field: f.name.clone(),
            message: m.to_owned(),
        })
    };
    match &f.kind {
        FieldKind::Text { min_length, max_length } => {
            if let (Some(a), Some(b)) = (min_length, max_length)
                && a > b
            {
                return bad("min_length is greater than max_length");
            }
        }
        FieldKind::Number { min, max, .. } => {
            if min.is_some_and(|v| !v.is_finite()) || max.is_some_and(|v| !v.is_finite()) {
                return bad("min and max must be finite");
            }
            if let (Some(a), Some(b)) = (min, max)
                && a > b
            {
                return bad("min is greater than max");
            }
        }
        FieldKind::Select { values, max_select } => {
            if values.is_empty() {
                return bad("select needs at least one value");
            }
            let mut seen = HashSet::new();
            if values.iter().any(|v| v.is_empty() || v.len() > 255 || !seen.insert(v)) {
                return bad("select values must be unique, non-empty and at most 255 bytes");
            }
            if *max_select == 0 || *max_select as usize > values.len() {
                return bad("max_select must be between 1 and the number of values");
            }
        }
        FieldKind::Array {
            min_items, max_items, ..
        } => {
            if let (Some(a), Some(b)) = (min_items, max_items)
                && a > b
            {
                return bad("min_items is greater than max_items");
            }
        }
        FieldKind::Relation {
            collection_id,
            max_select,
            ..
        } => {
            if collection_id.is_empty() {
                return bad("relation needs a target collection_id");
            }
            if *max_select == 0 {
                return bad("max_select must be at least 1");
            }
        }
        FieldKind::File { max_select } => {
            if *max_select == 0 {
                return bad("max_select must be at least 1");
            }
        }
        FieldKind::Password { min_length } => {
            if *min_length == 0 || *min_length > 256 {
                return bad("min_length must be between 1 and 256");
            }
            if f.unique || f.default.is_some() {
                return bad("password fields cannot be unique or have a default");
            }
        }
        FieldKind::Vector { dimensions } => {
            if *dimensions == 0 || *dimensions > 16_000 {
                return bad("dimensions must be between 1 and 16000");
            }
            if f.unique {
                return bad("vector fields cannot be unique");
            }
        }
        FieldKind::Json { .. } | FieldKind::Geo => {
            if f.unique {
                return bad("json and geo fields cannot be unique");
            }
        }
        FieldKind::Bool | FieldKind::Date | FieldKind::Email | FieldKind::Url => {}
    }
    if f.unique && f.kind.is_multi() {
        return bad("multi-value fields cannot be unique");
    }
    if let Some(default) = &f.default {
        if default.is_null() {
            return bad("a default cannot be null");
        }
        crate::values::normalize_value(f, default).map_err(|e| SchemaError::InvalidOption {
            field: f.name.clone(),
            message: format!("default is invalid: {}", e.message),
        })?;
    }
    Ok(())
}

/// Problems with a collection definition.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SchemaError {
    /// Malformed collection or field name.
    #[error("invalid name {0:?}: use letters, digits and underscores, starting with a letter (max 63)")]
    InvalidName(String),
    /// Reserved name.
    #[error("name {0:?} is reserved")]
    ReservedName(String),
    /// Two fields with the same name or id.
    #[error("duplicate field {0:?}")]
    DuplicateField(String),
    /// More than 500 fields.
    #[error("a collection can have at most 500 fields")]
    TooManyFields,
    /// Inconsistent field options.
    #[error("field {field:?}: {message}")]
    InvalidOption {
        /// Field name.
        field: String,
        /// What is wrong.
        message: String,
    },
    /// View definition problem.
    #[error("invalid view: {0}")]
    InvalidView(String),
}

impl ErrorCode for SchemaError {
    fn code(&self) -> &'static str {
        match self {
            SchemaError::InvalidName(_) => "invalid_name",
            SchemaError::ReservedName(_) => "reserved_name",
            SchemaError::DuplicateField(_) => "duplicate_field",
            SchemaError::TooManyFields => "too_many_fields",
            SchemaError::InvalidOption { .. } => "invalid_field_option",
            SchemaError::InvalidView(_) => "invalid_view",
        }
    }

    fn kind(&self) -> ErrorKind {
        ErrorKind::InvalidInput
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn every_option_a_field_kind_serialises_is_a_known_key() {
        // Every variant with every option set: the allowed lists cannot drift
        // from `FieldKind`.
        let kinds = [
            FieldKind::Text {
                min_length: Some(1),
                max_length: Some(2),
            },
            FieldKind::Number {
                min: Some(0.0),
                max: Some(1.0),
                integer: true,
            },
            FieldKind::Bool,
            FieldKind::Date,
            FieldKind::Email,
            FieldKind::Url,
            FieldKind::Select {
                values: vec!["a".into()],
                max_select: 2,
            },
            FieldKind::Json { max_size: Some(10) },
            FieldKind::Array {
                items: ArrayItems::Text,
                min_items: Some(1),
                max_items: Some(2),
            },
            FieldKind::Relation {
                collection_id: "c".into(),
                max_select: 2,
                cascade_delete: true,
            },
            FieldKind::File { max_select: 2 },
            FieldKind::Password { min_length: 9 },
            FieldKind::Geo,
            FieldKind::Vector { dimensions: 3 },
        ];
        for kind in kinds {
            let mut f = Field::new("f", kind);
            f.default = Some(json!(null));
            let v = serde_json::to_value(&f).unwrap();
            assert_eq!(unknown_field_keys(&v), Vec::<String>::new(), "{v}");
            assert!(field_option_keys(f.kind.type_name()).is_some(), "{v}");
        }
    }

    #[test]
    fn unknown_field_keys_are_found() {
        let f = json!({"name": "t", "type": "text", "options": {"max_length": 5}, "max_lenght": 5, "max_length": 5});
        assert_eq!(
            unknown_field_keys(&f),
            vec!["max_lenght".to_owned(), "options".to_owned()]
        );
        let f = json!({"name": "s", "type": "select", "values": ["a"], "max_length": 5});
        assert_eq!(
            unknown_field_keys(&f),
            vec!["max_length".to_owned()],
            "an option of another type"
        );
    }

    #[test]
    fn definitions_round_trip_through_json() {
        let c: Collection = serde_json::from_value(json!({
            "name": "products",
            "schema_mode": "flexible",
            "fields": [
                {"name": "name", "type": "text", "required": true},
                {"name": "price", "type": "number", "min": 0},
                {"name": "tags", "type": "select", "values": ["a", "b"], "max_select": 2},
                {"name": "owner", "type": "relation", "collection_id": "users0000000001"}
            ]
        }))
        .unwrap();
        assert_eq!(c.kind, CollectionType::Base);
        assert_eq!(c.fields[1].kind.storage(), Storage::Real);
        assert!(c.fields[2].kind.is_multi());
        assert_eq!(c.fields[3].kind.storage(), Storage::Text);
        c.validate().unwrap();
        let back: Collection = serde_json::from_value(serde_json::to_value(&c).unwrap()).unwrap();
        assert_eq!(back, c);
    }

    #[test]
    fn names_are_checked() {
        assert!(validate_collection_name("posts", false).is_ok());
        assert!(validate_collection_name("_users", false).is_err());
        assert!(validate_collection_name("_users", true).is_ok());
        assert!(validate_collection_name("_forgebase_meta", true).is_err());
        assert!(validate_collection_name("sqlite_master", false).is_err());
        assert!(validate_collection_name("a\"; DROP TABLE x; --", false).is_err());
        assert!(validate_field_name("id").is_err());
        assert!(validate_field_name("Created").is_err());
        assert!(validate_field_name("_data").is_err());
        assert!(validate_field_name("9lives").is_err());
        assert!(validate_field_name("title").is_ok());
    }

    #[test]
    fn options_are_checked() {
        let dup = Collection::base("x")
            .with_field(Field::new("a", FieldKind::Bool))
            .with_field(Field::new("A", FieldKind::Bool));
        assert!(matches!(dup.validate(), Err(SchemaError::DuplicateField(_))));
        let bad_default = Collection::base("x").with_field(
            Field::new(
                "n",
                FieldKind::Number {
                    min: Some(0.0),
                    max: Some(10.0),
                    integer: true,
                },
            )
            .with_default(json!(11)),
        );
        assert!(bad_default.validate().is_err());
        let multi_unique = Collection::base("x").with_field(
            Field::new(
                "tags",
                FieldKind::Select {
                    values: vec!["a".into(), "b".into()],
                    max_select: 2,
                },
            )
            .unique(),
        );
        assert!(multi_unique.validate().is_err());
    }

    #[test]
    fn auth_collections_get_system_fields() {
        let mut c = Collection::base("users");
        c.kind = CollectionType::Auth;
        c.ensure_auth_fields();
        c.ensure_auth_fields();
        assert_eq!(c.fields.len(), 5);
        assert!(c.fields.iter().all(|f| f.system));
        assert!(c.field("email").unwrap().unique);
        c.validate().unwrap();
    }
    #[test]
    fn postgres_name_rules() {
        assert!(validate_postgres_collection_name("posts").is_ok());
        for bad in ["pg_class", "PG_stats", "pg_"] {
            assert_eq!(
                validate_postgres_collection_name(bad),
                Err(SchemaError::ReservedName(bad.into())),
                "{bad}"
            );
        }
        assert!(validate_postgres_field_name("title").is_ok());
        assert!(validate_postgres_field_name("xmin_total").is_ok());
        for bad in ["ctid", "XMIN", "tableoid", "cmax"] {
            assert!(validate_postgres_field_name(bad).is_err(), "{bad}");
        }
        assert!(validate_postgres_field_name(&"a".repeat(64)).is_err());

        // Only new names are checked against a stored definition, so an
        // existing SQLite collection keeps its names.
        let mut old = Collection::base("pg_notes").with_field(Field::new(
            "ctid",
            FieldKind::Text {
                min_length: None,
                max_length: None,
            },
        ));
        old.fields[0].id = "f1".into();
        assert!(old.validate_postgres_names(None).is_err());
        assert!(old.validate_postgres_names(Some(&old)).is_ok());
        let mut renamed = old.clone();
        renamed.fields[0].name = "xmax".into();
        assert!(renamed.validate_postgres_names(Some(&old)).is_err());
        let mut added = old.clone();
        added.fields.push(Field::new("cmin", FieldKind::Bool));
        assert!(added.validate_postgres_names(Some(&old)).is_err());
        let mut moved = old.clone();
        moved.name = "pg_other".into();
        assert!(moved.validate_postgres_names(Some(&old)).is_err());
    }
}

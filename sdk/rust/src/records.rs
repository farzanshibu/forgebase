//! Collections and records: CRUD, filtering, pagination, import/export.

use std::marker::PhantomData;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::error::{Error, Result};
use crate::generated::{CollectionSchema, ImportResult};
use crate::http::{Req, Transport, seg};

/// Query options for [`Records::list`].
#[derive(Debug, Clone, Default)]
pub struct ListOptions {
    filter: Option<String>,
    params: Option<Value>,
    sort: Vec<String>,
    expand: Vec<String>,
    fields: Vec<String>,
    search: Option<String>,
    page: Option<u32>,
    per_page: Option<u32>,
    cursor: Option<String>,
    skip_total: bool,
}

impl ListOptions {
    /// Default options (first page, server page size).
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
    /// Filter-language expression, e.g. `views > {:min} && status = "live"`.
    #[must_use]
    pub fn filter(mut self, f: impl Into<String>) -> Self {
        self.filter = Some(f.into());
        self
    }
    /// Typed values for `{:name}` placeholders in the filter (uses the JSON query endpoint).
    #[must_use]
    pub fn params(mut self, params: Value) -> Self {
        self.params = Some(params);
        self
    }
    /// Sort keys: `"-created"` is descending. Call repeatedly for several keys.
    #[must_use]
    pub fn sort(mut self, key: impl Into<String>) -> Self {
        self.sort.push(key.into());
        self
    }
    /// Relations to expand inline.
    #[must_use]
    pub fn expand(mut self, relation: impl Into<String>) -> Self {
        self.expand.push(relation.into());
        self
    }
    /// Restricts the returned fields. Call repeatedly.
    #[must_use]
    pub fn field(mut self, name: impl Into<String>) -> Self {
        self.fields.push(name.into());
        self
    }
    /// Full-text search term.
    #[must_use]
    pub fn search(mut self, term: impl Into<String>) -> Self {
        self.search = Some(term.into());
        self
    }
    /// 1-based page (offset pagination).
    #[must_use]
    pub fn page(mut self, page: u32) -> Self {
        self.page = Some(page);
        self
    }
    /// Page size (server maximum 500).
    #[must_use]
    pub fn per_page(mut self, n: u32) -> Self {
        self.per_page = Some(n);
        self
    }
    /// Cursor from a previous page's `next_cursor`.
    #[must_use]
    pub fn cursor(mut self, c: impl Into<String>) -> Self {
        self.cursor = Some(c.into());
        self
    }
    /// Skips the total count (cheaper on large collections).
    #[must_use]
    pub fn skip_total(mut self, on: bool) -> Self {
        self.skip_total = on;
        self
    }
}

/// One page of records.
#[derive(Debug, Clone)]
pub struct ListResult<T> {
    /// The records.
    pub items: Vec<T>,
    /// 1-based page number (offset pagination).
    pub page: Option<u32>,
    /// Page size.
    pub per_page: u32,
    /// Total matching records (absent with `skip_total`).
    pub total_items: Option<u64>,
    /// Total pages (absent with `skip_total`).
    pub total_pages: Option<u32>,
    /// Pass to [`ListOptions::cursor`] for the next page; `None` on the last page.
    pub next_cursor: Option<String>,
}

/// Optimistic concurrency for updates.
#[derive(Debug, Clone, Copy, Default)]
pub struct WriteOptions {
    /// Sends `If-Match` with the record's `_version`; a stale write fails with a conflict error.
    pub version: Option<u64>,
}

/// Import / export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransferFormat {
    /// One JSON array.
    Json,
    /// One JSON object per line.
    Ndjson,
    /// Comma-separated values.
    Csv,
}

impl TransferFormat {
    fn as_str(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Ndjson => "ndjson",
            Self::Csv => "csv",
        }
    }
}

/// Records of one collection: `client.collection("posts")`.
pub struct Records<T = Value> {
    t: Arc<Transport>,
    name: String,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Records<T> {
    fn clone(&self) -> Self {
        Self {
            t: self.t.clone(),
            name: self.name.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: DeserializeOwned> Records<T> {
    pub(crate) fn new(t: Arc<Transport>, name: &str) -> Self {
        Self {
            t,
            name: name.to_owned(),
            _marker: PhantomData,
        }
    }

    /// The same collection decoding records into your own type.
    #[must_use]
    pub fn typed<U: DeserializeOwned>(&self) -> Records<U> {
        Records {
            t: self.t.clone(),
            name: self.name.clone(),
            _marker: PhantomData,
        }
    }

    /// Collection name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    fn base(&self) -> String {
        format!("/api/v1/collections/{}", seg(&self.name))
    }

    /// One page. With [`ListOptions::params`] the typed JSON query endpoint is used, else a plain GET.
    pub async fn list(&self, o: &ListOptions) -> Result<ListResult<T>> {
        let sort = (!o.sort.is_empty()).then(|| o.sort.join(","));
        let wire: Value = if let Some(params) = &o.params {
            let body = json!({
                "filter": o.filter, "sort": sort, "search": o.search, "page": o.page, "per_page": o.per_page,
                "cursor": o.cursor, "skip_total": o.skip_total.then_some(true), "params": params,
                "expand": (!o.expand.is_empty()).then_some(&o.expand), "fields": (!o.fields.is_empty()).then_some(&o.fields),
            });
            self.t
                .value(
                    Req::post(format!("{}/records/query", self.base()))
                        .json_value(body)
                        .idempotent(),
                )
                .await?
        } else {
            let r = Req::get(format!("{}/records", self.base()))
                .q_opt("filter", o.filter.as_deref())
                .q_opt("sort", sort)
                .q_opt("search", o.search.as_deref())
                .q_opt("page", o.page)
                .q_opt("per_page", o.per_page)
                .q_opt("cursor", o.cursor.as_deref())
                .q_opt("skip_total", o.skip_total.then_some(true))
                .q_opt("expand", (!o.expand.is_empty()).then(|| o.expand.join(",")))
                .q_opt("fields", (!o.fields.is_empty()).then(|| o.fields.join(",")));
            self.t.value(r).await?
        };
        decode_list(&wire)
    }

    /// Every record, lazily: `let mut it = posts.iter(opts); while let Some(r) = it.next().await? { ... }`.
    /// Follows `next_cursor`, else page numbers.
    #[must_use]
    pub fn iter(&self, opts: ListOptions) -> RecordIter<T> {
        RecordIter {
            records: self.clone(),
            opts,
            buf: Vec::new().into_iter(),
            done: false,
            started: false,
        }
    }

    /// Collects every record (mind the memory use on big collections).
    pub async fn all(&self, opts: ListOptions) -> Result<Vec<T>> {
        let mut it = self.iter(opts);
        let mut out = Vec::new();
        while let Some(r) = it.next().await? {
            out.push(r);
        }
        Ok(out)
    }

    /// First record matching `filter`.
    pub async fn first(&self, filter: &str) -> Result<Option<T>> {
        Ok(self
            .list(&ListOptions::new().filter(filter).per_page(1).skip_total(true))
            .await?
            .items
            .into_iter()
            .next())
    }

    /// One record by id.
    pub async fn get(&self, id: &str) -> Result<T> {
        self.t
            .json(Req::get(format!("{}/records/{}", self.base(), seg(id))))
            .await
    }

    /// One record with relations expanded.
    pub async fn get_expanded(&self, id: &str, expand: &[&str]) -> Result<T> {
        self.t
            .json(Req::get(format!("{}/records/{}", self.base(), seg(id))).q("expand", expand.join(",")))
            .await
    }

    /// Creates a record.
    pub async fn create<B: Serialize + ?Sized>(&self, data: &B) -> Result<T> {
        self.t
            .json(Req::post(format!("{}/records", self.base())).json(data)?)
            .await
    }

    /// Partial update (PATCH).
    pub async fn update<B: Serialize + ?Sized>(&self, id: &str, patch: &B, o: WriteOptions) -> Result<T> {
        self.t
            .json(
                self.write(Req::patch(format!("{}/records/{}", self.base(), seg(id))), o)
                    .json(patch)?,
            )
            .await
    }

    /// Full replace (PUT).
    pub async fn replace<B: Serialize + ?Sized>(&self, id: &str, data: &B, o: WriteOptions) -> Result<T> {
        self.t
            .json(
                self.write(Req::put(format!("{}/records/{}", self.base(), seg(id))), o)
                    .json(data)?,
            )
            .await
    }

    /// Deletes a record.
    pub async fn delete(&self, id: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("{}/records/{}", self.base(), seg(id))))
            .await
    }

    /// Number of records matching `filter` (all when `None`).
    pub async fn count(&self, filter: Option<&str>) -> Result<u64> {
        let v = self
            .t
            .value(Req::get(format!("{}/records/count", self.base())).q_opt("filter", filter))
            .await?;
        v.get("count")
            .and_then(Value::as_u64)
            .ok_or_else(|| Error::InvalidResponse("count response has no `count`".into()))
    }

    /// Group-by aggregates: `{"aggregates":[{"function":"sum","field":"views"}],"group_by":["status"]}`.
    pub async fn aggregate(&self, spec: Value) -> Result<Vec<Value>> {
        let v = self
            .t
            .value(
                Req::post(format!("{}/aggregate", self.base()))
                    .json_value(spec)
                    .idempotent(),
            )
            .await?;
        Ok(v.get("items").and_then(Value::as_array).cloned().unwrap_or_default())
    }

    /// Downloads the collection as text.
    pub async fn export(&self, format: TransferFormat, filter: Option<&str>) -> Result<String> {
        self.t
            .text(
                Req::get(format!("{}/export", self.base()))
                    .q("format", format.as_str())
                    .q_opt("filter", filter),
            )
            .await
    }

    /// Uploads records as text in the given format.
    pub async fn import(&self, format: TransferFormat, body: impl Into<bytes::Bytes>) -> Result<ImportResult> {
        let req = Req::post(format!("{}/import", self.base()))
            .q("format", format.as_str())
            .bytes(body.into(), "application/octet-stream");
        self.t.json(req).await
    }

    fn write(&self, r: Req, o: WriteOptions) -> Req {
        match o.version {
            Some(v) => r.header("If-Match", format!("\"{v}\"")),
            None => r,
        }
    }
}

fn decode_list<T: DeserializeOwned>(w: &Value) -> Result<ListResult<T>> {
    let items = w.get("items").cloned().unwrap_or(Value::Array(Vec::new()));
    let items: Vec<T> =
        serde_json::from_value(items).map_err(|e| Error::InvalidResponse(format!("record list: {e}")))?;
    let n = |k: &str| w.get(k).and_then(Value::as_u64);
    Ok(ListResult {
        items,
        page: n("page").and_then(|v| u32::try_from(v).ok()),
        per_page: n("per_page").and_then(|v| u32::try_from(v).ok()).unwrap_or(0),
        total_items: n("total_items"),
        total_pages: n("total_pages").and_then(|v| u32::try_from(v).ok()),
        next_cursor: w.get("next_cursor").and_then(Value::as_str).map(str::to_owned),
    })
}

/// Lazy iterator over every record of a query. See [`Records::iter`].
pub struct RecordIter<T> {
    records: Records<T>,
    opts: ListOptions,
    buf: std::vec::IntoIter<T>,
    done: bool,
    started: bool,
}

impl<T: DeserializeOwned> RecordIter<T> {
    /// The next record, fetching the next page when needed; `None` at the end.
    pub async fn next(&mut self) -> Result<Option<T>> {
        loop {
            if let Some(r) = self.buf.next() {
                return Ok(Some(r));
            }
            if self.done {
                return Ok(None);
            }
            let mut o = self.opts.clone();
            if !self.started {
                self.started = true;
                if o.cursor.is_none() && o.page.is_none() {
                    o.page = Some(1);
                }
            }
            let page = self.records.list(&o).await?;
            if let Some(c) = &page.next_cursor {
                self.opts.cursor = Some(c.clone());
                self.opts.page = None;
            } else if self.opts.cursor.is_none() && matches!((page.page, page.total_pages), (Some(p), Some(t)) if p < t)
            {
                self.opts.page = page.page.map(|p| p + 1);
            } else {
                self.done = true;
            }
            self.buf = page.items.into_iter();
        }
    }
}

/// Collection definitions: `client.collections()`.
#[derive(Clone)]
pub struct CollectionsApi {
    pub(crate) t: Arc<Transport>,
}

impl CollectionsApi {
    /// Every collection.
    pub async fn list(&self) -> Result<Vec<CollectionSchema>> {
        let v = self.t.value(Req::get("/api/v1/collections")).await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or(Value::Array(Vec::new())))
            .map_err(|e| Error::InvalidResponse(format!("collections: {e}")))
    }

    /// One collection's definition.
    pub async fn get(&self, name: &str) -> Result<CollectionSchema> {
        self.t
            .json(Req::get(format!("/api/v1/collections/{}", seg(name))))
            .await
    }

    /// Creates a collection. `def` is the definition as JSON, for example
    /// `{"name": "posts", "type": "base", "fields": [{"name": "title", "type": "text", "required": true}]}`.
    /// (It is not typed: the request schema in the OpenAPI document is the full [`CollectionSchema`], which
    /// also carries server-assigned fields such as `id` and `version`.)
    pub async fn create<B: Serialize + ?Sized>(&self, def: &B) -> Result<CollectionSchema> {
        self.t.json(Req::post("/api/v1/collections").json(def)?).await
    }

    /// Replaces a collection's definition (schema migration).
    pub async fn update<B: Serialize + ?Sized>(&self, name: &str, def: &B) -> Result<CollectionSchema> {
        self.t
            .json(Req::put(format!("/api/v1/collections/{}", seg(name))).json(def)?)
            .await
    }

    /// Deletes a collection and its records.
    pub async fn delete(&self, name: &str) -> Result<()> {
        self.t
            .unit(Req::delete(format!("/api/v1/collections/{}", seg(name))))
            .await
    }
}

//! Streamed import and export of records as JSON, NDJSON or CSV.
//!
//! **Export** pages through the collection with keyset cursors (stable under
//! concurrent writes, memory bounded to one page of 500 records) and streams
//! the response; the listing options of `GET .../records` (`filter`, `p.*`,
//! `sort`, `fields`) apply. The first page is fetched before the response
//! starts, so authorisation and query errors are normal error responses; a
//! failure after that aborts the connection, which leaves the output visibly
//! incomplete (an unterminated JSON array, a missing final newline).
//!
//! **Import** reads the request body incrementally, parses records as they
//! arrive and stores them in batches of 200 (each batch one all-or-nothing
//! transaction). The import as a whole is *not* atomic: on the first failing
//! record the import stops, earlier batches stay committed, and the error's
//! `details` say how many records were `imported` and which record failed.
//! Records keep their `id` when they have one; `created`/`updated` are reset.
//!
//! CSV: a header row names the columns; cells are converted by the field's
//! type (numbers, booleans; JSON text for `json`, `array`, `geo`, `vector` and
//! multi-value `select`/`relation`/`file`); an empty cell leaves the field
//! unset (its default applies). Export writes `id,created,updated` and the
//! declared fields (hidden ones for administrators only, never passwords); in
//! flexible collections the undeclared keys travel as JSON in an `_extra`
//! column.

use std::io;

use axum::body::{Body, Bytes};
use axum::extract::{Path, Request, State};
use axum::http::StatusCode;
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_LENGTH, CONTENT_TYPE, HeaderValue};
use axum::response::{IntoResponse, Response};
use forgebase_data::schema::{Collection, FieldKind, SchemaMode, Storage};
use forgebase_data::{AccessContext, DataError, DataService, ListQuery};
use futures_util::stream;
use http_body_util::BodyExt;
use serde_json::{Map, Value as Json, json};

use super::ApiResult;
use super::schemas::ImportResult;
use crate::AppState;
use crate::error::ApiError;
use crate::http::Query;
use crate::principal::Access;

const EXPORT_PAGE: u32 = 500;
const FIRST_BATCH: usize = 1;
const BATCH: usize = 200;
/// Largest single record or CSV field accepted on import.
const MAX_ELEMENT: usize = 8 * 1024 * 1024;
const MAX_COLUMNS: usize = 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Format {
    Json,
    Ndjson,
    Csv,
}

impl Format {
    fn parse(s: &str) -> Result<Self, ApiError> {
        match s {
            "json" => Ok(Self::Json),
            "ndjson" | "jsonl" => Ok(Self::Ndjson),
            "csv" => Ok(Self::Csv),
            other => Err(ApiError::bad_request(
                "invalid_format",
                format!("unknown format {other:?}; use json, ndjson or csv"),
            )),
        }
    }

    fn name(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Ndjson => "ndjson",
            Self::Csv => "csv",
        }
    }

    fn content_type(self) -> &'static str {
        match self {
            Self::Json => "application/json",
            Self::Ndjson => "application/x-ndjson",
            Self::Csv => "text/csv; charset=utf-8",
        }
    }
}

// ---------------------------------------------------------------- export

fn csv_cell(v: &Json) -> String {
    let text = match v {
        Json::Null => String::new(),
        Json::String(s) => s.clone(),
        Json::Bool(b) => b.to_string(),
        Json::Number(n) => n.to_string(),
        other => other.to_string(),
    };
    if text.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", text.replace('"', "\"\""))
    } else {
        text
    }
}

fn csv_line(cells: impl IntoIterator<Item = String>) -> String {
    let mut line = cells.into_iter().collect::<Vec<_>>().join(",");
    line.push('\n');
    line
}

fn csv_columns(
    coll: Option<&Collection>,
    privileged: bool,
    requested: &[String],
    first: &[Map<String, Json>],
) -> Vec<String> {
    if !requested.is_empty() {
        return requested.to_vec();
    }
    let mut cols: Vec<String> = vec!["id".into(), "created".into(), "updated".into()];
    match coll {
        Some(c) => {
            cols.extend(
                c.fields
                    .iter()
                    .filter(|f| !matches!(f.kind, FieldKind::Password { .. }) && (privileged || !f.hidden))
                    .map(|f| f.name.clone()),
            );
            if c.schema_mode == SchemaMode::Flexible {
                cols.push("_extra".into());
            }
        }
        None => {
            let mut extra: Vec<String> = first
                .iter()
                .flat_map(|r| r.keys().cloned())
                .filter(|k| !cols.contains(k) && k != "_version")
                .collect();
            extra.sort();
            extra.dedup();
            cols.extend(extra);
        }
    }
    cols
}

fn csv_row(rec: &Map<String, Json>, cols: &[String], coll: Option<&Collection>) -> String {
    csv_line(cols.iter().map(|c| {
        if c == "_extra" && !rec.contains_key("_extra") {
            let extra: Map<String, Json> = rec
                .iter()
                .filter(|(k, _)| {
                    !matches!(k.as_str(), "id" | "created" | "updated" | "_version" | "expand")
                        && coll.is_none_or(|c| c.field(k).is_none())
                })
                .map(|(k, v)| (k.clone(), v.clone()))
                .collect();
            if extra.is_empty() {
                String::new()
            } else {
                csv_cell(&Json::Object(extra))
            }
        } else {
            rec.get(c).map(csv_cell).unwrap_or_default()
        }
    }))
}

struct ExportState {
    data: DataService,
    ctx: AccessContext,
    collection: String,
    query: ListQuery,
    format: Format,
    coll: Option<Collection>,
    columns: Vec<String>,
    page: Option<forgebase_data::ListResult>,
    next_cursor: Option<String>,
    first: bool,
    wrote_item: bool,
}

async fn export_step(mut st: ExportState) -> io::Result<Option<(Bytes, ExportState)>> {
    if st.page.is_none() {
        let Some(cursor) = st.next_cursor.take() else {
            return Ok(None);
        };
        st.query.cursor = Some(cursor);
        match st.data.list_records(&st.ctx, &st.collection, &st.query).await {
            Ok(p) => st.page = Some(p),
            Err(e) => {
                tracing::error!(error = %e, "export failed after the response started");
                return Err(io::Error::other("export failed"));
            }
        }
    }
    let Some(page) = st.page.take() else {
        return Ok(None);
    };
    let mut out = String::new();
    if st.first {
        match st.format {
            Format::Json => out.push('['),
            Format::Csv => out.push_str(&csv_line(st.columns.iter().map(|c| csv_cell(&Json::String(c.clone()))))),
            Format::Ndjson => {}
        }
        st.first = false;
    }
    for rec in &page.items {
        match st.format {
            Format::Json => {
                if st.wrote_item {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(rec).map_err(io::Error::other)?);
            }
            Format::Ndjson => {
                out.push_str(&serde_json::to_string(rec).map_err(io::Error::other)?);
                out.push('\n');
            }
            Format::Csv => out.push_str(&csv_row(rec, &st.columns, st.coll.as_ref())),
        }
        st.wrote_item = true;
    }
    st.next_cursor = page.next_cursor;
    if st.next_cursor.is_none() && st.format == Format::Json {
        out.push_str("]\n");
    }
    Ok(Some((Bytes::from(out), st)))
}

/// Streams a collection as JSON, NDJSON or CSV.
#[utoipa::path(
    get, path = "/api/v1/collections/{collection}/export", tag = "import-export",
    params(("collection" = String, Path, description = "Collection name or id")),
    description = "Query string: `format` (`json` default, `ndjson`, `csv`), plus `filter`, `p.<name>`, `sort`, `fields`. \
        JSON is one array of records. The response is streamed page by page.",
    responses((status = 200, description = "The records, as an attachment", content_type = "application/json"))
)]
pub async fn export(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    q: Query,
) -> ApiResult<Response> {
    let format = Format::parse(q.get("format").unwrap_or("json"))?;
    let mut query = q.list_query()?;
    query.expand = Vec::new();
    query.search = None;
    query.page = None;
    query.cursor = None;
    query.per_page = Some(EXPORT_PAGE);
    query.skip_total = true;
    let requested = query.fields.clone().unwrap_or_default();
    // Schema access is optional here: without it CSV columns come from the data.
    let coll = s.data().get_collection(&ctx, &c).await.ok();
    let first_page = s.data().list_records(&ctx, &c, &query).await?;
    let columns = if format == Format::Csv {
        csv_columns(coll.as_ref(), ctx.is_privileged(), &requested, &first_page.items)
    } else {
        Vec::new()
    };
    let name = coll.as_ref().map_or(c.as_str(), |c| c.name.as_str()).to_owned();
    let state = ExportState {
        data: s.data().clone(),
        ctx,
        collection: c,
        query,
        format,
        coll,
        columns,
        page: Some(first_page),
        next_cursor: None,
        first: true,
        wrote_item: false,
    };
    let body = Body::from_stream(stream::try_unfold(state, export_step));
    let mut res = Response::new(body);
    res.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static(format.content_type()));
    let ext = if format == Format::Csv {
        "csv"
    } else if format == Format::Ndjson {
        "ndjson"
    } else {
        "json"
    };
    if let Ok(v) = HeaderValue::from_str(&format!("attachment; filename=\"{name}.{ext}\"")) {
        res.headers_mut().insert(CONTENT_DISPOSITION, v);
    }
    Ok(res)
}

// ---------------------------------------------------------------- import parsers

fn parse_error(row: u64, msg: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request("invalid_import", format!("record {row}: {msg}"))
}

fn parse_object(bytes: &[u8], row: u64) -> Result<Json, ApiError> {
    match serde_json::from_slice::<Json>(bytes) {
        Ok(v @ Json::Object(_)) => Ok(v),
        Ok(_) => Err(parse_error(row, "expected a JSON object")),
        Err(e) => Err(parse_error(row, format!("invalid JSON ({e})"))),
    }
}

#[derive(Default)]
struct Ndjson {
    buf: Vec<u8>,
    rows: u64,
}

impl Ndjson {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Json>) -> Result<(), ApiError> {
        self.buf.extend_from_slice(bytes);
        while let Some(pos) = self.buf.iter().position(|b| *b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            self.line(&line, out)?;
        }
        if self.buf.len() > MAX_ELEMENT {
            return Err(parse_error(self.rows + 1, "line is too long"));
        }
        Ok(())
    }

    fn line(&mut self, line: &[u8], out: &mut Vec<Json>) -> Result<(), ApiError> {
        if line.iter().all(u8::is_ascii_whitespace) {
            return Ok(());
        }
        self.rows += 1;
        out.push(parse_object(line, self.rows)?);
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Json>) -> Result<(), ApiError> {
        let rest = std::mem::take(&mut self.buf);
        self.line(&rest, out)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ArrState {
    Start,
    First,
    Next,
    Object,
    After,
    End,
}

struct JsonArray {
    state: ArrState,
    cur: Vec<u8>,
    depth: u32,
    in_str: bool,
    esc: bool,
    rows: u64,
}

impl JsonArray {
    fn new() -> Self {
        Self {
            state: ArrState::Start,
            cur: Vec::new(),
            depth: 0,
            in_str: false,
            esc: false,
            rows: 0,
        }
    }

    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Json>) -> Result<(), ApiError> {
        for &b in bytes {
            match self.state {
                ArrState::Object => {
                    self.cur.push(b);
                    if self.cur.len() > MAX_ELEMENT {
                        return Err(parse_error(self.rows + 1, "record is too large"));
                    }
                    if self.in_str {
                        if self.esc {
                            self.esc = false;
                        } else if b == b'\\' {
                            self.esc = true;
                        } else if b == b'"' {
                            self.in_str = false;
                        }
                    } else if b == b'"' {
                        self.in_str = true;
                    } else if b == b'{' {
                        self.depth += 1;
                    } else if b == b'}' {
                        self.depth -= 1;
                        if self.depth == 0 {
                            self.rows += 1;
                            out.push(parse_object(&self.cur, self.rows)?);
                            self.state = ArrState::After;
                        }
                    }
                }
                _ if b.is_ascii_whitespace() => {}
                ArrState::Start if b == b'[' => self.state = ArrState::First,
                ArrState::Start => {
                    return Err(ApiError::bad_request(
                        "invalid_import",
                        "expected a JSON array of objects (use format=ndjson for one object per line)",
                    ));
                }
                ArrState::First | ArrState::Next if b == b'{' => {
                    self.cur.clear();
                    self.cur.push(b);
                    self.depth = 1;
                    self.in_str = false;
                    self.esc = false;
                    self.state = ArrState::Object;
                }
                ArrState::First if b == b']' => self.state = ArrState::End,
                ArrState::After if b == b',' => self.state = ArrState::Next,
                ArrState::After if b == b']' => self.state = ArrState::End,
                ArrState::End => {
                    return Err(ApiError::bad_request(
                        "invalid_import",
                        "unexpected data after the array",
                    ));
                }
                _ => {
                    return Err(parse_error(self.rows + 1, "expected a JSON object"));
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self) -> Result<(), ApiError> {
        if self.state == ArrState::End {
            Ok(())
        } else {
            Err(ApiError::bad_request(
                "invalid_import",
                "the JSON array is not complete",
            ))
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum CsvState {
    FieldStart,
    Unquoted,
    Quoted,
    Quote,
}

struct Csv {
    coll: Option<Collection>,
    state: CsvState,
    field: Vec<u8>,
    row: Vec<String>,
    header: Option<Vec<String>>,
    records: u64,
    lines: u64,
}

impl Csv {
    fn new(coll: Option<Collection>) -> Self {
        Self {
            coll,
            state: CsvState::FieldStart,
            field: Vec::new(),
            row: Vec::new(),
            header: None,
            records: 0,
            lines: 1,
        }
    }

    fn end_field(&mut self) -> Result<(), ApiError> {
        let bytes = std::mem::take(&mut self.field);
        let s = String::from_utf8(bytes)
            .map_err(|_| ApiError::bad_request("invalid_import", format!("line {}: invalid UTF-8", self.lines)))?;
        if self.row.len() >= MAX_COLUMNS {
            return Err(ApiError::bad_request("invalid_import", "too many columns"));
        }
        self.row.push(s);
        self.state = CsvState::FieldStart;
        Ok(())
    }

    fn end_record(&mut self, out: &mut Vec<Json>) -> Result<(), ApiError> {
        self.end_field()?;
        let mut row = std::mem::take(&mut self.row);
        if row.len() == 1 && row[0].is_empty() {
            return Ok(()); // blank line
        }
        match &self.header {
            None => {
                if let Some(first) = row.first_mut() {
                    *first = first.trim_start_matches('\u{feff}').to_owned();
                }
                let mut seen = std::collections::HashSet::new();
                for h in &row {
                    if h.is_empty() || !seen.insert(h.clone()) {
                        return Err(ApiError::bad_request(
                            "invalid_import",
                            "the CSV header must name every column once",
                        ));
                    }
                }
                self.header = Some(row);
            }
            Some(header) => {
                self.records += 1;
                if row.len() != header.len() {
                    return Err(parse_error(
                        self.records,
                        format!("has {} columns, the header has {}", row.len(), header.len()),
                    ));
                }
                out.push(self.convert(header, &row, self.records)?);
            }
        }
        Ok(())
    }

    fn convert(&self, header: &[String], row: &[String], n: u64) -> Result<Json, ApiError> {
        let mut obj = Map::new();
        for (name, cell) in header.iter().zip(row) {
            if cell.is_empty() {
                continue;
            }
            if name == "_extra" {
                match serde_json::from_str::<Json>(cell) {
                    Ok(Json::Object(m)) => {
                        for (k, v) in m {
                            obj.entry(k).or_insert(v);
                        }
                    }
                    _ => return Err(parse_error(n, "_extra must hold a JSON object")),
                }
                continue;
            }
            let kind = self.coll.as_ref().and_then(|c| c.field(name)).map(|f| &f.kind);
            obj.insert(name.clone(), typed_cell(kind, cell));
        }
        Ok(Json::Object(obj))
    }

    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Json>) -> Result<(), ApiError> {
        for &b in bytes {
            if b == b'\n' {
                self.lines += 1;
            }
            if self.field.len() > MAX_ELEMENT {
                return Err(ApiError::bad_request(
                    "invalid_import",
                    format!("line {}: field is too large", self.lines),
                ));
            }
            match (self.state, b) {
                (CsvState::Quoted, b'"') => self.state = CsvState::Quote,
                (CsvState::Quoted, _) => self.field.push(b),
                (CsvState::Quote, b'"') => {
                    self.field.push(b'"');
                    self.state = CsvState::Quoted;
                }
                (CsvState::FieldStart, b'"') => self.state = CsvState::Quoted,
                (_, b',') => self.end_field()?,
                (_, b'\n') => self.end_record(out)?,
                (_, b'\r') => {}
                (CsvState::Quote, _) => {
                    return Err(ApiError::bad_request(
                        "invalid_import",
                        format!("line {}: unexpected character after a closing quote", self.lines),
                    ));
                }
                (_, _) => {
                    self.field.push(b);
                    self.state = CsvState::Unquoted;
                }
            }
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<Json>) -> Result<(), ApiError> {
        if self.state == CsvState::Quoted {
            return Err(ApiError::bad_request(
                "invalid_import",
                "a quoted CSV field is not closed",
            ));
        }
        if self.state != CsvState::FieldStart || !self.field.is_empty() || !self.row.is_empty() {
            self.end_record(out)?;
        }
        if self.header.is_none() {
            return Err(ApiError::bad_request("invalid_import", "the CSV has no header row"));
        }
        Ok(())
    }
}

/// A CSV cell as JSON, by field type. A cell that does not parse stays a
/// string, so the data core reports it as a normal field validation error.
fn typed_cell(kind: Option<&FieldKind>, cell: &str) -> Json {
    let as_json = || serde_json::from_str::<Json>(cell).unwrap_or_else(|_| Json::String(cell.to_owned()));
    let Some(kind) = kind else {
        return Json::String(cell.to_owned());
    };
    match kind {
        FieldKind::Number { .. } => cell
            .parse::<i64>()
            .map(Json::from)
            .or_else(|_| {
                cell.parse::<f64>()
                    .map_err(|_| ())
                    .and_then(|f| serde_json::Number::from_f64(f).map(Json::Number).ok_or(()))
            })
            .unwrap_or_else(|()| Json::String(cell.to_owned())),
        FieldKind::Bool => match cell {
            "true" | "1" => Json::Bool(true),
            "false" | "0" => Json::Bool(false),
            _ => Json::String(cell.to_owned()),
        },
        FieldKind::Select { max_select, .. }
        | FieldKind::Relation { max_select, .. }
        | FieldKind::File { max_select } => {
            if *max_select > 1 {
                as_json()
            } else {
                Json::String(cell.to_owned())
            }
        }
        FieldKind::Json { .. } | FieldKind::Array { .. } | FieldKind::Geo | FieldKind::Vector { .. } => as_json(),
        _ => {
            debug_assert!(!matches!(kind.storage(), Storage::Json));
            Json::String(cell.to_owned())
        }
    }
}

enum Parser {
    Ndjson(Ndjson),
    Json(JsonArray),
    Csv(Box<Csv>),
}

impl Parser {
    fn feed(&mut self, bytes: &[u8], out: &mut Vec<Json>) -> Result<(), ApiError> {
        match self {
            Parser::Ndjson(p) => p.feed(bytes, out),
            Parser::Json(p) => p.feed(bytes, out),
            Parser::Csv(p) => p.feed(bytes, out),
        }
    }

    fn finish(&mut self, out: &mut Vec<Json>) -> Result<(), ApiError> {
        match self {
            Parser::Ndjson(p) => p.finish(out),
            Parser::Json(p) => p.finish(),
            Parser::Csv(p) => p.finish(out),
        }
    }
}

// ---------------------------------------------------------------- import

fn with_imported(err: ApiError, imported: u64) -> ApiError {
    let mut details = match err.details() {
        Some(Json::Object(m)) => m.clone(),
        _ => Map::new(),
    };
    details.insert("imported".into(), json!(imported));
    err.with_details(Json::Object(details))
}

async fn flush(
    data: &DataService,
    ctx: &AccessContext,
    collection: &str,
    pending: &mut Vec<Json>,
    limit: usize,
    imported: &mut u64,
) -> Result<(), ApiError> {
    let n = pending.len().min(limit);
    let batch: Vec<Json> = pending.drain(..n).collect();
    let start = *imported;
    match data.create_records(ctx, collection, batch).await {
        Ok(recs) => {
            *imported += recs.len() as u64;
            Ok(())
        }
        Err(DataError::Validation(mut v)) => {
            let multi = n > 1;
            v.fields = std::mem::take(&mut v.fields)
                .into_iter()
                .map(|(k, e)| {
                    let (idx, rest) = match k
                        .split_once('.')
                        .and_then(|(i, r)| i.parse::<u64>().ok().map(|i| (i, r)))
                    {
                        Some((i, r)) if multi => (i, r.to_owned()),
                        _ => (0, k.clone()),
                    };
                    (format!("record {}: {rest}", start + idx + 1), e)
                })
                .collect();
            Err(with_imported(DataError::Validation(v).into(), start))
        }
        Err(e) => Err(with_imported(e.into(), start)),
    }
}

/// Imports records from a JSON array, NDJSON or CSV body (streamed).
#[utoipa::path(
    post, path = "/api/v1/collections/{collection}/import", tag = "import-export",
    params(("collection" = String, Path, description = "Collection name or id")),
    description = "Query string: `format` = `json` (array of objects), `ndjson` or `csv`; defaults from \
        `Content-Type` (`text/csv`, `application/x-ndjson`), else `json`. Stored in batches of 200, each \
        all-or-nothing; the import stops at the first failing record and earlier batches stay committed \
        (`error.details.imported` counts them).",
    request_body(content = Object, description = "Records in the chosen format", content_type = "application/octet-stream"),
    responses(
        (status = 200, description = "Records stored", body = ImportResult),
        (status = 413, description = "Body larger than the import limit"),
    )
)]
pub async fn import(
    State(s): State<AppState>,
    Access(ctx): Access,
    Path(c): Path<String>,
    q: Query,
    req: Request,
) -> ApiResult<Response> {
    let content_type = req
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let format = match q.get("format") {
        Some(f) => Format::parse(f)?,
        None if content_type.starts_with("text/csv") => Format::Csv,
        None if content_type.contains("ndjson") || content_type.contains("jsonl") => Format::Ndjson,
        None => Format::Json,
    };
    let max = s.limits().max_import_bytes;
    let declared = req
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if declared.is_some_and(|n| n > max) {
        return Err(too_large(max));
    }
    let mut parser = match format {
        Format::Ndjson => Parser::Ndjson(Ndjson::default()),
        Format::Json => Parser::Json(JsonArray::new()),
        Format::Csv => {
            let coll = s.data().get_collection(&ctx, &c).await?;
            Parser::Csv(Box::new(Csv::new(Some(coll))))
        }
    };

    let mut body = req.into_body();
    let mut pending: Vec<Json> = Vec::new();
    let mut imported = 0u64;
    let mut limit = FIRST_BATCH;
    let mut total = 0u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|e| {
            tracing::debug!(error = %e, "import body read failed");
            with_imported(
                ApiError::bad_request("invalid_body", "the request body could not be read"),
                imported,
            )
        })?;
        let Ok(data) = frame.into_data() else { continue };
        total += data.len() as u64;
        if total > max {
            return Err(with_imported(too_large(max), imported));
        }
        parser
            .feed(&data, &mut pending)
            .map_err(|e| with_imported(e, imported))?;
        while pending.len() >= limit {
            flush(s.data(), &ctx, &c, &mut pending, limit, &mut imported).await?;
            limit = BATCH;
        }
    }
    parser.finish(&mut pending).map_err(|e| with_imported(e, imported))?;
    while !pending.is_empty() {
        flush(s.data(), &ctx, &c, &mut pending, BATCH, &mut imported).await?;
    }
    Ok((
        StatusCode::OK,
        axum::Json(json!({ "imported": imported, "format": format.name() })),
    )
        .into_response())
}

fn too_large(max: u64) -> ApiError {
    ApiError::new(
        StatusCode::PAYLOAD_TOO_LARGE,
        "payload_too_large",
        format!("import bodies are limited to {max} bytes"),
    )
}

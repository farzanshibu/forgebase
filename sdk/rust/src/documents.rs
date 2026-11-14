//! Documents (`/api/v1/documents/{collection}`): the record operations of a document collection,
//! `client.documents()`.
//!
//! ```no_run
//! # async fn run(fb: forgebase_sdk::ForgeBase) -> forgebase_sdk::Result<()> {
//! use serde_json::json;
//! let notes = fb.documents().collection("notes");
//! let n = notes.create(&json!({"title": "Hi"})).await?;
//! let id = n["id"].as_str().unwrap_or_default().to_owned();
//! notes.patch(&id, &json!({"done": true}), Default::default()).await?;
//! # Ok(()) }
//! ```

use std::marker::PhantomData;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};

use crate::consistency::Consistency;
use crate::error::{Error, Result};
use crate::generated::{RecordVersionSchema, RecordVersions};
use crate::http::{Req, Transport, seg};
use crate::records::WriteOptions;

/// A stored version of a document (collections with `versioned: true`).
pub type DocumentVersion = RecordVersionSchema;

/// Options of [`DocumentCollection::get`].
#[derive(Debug, Clone, Default)]
pub struct GetDocumentOptions {
    /// Relations to expand inline.
    pub expand: Vec<String>,
    /// Fields to return.
    pub fields: Vec<String>,
    /// Consistency of this read.
    pub consistency: Option<Consistency>,
}

/// Document collections: `client.documents()`.
#[derive(Clone)]
pub struct DocumentsApi {
    pub(crate) t: Arc<Transport>,
}

impl DocumentsApi {
    /// One collection, as JSON objects: `fb.documents().collection("notes").get(id, ..)`.
    #[must_use]
    pub fn collection(&self, name: &str) -> DocumentCollection<Value> {
        DocumentCollection {
            t: self.t.clone(),
            name: name.to_owned(),
            _marker: PhantomData,
        }
    }
}

/// The documents of one collection.
pub struct DocumentCollection<T = Value> {
    t: Arc<Transport>,
    name: String,
    _marker: PhantomData<fn() -> T>,
}

impl<T> Clone for DocumentCollection<T> {
    fn clone(&self) -> Self {
        Self {
            t: self.t.clone(),
            name: self.name.clone(),
            _marker: PhantomData,
        }
    }
}

impl<T: DeserializeOwned> DocumentCollection<T> {
    /// The same collection decoding documents into your own type.
    #[must_use]
    pub fn typed<U: DeserializeOwned>(&self) -> DocumentCollection<U> {
        DocumentCollection {
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

    fn path(&self, id: Option<&str>, suffix: &str) -> String {
        let base = format!("/api/v1/documents/{}", seg(&self.name));
        match id {
            Some(id) => format!("{base}/{}{suffix}", seg(id)),
            None => base,
        }
    }

    fn write(r: Req, o: WriteOptions) -> Req {
        match o.version {
            Some(v) => r.header("If-Match", format!("\"{v}\"")),
            None => r,
        }
    }

    /// Creates one document.
    pub async fn create<B: Serialize + ?Sized>(&self, doc: &B) -> Result<T> {
        self.t.json(Req::post(self.path(None, "")).json(doc)?).await
    }

    /// Creates up to 1000 documents in one all-or-nothing transaction.
    pub async fn create_many<B: Serialize>(&self, docs: &[B]) -> Result<Vec<T>> {
        let v = self.t.value(Req::post(self.path(None, "")).json(docs)?).await?;
        serde_json::from_value(v.get("items").cloned().unwrap_or_else(|| json!([])))
            .map_err(|e| Error::InvalidResponse(format!("documents: {e}")))
    }

    /// One document.
    pub async fn get(&self, id: &str, o: &GetDocumentOptions) -> Result<T> {
        let mut r = Req::get(self.path(Some(id), ""));
        if !o.expand.is_empty() {
            r = r.q("expand", o.expand.join(","));
        }
        if !o.fields.is_empty() {
            r = r.q("fields", o.fields.join(","));
        }
        r.consistency = o.consistency;
        self.t.json(r).await
    }

    /// Merge patch (RFC 7386): objects merge, `null` removes a field. `o.version` sends `If-Match`.
    pub async fn patch<B: Serialize + ?Sized>(&self, id: &str, patch: &B, o: WriteOptions) -> Result<T> {
        self.t
            .json(Self::write(Req::patch(self.path(Some(id), "")), o).json(patch)?)
            .await
    }

    /// Replaces the whole document.
    pub async fn replace<B: Serialize + ?Sized>(&self, id: &str, doc: &B, o: WriteOptions) -> Result<T> {
        self.t
            .json(Self::write(Req::put(self.path(Some(id), "")), o).json(doc)?)
            .await
    }

    /// Deletes a document.
    pub async fn delete(&self, id: &str, o: WriteOptions) -> Result<()> {
        self.t.unit(Self::write(Req::delete(self.path(Some(id), "")), o)).await
    }

    /// Stored versions of a document, oldest first (collections with `versioned: true`).
    pub async fn versions(&self, id: &str) -> Result<Vec<DocumentVersion>> {
        let r: RecordVersions = self.t.json(Req::get(self.path(Some(id), "/versions"))).await?;
        Ok(r.items)
    }
}

//! Wire and storage types of the storage service.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value as Json;
use utoipa::ToSchema;

use crate::error::{Result, StorageError};

/// Who may read a bucket without an explicit grant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    /// Only the owner, administrators, platform permissions and ACL grants.
    #[default]
    Private,
    /// Any signed-in caller may read.
    Authenticated,
    /// Anyone may read, without credentials.
    Public,
}

impl Visibility {
    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Visibility::Private => "private",
            Visibility::Authenticated => "authenticated",
            Visibility::Public => "public",
        }
    }

    /// Parses the stored form.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "private" => Ok(Self::Private),
            "authenticated" => Ok(Self::Authenticated),
            "public" => Ok(Self::Public),
            _ => Err(StorageError::invalid(
                "invalid_visibility",
                "visibility is private, authenticated or public",
            )),
        }
    }
}

/// A permission level on a bucket or key prefix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Permission {
    /// List and download.
    Read,
    /// Read, upload, copy, move and delete.
    Write,
    /// Write, plus change the bucket's policy and ACL.
    Admin,
}

impl Permission {
    /// Stored form.
    pub fn as_str(self) -> &'static str {
        match self {
            Permission::Read => "read",
            Permission::Write => "write",
            Permission::Admin => "admin",
        }
    }

    /// Parses the stored form.
    pub fn parse(s: &str) -> Result<Self> {
        match s {
            "read" => Ok(Self::Read),
            "write" => Ok(Self::Write),
            "admin" => Ok(Self::Admin),
            _ => Err(StorageError::invalid(
                "invalid_permission",
                "permission is read, write or admin",
            )),
        }
    }
}

/// A bucket.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Bucket {
    /// Name.
    pub name: String,
    /// Default read access.
    pub visibility: Visibility,
    /// Whether overwrites and deletes keep earlier versions.
    pub versioning: bool,
    /// Largest object, in bytes (`null`: the server limit).
    pub max_object_bytes: Option<u64>,
    /// Allowed content types (`image/*` patterns allowed); `null` allows all.
    pub allowed_content_types: Option<Vec<String>>,
    /// Principal key of the creator.
    pub owner: String,
    /// RFC 3339 creation time.
    pub created_at: String,
    /// RFC 3339 time of the last change.
    pub updated_at: String,
}

/// A bucket to create.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BucketInput {
    /// Name.
    pub name: String,
    /// Default read access (default private).
    #[serde(default)]
    pub visibility: Visibility,
    /// Keep versions (default off).
    #[serde(default)]
    pub versioning: bool,
    /// Largest object in bytes.
    #[serde(default)]
    pub max_object_bytes: Option<u64>,
    /// Allowed content types.
    #[serde(default)]
    pub allowed_content_types: Option<Vec<String>>,
}

/// A partial bucket update.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct BucketPatch {
    /// New default read access.
    #[serde(default)]
    pub visibility: Option<Visibility>,
    /// Turn versioning on or off (earlier versions are kept).
    #[serde(default)]
    pub versioning: Option<bool>,
    /// New size limit (`0` clears it).
    #[serde(default)]
    pub max_object_bytes: Option<u64>,
    /// New content-type allow list (empty clears it).
    #[serde(default)]
    pub allowed_content_types: Option<Vec<String>>,
}

/// One ACL grant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct AclEntry {
    /// Key prefix the grant covers (`""`: the whole bucket).
    #[serde(default)]
    pub prefix: String,
    /// `user:<collection>/<id>`, `service:<id>`, `role:<name>`,
    /// `authenticated` or `public`.
    pub grantee: String,
    /// Access level.
    pub permission: Permission,
}

/// The ACL of a bucket (replaced as a whole).
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct Acl {
    /// Grants.
    pub entries: Vec<AclEntry>,
}

/// An object version.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ObjectInfo {
    /// Bucket.
    pub bucket: String,
    /// Key.
    pub key: String,
    /// Version id.
    pub version_id: String,
    /// Size in bytes.
    pub size: u64,
    /// Content type.
    pub content_type: String,
    /// SHA-256 of the content (hex); also the ETag.
    pub checksum_sha256: Option<String>,
    /// User metadata.
    #[schema(value_type = Object)]
    pub metadata: BTreeMap<String, String>,
    /// Whether this is the current version.
    pub is_latest: bool,
    /// Whether this version is a delete marker.
    pub delete_marker: bool,
    /// Principal key of the uploader.
    pub created_by: String,
    /// RFC 3339 upload time.
    pub created_at: String,
}

/// A page of a listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct ObjectPage {
    /// Objects (current versions only), in key order.
    pub objects: Vec<ObjectInfo>,
    /// "Directories" when a delimiter was given.
    pub prefixes: Vec<String>,
    /// Pass as `cursor` for the next page; `null` at the end.
    pub next_cursor: Option<String>,
}

/// What a delete did.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct DeleteResult {
    /// The version removed, or the delete marker created.
    pub version_id: String,
    /// `true` when a delete marker was added (versioned bucket).
    pub delete_marker: bool,
}

/// A multipart or resumable upload session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct Upload {
    /// Session id.
    pub id: String,
    /// Bucket.
    pub bucket: String,
    /// Target key.
    pub key: String,
    /// `multipart` (numbered parts, any order) or `resumable` (sequential
    /// offsets).
    pub kind: String,
    /// Content type of the finished object.
    pub content_type: String,
    /// Declared total size (resumable uploads complete when it is reached).
    pub declared_size: Option<u64>,
    /// Declared SHA-256 of the whole object.
    pub declared_sha256: Option<String>,
    /// Bytes received so far.
    pub received_bytes: u64,
    /// Received parts.
    pub parts: Vec<PartInfo>,
    /// RFC 3339 expiry.
    pub expires_at: String,
}

/// One received part.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ToSchema)]
pub struct PartInfo {
    /// Part number (1-10000).
    pub part_number: u32,
    /// Size in bytes.
    pub size: u64,
    /// SHA-256 of the part (hex).
    pub checksum_sha256: String,
}

/// A new upload session.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct UploadInput {
    /// Target key.
    pub key: String,
    /// `multipart` (default) or `resumable`.
    #[serde(default)]
    pub kind: Option<String>,
    /// Content type.
    #[serde(default)]
    pub content_type: Option<String>,
    /// User metadata.
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
    /// Total size, when known (required to auto-complete a resumable upload).
    #[serde(default)]
    pub size: Option<u64>,
    /// SHA-256 of the whole object, verified on completion.
    #[serde(default)]
    pub checksum_sha256: Option<String>,
}

/// Completes a multipart upload.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ToSchema)]
pub struct CompleteInput {
    /// Parts to assemble, in order, each optionally with its checksum.
    /// Default: every received part in part-number order.
    #[serde(default)]
    pub parts: Option<Vec<CompletePart>>,
    /// SHA-256 of the whole object (overrides the declared one).
    #[serde(default)]
    pub checksum_sha256: Option<String>,
}

/// A part named in a completion.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct CompletePart {
    /// Part number.
    pub part_number: u32,
    /// Expected checksum of the part.
    #[serde(default)]
    pub checksum_sha256: Option<String>,
}

/// Which HTTP method a signed URL allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum SignedMethod {
    /// Download.
    #[serde(rename = "GET")]
    Get,
    /// Upload.
    #[serde(rename = "PUT")]
    Put,
}

impl SignedMethod {
    /// `GET` or `PUT`.
    pub fn as_str(self) -> &'static str {
        match self {
            SignedMethod::Get => "GET",
            SignedMethod::Put => "PUT",
        }
    }
}

/// A request for a signed URL.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SignRequest {
    /// Object key.
    pub key: String,
    /// `GET` (default) or `PUT`.
    #[serde(default)]
    pub method: Option<SignedMethod>,
    /// Lifetime in seconds (default 900, at most the server limit).
    #[serde(default)]
    pub expires_in: Option<u64>,
    /// A specific version (downloads).
    #[serde(default)]
    pub version_id: Option<String>,
    /// File name to offer on download (`Content-Disposition: attachment`).
    #[serde(default)]
    pub download_as: Option<String>,
    /// Content type the upload must declare (uploads).
    #[serde(default)]
    pub content_type: Option<String>,
}

/// A signed URL.
#[derive(Debug, Clone, Serialize, Deserialize, ToSchema)]
pub struct SignedUrl {
    /// Path and query, relative to the server root; usable without credentials
    /// until `expires_at`.
    pub url: String,
    /// `GET` or `PUT`.
    pub method: SignedMethod,
    /// RFC 3339 expiry.
    pub expires_at: String,
}

/// What the reconciler did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct ReconcileReport {
    /// Expired upload sessions aborted.
    pub uploads_aborted: u64,
    /// Staged files removed (abandoned parts and single uploads).
    pub staging_deleted: u64,
    /// Stored blobs without metadata removed.
    pub orphan_blobs_deleted: u64,
    /// Unreferenced blobs (no object version uses them) removed.
    pub unreferenced_blobs_deleted: u64,
    /// Blobs the metadata names but the driver lacks (reported, not repaired).
    pub missing_blobs: Vec<String>,
}

/// Instance totals.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct StorageStats {
    /// Buckets.
    pub buckets: u64,
    /// Current (non-deleted) objects.
    pub objects: u64,
    /// Object versions, including deleted and older ones.
    pub versions: u64,
    /// Logical bytes of current objects.
    pub logical_bytes: u64,
    /// Physical bytes stored (after deduplication).
    pub stored_bytes: u64,
    /// Open upload sessions.
    pub open_uploads: u64,
}

/// Space used by one bucket.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, ToSchema)]
pub struct BucketUsage {
    /// Bucket name.
    pub bucket: String,
    /// Current (non-deleted) objects.
    pub objects: u64,
    /// Object versions, including older ones and delete markers.
    pub versions: u64,
    /// Logical bytes of current objects.
    pub logical_bytes: u64,
    /// Logical bytes of every version (before deduplication).
    pub version_bytes: u64,
}

pub(crate) fn json_map(v: &str) -> BTreeMap<String, String> {
    match serde_json::from_str::<Json>(v) {
        Ok(Json::Object(m)) => m
            .into_iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k, s.to_owned())))
            .collect(),
        _ => BTreeMap::new(),
    }
}

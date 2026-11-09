//! Small helpers: hex, hashing, ids, names, content-type detection.

use sha2::{Digest, Sha256};

use crate::error::{Result, StorageError};

/// Lower-case hex of `bytes`.
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        out.push(char::from(DIGITS[usize::from(b >> 4)]));
        out.push(char::from(DIGITS[usize::from(b & 15)]));
    }
    out
}

/// Decodes lower- or upper-case hex.
pub fn unhex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) || !s.is_ascii() {
        return None;
    }
    s.as_bytes()
        .chunks(2)
        .map(|p| {
            let hi = char::from(p[0]).to_digit(16)?;
            let lo = char::from(p[1]).to_digit(16)?;
            u8::try_from(hi * 16 + lo).ok()
        })
        .collect()
}

/// SHA-256 of `data` as hex.
pub fn sha256_hex(data: &[u8]) -> String {
    hex(&Sha256::digest(data))
}

/// A fresh random identifier (32 hex characters).
pub fn new_id() -> String {
    uuid::Uuid::new_v4().simple().to_string()
}

/// Validates and normalises a declared SHA-256 (64 hex characters).
pub fn parse_sha256(s: &str) -> Result<String> {
    let s = s.trim().trim_start_matches("sha256:").to_ascii_lowercase();
    if s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()) {
        Ok(s)
    } else {
        Err(StorageError::invalid(
            "invalid_checksum",
            "a SHA-256 checksum is 64 hexadecimal characters",
        ))
    }
}

/// Bucket names: 1-63 characters of `[a-z0-9._-]`, starting alphanumeric.
pub fn validate_bucket(name: &str) -> Result<()> {
    let ok = (1..=63).contains(&name.len())
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(StorageError::invalid(
            "invalid_bucket_name",
            "bucket names are 1-63 characters of a-z, 0-9, '.', '_' and '-', starting with a letter or digit",
        ))
    }
}

/// Object keys: non-empty UTF-8 up to 1024 bytes, no control characters, no
/// leading `/`.
pub fn validate_key(key: &str) -> Result<()> {
    if key.is_empty() || key.len() > 1024 || key.starts_with('/') || key.chars().any(char::is_control) {
        return Err(StorageError::invalid(
            "invalid_object_key",
            "object keys are 1-1024 bytes, without control characters or a leading '/'",
        ));
    }
    Ok(())
}

/// Detects a content type from leading bytes (a small, conservative table).
pub fn sniff(head: &[u8]) -> Option<&'static str> {
    let starts = |p: &[u8]| head.starts_with(p);
    if starts(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if starts(b"\xff\xd8\xff") {
        Some("image/jpeg")
    } else if starts(b"GIF87a") || starts(b"GIF89a") {
        Some("image/gif")
    } else if starts(b"%PDF-") {
        Some("application/pdf")
    } else if starts(b"PK\x03\x04") {
        Some("application/zip")
    } else if starts(b"\x1f\x8b") {
        Some("application/gzip")
    } else if head.len() >= 12 && &head[..4] == b"RIFF" && &head[8..12] == b"WEBP" {
        Some("image/webp")
    } else {
        None
    }
}

/// Types whose files start with a fixed signature that [`sniff`] knows.
const SIGNED_TYPES: [&str; 5] = ["image/png", "image/jpeg", "image/gif", "image/webp", "application/pdf"];

/// Whether a declared content type agrees with the bytes (FB-80): a type
/// with a known file signature (PNG, JPEG, GIF, WebP, PDF) must start with
/// it, so a bucket limited to `image/*` cannot be fed HTML or a script under
/// a false `Content-Type`. Other types are not checked (no reliable signature).
pub fn declared_matches_bytes(content_type: &str, head: &[u8]) -> bool {
    let base = content_type.split(';').next().unwrap_or("").trim().to_ascii_lowercase();
    if !SIGNED_TYPES.contains(&base.as_str()) {
        return true;
    }
    sniff(head) == Some(base.as_str())
}

/// Content type from a key's extension (used when the client sent none and
/// the bytes are not recognised).
pub fn from_extension(key: &str) -> Option<&'static str> {
    let ext = key.rsplit_once('.')?.1.to_ascii_lowercase();
    Some(match ext.as_str() {
        "txt" | "log" => "text/plain",
        "html" | "htm" => "text/html",
        "css" => "text/css",
        "js" | "mjs" => "text/javascript",
        "json" => "application/json",
        "csv" => "text/csv",
        "xml" => "application/xml",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "zip" => "application/zip",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        _ => return None,
    })
}

/// Formats epoch milliseconds as RFC 3339.
pub fn rfc3339(ms: i64) -> String {
    time::OffsetDateTime::from_unix_timestamp_nanos(i128::from(ms) * 1_000_000)
        .ok()
        .and_then(|t| t.format(&time::format_description::well_known::Rfc3339).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declared_types_with_a_signature_must_match_the_bytes() {
        let png = b"\x89PNG\r\n\x1a\n....";
        assert!(declared_matches_bytes("image/png", png));
        assert!(declared_matches_bytes("IMAGE/PNG; charset=binary", png));
        assert!(!declared_matches_bytes("image/png", b"<html><script>alert(1)</script>"));
        assert!(!declared_matches_bytes("image/jpeg", png), "a PNG is not a JPEG");
        assert!(!declared_matches_bytes("application/pdf", b""), "empty is not a PDF");
        assert!(declared_matches_bytes("text/plain", b"anything"), "no signature to check");
        assert!(declared_matches_bytes("application/octet-stream", png));
    }

    #[test]
    fn hex_round_trip_and_known_digest() {
        assert_eq!(hex(&[0, 1, 0xab, 0xff]), "0001abff");
        assert_eq!(unhex("0001ABff"), Some(vec![0, 1, 0xab, 0xff]));
        assert_eq!(unhex("abc"), None);
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn names_are_validated() {
        assert!(validate_bucket("photos-2026").is_ok());
        assert!(validate_bucket("Photos").is_err());
        assert!(validate_bucket("").is_err());
        assert!(validate_key("a/b.txt").is_ok());
        assert!(validate_key("/a").is_err());
        assert!(validate_key("").is_err());
        assert!(validate_key("a\nb").is_err());
    }

    #[test]
    fn content_types_are_detected() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nrest"), Some("image/png"));
        assert_eq!(sniff(b"hello"), None);
        assert_eq!(from_extension("a/b.JSON"), Some("application/json"));
    }
}

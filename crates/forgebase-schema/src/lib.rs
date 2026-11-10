//! The ForgeBase collection model (ADR-0003 §3).
//!
//! A *collection* is a named set of records stored in one SQL table. Its
//! definition lists typed *fields* (SPEC §4.3). This crate holds the model,
//! checks definitions, and validates and normalises record values. It does no
//! I/O, so the query compiler, the data service, the REST layer (FB-11) and
//! the policy engine (FB-21) share one definition of what a field is.

pub mod model;
pub mod values;

pub use model::{
    ArrayItems, Collection, CollectionType, Field, FieldKind, POSTGRES_MAX_IDENT, POSTGRES_SYSTEM_COLUMNS, SYSTEM_FIELDS,
    SchemaError, SchemaMode, Storage, is_reserved_field_name, validate_collection_name, validate_field_name,
    validate_postgres_collection_name, validate_postgres_field_name,
};
pub use values::{FieldError, ValidationErrors, normalize_date, normalize_value};

/// Current UTC time as a fixed-width timestamp `YYYY-MM-DDTHH:MM:SS.mmmZ`.
///
/// Fixed width matters: timestamps are stored as text and compared as text,
/// so every value must have the same number of fractional digits.
pub fn now_timestamp() -> String {
    format_timestamp(time::OffsetDateTime::now_utc())
}

/// Formats a time as a fixed-width UTC timestamp (millisecond precision).
pub fn format_timestamp(ts: time::OffsetDateTime) -> String {
    let t = ts.to_offset(time::UtcOffset::UTC);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        t.year(),
        u8::from(t.month()),
        t.day(),
        t.hour(),
        t.minute(),
        t.second(),
        t.millisecond()
    )
}

/// A new random record or collection id: 15 characters of `[a-z0-9]`
/// (about 77 bits of randomness).
pub fn generate_id() -> String {
    const ALPHABET: &[u8; 36] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    let mut id = String::with_capacity(15);
    while id.len() < 15 {
        for b in uuid::Uuid::new_v4().into_bytes() {
            // Rejection sampling keeps the distribution uniform (252 = 7 * 36).
            if b < 252 && id.len() < 15 {
                id.push(ALPHABET[(b % 36) as usize] as char);
            }
        }
    }
    id
}

/// Whether `id` is an acceptable record id: 1 to 64 characters of
/// `[A-Za-z0-9_-]`.
pub fn is_valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len()) && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_random_and_valid() {
        let a = generate_id();
        let b = generate_id();
        assert_eq!(a.len(), 15);
        assert_ne!(a, b);
        assert!(is_valid_id(&a));
        assert!(!is_valid_id("a b"));
        assert!(!is_valid_id(""));
        assert!(!is_valid_id(&"x".repeat(65)));
    }

    #[test]
    fn timestamps_are_fixed_width() {
        let t = time::OffsetDateTime::from_unix_timestamp(1_700_000_000).unwrap();
        assert_eq!(format_timestamp(t), "2023-11-14T22:13:20.000Z");
        assert_eq!(now_timestamp().len(), 24);
    }
}

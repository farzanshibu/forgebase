//! Webhook verification: the vectors of `sdk/js/test/webhooks.test.ts`, run against the Rust helper.
//! The reference MAC below is computed independently of the library (`hmac` + `sha2` directly).

use std::time::Duration;

use forgebase_sdk::webhooks::{
    DEFAULT_TOLERANCE, WebhookFailure, construct_event, construct_event_any, sign, verify, verify_any, verify_at,
};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{Value, json};
use sha2::Sha256;

/// What the server's `sign` (crates/forgebase-queue/src/push.rs) computes.
fn reference(secret: &str, ts: u64, body: &[u8]) -> String {
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("key");
    mac.update(format!("{ts}.").as_bytes());
    mac.update(body);
    mac.finalize().into_bytes().iter().map(|b| format!("{b:02x}")).collect()
}

const SECRET: &str = "whsec_4f1c9d2a7b";
const T: u64 = 1_700_000_000;
const NOW: u64 = T + 10;

fn envelope() -> Value {
    json!({
        "subscription": "orders-push",
        "delivery_attempt": 2,
        "message": {
            "id": "msg_1", "topic": "orders", "seq": 7,
            "data": {"order": 42, "note": "héllo ✓"},
            "attributes": {"kind": "created"},
            "publisher": "usr_1", "bytes": 31, "published_at": "2026-01-01T00:00:00Z"
        }
    })
}

fn body() -> String {
    envelope().to_string()
}

fn header_for(secret: &str, ts: u64, body: &[u8]) -> String {
    format!("t={ts},v1={}", reference(secret, ts, body))
}

fn ok(secret: &str, header: &str, body: &[u8], now: u64) -> bool {
    verify_at(&[secret], Some(header), body, DEFAULT_TOLERANCE, now).is_ok()
}

fn reason(secret: &str, header: Option<&str>, body: &[u8], now: u64) -> WebhookFailure {
    verify_at(&[secret], header, body, DEFAULT_TOLERANCE, now)
        .expect_err("must fail")
        .reason
}

#[test]
fn matches_a_vector_computed_with_openssl_and_the_key_is_the_whole_secret_string() {
    // printf '1700000000.{"a":1}' | openssl dgst -sha256 -hmac 'whsec_test'
    let h = "t=1700000000,v1=38877139021993b830af32feea6e18a8da83eb2f6e49ee50bd9e4cf4ca4d3789";
    assert!(ok("whsec_test", h, br#"{"a":1}"#, NOW));
    assert!(!ok("test", h, br#"{"a":1}"#, NOW), "the prefix is part of the key");
    assert_eq!(sign("whsec_test", br#"{"a":1}"#, 1_700_000_000), h);
}

#[test]
fn accepts_bytes_and_text_bodies_alike() {
    let b = body();
    let h = header_for(SECRET, T, b.as_bytes());
    assert!(ok(SECRET, &h, b.as_bytes(), NOW));
    // The same bytes through a Vec and a sub-slice of a larger buffer.
    let padded = [b"xx".as_slice(), b.as_bytes(), b"yy".as_slice()].concat();
    assert!(ok(SECRET, &h, &padded[2..padded.len() - 2], NOW));
}

#[test]
fn rejects_a_tampered_body() {
    let b = body();
    let h = header_for(SECRET, T, b.as_bytes());
    let spaced = format!("{b} ");
    let pretty = serde_json::to_string_pretty(&envelope()).expect("json");
    for tampered in [b.replace("42", "43"), spaced, pretty, String::new()] {
        assert!(!ok(SECRET, &h, tampered.as_bytes(), NOW), "{tampered}");
    }
}

#[test]
fn rejects_a_tampered_timestamp() {
    let b = body();
    let sig = reference(SECRET, T, b.as_bytes());
    for t in [T + 1, T - 1] {
        assert!(!ok(SECRET, &format!("t={t},v1={sig}"), b.as_bytes(), NOW));
    }
}

#[test]
fn rejects_the_wrong_secret_and_damaged_signatures() {
    let b = body();
    let h = header_for(SECRET, T, b.as_bytes());
    assert!(!ok("whsec_other", &h, b.as_bytes(), NOW));
    let sig = reference(SECRET, T, b.as_bytes());
    let flipped = format!("{}{}", if sig.starts_with('0') { "1" } else { "0" }, &sig[1..]);
    assert!(!ok(SECRET, &format!("t={T},v1={flipped}"), b.as_bytes(), NOW));
    assert!(!ok(
        SECRET,
        &format!("t={T},v1={}", &sig[..sig.len() - 2]),
        b.as_bytes(),
        NOW
    ));
    assert!(!ok(SECRET, &format!("t={T},v1={sig}00"), b.as_bytes(), NOW));
}

#[test]
fn accepts_uppercase_hex() {
    let b = body();
    let h = format!("t={T},v1={}", reference(SECRET, T, b.as_bytes()).to_uppercase());
    assert!(ok(SECRET, &h, b.as_bytes(), NOW));
}

#[test]
fn tolerance_defaults_to_300_seconds_both_ways() {
    let b = body();
    let h = header_for(SECRET, T, b.as_bytes());
    assert!(ok(SECRET, &h, b.as_bytes(), T + 300));
    assert!(!ok(SECRET, &h, b.as_bytes(), T + 301));
    assert!(ok(SECRET, &h, b.as_bytes(), T - 300));
    assert!(!ok(SECRET, &h, b.as_bytes(), T - 301));
    assert_eq!(DEFAULT_TOLERANCE, Duration::from_secs(300));
    assert_eq!(
        reason(SECRET, Some(&h), b.as_bytes(), T + 301),
        WebhookFailure::TimestampOutOfTolerance
    );
}

#[test]
fn tolerance_is_configurable() {
    let b = body();
    let h = header_for(SECRET, T, b.as_bytes());
    let at = |now: u64, tol: Duration| verify_at(&[SECRET], Some(&h), b.as_bytes(), tol, now).is_ok();
    let sixty = Duration::from_secs(60);
    assert!(at(T + 60, sixty));
    assert!(!at(T + 61, sixty));
    assert!(at(T, Duration::ZERO));
    assert!(!at(T + 1, Duration::ZERO));
    assert!(at(T + 10_000_000, Duration::MAX), "MAX turns the check off");
}

#[test]
fn the_clock_is_used_by_default() {
    let b = body();
    let fresh = sign(SECRET, b.as_bytes(), forgebase_sdk::webhooks::now_secs());
    assert!(verify(SECRET, Some(&fresh), b.as_bytes(), DEFAULT_TOLERANCE).is_ok());
    let old = header_for(SECRET, T, b.as_bytes());
    assert!(verify(SECRET, Some(&old), b.as_bytes(), DEFAULT_TOLERANCE).is_err());
}

#[test]
fn secret_rotation_accepts_any_matching_v1_and_any_receiving_secret() {
    let (old, new) = ("whsec_old", "whsec_new");
    let b = body();
    let both = format!(
        "t={T},v1={},v1={}",
        reference(new, T, b.as_bytes()),
        reference(old, T, b.as_bytes())
    );
    assert!(ok(new, &both, b.as_bytes(), NOW));
    assert!(ok(old, &both, b.as_bytes(), NOW));
    assert!(!ok("whsec_third", &both, b.as_bytes(), NOW));

    let one = |s: &str| header_for(s, T, b.as_bytes());
    let any = |secrets: &[&str], h: &str| verify_any(secrets, Some(h), b.as_bytes(), Duration::MAX).is_ok();
    assert!(any(&[old, new], &one(new)));
    assert!(any(&[old, new], &one(old)));
    assert!(!any(&["whsec_a", "whsec_b"], &both));
    assert!(!any(&[], &both), "no secret never verifies");

    let junk = format!("t={T},v1=nothex,v1=,v2=abcd,v1={}", reference(new, T, b.as_bytes()));
    assert!(
        ok(new, &junk, b.as_bytes(), NOW),
        "undecodable values and unknown schemes are skipped"
    );
    assert!(
        !ok(new, &both, b.as_bytes(), T + 400),
        "the timestamp is checked even when one v1 matches"
    );
}

#[test]
fn missing_and_malformed_headers_fail_without_panicking() {
    let b = body();
    let sig = reference(SECRET, T, b.as_bytes());
    assert_eq!(reason(SECRET, None, b.as_bytes(), NOW), WebhookFailure::MissingHeader);
    assert_eq!(
        reason(SECRET, Some(""), b.as_bytes(), NOW),
        WebhookFailure::MissingHeader
    );
    assert_eq!(
        reason(SECRET, Some("   "), b.as_bytes(), NOW),
        WebhookFailure::MissingHeader
    );
    let bad = [
        "garbage".to_owned(),
        format!("v1={sig}"),
        format!("t={T}"),
        format!("t={T},v2={sig}"),
        format!("t=abc,v1={sig}"),
        format!("t=-5,v1={sig}"),
        format!("t={T}.5,v1={sig}"),
        format!("t={T},t={T},v1={sig}"),
        format!("t=99999999999999999999,v1={sig}"),
    ];
    for h in bad {
        assert_eq!(
            reason(SECRET, Some(&h), b.as_bytes(), NOW),
            WebhookFailure::MalformedHeader,
            "{h}"
        );
    }
}

#[test]
fn tolerates_whitespace_around_the_parts() {
    let b = body();
    let h = format!(" t={T} , v1={} ", reference(SECRET, T, b.as_bytes()));
    assert!(ok(SECRET, &h, b.as_bytes(), NOW));
}

#[test]
fn construct_event_returns_the_typed_envelope() {
    #[derive(serde::Deserialize, Debug)]
    struct Order {
        order: i64,
        note: String,
    }
    let b = body();
    let h = header_for(SECRET, T, b.as_bytes());
    let ev =
        forgebase_sdk::webhooks::construct_event_at::<Order>(&[SECRET], Some(&h), b.as_bytes(), DEFAULT_TOLERANCE, NOW)
            .expect("event");
    assert_eq!(ev.subscription, "orders-push");
    assert_eq!(ev.delivery_attempt, 2);
    assert_eq!(ev.message.id, "msg_1");
    assert_eq!(ev.message.seq, 7);
    assert_eq!(ev.message.data.order, 42);
    assert_eq!(ev.message.data.note, "héllo ✓");
    assert_eq!(ev.message.attributes.get("kind").map(String::as_str), Some("created"));
}

#[test]
fn construct_event_reports_why() {
    let b = body();
    let at = |secrets: &[&str], h: Option<&str>, body: &[u8], now: u64| {
        forgebase_sdk::webhooks::construct_event_at::<Value>(secrets, h, body, DEFAULT_TOLERANCE, now)
            .expect_err("fails")
    };
    let good = header_for(SECRET, T, b.as_bytes());
    assert_eq!(
        at(&[SECRET], None, b.as_bytes(), NOW).reason,
        WebhookFailure::MissingHeader
    );
    assert_eq!(
        at(&[SECRET], Some("nope"), b.as_bytes(), NOW).reason,
        WebhookFailure::MalformedHeader
    );
    assert_eq!(
        at(&[SECRET], Some(&good), b.as_bytes(), T + 1000).reason,
        WebhookFailure::TimestampOutOfTolerance
    );
    assert_eq!(
        at(&[SECRET], Some(&good), format!("{b}x").as_bytes(), NOW).reason,
        WebhookFailure::SignatureMismatch
    );
    let e = at(&["whsec_other"], Some(&good), b.as_bytes(), NOW);
    assert_eq!(e.reason, WebhookFailure::SignatureMismatch);
    assert_eq!(e.code(), "webhook_signature_mismatch");
    assert_eq!(e.reason.as_str(), "signature_mismatch");
    assert!(e.to_string().starts_with("signature_mismatch:"));
}

#[test]
fn construct_event_rejects_a_signed_body_that_is_not_the_envelope() {
    let at = |body: &[u8]| {
        let h = header_for(SECRET, T, body);
        forgebase_sdk::webhooks::construct_event_at::<Value>(&[SECRET], Some(&h), body, DEFAULT_TOLERANCE, NOW)
            .expect_err("invalid payload")
            .reason
    };
    for body in [
        "not json",
        "[]",
        "null",
        r#"{"subscription":"s"}"#,
        r#"{"subscription":"s","delivery_attempt":"1","message":{}}"#,
    ] {
        assert_eq!(at(body.as_bytes()), WebhookFailure::InvalidPayload, "{body}");
    }
    assert_eq!(at(&[0x7b, 0xff, 0x7d]), WebhookFailure::InvalidPayload, "invalid UTF-8");
}

#[test]
fn a_bad_signature_is_reported_even_when_the_body_is_not_json() {
    // The body is never parsed before its signature checks out.
    let h = header_for(SECRET, T, body().as_bytes());
    let e =
        forgebase_sdk::webhooks::construct_event_at::<Value>(&[SECRET], Some(&h), b"not json", DEFAULT_TOLERANCE, NOW)
            .expect_err("bad signature");
    assert_eq!(e.reason, WebhookFailure::SignatureMismatch);
}

#[test]
fn the_convenience_entry_points_agree_with_the_clocked_ones() {
    let b = body();
    let h = sign(SECRET, b.as_bytes(), forgebase_sdk::webhooks::now_secs());
    let ev = construct_event::<Value>(SECRET, Some(&h), b.as_bytes(), DEFAULT_TOLERANCE).expect("event");
    assert_eq!(ev.message.data["order"], 42);
    let ev =
        construct_event_any::<Value>(&["whsec_old", SECRET], Some(&h), b.as_bytes(), DEFAULT_TOLERANCE).expect("event");
    assert_eq!(ev.subscription, "orders-push");
}

#[test]
fn signs_what_the_reference_computes() {
    let b = body();
    assert_eq!(sign(SECRET, b.as_bytes(), T), header_for(SECRET, T, b.as_bytes()));
}

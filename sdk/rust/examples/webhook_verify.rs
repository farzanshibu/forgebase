//! Verifying a push delivery, including a secret rotation.
//!
//!     cargo run --example webhook_verify
//!
//! In a real handler the header is `X-Forgebase-Signature` and the body is the raw request body.

use std::time::Duration;

use forgebase_sdk::webhooks::{self, DEFAULT_TOLERANCE, WebhookFailure};
use serde_json::Value;

fn main() {
    let (old_secret, new_secret) = ("whsec_old", "whsec_new");
    let body = br#"{"subscription":"orders-push","delivery_attempt":1,"message":{"id":"m1","topic":"orders","seq":1,"data":{"order":42}}}"#;

    // What the server sends while a rotation's grace window is open: both signatures, the new one first.
    let now = webhooks::now_secs();
    let new_sig = webhooks::sign(new_secret, body, now);
    let old_sig = webhooks::sign(old_secret, body, now);
    let header = format!("{new_sig},{}", old_sig.split_once(',').map_or("", |(_, v1)| v1));

    // A receiver that has only learned the new secret accepts it, and so does one that still has the old one.
    for secret in [new_secret, old_secret] {
        let event = webhooks::construct_event::<Value>(secret, Some(&header), body, DEFAULT_TOLERANCE);
        println!(
            "{secret}: {}",
            event.map_or_else(|e| e.to_string(), |e| format!("order {}", e.message.data["order"]))
        );
    }

    // A tampered body, the wrong secret and a replay all fail with a reason.
    let tampered = webhooks::verify(new_secret, Some(&header), b"{}", DEFAULT_TOLERANCE);
    assert_eq!(
        tampered.expect_err("tampered").reason,
        WebhookFailure::SignatureMismatch
    );
    let stale = webhooks::verify_at(&[new_secret], Some(&header), body, Duration::from_secs(60), now + 3600);
    assert_eq!(
        stale.expect_err("replayed").reason,
        WebhookFailure::TimestampOutOfTolerance
    );
    println!("rejects tampering and replays");
}

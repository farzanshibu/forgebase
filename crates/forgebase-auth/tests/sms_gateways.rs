//! The SMS gateways of ADR-0039 against an in-process mock server: request
//! shape and credentials, message ids, error mapping, 429/5xx retries, E.164
//! validation, and the SigV4 signature of Amazon SNS.
#![allow(clippy::unwrap_used, missing_docs)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::mock_http::{self, MockHttp, Seen};
use forgebase_auth::sigv4::{self, Credentials, SignInput};
use forgebase_auth::sms::{
    BirdSms, Gateway, MessageBirdSms, PlivoSms, RetryPolicy, Sms, SmsError, SmsKind, SmsProvider, SnsConfig, SnsSms,
    TwilioSms, VonageSms, WebhookSms,
};
use forgebase_auth::social::{HttpClient, PlainHttpClient};
use serde_json::{Value, json};

fn http() -> Arc<dyn HttpClient> {
    Arc::new(PlainHttpClient::default())
}

fn fast() -> Gateway {
    Gateway::new(http()).with_retry(RetryPolicy {
        attempts: 3,
        base: Duration::from_millis(1),
        max_wait: Duration::from_millis(20),
    })
}

fn sms(to: &str) -> Sms {
    Sms {
        to: to.into(),
        text: "Your code is 123456".into(),
        kind: SmsKind::LoginCode,
        secret: "123456".into(),
    }
}

const TO: &str = "+4915112345678";

fn body(r: &Seen) -> Value {
    serde_json::from_str(&r.body).unwrap()
}

/// Mock answering `(status, body)` per call, with an optional `Retry-After`.
async fn scripted(script: Vec<(u16, Option<&'static str>, String)>) -> (MockHttp, Arc<AtomicUsize>) {
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let m = mock_http::start_ext(move |_| {
        let i = n2.fetch_add(1, Ordering::SeqCst).min(script.len() - 1);
        let (st, ra, b) = script[i].clone();
        (
            st,
            ra.map(|v| vec![("Retry-After".to_owned(), v.to_owned())])
                .unwrap_or_default(),
            b,
        )
    })
    .await;
    (m, n)
}

// ---------------------------------------------------------------------------
// Vonage
// ---------------------------------------------------------------------------

#[tokio::test]
async fn vonage_request_receipt_and_errors() {
    let (m, _) = scripted(vec![(202, None, json!({"message_uuid": "uuid-1"}).to_string())]).await;
    let p = VonageSms::new("key1", "sec1", "Acme", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    let r = p.send(&sms(TO)).await.unwrap();
    assert_eq!(r.id.as_deref(), Some("uuid-1"));
    let seen = &m.requests()[0];
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/v1/messages"));
    // Basic base64("key1:sec1")
    assert_eq!(seen.header("authorization"), Some("Basic a2V5MTpzZWMx"));
    let b = body(seen);
    assert_eq!(b["channel"], "sms");
    assert_eq!(b["message_type"], "text");
    assert_eq!(b["to"], "4915112345678", "no leading +");
    assert_eq!(b["from"], "Acme");
    assert_eq!(b["text"], "Your code is 123456");

    for (status, payload, want) in [
        (401, json!({"title": "Unauthorized"}), "auth"),
        (
            422,
            json!({"title": "Invalid params", "detail": "The `to` parameter is invalid"}),
            "recipient",
        ),
        (
            422,
            json!({"title": "Invalid params", "detail": "from is bad"}),
            "rejected",
        ),
    ] {
        let (m, n) = scripted(vec![(status, None, payload.to_string())]).await;
        let p = VonageSms::new("k", "s", "Acme", Some(m.url.clone()), http())
            .unwrap()
            .with_gateway(fast());
        let e = p.send(&sms(TO)).await.unwrap_err();
        assert_eq!(n.load(Ordering::SeqCst), 1, "{status} is not retried");
        let ok = match want {
            "auth" => matches!(e, SmsError::Auth(_)),
            "recipient" => matches!(e, SmsError::InvalidRecipient(_)),
            _ => matches!(e, SmsError::Rejected(_)),
        };
        assert!(ok, "{status}: {e:?}");
    }
}

// ---------------------------------------------------------------------------
// MessageBird and Bird
// ---------------------------------------------------------------------------

#[tokio::test]
async fn messagebird_request_receipt_and_error_codes() {
    let (m, _) = scripted(vec![(201, None, json!({"id": "mb-1"}).to_string())]).await;
    let p = MessageBirdSms::new("live_key", "Acme", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    assert_eq!(p.send(&sms(TO)).await.unwrap().id.as_deref(), Some("mb-1"));
    let seen = &m.requests()[0];
    assert_eq!(seen.path, "/messages");
    assert_eq!(seen.header("authorization"), Some("AccessKey live_key"));
    let b = body(seen);
    assert_eq!(b["originator"], "Acme");
    assert_eq!(b["recipients"], json!(["4915112345678"]));
    assert_eq!(b["body"], "Your code is 123456");

    let err = |code: i64| json!({"errors": [{"code": code, "description": "x"}]}).to_string();
    for (status, code, kind) in [
        (422, 2, "auth"),
        (422, 9, "recipient"),
        (422, 25, "rate"),
        (422, 10, "rejected"),
    ] {
        let (m, _) = scripted(vec![(status, None, err(code))]).await;
        let p = MessageBirdSms::new("k", "Acme", Some(m.url.clone()), http())
            .unwrap()
            .with_gateway(fast());
        let e = p.send(&sms(TO)).await.unwrap_err();
        let ok = match kind {
            "auth" => matches!(e, SmsError::Auth(_)),
            "recipient" => matches!(e, SmsError::InvalidRecipient(_)),
            "rate" => matches!(e, SmsError::RateLimited { .. }),
            _ => matches!(e, SmsError::Rejected(_)),
        };
        assert!(ok, "code {code}: {e:?}");
    }
}

#[tokio::test]
async fn bird_request_and_receipt() {
    let (m, _) = scripted(vec![(
        202,
        None,
        json!({"id": "bird-1", "status": "accepted"}).to_string(),
    )])
    .await;
    let p = BirdSms::new("bird_key", "ws-1", "ch-9", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    assert_eq!(p.send(&sms(TO)).await.unwrap().id.as_deref(), Some("bird-1"));
    let seen = &m.requests()[0];
    assert_eq!(seen.path, "/workspaces/ws-1/channels/ch-9/messages");
    assert_eq!(seen.header("authorization"), Some("AccessKey bird_key"));
    let b = body(seen);
    assert_eq!(b["receiver"]["contacts"][0]["identifierValue"], TO);
    assert_eq!(b["receiver"]["contacts"][0]["identifierKey"], "phonenumber");
    assert_eq!(b["body"]["text"]["text"], "Your code is 123456");
    // Path segments are validated at construction.
    assert!(BirdSms::new("k", "../x", "c", None, http()).is_err());
}

// ---------------------------------------------------------------------------
// Plivo
// ---------------------------------------------------------------------------

#[tokio::test]
async fn plivo_request_receipt_and_errors() {
    let (m, _) = scripted(vec![(
        202,
        None,
        json!({"message": "message(s) queued", "message_uuid": ["pl-1"], "api_id": "a"}).to_string(),
    )])
    .await;
    let p = PlivoSms::new("MAXXXXXXXX", "tok", "+15550100", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    assert_eq!(p.send(&sms(TO)).await.unwrap().id.as_deref(), Some("pl-1"));
    let seen = &m.requests()[0];
    assert_eq!(seen.path, "/v1/Account/MAXXXXXXXX/Message/");
    // Basic base64("MAXXXXXXXX:tok")
    assert_eq!(seen.header("authorization"), Some("Basic TUFYWFhYWFhYWDp0b2s="));
    let b = body(seen);
    assert_eq!(
        (b["src"].as_str(), b["dst"].as_str()),
        (Some("+15550100"), Some("4915112345678"))
    );

    let (m, _) = scripted(vec![(
        400,
        None,
        json!({"error": "dst is invalid", "api_id": "a"}).to_string(),
    )])
    .await;
    let p = PlivoSms::new("MAXXXXXXXX", "tok", "+15550100", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    assert!(matches!(
        p.send(&sms(TO)).await.unwrap_err(),
        SmsError::InvalidRecipient(_)
    ));
    assert!(PlivoSms::new("bad/id", "t", "+1", None, http()).is_err());
}

// ---------------------------------------------------------------------------
// Twilio and the webhook: ids and mapping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn twilio_and_webhook_return_ids_and_map_errors() {
    let (m, _) = scripted(vec![(201, None, json!({"sid": "SM123"}).to_string())]).await;
    let t = TwilioSms::new(
        "AC1",
        "tok",
        Some("+15550000".into()),
        None,
        Some(m.url.clone()),
        http(),
    )
    .unwrap()
    .with_gateway(fast());
    assert_eq!(t.send(&sms(TO)).await.unwrap().id.as_deref(), Some("SM123"));

    let (m, _) = scripted(vec![(
        400,
        None,
        json!({"code": 21211, "message": "The 'To' number is not a valid phone number."}).to_string(),
    )])
    .await;
    let t = TwilioSms::new(
        "AC1",
        "tok",
        Some("+15550000".into()),
        None,
        Some(m.url.clone()),
        http(),
    )
    .unwrap()
    .with_gateway(fast());
    assert!(matches!(
        t.send(&sms(TO)).await.unwrap_err(),
        SmsError::InvalidRecipient(_)
    ));

    let (m, _) = scripted(vec![(200, None, json!({"message_id": "w-1"}).to_string())]).await;
    let w = WebhookSms::new(format!("{}/send", m.url), None, http()).with_gateway(fast());
    assert_eq!(w.send(&sms(TO)).await.unwrap().id.as_deref(), Some("w-1"));
}

// ---------------------------------------------------------------------------
// Rate limits, retries, validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn throttles_and_server_errors_are_retried_then_surface() {
    // 429 with Retry-After, then success: second attempt wins.
    let (m, n) = scripted(vec![
        (429, Some("0"), "{}".into()),
        (503, None, "{}".into()),
        (202, None, json!({"message_uuid": "ok"}).to_string()),
    ])
    .await;
    let p = VonageSms::new("k", "s", "Acme", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    assert_eq!(p.send(&sms(TO)).await.unwrap().id.as_deref(), Some("ok"));
    assert_eq!(n.load(Ordering::SeqCst), 3);

    // Persistent 429: the gateway's Retry-After comes back as the error.
    let (m, n) = scripted(vec![(429, Some("7"), "{}".into())]).await;
    let p = PlivoSms::new("MA1", "t", "+1555", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    let e = p.send(&sms(TO)).await.unwrap_err();
    assert_eq!(
        e,
        SmsError::RateLimited {
            retry_after_secs: Some(7)
        }
    );
    assert!(e.is_retryable());
    assert_eq!(n.load(Ordering::SeqCst), 3);

    // Persistent 500 is transient.
    let (m, _) = scripted(vec![(500, None, "{}".into())]).await;
    let p = BirdSms::new("k", "w", "c", Some(m.url.clone()), http())
        .unwrap()
        .with_gateway(fast());
    assert!(matches!(p.send(&sms(TO)).await.unwrap_err(), SmsError::Transient(_)));
}

#[tokio::test]
async fn every_gateway_refuses_non_e164_without_a_request() {
    let (m, n) = scripted(vec![(200, None, "{}".into())]).await;
    let u = Some(m.url.clone());
    let gws: Vec<Box<dyn SmsProvider>> = vec![
        Box::new(TwilioSms::new("AC1", "t", Some("+1555".into()), None, u.clone(), http()).unwrap()),
        Box::new(WebhookSms::new(format!("{}/x", m.url), None, http())),
        Box::new(VonageSms::new("k", "s", "A", u.clone(), http()).unwrap()),
        Box::new(MessageBirdSms::new("k", "A", u.clone(), http()).unwrap()),
        Box::new(BirdSms::new("k", "w", "c", u.clone(), http()).unwrap()),
        Box::new(PlivoSms::new("MA1", "t", "+1555", u.clone(), http()).unwrap()),
        Box::new(
            SnsSms::new(
                SnsConfig {
                    access_key_id: "AK".into(),
                    secret_access_key: "SK".into(),
                    session_token: None,
                    region: "us-east-1".into(),
                    sender_id: None,
                    sms_type: None,
                    endpoint: u.clone(),
                },
                http(),
            )
            .unwrap(),
        ),
    ];
    for g in &gws {
        for bad in [
            "015112345678",
            "+0123456789",
            "+49 151 1234 5678",
            "+49151",
            "+4915112345678901234",
            "",
            "tel:+4915112345678",
        ] {
            let e = g.send(&sms(bad)).await.unwrap_err();
            assert!(matches!(e, SmsError::InvalidRecipient(_)), "{g:?} {bad}: {e:?}");
        }
    }
    assert_eq!(n.load(Ordering::SeqCst), 0, "nothing reached the network");
}

// ---------------------------------------------------------------------------
// Amazon SNS
// ---------------------------------------------------------------------------

fn sns(url: &str, token: Option<&str>, sender: Option<&str>) -> SnsSms {
    SnsSms::new(
        SnsConfig {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: token.map(str::to_owned),
            region: "eu-west-1".into(),
            sender_id: sender.map(str::to_owned),
            sms_type: None,
            endpoint: Some(url.to_owned()),
        },
        http(),
    )
    .unwrap()
    .with_gateway(fast())
    .with_clock(|| 1_440_938_160)
}

#[tokio::test]
async fn sns_publish_is_signed_over_the_exact_bytes_it_sends() {
    let xml = r#"<PublishResponse xmlns="http://sns.amazonaws.com/doc/2010-03-31/"><PublishResult><MessageId>sns-msg-1</MessageId></PublishResult></PublishResponse>"#;
    let (m, _) = scripted(vec![(200, None, xml.into())]).await;
    let p = sns(&m.url, Some("TOKEN"), Some("Acme"));
    let r = p.send(&sms(TO)).await.unwrap();
    assert_eq!(r.id.as_deref(), Some("sns-msg-1"));
    let seen = &m.requests()[0];
    assert_eq!((seen.method.as_str(), seen.path.as_str()), ("POST", "/"));
    assert_eq!(seen.form("Action").as_deref(), Some("Publish"));
    assert_eq!(seen.form("Version").as_deref(), Some("2010-03-31"));
    assert_eq!(seen.form("PhoneNumber").as_deref(), Some(TO));
    assert_eq!(seen.form("Message").as_deref(), Some("Your code is 123456"));
    assert_eq!(
        seen.form("MessageAttributes.entry.1.Value.StringValue").as_deref(),
        Some("Transactional")
    );
    assert_eq!(
        seen.form("MessageAttributes.entry.2.Value.StringValue").as_deref(),
        Some("Acme")
    );
    assert_eq!(seen.header("x-amz-date"), Some("20150830T123600Z"));
    assert_eq!(seen.header("x-amz-security-token"), Some("TOKEN"));
    assert!(
        seen.header("x-amz-content-sha256").is_none(),
        "only S3 signs the payload header"
    );

    // Recompute the signature a verifying server would: the host as the
    // client sent it, the form body as received, the signed headers. The
    // signing core itself is pinned by the AWS test-suite vectors (see the
    // `sigv4` unit tests).
    let host = seen.header("host").unwrap();
    let creds = Credentials {
        access_key: "AKIDEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        session_token: Some("TOKEN".into()),
        region: "eu-west-1".into(),
        service: "sns".into(),
    };
    let expect = sigv4::sign(
        &creds,
        &SignInput {
            method: "POST",
            host,
            path: "/",
            query: &[],
            headers: &[("content-type".into(), "application/x-www-form-urlencoded".into())],
            payload_sha256: &sigv4::sha256_hex(seen.body.as_bytes()),
            now: 1_440_938_160,
        },
    );
    let want = &expect.iter().find(|(k, _)| k == "authorization").unwrap().1;
    assert_eq!(seen.header("authorization"), Some(want.as_str()));
    assert!(want.starts_with(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/eu-west-1/sns/aws4_request, \
         SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, Signature="
    ));
    // A tampered body no longer verifies.
    let other = sigv4::sign(
        &creds,
        &SignInput {
            method: "POST",
            host,
            path: "/",
            query: &[],
            headers: &[("content-type".into(), "application/x-www-form-urlencoded".into())],
            payload_sha256: &sigv4::sha256_hex(b"Action=Publish&PhoneNumber=%2B1555"),
            now: 1_440_938_160,
        },
    );
    assert_ne!(&other.iter().find(|(k, _)| k == "authorization").unwrap().1, want);
}

#[tokio::test]
async fn sns_error_codes_map() {
    let e = |code: &str, msg: &str| {
        format!(
            r#"<ErrorResponse><Error><Type>Sender</Type><Code>{code}</Code><Message>{msg}</Message></Error><RequestId>r</RequestId></ErrorResponse>"#
        )
    };
    let cases = [
        (
            403,
            e(
                "InvalidClientTokenId",
                "The security token included in the request is invalid.",
            ),
            "auth",
        ),
        (403, e("SignatureDoesNotMatch", "bad sig"), "auth"),
        (
            400,
            e(
                "InvalidParameter",
                "Invalid parameter: PhoneNumber Reason: +1 is not valid",
            ),
            "recipient",
        ),
        (
            400,
            e("InvalidParameter", "Invalid parameter: Message too long"),
            "rejected",
        ),
        (400, e("Throttling", "Rate exceeded"), "rate"),
        (500, e("InternalFailure", "oops"), "transient"),
        (400, e("OptedOut", "Phone number is opted out"), "rejected"),
    ];
    for (status, body, want) in cases {
        let (m, _) = scripted(vec![(status, None, body.clone())]).await;
        let err = sns(&m.url, None, None).send(&sms(TO)).await.unwrap_err();
        let ok = match want {
            "auth" => matches!(err, SmsError::Auth(_)),
            "recipient" => matches!(err, SmsError::InvalidRecipient(_)),
            "rate" => matches!(err, SmsError::RateLimited { .. }),
            "transient" => matches!(err, SmsError::Transient(_)),
            _ => matches!(err, SmsError::Rejected(_)),
        };
        assert!(ok, "{body}: {err:?}");
    }
    // The secret never appears in an error.
    let (m, _) = scripted(vec![(
        403,
        None,
        "<ErrorResponse><Error><Code>AccessDenied</Code><Message>no</Message></Error></ErrorResponse>".into(),
    )])
    .await;
    let err = sns(&m.url, None, None).send(&sms(TO)).await.unwrap_err();
    assert!(!err.to_string().contains("wJalr"));
}

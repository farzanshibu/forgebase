//! The HTTP mail transports against an in-process mock server: request shape
//! and credentials, message ids, and how success, 4xx, 429 and 5xx map.
#![allow(clippy::unwrap_used, clippy::expect_used, missing_docs)]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use base64::Engine as _;
use common::mock_http::{self, MockHttp, Seen};
use forgebase_auth::sigv4::{self, Credentials, SignInput};
use forgebase_auth::social::{HttpClient, PlainHttpClient};
use forgebase_customize::transports::{
    MailgunProvider, PostmarkProvider, ResendProvider, SendGridProvider, SesProvider, SesSettings,
};
use forgebase_customize::{MailProvider, OutMail, SendError};
use serde_json::{Value, json};

fn http() -> Arc<dyn HttpClient> {
    Arc::new(PlainHttpClient::default())
}

fn mail() -> OutMail {
    OutMail {
        id: "eml_42".into(),
        realm: "default".into(),
        to: "ann@example.com".into(),
        from: "\"Acme Inc\" <no-reply@acme.test>".into(),
        subject: "Confirm your email".into(),
        text: "Confirm: https://acme.test/m?t=1".into(),
        html: Some("<p>Confirm</p>".into()),
        reply_to: Some("help@acme.test".into()),
    }
}

/// `(status, headers, body)` answered to every request.
async fn answering(status: u16, headers: Vec<(&str, &str)>, body: Value) -> MockHttp {
    let headers: Vec<(String, String)> = headers.into_iter().map(|(k, v)| (k.into(), v.into())).collect();
    mock_http::start_ext(move |_| (status, headers.clone(), body.to_string())).await
}

fn permanent(e: &SendError) -> bool {
    matches!(e, SendError::Permanent(_))
}

fn transient(e: &SendError) -> bool {
    matches!(e, SendError::Transient(_))
}

fn first(m: &MockHttp) -> Seen {
    m.requests().remove(0)
}

// ---------------------------------------------------------------------------
// Resend
// ---------------------------------------------------------------------------

#[tokio::test]
async fn resend_request_and_receipt() {
    let m = answering(200, vec![], json!({"id": "re-1"})).await;
    let p = ResendProvider::new("re_secret", Some(m.url.clone()), http());
    assert_eq!(p.send(&mail()).await.unwrap().message_id.as_deref(), Some("re-1"));
    let r = first(&m);
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/emails"));
    assert_eq!(r.header("authorization"), Some("Bearer re_secret"));
    assert_eq!(
        r.header("idempotency-key"),
        Some("eml_42"),
        "a retry is the same message"
    );
    let b = r.json();
    assert_eq!(b["from"], "Acme Inc <no-reply@acme.test>");
    assert_eq!(b["to"], json!(["ann@example.com"]));
    assert_eq!(b["subject"], "Confirm your email");
    assert_eq!(b["text"], "Confirm: https://acme.test/m?t=1");
    assert_eq!(b["html"], "<p>Confirm</p>");
    assert_eq!(b["reply_to"], "help@acme.test");
}

#[tokio::test]
async fn resend_errors_map() {
    let body = json!({"statusCode": 422, "name": "validation_error", "message": "Invalid `to` field."});
    let e = ResendProvider::new("k", Some(answering(422, vec![], body).await.url), http())
        .send(&mail())
        .await
        .unwrap_err();
    assert!(permanent(&e) && e.to_string().contains("Invalid `to` field."), "{e}");
    let m = answering(429, vec![("Retry-After", "30")], json!({"name": "rate_limit_exceeded"})).await;
    let e = ResendProvider::new("k", Some(m.url.clone()), http())
        .send(&mail())
        .await
        .unwrap_err();
    assert!(transient(&e) && e.to_string().contains("retry after 30s"), "{e}");
    let e = ResendProvider::new("k", Some(answering(500, vec![], json!({})).await.url), http())
        .send(&mail())
        .await
        .unwrap_err();
    assert!(transient(&e), "{e}");
    let e = ResendProvider::new(
        "bad",
        Some(
            answering(401, vec![], json!({"message": "API key is invalid"}))
                .await
                .url,
        ),
        http(),
    )
    .send(&mail())
    .await
    .unwrap_err();
    assert!(permanent(&e) && e.to_string().contains("credentials"), "{e}");
    // The key never appears in an error.
    assert!(!e.to_string().contains("bad"));
}

// ---------------------------------------------------------------------------
// Postmark
// ---------------------------------------------------------------------------

#[tokio::test]
async fn postmark_request_and_receipt() {
    let m = answering(
        200,
        vec![],
        json!({"ErrorCode": 0, "Message": "OK", "MessageID": "pm-uuid-1"}),
    )
    .await;
    let p = PostmarkProvider::new("srv-token", Some("broadcast".into()), Some(m.url.clone()), http());
    assert_eq!(p.send(&mail()).await.unwrap().message_id.as_deref(), Some("pm-uuid-1"));
    let r = first(&m);
    assert_eq!(r.path, "/email");
    assert_eq!(r.header("x-postmark-server-token"), Some("srv-token"));
    let b = r.json();
    assert_eq!(b["From"], "Acme Inc <no-reply@acme.test>");
    assert_eq!(b["To"], "ann@example.com");
    assert_eq!(b["Subject"], "Confirm your email");
    assert_eq!(b["TextBody"], "Confirm: https://acme.test/m?t=1");
    assert_eq!(b["HtmlBody"], "<p>Confirm</p>");
    assert_eq!(b["ReplyTo"], "help@acme.test");
    assert_eq!(b["MessageStream"], "broadcast");
}

#[tokio::test]
async fn postmark_errors_map() {
    let inactive =
        json!({"ErrorCode": 406, "Message": "You tried to send to a recipient that has been marked as inactive."});
    let p = |m: &MockHttp| PostmarkProvider::new("t", None, Some(m.url.clone()), http());
    let e = p(&answering(422, vec![], inactive).await)
        .send(&mail())
        .await
        .unwrap_err();
    assert!(
        permanent(&e) && e.to_string().contains("406") && e.to_string().contains("inactive"),
        "{e}"
    );
    // A refusal on a 200 is still a refusal.
    let e = p(&answering(
        200,
        vec![],
        json!({"ErrorCode": 300, "Message": "Invalid email request"}),
    )
    .await)
    .send(&mail())
    .await
    .unwrap_err();
    assert!(permanent(&e), "{e}");
    assert!(transient(
        &p(&answering(429, vec![], json!({})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
    assert!(transient(
        &p(&answering(503, vec![], json!({})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
    assert!(permanent(
        &p(&answering(
            401,
            vec![],
            json!({"ErrorCode": 10, "Message": "Bad or missing Server API token."})
        )
        .await)
        .send(&mail())
        .await
        .unwrap_err()
    ));
}

// ---------------------------------------------------------------------------
// SendGrid
// ---------------------------------------------------------------------------

#[tokio::test]
async fn sendgrid_request_receipt_and_regions() {
    let m = answering(202, vec![("X-Message-Id", "sg-msg-1")], json!({})).await;
    let p = SendGridProvider::new("SG.key", false, Some(m.url.clone()), http());
    assert_eq!(p.send(&mail()).await.unwrap().message_id.as_deref(), Some("sg-msg-1"));
    let r = first(&m);
    assert_eq!(r.path, "/v3/mail/send");
    assert_eq!(r.header("authorization"), Some("Bearer SG.key"));
    let b = r.json();
    assert_eq!(b["personalizations"][0]["to"][0]["email"], "ann@example.com");
    assert_eq!(b["from"], json!({"email": "no-reply@acme.test", "name": "Acme Inc"}));
    assert_eq!(b["subject"], "Confirm your email");
    // text/plain must precede text/html.
    assert_eq!(b["content"][0]["type"], "text/plain");
    assert_eq!(b["content"][1]["type"], "text/html");
    assert_eq!(b["reply_to"]["email"], "help@acme.test");
    // Regions.
    let dummy = mail();
    assert_eq!(
        SendGridProvider::new("k", false, None, http()).request(&dummy).url,
        "https://api.sendgrid.com/v3/mail/send"
    );
    assert_eq!(
        SendGridProvider::new("k", true, None, http()).request(&dummy).url,
        "https://api.eu.sendgrid.com/v3/mail/send"
    );
}

#[tokio::test]
async fn sendgrid_errors_map() {
    let p = |m: &MockHttp| SendGridProvider::new("k", false, Some(m.url.clone()), http());
    let bad = json!({"errors": [{"message": "The from address does not match a verified Sender Identity.", "field": "from"}]});
    let e = p(&answering(400, vec![], bad).await).send(&mail()).await.unwrap_err();
    assert!(
        permanent(&e) && e.to_string().contains("verified Sender Identity"),
        "{e}"
    );
    assert!(permanent(
        &p(&answering(401, vec![], json!({})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
    assert!(transient(
        &p(&answering(429, vec![("Retry-After", "5")], json!({})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
    assert!(transient(
        &p(&answering(502, vec![], json!({})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
}

// ---------------------------------------------------------------------------
// Mailgun
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mailgun_request_receipt_and_regions() {
    let m = answering(
        200,
        vec![],
        json!({"id": "<2026.1@mg.acme.test>", "message": "Queued. Thank you."}),
    )
    .await;
    let p = MailgunProvider::new("key-secret", "mg.acme.test", false, Some(m.url.clone()), http());
    assert_eq!(
        p.send(&mail()).await.unwrap().message_id.as_deref(),
        Some("<2026.1@mg.acme.test>")
    );
    let r = first(&m);
    assert_eq!(r.path, "/v3/mg.acme.test/messages");
    let basic = base64::engine::general_purpose::STANDARD.encode("api:key-secret");
    assert_eq!(r.header("authorization"), Some(format!("Basic {basic}").as_str()));
    assert_eq!(r.form("from").as_deref(), Some("Acme Inc <no-reply@acme.test>"));
    assert_eq!(r.form("to").as_deref(), Some("ann@example.com"));
    assert_eq!(r.form("subject").as_deref(), Some("Confirm your email"));
    assert_eq!(r.form("text").as_deref(), Some("Confirm: https://acme.test/m?t=1"));
    assert_eq!(r.form("html").as_deref(), Some("<p>Confirm</p>"));
    assert_eq!(r.form("h:Reply-To").as_deref(), Some("help@acme.test"));
    let d = mail();
    assert_eq!(
        MailgunProvider::new("k", "mg.acme.test", false, None, http())
            .request(&d)
            .url,
        "https://api.mailgun.net/v3/mg.acme.test/messages"
    );
    assert_eq!(
        MailgunProvider::new("k", "mg.acme.test", true, None, http())
            .request(&d)
            .url,
        "https://api.eu.mailgun.net/v3/mg.acme.test/messages"
    );
}

#[tokio::test]
async fn mailgun_errors_map() {
    let p = |m: &MockHttp| MailgunProvider::new("k", "mg.acme.test", false, Some(m.url.clone()), http());
    let e = p(&answering(
        400,
        vec![],
        json!({"message": "'to' parameter is not a valid address."}),
    )
    .await)
    .send(&mail())
    .await
    .unwrap_err();
    assert!(permanent(&e) && e.to_string().contains("not a valid address"), "{e}");
    assert!(permanent(
        &p(&answering(401, vec![], json!({"message": "Forbidden"})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
    assert!(transient(
        &p(&answering(429, vec![], json!({})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
    assert!(transient(
        &p(&answering(500, vec![], json!({})).await)
            .send(&mail())
            .await
            .unwrap_err()
    ));
}

// ---------------------------------------------------------------------------
// Amazon SES v2
// ---------------------------------------------------------------------------

fn ses(m: &MockHttp, token: Option<&str>) -> SesProvider {
    SesProvider::new(
        SesSettings {
            access_key_id: "AKIDEXAMPLE".into(),
            secret_access_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
            session_token: token.map(str::to_owned),
            region: "eu-west-1".into(),
            configuration_set: Some("transactional".into()),
            endpoint: Some(m.url.clone()),
        },
        http(),
    )
    .unwrap()
    .with_clock(|| 1_440_938_160)
}

#[tokio::test]
async fn ses_request_is_signed_over_the_bytes_it_sends() {
    let m = answering(200, vec![], json!({"MessageId": "ses-0101-abc"})).await;
    let p = ses(&m, Some("SESSION"));
    assert_eq!(
        p.send(&mail()).await.unwrap().message_id.as_deref(),
        Some("ses-0101-abc")
    );
    let r = first(&m);
    assert_eq!(
        (r.method.as_str(), r.path.as_str()),
        ("POST", "/v2/email/outbound-emails")
    );
    let b = r.json();
    assert_eq!(b["FromEmailAddress"], "\"Acme Inc\" <no-reply@acme.test>");
    assert_eq!(b["Destination"]["ToAddresses"], json!(["ann@example.com"]));
    assert_eq!(b["ReplyToAddresses"], json!(["help@acme.test"]));
    assert_eq!(b["ConfigurationSetName"], "transactional");
    assert_eq!(b["Content"]["Simple"]["Subject"]["Data"], "Confirm your email");
    assert_eq!(
        b["Content"]["Simple"]["Body"]["Text"]["Data"],
        "Confirm: https://acme.test/m?t=1"
    );
    assert_eq!(b["Content"]["Simple"]["Body"]["Html"]["Data"], "<p>Confirm</p>");
    assert_eq!(r.header("x-amz-date"), Some("20150830T123600Z"));
    assert_eq!(r.header("x-amz-security-token"), Some("SESSION"));

    // Recompute what a verifying server would from the received request.
    // (The signing core is pinned by the AWS test-suite vectors in
    // `forgebase_auth::sigv4`.)
    let creds = Credentials {
        access_key: "AKIDEXAMPLE".into(),
        secret_key: "wJalrXUtnFEMI/K7MDENG+bPxRfiCYEXAMPLEKEY".into(),
        session_token: Some("SESSION".into()),
        region: "eu-west-1".into(),
        service: "ses".into(),
    };
    let sign = |body: &str| {
        sigv4::sign(
            &creds,
            &SignInput {
                method: "POST",
                host: r.header("host").unwrap(),
                path: "/v2/email/outbound-emails",
                query: &[],
                headers: &[("content-type".into(), "application/json".into())],
                payload_sha256: &sigv4::sha256_hex(body.as_bytes()),
                now: 1_440_938_160,
            },
        )
        .into_iter()
        .find(|(k, _)| k == "authorization")
        .unwrap()
        .1
    };
    assert_eq!(r.header("authorization"), Some(sign(&r.body).as_str()));
    assert_ne!(
        r.header("authorization"),
        Some(sign("{}").as_str()),
        "the payload hash is signed"
    );
    let auth = r.header("authorization").unwrap();
    assert!(auth.starts_with(
        "AWS4-HMAC-SHA256 Credential=AKIDEXAMPLE/20150830/eu-west-1/ses/aws4_request, \
         SignedHeaders=content-type;host;x-amz-date;x-amz-security-token, Signature="
    ));
    assert!(!auth.contains("wJalr"));
}

#[tokio::test]
async fn ses_errors_map() {
    let err = |status: u16, kind: &str, msg: &str| {
        let kind = kind.to_owned();
        let body = json!({"message": msg}).to_string();
        async move {
            let m = mock_http::start_ext(move |_| {
                (
                    status,
                    vec![(
                        "x-amzn-ErrorType".to_owned(),
                        format!("{kind}:http://internal.amazon.com/coral/"),
                    )],
                    body.clone(),
                )
            })
            .await;
            ses(&m, None).send(&mail()).await.unwrap_err()
        }
    };
    let e = err(400, "MessageRejected", "Email address is not verified.").await;
    assert!(
        permanent(&e) && e.to_string().contains("MessageRejected") && e.to_string().contains("not verified"),
        "{e}"
    );
    assert!(transient(
        &err(429, "TooManyRequestsException", "Maximum sending rate exceeded.").await
    ));
    // Throttling reported on a 400 is still a throttle.
    assert!(transient(&err(400, "ThrottlingException", "Rate exceeded").await));
    assert!(permanent(
        &err(403, "InvalidSignatureException", "The request signature does not match").await
    ));
    assert!(permanent(
        &err(400, "AccountSuspendedException", "Account suspended").await
    ));
    assert!(transient(&err(500, "InternalFailure", "oops").await));
    assert!(transient(&err(503, "ServiceUnavailable", "later").await));
}

#[tokio::test]
async fn ses_settings_are_validated() {
    let m = answering(200, vec![], json!({})).await;
    let mk = |region: &str, endpoint: Option<String>, key: &str| {
        SesProvider::new(
            SesSettings {
                access_key_id: key.into(),
                secret_access_key: "s".into(),
                session_token: None,
                region: region.into(),
                configuration_set: None,
                endpoint,
            },
            http(),
        )
    };
    assert!(mk("us-east-1", None, "AK").is_ok());
    assert!(mk("US EAST", None, "AK").is_err());
    assert!(mk("us-east-1", None, "").is_err());
    assert!(mk("us-east-1", Some("ftp://x".into()), "AK").is_err());
    assert!(mk("us-east-1", Some("https://u:p@host".into()), "AK").is_err());
    assert!(mk("us-east-1", Some(m.url.clone()), "AK").is_ok());
}

#[tokio::test]
async fn connection_failures_are_transient() {
    // Nothing listens on this port.
    let p = ResendProvider::new("k", Some("http://127.0.0.1:1".into()), http());
    assert!(transient(&p.send(&mail()).await.unwrap_err()));
    // And every transport counts its own calls once.
    let n = Arc::new(AtomicUsize::new(0));
    let n2 = n.clone();
    let m = mock_http::start(move |_| {
        n2.fetch_add(1, Ordering::SeqCst);
        (200, json!({"id": "x"}).to_string())
    })
    .await;
    ResendProvider::new("k", Some(m.url.clone()), http())
        .send(&mail())
        .await
        .unwrap();
    assert_eq!(n.load(Ordering::SeqCst), 1);
}

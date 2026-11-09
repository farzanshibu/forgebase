//! The SMTP provider against a scripted in-process server (plain, with AUTH).
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use forgebase_customize::*;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

#[derive(Default, Debug)]
struct Seen {
    lines: Vec<String>,
    data: String,
}

async fn server(rcpt_reply: &'static str, offer_starttls: bool) -> (u16, Arc<Mutex<Seen>>) {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    let seen = Arc::new(Mutex::new(Seen::default()));
    let s2 = seen.clone();
    tokio::spawn(async move {
        let (sock, _) = l.accept().await.unwrap();
        let (r, mut w) = sock.into_split();
        let mut r = BufReader::new(r);
        w.write_all(b"220 mock ESMTP\r\n").await.unwrap();
        let mut line = String::new();
        loop {
            line.clear();
            if r.read_line(&mut line).await.unwrap() == 0 {
                return;
            }
            let cmd = line.trim_end().to_owned();
            s2.lock().unwrap().lines.push(cmd.clone());
            let up = cmd.to_ascii_uppercase();
            let reply: String = if up.starts_with("EHLO") {
                if offer_starttls {
                    "250-mock\r\n250-STARTTLS\r\n250 AUTH PLAIN LOGIN\r\n".into()
                } else {
                    "250-mock\r\n250 AUTH PLAIN LOGIN\r\n".into()
                }
            } else if up.starts_with("AUTH PLAIN") {
                "235 ok\r\n".into()
            } else if up.starts_with("MAIL FROM") {
                "250 ok\r\n".into()
            } else if up.starts_with("RCPT TO") {
                format!("{rcpt_reply}\r\n")
            } else if up == "DATA" {
                w.write_all(b"354 go\r\n").await.unwrap();
                let mut buf = Vec::new();
                let mut b = [0u8; 1];
                while !buf.ends_with(b"\r\n.\r\n") {
                    if r.read(&mut b).await.unwrap() == 0 {
                        return;
                    }
                    buf.push(b[0]);
                }
                s2.lock().unwrap().data = String::from_utf8_lossy(&buf).into_owned();
                "250 queued\r\n".into()
            } else if up == "QUIT" {
                w.write_all(b"221 bye\r\n").await.unwrap();
                return;
            } else {
                "500 what\r\n".into()
            };
            w.write_all(reply.as_bytes()).await.unwrap();
        }
    });
    (port, seen)
}

fn mail(to: &str) -> OutMail {
    OutMail {
        id: "eml_1".into(),
        realm: "default".into(),
        to: to.into(),
        from: "\"Acme\" <no-reply@acme.test>".into(),
        subject: "Héllo".into(),
        text: ".leading dot\nsecond line".into(),
        html: Some("<p>hi</p>".into()),
    }
}

fn provider(port: u16, tls: SmtpTls) -> SmtpProvider {
    let mut c = SmtpConfig::new("127.0.0.1", "no-reply@acme.test");
    c.port = port;
    c.tls = tls;
    c.username = Some("user".into());
    c.password = Some("pw".into());
    c.timeout = Duration::from_secs(5);
    SmtpProvider::new(c)
}

#[tokio::test]
async fn sends_a_multipart_message_with_auth() {
    let (port, seen) = server("250 ok", false).await;
    provider(port, SmtpTls::None)
        .send(&mail("ann@example.com"))
        .await
        .unwrap();
    let s = seen.lock().unwrap();
    let tok = base64::engine::general_purpose::STANDARD.encode("\0user\0pw");
    assert!(
        s.lines.iter().any(|l| l == &format!("AUTH PLAIN {tok}")),
        "{:?}",
        s.lines
    );
    assert!(s.lines.contains(&"MAIL FROM:<no-reply@acme.test>".to_owned()));
    assert!(s.lines.contains(&"RCPT TO:<ann@example.com>".to_owned()));
    assert!(s.data.contains("multipart/alternative"));
    assert!(s.data.contains("To: ann@example.com"));
    assert!(s.data.contains("Subject: =?UTF-8?B?"), "non-ASCII subject is encoded");
    // The password never appears in the message itself.
    assert!(!s.data.contains("pw"));
    // Bodies are base64: the leading-dot line cannot start a bare dot line.
    let body = s.data.strip_suffix("\r\n.\r\n").unwrap();
    assert!(!body.lines().any(|l| l == "."), "only the terminator is a lone dot");
}

#[tokio::test]
async fn smtp_4xx_is_transient_and_5xx_permanent() {
    let (port, _) = server("451 try later", false).await;
    let e = provider(port, SmtpTls::None)
        .send(&mail("ann@example.com"))
        .await
        .unwrap_err();
    assert!(matches!(e, SendError::Transient(_)), "{e:?}");
    let (port, _) = server("550 no such user", false).await;
    let e = provider(port, SmtpTls::None)
        .send(&mail("ann@example.com"))
        .await
        .unwrap_err();
    assert!(matches!(e, SendError::Permanent(_)), "{e:?}");
}

#[tokio::test]
async fn refuses_to_send_in_the_clear_when_starttls_is_required() {
    let (port, seen) = server("250 ok", false).await;
    let e = provider(port, SmtpTls::StartTls)
        .send(&mail("ann@example.com"))
        .await
        .unwrap_err();
    assert!(matches!(&e, SendError::Permanent(m) if m.contains("STARTTLS")), "{e:?}");
    assert!(
        !seen.lock().unwrap().lines.iter().any(|l| l.starts_with("AUTH")),
        "credentials must not be sent"
    );
}

#[tokio::test]
async fn bad_recipient_is_permanent_and_connection_refused_is_transient() {
    let (port, _) = server("250 ok", false).await;
    let e = provider(port, SmtpTls::None)
        .send(&mail("a@b.com\r\nRCPT TO:<x@y.z>"))
        .await
        .unwrap_err();
    assert!(matches!(e, SendError::Permanent(_)));
    // Nothing listens on this port.
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    let e = provider(port, SmtpTls::None)
        .send(&mail("ann@example.com"))
        .await
        .unwrap_err();
    assert!(matches!(e, SendError::Transient(_)), "{e:?}");
}

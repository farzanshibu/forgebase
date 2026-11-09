//! Mail providers: where a rendered message is handed over.
//!
//! * [`LogProvider`] writes the envelope (never the body) to the log and can
//!   keep messages in memory for tests: for development.
//! * [`SmtpProvider`] speaks SMTP (RFC 5321) itself over `tokio`: plain,
//!   STARTTLS or implicit TLS (rustls through `forgebase-auth`), `AUTH PLAIN`
//!   / `AUTH LOGIN`, MIME `multipart/alternative` with base64 bodies.
//!
//! A provider says whether a failure is worth retrying ([`SendError`]): 4xx
//! replies and connection problems are transient, 5xx replies permanent.

use std::fmt;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use forgebase_auth::net::TlsSettings;
use forgebase_db::backend::BoxFuture;
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;

/// A rendered message ready to send.
#[derive(Clone, PartialEq, Eq)]
pub struct OutMail {
    /// Delivery id (also the `Message-ID` local part).
    pub id: String,
    /// Realm it was sent for.
    pub realm: String,
    /// Recipient address.
    pub to: String,
    /// `From` header value (`Name <addr>` or `addr`).
    pub from: String,
    /// Subject (no control characters).
    pub subject: String,
    /// Plain-text body.
    pub text: String,
    /// HTML body.
    pub html: Option<String>,
}

impl fmt::Debug for OutMail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OutMail")
            .field("id", &self.id)
            .field("realm", &self.realm)
            .field("to", &self.to)
            .finish_non_exhaustive()
    }
}

/// Why a send failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SendError {
    /// Worth retrying (connection trouble, a 4xx reply).
    Transient(String),
    /// Retrying cannot help (a 5xx reply, a bad address).
    Permanent(String),
}

impl fmt::Display for SendError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SendError::Transient(m) | SendError::Permanent(m) => f.write_str(m),
        }
    }
}

/// Delivers messages. Must not log bodies.
pub trait MailProvider: Send + Sync + fmt::Debug {
    /// Short name recorded in the delivery log (`smtp`, `log`).
    fn name(&self) -> &'static str;
    /// Hands one message over.
    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<(), SendError>>;
}

/// The development provider: logs the envelope, optionally captures messages.
#[derive(Debug, Clone, Default)]
pub struct LogProvider {
    captured: Option<Arc<Mutex<Vec<OutMail>>>>,
}

impl LogProvider {
    /// Logs only.
    pub fn new() -> Self {
        Self::default()
    }

    /// Also keeps every message in memory (tests).
    pub fn capturing() -> Self {
        Self {
            captured: Some(Arc::default()),
        }
    }

    /// Messages captured so far, oldest first.
    pub fn sent(&self) -> Vec<OutMail> {
        self.captured
            .as_ref()
            .and_then(|c| c.lock().ok().map(|v| v.clone()))
            .unwrap_or_default()
    }
}

impl MailProvider for LogProvider {
    fn name(&self) -> &'static str {
        "log"
    }

    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<(), SendError>> {
        Box::pin(async move {
            tracing::info!(id = %mail.id, realm = %mail.realm, to = %mail.to, subject = %mail.subject, "mail (log provider)");
            if let Some(c) = &self.captured
                && let Ok(mut v) = c.lock()
            {
                v.push(mail.clone());
            }
            Ok(())
        })
    }
}

/// How the SMTP connection is secured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SmtpTls {
    /// No TLS (a local relay, tests).
    None,
    /// Plain connect, then `STARTTLS` (port 587); refuses to continue without it.
    StartTls,
    /// TLS from the first byte (port 465).
    Implicit,
}

/// SMTP settings.
#[derive(Clone)]
pub struct SmtpConfig {
    /// Server host (also the TLS server name).
    pub host: String,
    /// Server port.
    pub port: u16,
    /// TLS mode.
    pub tls: SmtpTls,
    /// Login name (`None`: no authentication).
    pub username: Option<String>,
    /// Password.
    pub password: Option<String>,
    /// Envelope sender (`MAIL FROM`).
    pub envelope_from: String,
    /// Name announced in `EHLO`.
    pub helo: String,
    /// Timeout of each network step.
    pub timeout: Duration,
}

impl fmt::Debug for SmtpConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SmtpConfig")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("tls", &self.tls)
            .field("username", &self.username)
            .field("password", &self.password.as_ref().map(|_| "********"))
            .finish_non_exhaustive()
    }
}

impl SmtpConfig {
    /// STARTTLS on port 587 with the given sender.
    pub fn new(host: impl Into<String>, envelope_from: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            port: 587,
            tls: SmtpTls::StartTls,
            username: None,
            password: None,
            envelope_from: envelope_from.into(),
            helo: "forgebase.local".into(),
            timeout: Duration::from_secs(30),
        }
    }
}

/// SMTP delivery.
#[derive(Debug, Clone)]
pub struct SmtpProvider {
    cfg: SmtpConfig,
    tls: TlsSettings,
}

impl SmtpProvider {
    /// A provider using `cfg` and the default CA bundle.
    pub fn new(cfg: SmtpConfig) -> Self {
        Self {
            cfg,
            tls: TlsSettings::new(),
        }
    }

    /// Trusts extra CAs (a private mail relay).
    #[must_use]
    pub fn with_tls(mut self, tls: TlsSettings) -> Self {
        self.tls = tls;
        self
    }
}

trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

type Conn = BufReader<Box<dyn Io>>;

fn transient(m: impl Into<String>) -> SendError {
    SendError::Transient(m.into())
}

async fn step<T, E: fmt::Display>(
    d: Duration,
    what: &str,
    f: impl Future<Output = Result<T, E>>,
) -> Result<T, SendError> {
    match tokio::time::timeout(d, f).await {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(transient(format!("SMTP {what}: {e}"))),
        Err(_) => Err(transient(format!("SMTP {what}: timed out"))),
    }
}

/// One reply: the code and its text lines.
struct Reply {
    code: u16,
    lines: Vec<String>,
}

impl Reply {
    fn text(&self) -> String {
        self.lines.join(" ")
    }

    fn expect(self, ok: &[u16], what: &str) -> Result<Reply, SendError> {
        if ok.contains(&self.code) {
            return Ok(self);
        }
        let m = format!("SMTP {what}: {} {}", self.code, self.text());
        Err(if (400..500).contains(&self.code) {
            SendError::Transient(m)
        } else {
            SendError::Permanent(m)
        })
    }
}

async fn read_reply(c: &mut Conn, d: Duration) -> Result<Reply, SendError> {
    let mut lines = Vec::new();
    loop {
        let mut line = String::new();
        let n = step(d, "read", c.read_line(&mut line)).await?;
        if n == 0 {
            return Err(transient("SMTP: connection closed by the server"));
        }
        if line.len() > 4096 || lines.len() > 100 {
            return Err(transient("SMTP: reply too long"));
        }
        let l = line.trim_end_matches(['\r', '\n']);
        let code: u16 = l
            .get(..3)
            .and_then(|c| c.parse().ok())
            .ok_or_else(|| transient(format!("SMTP: malformed reply {l:?}")))?;
        let more = l.as_bytes().get(3) == Some(&b'-');
        lines.push(l.get(4..).unwrap_or_default().to_owned());
        if !more {
            return Ok(Reply { code, lines });
        }
    }
}

async fn send_line(c: &mut Conn, d: Duration, line: &str) -> Result<(), SendError> {
    let mut b = Vec::with_capacity(line.len() + 2);
    b.extend_from_slice(line.as_bytes());
    b.extend_from_slice(b"\r\n");
    step(d, "write", async {
        c.get_mut().write_all(&b).await?;
        c.get_mut().flush().await
    })
    .await
}

async fn command(c: &mut Conn, d: Duration, line: &str, ok: &[u16], what: &str) -> Result<Reply, SendError> {
    send_line(c, d, line).await?;
    read_reply(c, d).await?.expect(ok, what)
}

/// `a@b.c` without anything that could break out of an SMTP command or header.
pub fn valid_address(a: &str) -> bool {
    let Some((local, domain)) = a.split_once('@') else {
        return false;
    };
    a.len() <= 254
        && !local.is_empty()
        && local.len() <= 64
        && !domain.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "!#$%&'*+/=?^_`{|}~.-@".contains(c))
}

fn b64_lines(data: &[u8]) -> String {
    let enc = STANDARD.encode(data);
    let mut out = String::with_capacity(enc.len() + enc.len() / 76 * 2 + 2);
    for chunk in enc.as_bytes().chunks(76) {
        out.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        out.push_str("\r\n");
    }
    out
}

fn header_text(s: &str) -> String {
    let clean: String = s.chars().map(|c| if c.is_control() { ' ' } else { c }).collect();
    if clean.is_ascii() {
        clean
    } else {
        format!("=?UTF-8?B?{}?=", STANDARD.encode(clean.as_bytes()))
    }
}

/// The `From` header value for a display name and an address.
pub fn from_header(display: &str, addr: &str) -> String {
    let name: String = display
        .chars()
        .filter(|c| !c.is_control() && !matches!(c, '"' | '\\' | '<' | '>'))
        .collect();
    let name = name.trim();
    if name.is_empty() {
        addr.to_owned()
    } else if name.is_ascii() {
        format!("\"{name}\" <{addr}>")
    } else {
        format!("{} <{addr}>", header_text(name))
    }
}

/// The RFC 5322 message for `m` (CRLF line ends, no dot stuffing).
pub fn build_message(m: &OutMail, date: &str, domain: &str) -> String {
    let mut s = String::new();
    s.push_str(&format!("From: {}\r\n", m.from.replace(['\r', '\n'], " ")));
    s.push_str(&format!("To: {}\r\n", m.to));
    s.push_str(&format!("Subject: {}\r\n", header_text(&m.subject)));
    s.push_str(&format!("Date: {date}\r\n"));
    s.push_str(&format!("Message-ID: <{}@{domain}>\r\n", m.id));
    s.push_str("MIME-Version: 1.0\r\n");
    s.push_str("X-Auto-Response-Suppress: All\r\n");
    match &m.html {
        None => {
            s.push_str("Content-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n");
            s.push_str(&b64_lines(m.text.as_bytes()));
        }
        Some(html) => {
            let boundary = format!("fb-{}", m.id.replace(|c: char| !c.is_ascii_alphanumeric(), ""));
            s.push_str(&format!(
                "Content-Type: multipart/alternative; boundary=\"{boundary}\"\r\n\r\n"
            ));
            s.push_str(&format!(
                "--{boundary}\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"
            ));
            s.push_str(&b64_lines(m.text.as_bytes()));
            s.push_str(&format!(
                "--{boundary}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\n"
            ));
            s.push_str(&b64_lines(html.as_bytes()));
            s.push_str(&format!("--{boundary}--\r\n"));
        }
    }
    s
}

fn http_date() -> String {
    // RFC 5322 date from the wall clock (UTC).
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(0));
    let days = secs.div_euclid(86_400);
    let rem = secs.rem_euclid(86_400);
    // civil from days (Howard Hinnant)
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    let wd = ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][usize::try_from(days.rem_euclid(7)).unwrap_or(0)];
    let mon = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ][usize::try_from(m - 1).unwrap_or(0)];
    format!(
        "{wd}, {d:02} {mon} {y} {:02}:{:02}:{:02} +0000",
        rem / 3600,
        rem % 3600 / 60,
        rem % 60
    )
}

fn dot_stuff(msg: &str) -> String {
    let mut out = String::with_capacity(msg.len() + 16);
    for line in msg.split("\r\n") {
        if line.starts_with('.') {
            out.push('.');
        }
        out.push_str(line);
        out.push_str("\r\n");
    }
    // `split` yields a final empty piece after the trailing CRLF; drop its CRLF.
    out.truncate(out.len().saturating_sub(2));
    out
}

impl SmtpProvider {
    async fn connect(&self) -> Result<Conn, SendError> {
        let c = &self.cfg;
        let d = c.timeout;
        let tcp = step(d, "connect", TcpStream::connect((c.host.as_str(), c.port))).await?;
        let io: Box<dyn Io> = if c.tls == SmtpTls::Implicit {
            let cfg = self
                .tls
                .client_config(&[])
                .map_err(|e| SendError::Permanent(e.to_string()))?;
            Box::new(step(d, "TLS", TlsSettings::upgrade(&cfg, &c.host, tcp)).await?)
        } else {
            Box::new(tcp)
        };
        Ok(BufReader::new(io))
    }

    async fn session(&self, mail: &OutMail) -> Result<(), SendError> {
        let c = &self.cfg;
        let d = c.timeout;
        if !valid_address(&mail.to) {
            return Err(SendError::Permanent(format!(
                "{:?} is not a valid recipient address",
                mail.to
            )));
        }
        if !valid_address(&c.envelope_from) {
            return Err(SendError::Permanent(
                "the configured sender address is not valid".into(),
            ));
        }
        let mut conn = self.connect().await?;
        read_reply(&mut conn, d).await?.expect(&[220], "greeting")?;
        let mut ehlo = command(&mut conn, d, &format!("EHLO {}", c.helo), &[250], "EHLO").await?;
        if c.tls == SmtpTls::StartTls {
            if !ehlo.lines.iter().any(|l| l.eq_ignore_ascii_case("STARTTLS")) {
                return Err(SendError::Permanent(
                    "the server does not offer STARTTLS; refusing to send in the clear".into(),
                ));
            }
            command(&mut conn, d, "STARTTLS", &[220], "STARTTLS").await?;
            // The reply was the last server byte before the handshake: nothing is buffered.
            let inner = conn.into_inner();
            let cfg = self
                .tls
                .client_config(&[])
                .map_err(|e| SendError::Permanent(e.to_string()))?;
            // The boxed stream is a TcpStream here; upgrade needs the concrete type.
            let tls = upgrade_boxed(&cfg, &c.host, inner, d).await?;
            conn = BufReader::new(tls);
            ehlo = command(&mut conn, d, &format!("EHLO {}", c.helo), &[250], "EHLO").await?;
        }
        if let (Some(u), Some(p)) = (&c.username, &c.password) {
            if c.tls == SmtpTls::None {
                tracing::warn!("SMTP authentication over an unencrypted connection");
            }
            let mechs = ehlo
                .lines
                .iter()
                .find_map(|l| l.strip_prefix("AUTH ").or_else(|| l.strip_prefix("AUTH=")))
                .unwrap_or_default()
                .to_ascii_uppercase();
            if mechs.split_whitespace().any(|m| m == "PLAIN") || mechs.is_empty() {
                let tok = STANDARD.encode(format!("\0{u}\0{p}"));
                // A 535 reply is permanent (wrong credentials); 454 is transient.
                command(&mut conn, d, &format!("AUTH PLAIN {tok}"), &[235], "AUTH").await?;
            } else if mechs.split_whitespace().any(|m| m == "LOGIN") {
                command(&mut conn, d, "AUTH LOGIN", &[334], "AUTH").await?;
                command(&mut conn, d, &STANDARD.encode(u), &[334], "AUTH").await?;
                command(&mut conn, d, &STANDARD.encode(p), &[235], "AUTH").await?;
            } else {
                return Err(SendError::Permanent(format!(
                    "no supported AUTH mechanism offered ({mechs})"
                )));
            }
        }
        command(
            &mut conn,
            d,
            &format!("MAIL FROM:<{}>", c.envelope_from),
            &[250],
            "MAIL FROM",
        )
        .await?;
        command(&mut conn, d, &format!("RCPT TO:<{}>", mail.to), &[250, 251], "RCPT TO").await?;
        command(&mut conn, d, "DATA", &[354], "DATA").await?;
        let msg = build_message(mail, &http_date(), &c.helo);
        let body = format!("{}\r\n.\r\n", dot_stuff(&msg));
        step(d, "write", async {
            conn.get_mut().write_all(body.as_bytes()).await?;
            conn.get_mut().flush().await
        })
        .await?;
        read_reply(&mut conn, d).await?.expect(&[250], "message")?;
        let _ = send_line(&mut conn, d, "QUIT").await;
        Ok(())
    }
}

/// Upgrades a boxed plain stream to TLS (certificate verified against `host`).
async fn upgrade_boxed(
    cfg: &Arc<rustls::ClientConfig>,
    host: &str,
    inner: Box<dyn Io>,
    d: Duration,
) -> Result<Box<dyn Io>, SendError> {
    let name = rustls::pki_types::ServerName::try_from(host.to_owned())
        .map_err(|_| SendError::Permanent(format!("{host:?} is not a valid TLS server name")))?;
    let tls = step(
        d,
        "TLS",
        tokio_rustls::TlsConnector::from(cfg.clone()).connect(name, inner),
    )
    .await?;
    Ok(Box::new(tls))
}

impl MailProvider for SmtpProvider {
    fn name(&self) -> &'static str {
        "smtp"
    }

    fn send<'a>(&'a self, mail: &'a OutMail) -> BoxFuture<'a, Result<(), SendError>> {
        Box::pin(self.session(mail))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_validation_blocks_injection() {
        assert!(valid_address("a.b+c@example.com"));
        for bad in [
            "a@b",
            "a b@c.com",
            "a@b.com\r\nRCPT TO:<x@y.z>",
            "<a@b.com>",
            "a@@b.com",
            "@b.com",
            "a@b.com>",
        ] {
            assert!(!valid_address(bad), "{bad}");
        }
    }

    #[test]
    fn from_header_is_safe() {
        assert_eq!(from_header("Acme \"Inc\"\r\n<x>", "a@b.com"), "\"Acme Incx\" <a@b.com>");
        assert_eq!(from_header("", "a@b.com"), "a@b.com");
        assert!(from_header("Zoë", "a@b.com").starts_with("=?UTF-8?B?"));
    }

    #[test]
    fn dot_stuffing() {
        assert_eq!(dot_stuff("a\r\n.b\r\n"), "a\r\n..b\r\n");
    }

    #[test]
    fn date_format() {
        let d = http_date();
        assert!(d.ends_with("+0000") && d.len() == 31, "{d}");
    }
}

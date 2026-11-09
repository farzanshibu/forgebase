//! Outgoing mail: the pluggable [`Mailer`] trait and the development mailer.
//!
//! Production deployments plug in an SMTP or provider mailer (a later card);
//! the [`DevMailer`] writes each message as a JSON file into
//! `<data_dir>/mail/` (directory 0700, files 0600) so local development and
//! tests can read verification links and codes without a mail server.

use std::fmt;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

/// Boxed future returned by [`Mailer::send`].
pub type MailFuture<'a> = Pin<Box<dyn Future<Output = Result<(), String>> + Send + 'a>>;

/// What a message is for (lets tests and templates find the right mail).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MailKind {
    /// Email address verification link.
    VerifyEmail,
    /// Password reset link.
    PasswordReset,
    /// Magic sign-in link.
    MagicLink,
    /// One-time sign-in code.
    EmailOtp,
    /// Invitation to an organization (FB-21).
    Invitation,
}

/// One outgoing message.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Mail {
    /// Recipient address.
    pub to: String,
    /// Subject line.
    pub subject: String,
    /// Plain-text body.
    pub text: String,
    /// Purpose.
    pub kind: MailKind,
    /// The token or code the message carries (for the dev mailer and tests;
    /// real mailers only send `text`).
    pub secret: String,
    /// Full link, when the message carries one.
    pub link: Option<String>,
}

/// Sends mail. Implementations must not log `secret` or `text`.
pub trait Mailer: Send + Sync + fmt::Debug {
    /// Delivers one message.
    fn send<'a>(&'a self, mail: &'a Mail) -> MailFuture<'a>;
}

/// Writes every message to `<dir>/<time>-<kind>-<random>.json`.
#[derive(Debug, Clone)]
pub struct DevMailer {
    dir: PathBuf,
}

impl DevMailer {
    /// Mailer writing into `dir` (created on first use).
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        Self { dir: dir.into() }
    }

    /// The output directory.
    pub fn dir(&self) -> &PathBuf {
        &self.dir
    }

    /// Reads back every message written so far, oldest first.
    pub fn read_all(&self) -> Vec<Mail> {
        let Ok(entries) = std::fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut files: Vec<PathBuf> = entries
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.extension().is_some_and(|x| x == "json"))
            .collect();
        files.sort();
        files
            .into_iter()
            .filter_map(|p| std::fs::read(&p).ok())
            .filter_map(|b| serde_json::from_slice(&b).ok())
            .collect()
    }

    /// The newest message of `kind` sent to `to`.
    pub fn last(&self, to: &str, kind: MailKind) -> Option<Mail> {
        self.read_all()
            .into_iter()
            .rev()
            .find(|m| m.kind == kind && m.to.eq_ignore_ascii_case(to))
    }
}

impl Mailer for DevMailer {
    fn send<'a>(&'a self, mail: &'a Mail) -> MailFuture<'a> {
        Box::pin(async move {
            let dir = self.dir.clone();
            let body = serde_json::to_vec_pretty(mail).map_err(|e| e.to_string())?;
            let kind = serde_json::to_value(mail.kind)
                .ok()
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default();
            let name = format!(
                "{:020}-{kind}-{}.json",
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos())
                    .unwrap_or(0),
                crate::crypto::random_id(6).map_err(|e| e.to_string())?
            );
            tokio::task::spawn_blocking(move || -> Result<PathBuf, String> {
                create_private_dir(&dir)?;
                let path = dir.join(name);
                write_private(&path, &body)?;
                Ok(path)
            })
            .await
            .map_err(|e| e.to_string())?
            .map(|path| tracing::info!(path = %path.display(), "dev mailer wrote a message"))
        })
    }
}

fn create_private_dir(dir: &PathBuf) -> Result<(), String> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir).map_err(|e| format!("{}: {e}", dir.display()))
}

fn write_private(path: &PathBuf, body: &[u8]) -> Result<(), String> {
    use std::io::Write;
    let mut o = std::fs::OpenOptions::new();
    o.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    let mut f = o.open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    f.write_all(body).map_err(|e| e.to_string())
}

/// A mailer that drops everything (auth flows that need mail then fail
/// loudly at configuration time instead; used where mail is disabled).
#[derive(Debug, Default, Clone)]
pub struct NoMailer;

impl Mailer for NoMailer {
    fn send<'a>(&'a self, _mail: &'a Mail) -> MailFuture<'a> {
        Box::pin(async { Err("no mailer is configured".to_owned()) })
    }
}

/// Shared mailer handle.
pub type SharedMailer = Arc<dyn Mailer>;

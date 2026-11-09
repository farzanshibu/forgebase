//! LDAP sign-in (FB-25, ADR-0015 §5): a small pure-Rust LDAPv3 client
//! (simple bind and search over `ldap://`, StartTLS or `ldaps://`) and the
//! `POST /api/auth/ldap/login` flow.
//!
//! Flow: connect (TLS verified against webpki-roots plus `tls_ca_file` and
//! the realm's `ca_pem`), bind as the configured service account (or search
//! anonymously), search `base_dn` with `user_filter` (the username is
//! RFC 4515-escaped before substitution, so it cannot change the filter),
//! require exactly one entry, then bind as that entry's DN with the user's
//! password. An empty password is refused before any network traffic (an
//! LDAP simple bind with an empty password is an unauthenticated bind and
//! "succeeds"). Unknown user, ambiguous match and wrong password are the same
//! `invalid_credentials`. Plain `ldap://` without StartTLS sends passwords in
//! clear and is refused unless `insecure_plaintext` is set.
//!
//! The directory account is linked to a local user of the realm through
//! `_forgebase_oauth_identities` (provider `ldap`, subject = `id_attribute`
//! or the DN), with the same linking rules as social login (ADR-0009 §5).

pub mod ber;
pub mod filter;

use std::collections::HashMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;

use self::ber::{Reader, SEQUENCE};
use self::filter::{Filter, escape};
use crate::realm::{ConfigSecret, Method};
use crate::service::{ClientInfo, LoginOptions, LoginOutcome};
use crate::social::flow::Profile;
use crate::turnstile::TurnstileAction;
use crate::{AuthError, AuthMethod, AuthService, Subject};

/// Attributes of an entry: lower-cased name to values.
pub type Attributes = HashMap<String, Vec<Vec<u8>>>;

/// Provider id of LDAP identities.
pub const PROVIDER: &str = "ldap";

const STARTTLS_OID: &[u8] = b"1.3.6.1.4.1.1466.20037";

/// Search scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LdapScope {
    /// The base object only.
    Base,
    /// Its direct children.
    One,
    /// The whole subtree.
    #[default]
    Sub,
}

/// LDAP settings of a realm.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct LdapSettings {
    /// `ldap://host[:389]` or `ldaps://host[:636]`.
    pub url: String,
    /// Upgrade `ldap://` with StartTLS before binding.
    pub start_tls: bool,
    /// Allow `ldap://` without StartTLS (passwords in clear; tests, local
    /// sockets).
    pub insecure_plaintext: bool,
    /// Extra trust anchors (PEM) for this directory.
    pub ca_pem: Option<String>,
    /// Service account DN for the search (`None`: anonymous search).
    pub bind_dn: Option<String>,
    /// Its password (write-only).
    pub bind_password: ConfigSecret,
    /// Search base.
    pub base_dn: String,
    /// RFC 4515 filter with `{username}` (default `(uid={username})`).
    pub user_filter: String,
    /// Search scope.
    pub scope: LdapScope,
    /// Attribute holding the email.
    pub email_attribute: String,
    /// Attribute holding the display name.
    pub name_attribute: String,
    /// Stable id attribute (e.g. `entryUUID`, `objectGUID`); default the DN.
    pub id_attribute: Option<String>,
    /// The directory's email counts as verified (for signup and linking).
    pub trust_email: bool,
    /// Link to an existing local account with the same verified email.
    pub auto_link_verified_email: bool,
    /// Directory accounts without a local user may sign up.
    pub allow_signup: bool,
    /// Network timeout of the whole exchange (seconds, 1-60).
    pub timeout_secs: u64,
}

impl Default for LdapSettings {
    fn default() -> Self {
        Self {
            url: String::new(),
            start_tls: false,
            insecure_plaintext: false,
            ca_pem: None,
            bind_dn: None,
            bind_password: ConfigSecret::Keep,
            base_dn: String::new(),
            user_filter: "(uid={username})".into(),
            scope: LdapScope::Sub,
            email_attribute: "mail".into(),
            name_attribute: "cn".into(),
            id_attribute: None,
            trust_email: true,
            auto_link_verified_email: false,
            allow_signup: true,
            timeout_secs: 10,
        }
    }
}

impl LdapSettings {
    pub(crate) fn validate(&self) -> Result<(), AuthError> {
        let url = url::Url::parse(&self.url).map_err(|_| AuthError::invalid("ldap.url must be ldap:// or ldaps://"))?;
        match url.scheme() {
            "ldap" if !self.start_tls && !self.insecure_plaintext => {
                return Err(AuthError::invalid(
                    "ldap:// sends passwords in clear: set start_tls, use ldaps://, or set insecure_plaintext",
                ));
            }
            "ldaps" if self.start_tls => {
                return Err(AuthError::invalid("start_tls does not apply to ldaps://"));
            }
            "ldap" | "ldaps" => {}
            _ => return Err(AuthError::invalid("ldap.url must be ldap:// or ldaps://")),
        }
        if url.host_str().is_none_or(str::is_empty) {
            return Err(AuthError::invalid("ldap.url has no host"));
        }
        if self.base_dn.trim().is_empty() {
            return Err(AuthError::invalid("ldap.base_dn is required"));
        }
        if !self.user_filter.contains("{username}") {
            return Err(AuthError::invalid("ldap.user_filter must contain {username}"));
        }
        Filter::parse(&self.user_filter.replace("{username}", "x")).map_err(|e| AuthError::invalid(e.to_string()))?;
        if !(1..=60).contains(&self.timeout_secs) {
            return Err(AuthError::invalid("ldap.timeout_secs must be 1-60"));
        }
        if self.bind_dn.is_some() && self.bind_password.get().is_none() {
            return Err(AuthError::invalid("ldap.bind_dn needs bind_password"));
        }
        for a in [&self.email_attribute, &self.name_attribute]
            .into_iter()
            .chain(self.id_attribute.as_ref())
        {
            if a.is_empty() || !a.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'.') {
                return Err(AuthError::invalid(format!("bad LDAP attribute name {a:?}")));
            }
        }
        #[cfg(feature = "https")]
        if let Some(pem) = &self.ca_pem {
            crate::net::TlsSettings::new().with_pem(pem.as_bytes())?;
        }
        Ok(())
    }
}

/// A directory user who passed the bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LdapUser {
    /// Entry DN.
    pub dn: String,
    /// Stable id (`id_attribute` or the DN, lower-cased).
    pub id: String,
    /// Email attribute.
    pub email: Option<String>,
    /// Display name attribute.
    pub name: Option<String>,
}

/// Why a directory sign-in failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LdapFailure {
    /// Unknown user, ambiguous match, or wrong password.
    InvalidCredentials,
    /// Network, TLS, protocol or configuration failure (detail for logs).
    Unavailable(String),
}

fn unavailable(m: impl Into<String>) -> LdapFailure {
    LdapFailure::Unavailable(m.into())
}

// ---------------------------------------------------------------------------
// Protocol
// ---------------------------------------------------------------------------

/// A decoded LDAP message.
#[derive(Debug, Clone)]
pub struct Message {
    /// Message id.
    pub id: i64,
    /// Protocol operation tag.
    pub tag: u8,
    /// Operation contents.
    pub body: Vec<u8>,
}

/// Encodes an LDAPMessage.
pub fn message(id: i64, op: Vec<u8>) -> Vec<u8> {
    ber::constructed(SEQUENCE, &[ber::integer(id), op])
}

/// Decodes one LDAPMessage frame.
pub fn parse_message(frame: &[u8]) -> Result<Message, ber::BerError> {
    let seq = Reader::new(frame).expect(SEQUENCE)?;
    let mut r = Reader::new(seq);
    let id = r.integer()?;
    let (tag, body) = r.read_tlv()?;
    Ok(Message {
        id,
        tag,
        body: body.to_vec(),
    })
}

/// `(resultCode, diagnosticMessage)` of an LDAPResult.
pub fn ldap_result(body: &[u8]) -> Result<(i64, String), ber::BerError> {
    let mut r = Reader::new(body);
    let code = r.enumerated()?;
    let _matched = r.octets()?;
    let diag = String::from_utf8_lossy(r.octets()?).into_owned();
    Ok((code, diag))
}

/// Buffered message stream.
struct Conn<S> {
    io: S,
    buf: Vec<u8>,
    next_id: i64,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Conn<S> {
    fn new(io: S, next_id: i64) -> Self {
        Self {
            io,
            buf: Vec::new(),
            next_id,
        }
    }

    async fn send(&mut self, op: Vec<u8>) -> Result<i64, LdapFailure> {
        let id = self.next_id;
        self.next_id += 1;
        self.io
            .write_all(&message(id, op))
            .await
            .map_err(|e| unavailable(format!("write: {e}")))?;
        self.io.flush().await.map_err(|e| unavailable(format!("write: {e}")))?;
        Ok(id)
    }

    async fn recv(&mut self) -> Result<Message, LdapFailure> {
        loop {
            if let Some(n) = ber::frame_len(&self.buf).map_err(|_| unavailable("malformed LDAP message"))?
                && self.buf.len() >= n
            {
                let frame: Vec<u8> = self.buf.drain(..n).collect();
                let m = parse_message(&frame).map_err(|_| unavailable("malformed LDAP message"))?;
                if m.id == 0 {
                    return Err(unavailable("the directory closed the connection"));
                }
                return Ok(m);
            }
            let mut chunk = [0u8; 8192];
            let n = self
                .io
                .read(&mut chunk)
                .await
                .map_err(|e| unavailable(format!("read: {e}")))?;
            if n == 0 {
                return Err(unavailable("the directory closed the connection"));
            }
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }

    /// Response to request `id` with operation `tag`.
    async fn expect(&mut self, id: i64, tag: u8) -> Result<Vec<u8>, LdapFailure> {
        let m = self.recv().await?;
        if m.id != id || m.tag != tag {
            return Err(unavailable("unexpected LDAP response"));
        }
        Ok(m.body)
    }

    async fn bind(&mut self, dn: &str, password: &str) -> Result<i64, LdapFailure> {
        let op = ber::constructed(
            0x60,
            &[
                ber::integer(3),
                ber::octets(dn.as_bytes()),
                ber::tlv(0x80, password.as_bytes()),
            ],
        );
        let id = self.send(op).await?;
        let body = self.expect(id, 0x61).await?;
        let (code, _) = ldap_result(&body).map_err(|_| unavailable("malformed bind response"))?;
        Ok(code)
    }

    async fn search(
        &mut self,
        s: &LdapSettings,
        filter: &Filter,
        attrs: &[&str],
    ) -> Result<Vec<(String, Attributes)>, LdapFailure> {
        let scope = match s.scope {
            LdapScope::Base => 0,
            LdapScope::One => 1,
            LdapScope::Sub => 2,
        };
        let op = ber::constructed(
            0x63,
            &[
                ber::octets(s.base_dn.as_bytes()),
                ber::enumerated(scope),
                ber::enumerated(0), // neverDerefAliases
                ber::integer(2),    // two are enough to see ambiguity
                ber::integer(s.timeout_secs as i64),
                ber::boolean(false),
                filter.encode(),
                ber::constructed(
                    SEQUENCE,
                    &attrs.iter().map(|a| ber::octets(a.as_bytes())).collect::<Vec<_>>(),
                ),
            ],
        );
        let id = self.send(op).await?;
        let mut entries = Vec::new();
        loop {
            let m = self.recv().await?;
            if m.id != id {
                return Err(unavailable("unexpected LDAP response"));
            }
            match m.tag {
                0x64 => entries.push(parse_entry(&m.body).map_err(|_| unavailable("malformed search entry"))?),
                0x73 => {} // continuation reference: not followed
                0x65 => {
                    let (code, diag) = ldap_result(&m.body).map_err(|_| unavailable("malformed search result"))?;
                    // 4 = sizeLimitExceeded: more than one match.
                    if code != 0 && code != 4 {
                        return Err(unavailable(format!("search failed with result {code}: {diag}")));
                    }
                    return Ok(entries);
                }
                _ => return Err(unavailable("unexpected LDAP response")),
            }
        }
    }

    async fn unbind(mut self) {
        let _ = self.send(ber::tlv(0x42, &[])).await;
        let _ = self.io.shutdown().await;
    }
}

fn parse_entry(body: &[u8]) -> Result<(String, Attributes), ber::BerError> {
    let mut r = Reader::new(body);
    let dn = String::from_utf8_lossy(r.octets()?).into_owned();
    let mut attrs = HashMap::new();
    let mut list = Reader::new(r.expect(SEQUENCE)?);
    while !list.is_empty() {
        let mut pa = Reader::new(list.expect(SEQUENCE)?);
        let name = String::from_utf8_lossy(pa.octets()?).to_ascii_lowercase();
        let mut vals = Reader::new(pa.expect(ber::SET)?);
        let mut v = Vec::new();
        while !vals.is_empty() {
            v.push(vals.octets()?.to_vec());
        }
        attrs.insert(name, v);
    }
    Ok((dn, attrs))
}

/// Authenticates `username` / `password` against the directory.
pub async fn authenticate(
    s: &LdapSettings,
    #[cfg(feature = "https")] tls: &crate::net::TlsSettings,
    username: &str,
    password: &str,
) -> Result<LdapUser, LdapFailure> {
    if password.is_empty() {
        return Err(LdapFailure::InvalidCredentials);
    }
    let url = url::Url::parse(&s.url).map_err(|_| unavailable("bad ldap url"))?;
    let secure = url.scheme() == "ldaps";
    let (host, port) =
        crate::net::host_port(&url, if secure { 636 } else { 389 }).map_err(|e| unavailable(e.to_string()))?;
    let tcp = TcpStream::connect((host.as_str(), port))
        .await
        .map_err(|e| unavailable(format!("connect {host}:{port}: {e}")))?;
    let _ = tcp.set_nodelay(true);
    if secure || s.start_tls {
        #[cfg(feature = "https")]
        {
            let mut tls = tls.clone();
            if let Some(pem) = &s.ca_pem {
                tls = tls.with_pem(pem.as_bytes()).map_err(|e| unavailable(e.to_string()))?;
            }
            let cfg = tls.client_config(&[]).map_err(|e| unavailable(e.to_string()))?;
            let mut next_id = 1;
            let tcp = if s.start_tls {
                let mut c = Conn::new(tcp, 1);
                let id = c.send(ber::constructed(0x77, &[ber::tlv(0x80, STARTTLS_OID)])).await?;
                let body = c.expect(id, 0x78).await?;
                let (code, diag) = ldap_result(&body).map_err(|_| unavailable("malformed StartTLS response"))?;
                if code != 0 || !c.buf.is_empty() {
                    return Err(unavailable(format!("StartTLS refused ({code}): {diag}")));
                }
                next_id = c.next_id;
                c.io
            } else {
                tcp
            };
            let stream = crate::net::TlsSettings::upgrade(&cfg, &host, tcp)
                .await
                .map_err(|e| unavailable(e.to_string()))?;
            return run(Conn::new(stream, next_id), s, username, password).await;
        }
        #[cfg(not(feature = "https"))]
        return Err(unavailable("LDAP over TLS needs the `https` feature"));
    }
    run(Conn::new(tcp, 1), s, username, password).await
}

async fn run<S: AsyncRead + AsyncWrite + Unpin>(
    mut c: Conn<S>,
    s: &LdapSettings,
    username: &str,
    password: &str,
) -> Result<LdapUser, LdapFailure> {
    if let Some(dn) = &s.bind_dn {
        let code = c.bind(dn, s.bind_password.get().unwrap_or("")).await?;
        if code != 0 {
            return Err(unavailable(format!("service account bind failed with result {code}")));
        }
    }
    let filter = Filter::parse(&s.user_filter.replace("{username}", &escape(username)))
        .map_err(|e| unavailable(e.to_string()))?;
    let mut attrs = vec![s.email_attribute.as_str(), s.name_attribute.as_str()];
    if let Some(a) = &s.id_attribute {
        attrs.push(a);
    }
    let entries = c.search(s, &filter, &attrs).await?;
    if entries.len() != 1 {
        if entries.len() > 1 {
            tracing::warn!("LDAP user filter matched several entries; refusing the sign-in");
        }
        c.unbind().await;
        return Err(LdapFailure::InvalidCredentials);
    }
    let (dn, attrs) = entries.into_iter().next().unwrap_or_default();
    let code = c.bind(&dn, password).await?;
    c.unbind().await;
    match code {
        0 => {}
        49 => return Err(LdapFailure::InvalidCredentials),
        other => return Err(unavailable(format!("user bind failed with result {other}"))),
    }
    let first = |a: &str| {
        attrs
            .get(&a.to_ascii_lowercase())
            .and_then(|v| v.first())
            .map(|v| String::from_utf8_lossy(v).trim().to_owned())
            .filter(|v| !v.is_empty())
    };
    let id = match &s.id_attribute {
        Some(a) => first(a).ok_or_else(|| unavailable(format!("entry has no {a}")))?,
        None => dn.to_ascii_lowercase(),
    };
    Ok(LdapUser {
        id,
        email: first(&s.email_attribute),
        name: first(&s.name_attribute),
        dn,
    })
}

// ---------------------------------------------------------------------------
// Sign-in
// ---------------------------------------------------------------------------

impl AuthService {
    /// Signs in with a directory username and password.
    pub async fn login_ldap(
        &self,
        ci: &ClientInfo,
        username: &str,
        password: &str,
        opts: &LoginOptions,
    ) -> Result<LoginOutcome, AuthError> {
        self.limit_attempt(ci)?;
        let realm = self.request_realm(ci)?;
        realm.require(Method::Ldap)?;
        self.turnstile_check(&realm, TurnstileAction::Login, ci).await?;
        let settings = realm
            .config
            .ldap
            .clone()
            .ok_or_else(|| AuthError::Forbidden("LDAP is not configured in this realm".into()))?;
        let username = username.trim();
        if username.is_empty() || username.len() > 256 || username.chars().any(char::is_control) {
            return Err(AuthError::invalid("username must be 1-256 printable characters"));
        }
        let lock_key = format!("ldap:{}:{}", realm.realm, username.to_lowercase());
        self.lockout_check(&lock_key).await?;
        let timeout = Duration::from_secs(settings.timeout_secs);
        #[cfg(feature = "https")]
        let tls = self.tls_settings()?;
        let res = tokio::time::timeout(
            timeout,
            authenticate(
                &settings,
                #[cfg(feature = "https")]
                &tls,
                username,
                password,
            ),
        )
        .await
        .unwrap_or_else(|_| Err(unavailable("timed out")));
        let user = match res {
            Ok(u) => u,
            Err(LdapFailure::InvalidCredentials) => {
                let locked = self.lockout_fail(&lock_key).await?;
                self.audit(
                    "login",
                    false,
                    ci,
                    "anonymous",
                    None,
                    json!({"realm": realm.realm, "method": "ldap", "reason": "invalid_credentials", "locked": locked}),
                )
                .await?;
                return Err(AuthError::InvalidCredentials);
            }
            Err(LdapFailure::Unavailable(detail)) => {
                tracing::warn!(realm = %realm.realm, %detail, "LDAP directory unavailable");
                self.audit(
                    "login",
                    false,
                    ci,
                    "anonymous",
                    None,
                    json!({"realm": realm.realm, "method": "ldap", "reason": "directory_unavailable"}),
                )
                .await?;
                return Err(AuthError::Provider("the directory is unavailable".into()));
            }
        };
        self.lockout_clear(&lock_key).await?;
        let coll = self.auth_collection(Some(&realm.collection)).await?;
        let subject_key: String = if user.id.len() <= 255 {
            user.id.clone()
        } else {
            crate::crypto::hex(&<sha2::Sha256 as sha2::Digest>::digest(user.id.as_bytes()))
        };
        let email = user
            .email
            .as_deref()
            .and_then(|e| crate::service::normalize_email(e).ok());
        let profile = Profile {
            subject: subject_key,
            email_verified: settings.trust_email && email.is_some(),
            email,
            name: user.name.clone(),
        };
        let (collection, uid) = match self
            .identity_by_subject(&realm.realm, PROVIDER, &profile.subject)
            .await?
        {
            Some((_, c, u)) => (c, u),
            None => {
                self.external_first_login(
                    ci,
                    &realm,
                    PROVIDER,
                    settings.auto_link_verified_email,
                    settings.allow_signup,
                    &coll,
                    &profile,
                    "ldap.invalid",
                )
                .await?
            }
        };
        let subject = Subject::User { collection, id: uid };
        if !self.subject_exists(&subject).await? {
            return Err(AuthError::InvalidCredentials);
        }
        self.complete_first_factor(&subject, AuthMethod::Ldap, ci, opts).await
    }
}

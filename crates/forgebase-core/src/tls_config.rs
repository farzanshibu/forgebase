//! `[tls]` and `[tls.acme]`: native TLS (ADR-0033).
//!
//! Off by default: the binary serves plain HTTP and a reverse proxy
//! terminates TLS in front of it (docs/deployment.md §5). `mode = "files"`
//! serves HTTPS with a certificate chain and key read from PEM files (reloaded
//! when the files change or on `SIGHUP`); `mode = "acme"` obtains and renews a
//! certificate from an RFC 8555 certificate authority (Let's Encrypt by
//! default).
//!
//! Every key is in the FB-91 registry ([`crate::features`]) and has an
//! environment variable `FORGEBASE_TLS_<KEY>` / `FORGEBASE_TLS_ACME_<KEY>`.
//! Lists (`domains`, `challenges`) take comma-separated values in the
//! environment. The EAB HMAC key is a secret: never listed with a value.

use std::fmt;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use super::{ConfigError, normalize, parse_bool, parse_num, whole};

/// Let's Encrypt's production directory, the default ACME CA.
pub const LETS_ENCRYPT_PRODUCTION: &str = "https://acme-v02.api.letsencrypt.org/directory";
/// Let's Encrypt's staging directory (untrusted certificates, high rate limits).
pub const LETS_ENCRYPT_STAGING: &str = "https://acme-staging-v02.api.letsencrypt.org/directory";
/// Environment variable holding the EAB HMAC key (a secret).
pub const ENV_ACME_EAB_HMAC_KEY: &str = "FORGEBASE_TLS_ACME_EAB_HMAC_KEY";

/// How the HTTP listener does TLS (`[tls] mode`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TlsMode {
    /// Plain HTTP; a reverse proxy terminates TLS (the default).
    #[default]
    Off,
    /// HTTPS with a certificate and key from PEM files.
    Files,
    /// HTTPS with a certificate obtained and renewed over ACME.
    Acme,
}

impl TlsMode {
    /// The configuration value.
    pub fn as_str(self) -> &'static str {
        match self {
            TlsMode::Off => "off",
            TlsMode::Files => "files",
            TlsMode::Acme => "acme",
        }
    }

    /// Whether the HTTP listener serves HTTPS.
    pub fn is_on(self) -> bool {
        self != TlsMode::Off
    }
}

impl FromStr for TlsMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "off" => Ok(TlsMode::Off),
            "files" => Ok(TlsMode::Files),
            "acme" => Ok(TlsMode::Acme),
            other => Err(format!("{other:?}: expected off, files or acme")),
        }
    }
}

impl fmt::Display for TlsMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Oldest TLS version accepted (`[tls] min_version`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum TlsVersion {
    /// TLS 1.2 and 1.3 (the default).
    #[default]
    #[serde(rename = "1.2")]
    V1_2,
    /// TLS 1.3 only.
    #[serde(rename = "1.3")]
    V1_3,
}

impl TlsVersion {
    /// The configuration value.
    pub fn as_str(self) -> &'static str {
        match self {
            TlsVersion::V1_2 => "1.2",
            TlsVersion::V1_3 => "1.3",
        }
    }
}

impl FromStr for TlsVersion {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "1.2" => Ok(TlsVersion::V1_2),
            "1.3" => Ok(TlsVersion::V1_3),
            other => Err(format!("{other:?}: expected 1.2 or 1.3")),
        }
    }
}

/// An ACME challenge type (`[tls.acme] challenges`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AcmeChallenge {
    /// RFC 8737: answered on the HTTPS port (443 seen from the CA).
    #[serde(rename = "tls-alpn-01")]
    TlsAlpn01,
    /// RFC 8555 §8.3: answered on the plain HTTP port (80 seen from the CA).
    #[serde(rename = "http-01")]
    Http01,
}

impl AcmeChallenge {
    /// The configuration value (the ACME challenge type).
    pub fn as_str(self) -> &'static str {
        match self {
            AcmeChallenge::TlsAlpn01 => "tls-alpn-01",
            AcmeChallenge::Http01 => "http-01",
        }
    }
}

impl FromStr for AcmeChallenge {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s.trim() {
            "tls-alpn-01" => Ok(AcmeChallenge::TlsAlpn01),
            "http-01" => Ok(AcmeChallenge::Http01),
            other => Err(format!("{other:?}: expected tls-alpn-01 or http-01")),
        }
    }
}

/// A secret value (the EAB HMAC key); `Debug` never prints it.
#[derive(Clone, PartialEq, Eq)]
pub struct SecretValue(String);

impl SecretValue {
    /// Wraps a secret.
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// The secret itself.
    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SecretValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SecretValue(***)")
    }
}

/// `[tls.acme]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AcmeConfig {
    /// DNS names on the certificate; the first is its subject.
    pub domains: Vec<String>,
    /// Contact address the CA writes to about expiry and policy changes.
    pub email: String,
    /// The CA's directory URL, or `letsencrypt` / `letsencrypt-staging`.
    pub directory_url: String,
    /// Challenge types to answer, in order of preference.
    pub challenges: Vec<AcmeChallenge>,
    /// External account binding key id (ZeroSSL, Google, some step-ca setups).
    pub eab_kid: String,
    /// External account binding HMAC key (base64url). Secret: never listed.
    #[serde(skip)]
    pub eab_hmac_key: Option<SecretValue>,
    /// Extra PEM trust anchors for the CA's own HTTPS (a private CA such as
    /// step-ca, or Pebble in tests); absolute once loaded, `""` = none.
    pub ca_file: String,
}

impl Default for AcmeConfig {
    fn default() -> Self {
        Self {
            domains: Vec::new(),
            email: String::new(),
            directory_url: LETS_ENCRYPT_PRODUCTION.to_owned(),
            challenges: vec![AcmeChallenge::TlsAlpn01],
            eab_kid: String::new(),
            eab_hmac_key: None,
            ca_file: String::new(),
        }
    }
}

impl AcmeConfig {
    /// The directory URL with the `letsencrypt` and `letsencrypt-staging`
    /// shortcuts resolved.
    pub fn directory(&self) -> &str {
        match self.directory_url.trim() {
            "letsencrypt" | "letsencrypt-production" => LETS_ENCRYPT_PRODUCTION,
            "letsencrypt-staging" => LETS_ENCRYPT_STAGING,
            other => other,
        }
    }

    /// Whether `challenge` is one of the configured challenge types.
    pub fn uses(&self, challenge: AcmeChallenge) -> bool {
        self.challenges.contains(&challenge)
    }
}

/// Default interval of the certificate file check (`reload_interval_secs`).
pub const DEFAULT_RELOAD_INTERVAL_SECS: u64 = 30;
/// Default port of the plain HTTP listener (`http_port`).
pub const DEFAULT_HTTP_PORT: u16 = 80;

/// `[tls]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TlsConfig {
    /// `off`, `files` or `acme`.
    pub mode: TlsMode,
    /// Files mode: PEM certificate chain, leaf first (absolute once loaded).
    pub cert_file: String,
    /// Files mode: PEM private key of the leaf (absolute once loaded).
    pub key_file: String,
    /// Files mode: how often the files are checked for changes, in seconds;
    /// `0` = only on `SIGHUP`.
    pub reload_interval_secs: u64,
    /// Oldest TLS version accepted.
    pub min_version: TlsVersion,
    /// `Strict-Transport-Security` max-age in seconds; `0` = no header.
    pub hsts_max_age_secs: u64,
    /// Add `includeSubDomains` to the HSTS header.
    pub hsts_include_subdomains: bool,
    /// Run a plain HTTP listener on `http_port` that redirects to HTTPS (it
    /// always runs when the `http-01` challenge is used).
    pub redirect_http: bool,
    /// Port of the plain HTTP listener (redirects and `http-01`), on
    /// `server.host`; `0` picks a free port.
    pub http_port: u16,
    /// `[tls.acme]`.
    pub acme: AcmeConfig,
}

impl Default for TlsConfig {
    fn default() -> Self {
        Self {
            mode: TlsMode::Off,
            cert_file: String::new(),
            key_file: String::new(),
            reload_interval_secs: DEFAULT_RELOAD_INTERVAL_SECS,
            min_version: TlsVersion::V1_2,
            hsts_max_age_secs: 0,
            hsts_include_subdomains: false,
            redirect_http: false,
            http_port: DEFAULT_HTTP_PORT,
            acme: AcmeConfig::default(),
        }
    }
}

impl TlsConfig {
    /// Whether the plain HTTP listener runs: TLS is on and either redirects
    /// are asked for or `http-01` must be answered.
    pub fn plain_listener(&self) -> bool {
        match self.mode {
            TlsMode::Off => false,
            TlsMode::Files => self.redirect_http,
            TlsMode::Acme => self.redirect_http || self.acme.uses(AcmeChallenge::Http01),
        }
    }
}

/// `[tls.acme]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AcmeOverrides {
    /// See [`AcmeConfig::domains`].
    pub domains: Option<Vec<String>>,
    /// See [`AcmeConfig::email`].
    pub email: Option<String>,
    /// See [`AcmeConfig::directory_url`].
    pub directory_url: Option<String>,
    /// See [`AcmeConfig::challenges`].
    pub challenges: Option<Vec<AcmeChallenge>>,
    /// See [`AcmeConfig::eab_kid`].
    pub eab_kid: Option<String>,
    /// See [`AcmeConfig::eab_hmac_key`]; prefer the environment variable.
    pub eab_hmac_key: Option<String>,
    /// See [`AcmeConfig::ca_file`] (relative to the config file's directory).
    pub ca_file: Option<String>,
}

/// `[tls]` from the file or the environment.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsOverrides {
    /// See [`TlsConfig::mode`].
    pub mode: Option<TlsMode>,
    /// See [`TlsConfig::cert_file`] (relative to the config file's directory).
    pub cert_file: Option<String>,
    /// See [`TlsConfig::key_file`] (relative to the config file's directory).
    pub key_file: Option<String>,
    /// See [`TlsConfig::reload_interval_secs`].
    pub reload_interval_secs: Option<u64>,
    /// See [`TlsConfig::min_version`].
    pub min_version: Option<TlsVersion>,
    /// See [`TlsConfig::hsts_max_age_secs`].
    pub hsts_max_age_secs: Option<u64>,
    /// See [`TlsConfig::hsts_include_subdomains`].
    pub hsts_include_subdomains: Option<bool>,
    /// See [`TlsConfig::redirect_http`].
    pub redirect_http: Option<bool>,
    /// See [`TlsConfig::http_port`].
    pub http_port: Option<u16>,
    /// `[tls.acme]`.
    pub acme: Option<AcmeOverrides>,
}

fn list(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .collect()
}

impl TlsOverrides {
    fn acme(&mut self) -> &mut AcmeOverrides {
        self.acme.get_or_insert_with(AcmeOverrides::default)
    }

    /// Reads one environment variable: `key` is the name after
    /// `FORGEBASE_`. `Ok(false)` when it is not a TLS variable.
    ///
    /// # Errors
    /// A value that does not parse (the message says what to enter).
    pub fn env(&mut self, key: &str, value: &str) -> Result<bool, String> {
        if let Some(k) = key.strip_prefix("TLS_ACME_") {
            let a = self.acme();
            match k {
                "DOMAINS" => a.domains = Some(list(value)),
                "EMAIL" => a.email = Some(value.trim().to_owned()),
                "DIRECTORY_URL" => a.directory_url = Some(value.trim().to_owned()),
                "CHALLENGES" => {
                    a.challenges = Some(list(value).iter().map(|c| c.parse()).collect::<Result<_, _>>()?);
                }
                "EAB_KID" => a.eab_kid = Some(value.trim().to_owned()),
                "EAB_HMAC_KEY" => a.eab_hmac_key = Some(value.trim().to_owned()),
                "CA_FILE" => a.ca_file = Some(value.to_owned()),
                _ => return Ok(false),
            }
            return Ok(true);
        }
        let Some(k) = key.strip_prefix("TLS_") else {
            return Ok(false);
        };
        match k {
            "MODE" => self.mode = Some(value.parse()?),
            "CERT_FILE" => self.cert_file = Some(value.to_owned()),
            "KEY_FILE" => self.key_file = Some(value.to_owned()),
            "RELOAD_INTERVAL_SECS" => self.reload_interval_secs = Some(parse_num(value)?),
            "MIN_VERSION" => self.min_version = Some(value.parse()?),
            "HSTS_MAX_AGE_SECS" => self.hsts_max_age_secs = Some(parse_num(value)?),
            "HSTS_INCLUDE_SUBDOMAINS" => self.hsts_include_subdomains = Some(parse_bool(value)?),
            "REDIRECT_HTTP" => self.redirect_http = Some(parse_bool(value)?),
            "HTTP_PORT" => self.http_port = Some(parse_num(value)?),
            _ => return Ok(false),
        }
        Ok(true)
    }

    /// Whether this layer sets the EAB HMAC key (a secret, reported by its
    /// variable name).
    pub fn sets_secret(&self) -> bool {
        self.acme
            .as_ref()
            .and_then(|a| a.eab_hmac_key.as_deref())
            .is_some_and(|k| !k.trim().is_empty())
    }

    /// Dotted registry keys this layer sets.
    pub fn keys(&self, put: &mut dyn FnMut(bool, &'static str)) {
        put(self.mode.is_some(), "tls.mode");
        put(self.cert_file.is_some(), "tls.cert_file");
        put(self.key_file.is_some(), "tls.key_file");
        put(self.reload_interval_secs.is_some(), "tls.reload_interval_secs");
        put(self.min_version.is_some(), "tls.min_version");
        put(self.hsts_max_age_secs.is_some(), "tls.hsts_max_age_secs");
        put(self.hsts_include_subdomains.is_some(), "tls.hsts_include_subdomains");
        put(self.redirect_http.is_some(), "tls.redirect_http");
        put(self.http_port.is_some(), "tls.http_port");
        if let Some(a) = &self.acme {
            put(a.domains.is_some(), "tls.acme.domains");
            put(a.email.is_some(), "tls.acme.email");
            put(a.directory_url.is_some(), "tls.acme.directory_url");
            put(a.challenges.is_some(), "tls.acme.challenges");
            put(a.eab_kid.is_some(), "tls.acme.eab_kid");
            put(a.ca_file.is_some(), "tls.acme.ca_file");
        }
    }
}

fn path_value(base_dir: &Path, v: &str) -> String {
    let v = v.trim();
    if v.is_empty() {
        String::new()
    } else {
        normalize(&base_dir.join(v)).display().to_string()
    }
}

impl TlsConfig {
    /// Applies one layer; relative paths resolve against `base_dir`.
    pub fn apply(&mut self, o: &TlsOverrides, base_dir: &Path) {
        if let Some(v) = o.mode {
            self.mode = v;
        }
        if let Some(v) = &o.cert_file {
            self.cert_file = path_value(base_dir, v);
        }
        if let Some(v) = &o.key_file {
            self.key_file = path_value(base_dir, v);
        }
        if let Some(v) = o.reload_interval_secs {
            self.reload_interval_secs = v;
        }
        if let Some(v) = o.min_version {
            self.min_version = v;
        }
        if let Some(v) = o.hsts_max_age_secs {
            self.hsts_max_age_secs = v;
        }
        if let Some(v) = o.hsts_include_subdomains {
            self.hsts_include_subdomains = v;
        }
        if let Some(v) = o.redirect_http {
            self.redirect_http = v;
        }
        if let Some(v) = o.http_port {
            self.http_port = v;
        }
        if let Some(a) = &o.acme {
            let c = &mut self.acme;
            if let Some(v) = &a.domains {
                c.domains = v
                    .iter()
                    .map(|d| d.trim().trim_end_matches('.').to_ascii_lowercase())
                    .filter(|d| !d.is_empty())
                    .collect();
            }
            if let Some(v) = &a.email {
                v.trim().clone_into(&mut c.email);
            }
            if let Some(v) = &a.directory_url {
                v.trim().clone_into(&mut c.directory_url);
            }
            if let Some(v) = &a.challenges {
                c.challenges.clone_from(v);
            }
            if let Some(v) = &a.eab_kid {
                v.trim().clone_into(&mut c.eab_kid);
            }
            if let Some(v) = &a.eab_hmac_key {
                let v = v.trim();
                c.eab_hmac_key = (!v.is_empty()).then(|| SecretValue::new(v));
            }
            if let Some(v) = &a.ca_file {
                c.ca_file = path_value(base_dir, v);
            }
        }
    }

    /// Problems with the values (each says what to enter instead). `server_port`
    /// is `[server] port`, which the plain HTTP listener must not reuse.
    pub fn problems(&self, server_port: u16, out: &mut Vec<ConfigError>) {
        let d = TlsConfig::default();
        let mut invalid = |field: &'static str, message: String| out.push(ConfigError::Invalid { field, message });
        if self.reload_interval_secs > 86_400 {
            invalid(
                "tls.reload_interval_secs",
                whole(0, 86_400, "seconds", d.reload_interval_secs),
            );
        }
        if self.hsts_max_age_secs > 63_072_000 {
            invalid("tls.hsts_max_age_secs", whole(0, 63_072_000, "seconds", 31_536_000));
        }
        if self.plain_listener() && self.http_port != 0 && self.http_port == server_port {
            invalid(
                "tls.http_port",
                format!(
                    "must differ from server.port ({server_port}): it is the plain HTTP listener that redirects to HTTPS, such as {}",
                    d.http_port
                ),
            );
        }
        match self.mode {
            TlsMode::Off => {}
            TlsMode::Files => {
                if self.cert_file.trim().is_empty() {
                    invalid(
                        "tls.cert_file",
                        "must be set when mode is files: the PEM certificate chain, such as /etc/forgebase/tls/fullchain.pem"
                            .into(),
                    );
                }
                if self.key_file.trim().is_empty() {
                    invalid(
                        "tls.key_file",
                        "must be set when mode is files: the PEM private key, such as /etc/forgebase/tls/privkey.pem"
                            .into(),
                    );
                }
            }
            TlsMode::Acme => {
                let a = &self.acme;
                if a.domains.is_empty() {
                    invalid(
                        "tls.acme.domains",
                        "must list at least one domain name when mode is acme, such as [\"forgebase.example.com\"]"
                            .into(),
                    );
                }
                for name in &a.domains {
                    if let Some(why) = domain_problem(name) {
                        invalid("tls.acme.domains", format!("{name:?} {why}"));
                    }
                }
                let mut seen = Vec::new();
                for c in &a.domains {
                    if seen.contains(&c) {
                        invalid("tls.acme.domains", format!("{c:?} is listed twice"));
                    }
                    seen.push(c);
                }
                if a.challenges.is_empty() {
                    invalid(
                        "tls.acme.challenges",
                        "must list tls-alpn-01, http-01 or both, such as [\"tls-alpn-01\"]".into(),
                    );
                }
                if a.challenges.len() == 2 && a.challenges[0] == a.challenges[1] {
                    invalid("tls.acme.challenges", "lists the same challenge twice".into());
                }
                let email = a.email.trim();
                if !email.is_empty()
                    && (!email.contains('@') || email.contains(|c: char| c.is_whitespace() || c == ',' || c == ':'))
                {
                    invalid(
                        "tls.acme.email",
                        "must be one email address, or empty, such as ops@example.com".into(),
                    );
                }
                let dir = a.directory();
                if !dir.starts_with("https://") || !super::is_origin_url(origin_of(dir)) {
                    invalid(
                        "tls.acme.directory_url",
                        format!(
                            "must be the https:// directory URL of an ACME certificate authority, or letsencrypt or letsencrypt-staging, such as {LETS_ENCRYPT_PRODUCTION}"
                        ),
                    );
                }
                if a.eab_kid.trim().is_empty() != a.eab_hmac_key.is_none() {
                    invalid(
                        "tls.acme.eab_kid",
                        format!("set eab_kid together with the HMAC key ({ENV_ACME_EAB_HMAC_KEY}), or neither"),
                    );
                }
                if let Some(k) = &a.eab_hmac_key
                    && !k
                        .expose()
                        .bytes()
                        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'='))
                {
                    invalid(
                        "tls.acme.eab_kid",
                        format!("the HMAC key in {ENV_ACME_EAB_HMAC_KEY} must be base64url text, as the CA shows it"),
                    );
                }
            }
        }
    }
}

/// `https://host:port` of a URL with a path.
fn origin_of(url: &str) -> &str {
    let rest = url.strip_prefix("https://").unwrap_or(url);
    match rest.find(['/', '?', '#']) {
        Some(i) => &url[..url.len() - rest.len() + i],
        None => url,
    }
}

/// Why `name` cannot be on an ACME certificate, if it cannot.
fn domain_problem(name: &str) -> Option<&'static str> {
    if name.starts_with("*.") {
        return Some(
            "is a wildcard; wildcard certificates need the dns-01 challenge, which ForgeBase does not answer (use mode = \"files\" with a wildcard certificate)",
        );
    }
    if name.parse::<std::net::IpAddr>().is_ok() {
        return Some("is an IP address; list domain names only");
    }
    let ok = !name.is_empty()
        && name.len() <= 253
        && name.contains('.')
        && name.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        });
    (!ok).then_some("is not a domain name such as forgebase.example.com")
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn env_variables_map_to_keys() {
        let mut o = TlsOverrides::default();
        assert!(o.env("TLS_MODE", "acme").unwrap());
        assert!(o.env("TLS_MIN_VERSION", "1.3").unwrap());
        assert!(o.env("TLS_ACME_DOMAINS", "a.example.com, b.example.com").unwrap());
        assert!(o.env("TLS_ACME_CHALLENGES", "http-01,tls-alpn-01").unwrap());
        assert!(o.env("TLS_ACME_EAB_HMAC_KEY", "c2VjcmV0").unwrap());
        assert!(!o.env("TLS_NOPE", "1").unwrap());
        assert!(!o.env("RESP_TLS", "1").unwrap());
        assert!(o.env("TLS_MODE", "maybe").is_err());
        assert!(o.env("TLS_ACME_CHALLENGES", "dns-01").is_err());
        assert!(o.sets_secret());
        let mut keys = Vec::new();
        o.keys(&mut |set, k| {
            if set {
                keys.push(k);
            }
        });
        assert_eq!(
            keys,
            ["tls.mode", "tls.min_version", "tls.acme.domains", "tls.acme.challenges"]
        );
        let mut c = TlsConfig::default();
        c.apply(&o, Path::new("/etc/fb"));
        assert_eq!(c.mode, TlsMode::Acme);
        assert_eq!(c.min_version, TlsVersion::V1_3);
        assert_eq!(c.acme.domains, ["a.example.com", "b.example.com"]);
        assert_eq!(c.acme.challenges, [AcmeChallenge::Http01, AcmeChallenge::TlsAlpn01]);
        assert_eq!(c.acme.eab_hmac_key.as_ref().map(SecretValue::expose), Some("c2VjcmV0"));
        assert!(c.plain_listener(), "http-01 needs the plain listener");
        assert!(!format!("{c:?}").contains("c2VjcmV0"), "the secret is not printed");
    }

    #[test]
    fn relative_paths_resolve_against_the_base_directory() {
        let o = TlsOverrides {
            mode: Some(TlsMode::Files),
            cert_file: Some("tls/fullchain.pem".into()),
            key_file: Some("/abs/key.pem".into()),
            ..TlsOverrides::default()
        };
        let mut c = TlsConfig::default();
        c.apply(&o, Path::new("/etc/fb"));
        assert_eq!(c.cert_file, "/etc/fb/tls/fullchain.pem");
        assert_eq!(c.key_file, "/abs/key.pem");
    }

    #[test]
    fn validation_says_what_to_enter() {
        let mut out = Vec::new();
        TlsConfig::default().problems(8090, &mut out);
        assert!(out.is_empty(), "defaults are valid: {out:?}");

        let mut c = TlsConfig {
            mode: TlsMode::Files,
            ..TlsConfig::default()
        };
        c.problems(8090, &mut out);
        let all = out.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        assert!(all.contains("tls.cert_file") && all.contains("tls.key_file"), "{all}");

        out.clear();
        c.mode = TlsMode::Acme;
        c.redirect_http = true;
        c.http_port = 8090;
        c.acme.domains = vec![
            "*.example.com".into(),
            "10.0.0.1".into(),
            "bad_name".into(),
            "ok.example.com".into(),
            "ok.example.com".into(),
        ];
        c.acme.email = "not an email".into();
        c.acme.directory_url = "http://ca.example.com/dir".into();
        c.acme.eab_kid = "kid".into();
        c.acme.challenges.clear();
        c.reload_interval_secs = 90_000;
        c.problems(8090, &mut out);
        let all = out.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n");
        for needle in [
            "wildcard",
            "IP address",
            "\"bad_name\" is not a domain name",
            "listed twice",
            "tls.acme.email",
            "tls.acme.directory_url",
            "tls.acme.eab_kid",
            "tls.acme.challenges",
            "must differ from server.port (8090)",
            "tls.reload_interval_secs",
        ] {
            assert!(all.contains(needle), "{needle} in:\n{all}");
        }

        out.clear();
        let ok = TlsConfig {
            mode: TlsMode::Acme,
            acme: AcmeConfig {
                domains: vec!["forgebase.example.com".into()],
                directory_url: "letsencrypt-staging".into(),
                ..AcmeConfig::default()
            },
            ..TlsConfig::default()
        };
        ok.problems(443, &mut out);
        assert!(out.is_empty(), "{out:?}");
        assert_eq!(ok.acme.directory(), LETS_ENCRYPT_STAGING);
        assert!(!ok.plain_listener(), "tls-alpn-01 alone needs no port 80");
    }

    #[test]
    fn a_private_ca_directory_with_a_port_is_accepted() {
        let mut out = Vec::new();
        let c = TlsConfig {
            mode: TlsMode::Acme,
            acme: AcmeConfig {
                domains: vec!["app.internal.example".into()],
                directory_url: "https://ca.internal:9000/acme/acme/directory".into(),
                ..AcmeConfig::default()
            },
            ..TlsConfig::default()
        };
        c.problems(443, &mut out);
        assert!(out.is_empty(), "{out:?}");
    }
}

//! Outbound networking shared by social login, Turnstile, SMS gateways and
//! LDAP (FB-25, ADR-0015 §2).
//!
//! * [`http1`]: a small HTTP/1.1 client exchange over any byte stream, with a
//!   header and body size cap and proper message framing (`Content-Length`,
//!   chunked, or read to close), so a server that ignores `Connection: close`
//!   cannot stall a request until its timeout.
//! * [`tls`] (feature `https`, on by default): rustls with the ring provider
//!   and the webpki-roots trust anchors, plus optional extra PEM anchors, and
//!   [`HttpsClient`], the default [`HttpClient`](crate::social::HttpClient)
//!   of the auth service.

pub mod http1;
#[cfg(feature = "https")]
pub mod tls;

#[cfg(feature = "https")]
pub use tls::{HttpsClient, TlsSettings};

/// Host and port of a URL, with IPv6 literals unbracketed (for `connect` and
/// for TLS server names).
pub(crate) fn host_port(url: &url::Url, default_port: u16) -> Result<(String, u16), crate::AuthError> {
    let host = match url.host() {
        Some(url::Host::Domain(d)) => d.to_owned(),
        Some(url::Host::Ipv4(a)) => a.to_string(),
        Some(url::Host::Ipv6(a)) => a.to_string(),
        None => return Err(crate::AuthError::Provider("URL has no host".into())),
    };
    Ok((host, url.port_or_known_default().unwrap_or(default_port)))
}

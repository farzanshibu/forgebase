//! SSRF-safe outbound HTTP for the `fetch` capability.
//!
//! Every hop of a request (the first URL and each redirect target) is checked
//! the same way:
//!
//! 1. The URL is `http` or `https`, has a host, no user name or password, and
//!    (if the version lists `fetch_hosts`) a host on that list.
//! 2. The host is resolved **once** ([`Resolver`]) and **every** address is
//!    checked against the blocked ranges ([`blocked_reason`]): loopback,
//!    private, carrier-grade NAT, link-local (which holds the cloud metadata
//!    service at `169.254.169.254`), unique-local, multicast, documentation,
//!    benchmark, reserved and unspecified ranges, for IPv4 and IPv6, including
//!    IPv4 addresses embedded in IPv6 (mapped, compatible, NAT64, 6to4).
//!    One blocked address refuses the request.
//! 3. The connection goes to one of those vetted addresses ([`Connector`]);
//!    the name is never resolved again, so a DNS answer that changes between
//!    the check and the connect (DNS rebinding) cannot redirect the
//!    connection to an internal address. TLS uses the host name for SNI and
//!    certificate verification (FB-25's rustls client settings).
//!
//! Redirects (301, 302, 303, 307, 308) are followed by this module, at most
//! [`FetchConfig::max_redirects`] times, each target going through the same
//! checks. `Authorization` and `Cookie` headers are dropped when a redirect
//! leaves the original origin.

use std::fmt;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use forgebase_auth::net::TlsSettings;
use forgebase_auth::social::{HttpRequest, HttpResponse};
use forgebase_db::BoxFuture;
use tokio::net::TcpStream;

/// Resolves host names to addresses.
pub trait Resolver: Send + Sync + fmt::Debug {
    /// Every address of `host` (no port semantics; `port` is for resolvers
    /// that need it).
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>>;
}

/// The system resolver (`getaddrinfo` through Tokio).
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemResolver;

impl Resolver for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str, port: u16) -> BoxFuture<'a, io::Result<Vec<IpAddr>>> {
        Box::pin(async move {
            let addrs = tokio::net::lookup_host((host, port)).await?;
            Ok(addrs.map(|a| a.ip()).collect())
        })
    }
}

/// Opens TCP connections to vetted addresses.
pub trait Connector: Send + Sync + fmt::Debug {
    /// Connects to `addr`.
    fn connect(&self, addr: SocketAddr) -> BoxFuture<'_, io::Result<TcpStream>>;
}

/// Plain `TcpStream::connect`.
#[derive(Debug, Clone, Copy, Default)]
pub struct DirectConnector;

impl Connector for DirectConnector {
    fn connect(&self, addr: SocketAddr) -> BoxFuture<'_, io::Result<TcpStream>> {
        Box::pin(TcpStream::connect(addr))
    }
}

/// Limits of outbound requests.
#[derive(Debug, Clone)]
pub struct FetchConfig {
    /// Redirects followed per request.
    pub max_redirects: usize,
    /// Timeout of one request including redirects.
    pub timeout: Duration,
    /// Largest response body.
    pub max_response_bytes: usize,
    /// Largest request body.
    pub max_request_bytes: usize,
    /// Requests per execution.
    pub max_requests_per_execution: u32,
}

impl Default for FetchConfig {
    fn default() -> Self {
        Self {
            max_redirects: 5,
            timeout: Duration::from_secs(10),
            max_response_bytes: 2 * 1024 * 1024,
            max_request_bytes: 1024 * 1024,
            max_requests_per_execution: 16,
        }
    }
}

/// Why a fetch did not produce a response.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FetchError {
    /// The request was refused before any connection (SSRF protection,
    /// host allow-list, scheme, credentials in the URL).
    #[error("fetch blocked: {0}")]
    Blocked(String),
    /// The request is malformed (method, headers, body size).
    #[error("invalid fetch request: {0}")]
    Invalid(String),
    /// Resolution, connection, TLS or protocol failure, or timeout.
    #[error("fetch failed: {0}")]
    Failed(String),
}

impl FetchError {
    /// Stable code given to the function.
    pub fn code(&self) -> &'static str {
        match self {
            FetchError::Blocked(_) => "fetch_blocked",
            FetchError::Invalid(_) => "fetch_invalid",
            FetchError::Failed(_) => "fetch_failed",
        }
    }
}

/// One outbound request.
#[derive(Debug, Clone)]
pub struct FetchRequest {
    /// `GET`, `POST`, `PUT`, `PATCH`, `DELETE` or `HEAD`.
    pub method: String,
    /// Absolute `http(s)` URL.
    pub url: String,
    /// Request headers.
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
}

/// The final response (after redirects).
#[derive(Debug, Clone)]
pub struct FetchResponse {
    /// Status code.
    pub status: u16,
    /// Lower-case header names.
    pub headers: Vec<(String, String)>,
    /// Body.
    pub body: Vec<u8>,
    /// The URL that answered (after redirects).
    pub url: String,
}

/// Why an address may not be contacted (`None`: a public unicast address).
pub fn blocked_reason(ip: IpAddr) -> Option<&'static str> {
    match ip {
        IpAddr::V4(v4) => blocked_v4(v4),
        IpAddr::V6(v6) => blocked_v6(v6),
    }
}

fn blocked_v4(ip: Ipv4Addr) -> Option<&'static str> {
    let o = ip.octets();
    let reason = match o {
        [0, ..] => "the unspecified/\"this network\" range 0.0.0.0/8",
        [10, ..] => "the private range 10.0.0.0/8",
        [100, b, ..] if (64..128).contains(&b) => "the carrier-grade NAT range 100.64.0.0/10",
        [127, ..] => "the loopback range 127.0.0.0/8",
        [169, 254, 169, 254] => "the cloud metadata address 169.254.169.254",
        [169, 254, ..] => "the link-local range 169.254.0.0/16",
        [172, b, ..] if (16..32).contains(&b) => "the private range 172.16.0.0/12",
        [192, 0, 0, _] => "the IETF protocol range 192.0.0.0/24",
        [192, 0, 2, _] | [198, 51, 100, _] | [203, 0, 113, _] => "a documentation range",
        [192, 88, 99, _] => "the 6to4 relay range 192.88.99.0/24",
        [192, 168, ..] => "the private range 192.168.0.0/16",
        [198, b, ..] if b == 18 || b == 19 => "the benchmarking range 198.18.0.0/15",
        [a, ..] if (224..240).contains(&a) => "the multicast range 224.0.0.0/4",
        [a, ..] if a >= 240 => "the reserved range 240.0.0.0/4",
        _ => return None,
    };
    Some(reason)
}

fn blocked_v6(ip: Ipv6Addr) -> Option<&'static str> {
    let s = ip.segments();
    if ip.is_unspecified() {
        return Some("the unspecified address ::");
    }
    if ip.is_loopback() {
        return Some("the IPv6 loopback address ::1");
    }
    // IPv4 inside IPv6: mapped (::ffff:a.b.c.d), compatible (::a.b.c.d),
    // NAT64 (64:ff9b::/96 and 64:ff9b:1::/48), 6to4 (2002::/16).
    if let Some(v4) = ip.to_ipv4_mapped() {
        return blocked_v4(v4).map(|_| "an IPv4-mapped IPv6 address of a blocked IPv4 range");
    }
    if s[..6] == [0, 0, 0, 0, 0, 0] {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        return blocked_v4(v4).map(|_| "an IPv4-compatible IPv6 address of a blocked IPv4 range");
    }
    if s[0] == 0x64 && s[1] == 0xff9b {
        let v4 = Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
        if blocked_v4(v4).is_some() || s[2] == 1 {
            return Some("a NAT64 address of a blocked IPv4 range");
        }
    }
    if s[0] == 0x2002 {
        let v4 = Ipv4Addr::new((s[1] >> 8) as u8, s[1] as u8, (s[2] >> 8) as u8, s[2] as u8);
        if blocked_v4(v4).is_some() {
            return Some("a 6to4 address of a blocked IPv4 range");
        }
    }
    let reason = match s[0] {
        x if x & 0xfe00 == 0xfc00 => "the unique-local range fc00::/7",
        x if x & 0xffc0 == 0xfe80 => "the link-local range fe80::/10",
        x if x & 0xffc0 == 0xfec0 => "the site-local range fec0::/10",
        x if x & 0xff00 == 0xff00 => "the multicast range ff00::/8",
        0x0100 if s[1..4] == [0, 0, 0] => "the discard range 100::/64",
        0x2001 if s[1] == 0x0db8 => "the documentation range 2001:db8::/32",
        0x2001 if s[1] == 0 => "the Teredo range 2001::/32",
        0x2001 if s[1] < 0x0200 => "the IETF protocol range 2001::/23",
        0x3fff if s[1] & 0xf000 == 0 => "the documentation range 3fff::/20",
        _ => return None,
    };
    Some(reason)
}

/// Whether `host` matches an allow-list entry (`example.com` exactly, or
/// `*.example.com` for any subdomain).
fn host_allowed(host: &str, allowed: &[String]) -> bool {
    if allowed.is_empty() {
        return true;
    }
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    allowed.iter().any(|a| {
        let a = a.trim_end_matches('.').to_ascii_lowercase();
        match a.strip_prefix("*.") {
            Some(suffix) => host.len() > suffix.len() + 1 && host.ends_with(&format!(".{suffix}")),
            None => host == a,
        }
    })
}

/// Validates a URL for one hop.
fn check_url(raw: &str, allowed: &[String]) -> Result<(url::Url, String, u16, bool), FetchError> {
    let url = url::Url::parse(raw).map_err(|_| FetchError::Invalid(format!("{raw:?} is not an absolute URL")))?;
    let tls = match url.scheme() {
        "https" => true,
        "http" => false,
        other => return Err(FetchError::Blocked(format!("the {other} scheme is not allowed"))),
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(FetchError::Blocked("URLs with credentials are not allowed".into()));
    }
    let host = match url.host() {
        Some(url::Host::Domain(d)) => d.to_owned(),
        Some(url::Host::Ipv4(a)) => a.to_string(),
        Some(url::Host::Ipv6(a)) => a.to_string(),
        None => return Err(FetchError::Invalid("the URL has no host".into())),
    };
    if !host_allowed(&host, allowed) {
        return Err(FetchError::Blocked(format!(
            "{host} is not in this function's fetch_hosts"
        )));
    }
    let port = url
        .port_or_known_default()
        .ok_or_else(|| FetchError::Invalid("the URL has no port".into()))?;
    Ok((url, host, port, tls))
}

/// The outbound client of the `fetch` capability.
#[derive(Clone)]
pub struct Fetcher {
    resolver: Arc<dyn Resolver>,
    connector: Arc<dyn Connector>,
    tls: Arc<rustls::ClientConfig>,
    cfg: FetchConfig,
}

impl fmt::Debug for Fetcher {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Fetcher")
            .field("resolver", &self.resolver)
            .field("connector", &self.connector)
            .field("cfg", &self.cfg)
            .finish_non_exhaustive()
    }
}

impl Fetcher {
    /// The production client: system resolver, direct connections, the
    /// webpki-roots trust anchors plus `tls`'s extra anchors.
    pub fn new(tls: &TlsSettings, cfg: FetchConfig) -> Result<Self, FetchError> {
        Self::with(tls, cfg, Arc::new(SystemResolver), Arc::new(DirectConnector))
    }

    /// A client with an explicit resolver and connector (tests, egress proxies).
    pub fn with(
        tls: &TlsSettings,
        cfg: FetchConfig,
        resolver: Arc<dyn Resolver>,
        connector: Arc<dyn Connector>,
    ) -> Result<Self, FetchError> {
        let tls = tls
            .client_config(&[b"http/1.1"])
            .map_err(|e| FetchError::Failed(format!("TLS setup: {e}")))?;
        Ok(Self {
            resolver,
            connector,
            tls,
            cfg,
        })
    }

    /// Limits.
    pub fn config(&self) -> &FetchConfig {
        &self.cfg
    }

    /// Resolves `host` and returns its addresses if none is blocked.
    async fn vet(&self, host: &str, port: u16) -> Result<Vec<SocketAddr>, FetchError> {
        let ips: Vec<IpAddr> = match host.parse::<IpAddr>() {
            Ok(ip) => vec![ip],
            Err(_) => self
                .resolver
                .resolve(host, port)
                .await
                .map_err(|_| FetchError::Failed(format!("could not resolve {host}")))?,
        };
        if ips.is_empty() {
            return Err(FetchError::Failed(format!("{host} has no addresses")));
        }
        for ip in &ips {
            if let Some(why) = blocked_reason(*ip) {
                return Err(FetchError::Blocked(format!("{host} resolves to {ip}, in {why}")));
            }
        }
        Ok(ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect())
    }

    /// Performs `req`, following redirects, each hop checked as described in
    /// the module documentation.
    pub async fn fetch(&self, req: FetchRequest, allowed_hosts: &[String]) -> Result<FetchResponse, FetchError> {
        tokio::time::timeout(self.cfg.timeout, self.fetch_inner(req, allowed_hosts))
            .await
            .map_err(|_| FetchError::Failed("the request timed out".into()))?
    }

    async fn fetch_inner(&self, req: FetchRequest, allowed: &[String]) -> Result<FetchResponse, FetchError> {
        if req.body.len() > self.cfg.max_request_bytes {
            return Err(FetchError::Invalid(format!(
                "request bodies are limited to {} bytes",
                self.cfg.max_request_bytes
            )));
        }
        let mut method = static_method(&req.method)?;
        let mut headers = sanitize_headers(&req.headers)?;
        let mut body = req.body;
        let mut target = req.url;
        let mut origin: Option<(String, String, u16)> = None;
        for hop in 0..=self.cfg.max_redirects {
            let (url, host, port, tls) = check_url(&target, allowed)?;
            let this_origin = (url.scheme().to_owned(), host.clone(), port);
            match &origin {
                None => origin = Some(this_origin),
                Some(o) if *o != this_origin => {
                    headers.retain(|(k, _)| k != "authorization" && k != "cookie" && k != "proxy-authorization");
                }
                Some(_) => {}
            }
            let addrs = self.vet(&host, port).await?;
            let request = HttpRequest {
                method,
                url: url.to_string(),
                headers: headers.clone(),
                body: body.clone(),
            };
            let res = self.exchange(&addrs, &host, tls, &url, &request).await?;
            let location = res.header("location").map(str::to_owned);
            match (res.status, location) {
                (301 | 302 | 303 | 307 | 308, Some(loc)) => {
                    if hop == self.cfg.max_redirects {
                        return Err(FetchError::Failed(format!(
                            "more than {} redirects",
                            self.cfg.max_redirects
                        )));
                    }
                    let next = url
                        .join(&loc)
                        .map_err(|_| FetchError::Failed("a redirect has an invalid Location".into()))?;
                    if res.status == 303 || (matches!(res.status, 301 | 302) && method == "POST") {
                        method = "GET";
                        body.clear();
                        headers.retain(|(k, _)| k != "content-type");
                    }
                    target = next.to_string();
                }
                _ => {
                    return Ok(FetchResponse {
                        status: res.status,
                        headers: res.headers,
                        body: res.body,
                        url: url.to_string(),
                    });
                }
            }
        }
        Err(FetchError::Failed("too many redirects".into()))
    }

    async fn exchange(
        &self,
        addrs: &[SocketAddr],
        host: &str,
        tls: bool,
        url: &url::Url,
        req: &HttpRequest,
    ) -> Result<HttpResponse, FetchError> {
        let mut last = None;
        let mut tcp = None;
        for a in addrs {
            match self.connector.connect(*a).await {
                Ok(s) => {
                    tcp = Some(s);
                    break;
                }
                Err(e) => last = Some(e),
            }
        }
        let tcp = tcp.ok_or_else(|| {
            FetchError::Failed(format!(
                "could not connect to {host}{}",
                last.map(|e| format!(" ({})", e.kind())).unwrap_or_default()
            ))
        })?;
        let _ = tcp.set_nodelay(true);
        let max = self.cfg.max_response_bytes;
        let fail = |e: forgebase_auth::AuthError| FetchError::Failed(e.to_string());
        if tls {
            let mut s = TlsSettings::upgrade(&self.tls, host, tcp).await.map_err(fail)?;
            forgebase_auth::net::http1::exchange(&mut s, req, url, max)
                .await
                .map_err(fail)
        } else {
            let mut s = tcp;
            forgebase_auth::net::http1::exchange(&mut s, req, url, max)
                .await
                .map_err(fail)
        }
    }
}

fn static_method(m: &str) -> Result<&'static str, FetchError> {
    Ok(match m.to_ascii_uppercase().as_str() {
        "" | "GET" => "GET",
        "POST" => "POST",
        "PUT" => "PUT",
        "PATCH" => "PATCH",
        "DELETE" => "DELETE",
        "HEAD" => "HEAD",
        other => return Err(FetchError::Invalid(format!("method {other} is not supported"))),
    })
}

/// Lower-cases names and drops headers the client controls.
fn sanitize_headers(h: &[(String, String)]) -> Result<Vec<(String, String)>, FetchError> {
    const CONTROLLED: &[&str] = &[
        "host",
        "content-length",
        "connection",
        "transfer-encoding",
        "upgrade",
        "te",
        "trailer",
        "keep-alive",
        "proxy-connection",
        "accept-encoding",
    ];
    if h.len() > 64 {
        return Err(FetchError::Invalid("at most 64 request headers".into()));
    }
    let mut out = Vec::with_capacity(h.len());
    for (k, v) in h {
        let k = k.to_ascii_lowercase();
        let k_ok =
            !k.is_empty() && k.len() <= 128 && k.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_".contains(&b));
        if !k_ok || v.len() > 8192 || v.contains(['\r', '\n', '\0']) {
            return Err(FetchError::Invalid(format!("bad request header {k:?}")));
        }
        if CONTROLLED.contains(&k.as_str()) || k.starts_with("proxy-") {
            continue;
        }
        out.push((k, v.clone()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED))
    }

    #[test]
    fn blocked_ranges() {
        for s in [
            "127.0.0.1",
            "127.255.0.9",
            "10.1.2.3",
            "172.16.0.1",
            "172.31.255.255",
            "192.168.1.1",
            "169.254.169.254",
            "169.254.0.1",
            "100.64.0.1",
            "100.100.100.200",
            "0.0.0.0",
            "224.0.0.1",
            "255.255.255.255",
            "198.18.0.1",
            "192.0.2.10",
            "::1",
            "::",
            "fe80::1",
            "fc00::1",
            "fd00:ec2::254",
            "ff02::1",
            "::ffff:127.0.0.1",
            "::ffff:169.254.169.254",
            "::ffff:10.0.0.1",
            "::127.0.0.1",
            "64:ff9b::a9fe:a9fe",
            "2002:7f00:1::",
            "2001:db8::1",
        ] {
            assert!(blocked_reason(ip(s)).is_some(), "{s} must be blocked");
        }
        for s in [
            "93.184.215.14",
            "1.1.1.1",
            "8.8.8.8",
            "172.32.0.1",
            "100.128.0.1",
            "2606:4700::1111",
            "2a00:1450:4001::200e",
        ] {
            assert_eq!(blocked_reason(ip(s)), None, "{s} is public");
        }
    }

    #[test]
    fn allow_list() {
        let a = vec!["api.example.com".to_owned(), "*.cdn.test".to_owned()];
        assert!(host_allowed("api.example.com", &a));
        assert!(host_allowed("API.example.com.", &a));
        assert!(host_allowed("x.cdn.test", &a));
        assert!(!host_allowed("cdn.test", &a));
        assert!(!host_allowed("evilcdn.test", &a));
        assert!(!host_allowed("api.example.com.evil", &a));
        assert!(host_allowed("anything", &[]));
    }

    #[test]
    fn urls() {
        assert!(matches!(check_url("ftp://x/", &[]), Err(FetchError::Blocked(_))));
        assert!(matches!(
            check_url("file:///etc/passwd", &[]),
            Err(FetchError::Blocked(_))
        ));
        assert!(matches!(check_url("http://u:p@x/", &[]), Err(FetchError::Blocked(_))));
        assert!(matches!(check_url("not a url", &[]), Err(FetchError::Invalid(_))));
        // WHATWG parsing turns decimal and hex IPv4 forms into addresses.
        let (_, host, _, _) = check_url("http://2130706433/", &[]).unwrap_or_else(|_| unreachable!());
        assert_eq!(host, "127.0.0.1");
        let (_, host, _, _) = check_url("http://0x7f.1/", &[]).unwrap_or_else(|_| unreachable!());
        assert_eq!(host, "127.0.0.1");
    }

    #[test]
    fn headers() {
        let h = sanitize_headers(&[
            ("Host".into(), "evil".into()),
            ("X-Custom".into(), "1".into()),
            ("Proxy-Authorization".into(), "x".into()),
        ])
        .unwrap_or_default();
        assert_eq!(h, vec![("x-custom".to_owned(), "1".to_owned())]);
        assert!(sanitize_headers(&[("x".into(), "a\r\nb".into())]).is_err());
    }
}

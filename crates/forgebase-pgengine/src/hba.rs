//! `pg_hba.conf` and the TLS certificate (FB-153, rls-coverage §3.5).
//!
//! ForgeBase writes `pg_hba.conf` on every start, replacing whatever
//! `initdb` or a person left there:
//!
//! * the Unix socket (a directory only the server's OS user can open)
//!   accepts every role with SCRAM;
//! * with no `listen` setting there is **no** `host` line at all, and
//!   `listen_addresses = ''`: no TCP port is open (human decision
//!   2026-11-11). Windows, which has no socket, listens on loopback and gets
//!   `host` lines for loopback only;
//! * with `listen` (an operator's explicit choice) TLS is on and every TCP
//!   login must use it (`hostnossl … reject` first). ForgeBase's own roles
//!   (`forgebase_super`, `forgebase_server`, `forgebase_reader`) may log in
//!   over loopback only; named logins (members of `forgebase_login`) from
//!   the listen address's reach (loopback for a loopback address, anywhere
//!   otherwise), to the `forgebase` database only.
//!
//! The certificate is the operator's ([`crate::TlsFiles`]) or a self-signed
//! ECDSA P-256 certificate generated once under `<node>/tls/` (10 years,
//! `CN=ForgeBase PostgreSQL`, subject names `localhost` and the listen
//! address). Clients connect with `sslmode=require`; for `verify-full`,
//! configure a certificate from your own authority.

use std::fmt::Write as _;
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};

use crate::error::{EngineError, Result};
use crate::fsutil;
use crate::secret::Role;

/// The NOLOGIN group of named direct logins.
pub const LOGIN_GROUP: &str = "forgebase_login";

/// What `pg_hba.conf` depends on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HbaInput {
    /// The engine has a Unix socket.
    pub socket: bool,
    /// The TCP address it listens on, if any.
    pub tcp: Option<SocketAddr>,
    /// TCP comes from `[database.postgres] listen` (TLS required), not the
    /// Windows loopback fallback.
    pub tls: bool,
}

fn loopback_cidrs() -> [&'static str; 2] {
    ["127.0.0.1/32", "::1/128"]
}

/// Renders `pg_hba.conf`.
pub fn render(input: &HbaInput) -> String {
    let mut s = String::from(
        "# Written by ForgeBase on every start (FB-153). Do not edit: changes are\n\
         # overwritten. TCP logins are configured with [database.postgres] listen.\n\
         # TYPE     DATABASE     USER   ADDRESS   METHOD\n",
    );
    if input.socket {
        s.push_str("local      all          all              scram-sha-256\n");
        s.push_str("local      replication  all              scram-sha-256\n");
    }
    let Some(addr) = input.tcp else {
        s.push_str("# No TCP logins: listen is not set.\n");
        return s;
    };
    let own = [Role::Super.name(), Role::Server.name(), Role::Reader.name()].join(",");
    if !input.tls {
        // Windows without a socket: loopback only, as before FB-153.
        for c in loopback_cidrs() {
            let _ = writeln!(s, "host       all          all    {c}  scram-sha-256");
            let _ = writeln!(s, "host       replication  all    {c}  scram-sha-256");
        }
        return s;
    }
    s.push_str("hostnossl  all          all    all       reject\n");
    for c in loopback_cidrs() {
        let _ = writeln!(s, "hostssl    all          {own}  {c}  scram-sha-256");
        let _ = writeln!(s, "hostssl    replication  {own}  {c}  scram-sha-256");
    }
    let remote = !addr.ip().is_loopback();
    let reach: Vec<&str> = if remote { vec!["all"] } else { loopback_cidrs().to_vec() };
    for c in reach {
        let _ = writeln!(
            s,
            "hostssl    {}    +{LOGIN_GROUP}  {c}  scram-sha-256",
            crate::DATABASE
        );
    }
    s.push_str("host       all          all    all       reject\n");
    s
}

/// Writes `pg_hba.conf` into `pgdata` (atomically: a new file renamed over
/// the old one).
///
/// # Errors
/// File-system errors.
pub fn write(pgdata: &Path, input: &HbaInput) -> Result<()> {
    let path = pgdata.join("pg_hba.conf");
    let tmp = pgdata.join("pg_hba.conf.forgebase");
    {
        use std::io::Write;
        let mut f = fsutil::create_private_file(&tmp)?;
        f.write_all(render(input).as_bytes())
            .map_err(|e| EngineError::io("write", &tmp, e))?;
        f.sync_all().map_err(|e| EngineError::io("sync", &tmp, e))?;
    }
    std::fs::rename(&tmp, &path).map_err(|e| EngineError::io("rename into place", &path, e))
}

/// The certificate and key to serve, generating the self-signed pair under
/// `tls_dir` when none is configured and none was generated yet.
///
/// # Errors
/// A configured file is missing; generation or file-system errors.
pub fn ensure_certificate(
    configured: Option<&crate::TlsFiles>,
    tls_dir: &Path,
    listen: SocketAddr,
) -> Result<(PathBuf, PathBuf)> {
    if let Some(t) = configured {
        for p in [&t.cert, &t.key] {
            if !p.is_file() {
                return Err(EngineError::Config(format!(
                    "TLS file {} does not exist (set by [database.postgres] tls_cert / tls_key)",
                    p.display()
                )));
            }
        }
        return Ok((t.cert.clone(), t.key.clone()));
    }
    let cert = tls_dir.join("server.crt");
    let key = tls_dir.join("server.key");
    if cert.is_file() && key.is_file() {
        return Ok((cert, key));
    }
    fsutil::private_dir(tls_dir)?;
    let (cert_pem, key_pem) = self_signed(listen.ip(), now_unix())?;
    {
        use std::io::Write;
        let mut f = fsutil::create_private_file(&key)?;
        f.write_all(key_pem.as_bytes())
            .map_err(|e| EngineError::io("write", &key, e))?;
        f.sync_all().map_err(|e| EngineError::io("sync", &key, e))?;
        let mut f = fsutil::create_private_file(&cert)?;
        f.write_all(cert_pem.as_bytes())
            .map_err(|e| EngineError::io("write", &cert, e))?;
        f.sync_all().map_err(|e| EngineError::io("sync", &cert, e))?;
    }
    tracing::info!(target: "forgebase::postgres", cert = %cert.display(),
        "generated a self-signed TLS certificate for TCP logins");
    Ok((cert, key))
}

fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

// ---------------------------------------------------------------------------
// A minimal DER writer for one self-signed X.509 v3 certificate.
// ---------------------------------------------------------------------------

fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    let n = content.len();
    if n < 0x80 {
        v.push(n as u8);
    } else if n < 0x100 {
        v.extend([0x81, n as u8]);
    } else {
        v.extend([0x82, (n >> 8) as u8, n as u8]);
    }
    v.extend_from_slice(content);
    v
}

fn seq(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x30, &parts.concat())
}

fn set(parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(0x31, &parts.concat())
}

fn uint(bytes: &[u8]) -> Vec<u8> {
    let mut b: Vec<u8> = bytes.iter().copied().skip_while(|x| *x == 0).collect();
    if b.is_empty() || b[0] & 0x80 != 0 {
        b.insert(0, 0);
    }
    tlv(0x02, &b)
}

fn bits(bytes: &[u8]) -> Vec<u8> {
    let mut b = vec![0u8];
    b.extend_from_slice(bytes);
    tlv(0x03, &b)
}

const OID_ECDSA_SHA256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x04, 0x03, 0x02];
const OID_EC_PUBLIC_KEY: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x02, 0x01];
const OID_P256: &[u8] = &[0x2A, 0x86, 0x48, 0xCE, 0x3D, 0x03, 0x01, 0x07];
const OID_CN: &[u8] = &[0x55, 0x04, 0x03];
const OID_SAN: &[u8] = &[0x55, 0x1D, 0x11];

/// `UTCTime` before 2050, `GeneralizedTime` after (RFC 5280 §4.1.2.5).
fn der_time(unix: i64) -> Vec<u8> {
    let days = unix.div_euclid(86_400);
    let secs = unix.rem_euclid(86_400);
    // Civil date from days since 1970-01-01 (H. Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    let (hh, mm, ss) = (secs / 3600, secs % 3600 / 60, secs % 60);
    if (1950..2050).contains(&y) {
        tlv(
            0x17,
            format!("{:02}{m:02}{d:02}{hh:02}{mm:02}{ss:02}Z", y % 100).as_bytes(),
        )
    } else {
        tlv(0x18, format!("{y:04}{m:02}{d:02}{hh:02}{mm:02}{ss:02}Z").as_bytes())
    }
}

fn pem(label: &str, der: &[u8]) -> String {
    let b64 = base64::engine::general_purpose::STANDARD.encode(der);
    let mut s = format!("-----BEGIN {label}-----\n");
    for chunk in b64.as_bytes().chunks(64) {
        s.push_str(std::str::from_utf8(chunk).unwrap_or_default());
        s.push('\n');
    }
    let _ = writeln!(s, "-----END {label}-----");
    s
}

/// A self-signed certificate for `ip` (and `localhost`) valid from a day
/// before `now` for ten years: `(certificate PEM, PKCS#8 key PEM)`.
fn self_signed(ip: IpAddr, now: i64) -> Result<(String, String)> {
    let rng = SystemRandom::new();
    let fail = |what: &str| EngineError::Config(format!("cannot generate the TLS certificate: {what}"));
    let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(|_| fail("key"))?;
    let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng)
        .map_err(|_| fail("key"))?;
    let mut serial = [0u8; 16];
    rng.fill(&mut serial).map_err(|_| fail("serial"))?;
    serial[0] &= 0x7F;
    let alg = seq(&[tlv(0x06, OID_ECDSA_SHA256)]);
    let name = seq(&[set(&[seq(&[
        tlv(0x06, OID_CN),
        tlv(0x0C, b"ForgeBase PostgreSQL"),
    ])])]);
    let spki = seq(&[
        seq(&[tlv(0x06, OID_EC_PUBLIC_KEY), tlv(0x06, OID_P256)]),
        bits(pair.public_key().as_ref()),
    ]);
    let mut names = vec![tlv(0x82, b"localhost")];
    let ips: Vec<IpAddr> = if ip.is_unspecified() {
        vec![IpAddr::from([127, 0, 0, 1]), IpAddr::from(std::net::Ipv6Addr::LOCALHOST)]
    } else {
        vec![ip]
    };
    for a in ips {
        names.push(match a {
            IpAddr::V4(v) => tlv(0x87, &v.octets()),
            IpAddr::V6(v) => tlv(0x87, &v.octets()),
        });
    }
    let san = seq(&[tlv(0x06, OID_SAN), tlv(0x04, &seq(&names))]);
    let tbs = seq(&[
        tlv(0xA0, &uint(&[2])),
        uint(&serial),
        alg.clone(),
        name.clone(),
        seq(&[der_time(now - 86_400), der_time(now + 10 * 365 * 86_400)]),
        name,
        spki,
        tlv(0xA3, &seq(&[san])),
    ]);
    let sig = pair.sign(&rng, &tbs).map_err(|_| fail("signature"))?;
    let cert = seq(&[tbs, alg, bits(sig.as_ref())]);
    Ok((pem("CERTIFICATE", &cert), pem("PRIVATE KEY", pkcs8.as_ref())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(tcp: Option<&str>, tls: bool) -> HbaInput {
        HbaInput {
            socket: true,
            tcp: tcp.map(|a| a.parse().expect("addr")),
            tls,
        }
    }

    fn rules(s: &str) -> Vec<&str> {
        s.lines().filter(|l| !l.trim_start().starts_with('#')).collect()
    }

    #[test]
    fn default_has_no_tcp_line() {
        let s = render(&input(None, false));
        assert!(
            rules(&s).iter().all(|l| l.starts_with("local")),
            "only socket lines without listen:\n{s}"
        );
    }

    #[test]
    fn listen_requires_tls_and_keeps_own_roles_on_loopback() {
        let s = render(&input(Some("0.0.0.0:5433"), true));
        let r = rules(&s);
        let first_host = r.iter().position(|l| l.starts_with("host")).expect("host lines");
        assert!(r[first_host].starts_with("hostnossl") && r[first_host].ends_with("reject"), "{s}");
        for l in r.iter().filter(|l| l.starts_with("hostssl")) {
            if l.contains("forgebase_server") || l.contains("forgebase_super") || l.contains("forgebase_reader") {
                assert!(l.contains("127.0.0.1/32") || l.contains("::1/128"), "own roles off loopback: {l}");
            }
        }
        assert!(r.iter().any(|l| l.starts_with("hostssl") && l.contains("+forgebase_login") && l.contains(" all ")));
        assert!(!r.iter().any(|l| l.starts_with("host ") && !l.ends_with("reject")), "{s}");

        let s = render(&input(Some("127.0.0.1:5433"), true));
        assert!(
            rules(&s)
                .iter()
                .filter(|l| l.contains("+forgebase_login"))
                .all(|l| l.contains("127.0.0.1/32") || l.contains("::1/128")),
            "a loopback listen keeps logins on loopback:\n{s}"
        );
    }

    #[test]
    fn windows_fallback_is_loopback_only() {
        let mut i = input(Some("127.0.0.1:50000"), false);
        i.socket = false;
        let s = render(&i);
        let r = rules(&s);
        assert!(!r.is_empty());
        assert!(r.iter().all(|l| l.contains("127.0.0.1/32") || l.contains("::1/128")), "{s}");
    }

    #[test]
    fn der_times() {
        assert_eq!(der_time(0), tlv(0x17, b"700101000000Z"));
        // 2026-11-11T09:12:22Z
        assert_eq!(der_time(1_794_388_342), tlv(0x17, b"261111091222Z"));
        // 2050-01-01T00:00:00Z switches to GeneralizedTime.
        assert_eq!(der_time(2_524_608_000), tlv(0x18, b"20500101000000Z"));
    }

    #[test]
    fn self_signed_certificate_is_well_formed() {
        let (cert, key) = self_signed("10.1.2.3".parse().expect("ip"), 1_794_388_342).expect("cert");
        assert!(cert.starts_with("-----BEGIN CERTIFICATE-----\n"), "{cert}");
        assert!(key.starts_with("-----BEGIN PRIVATE KEY-----\n"));
        let b64: String = cert.lines().filter(|l| !l.starts_with("-----")).collect();
        let der = base64::engine::general_purpose::STANDARD
            .decode(b64)
            .expect("base64");
        // One outer SEQUENCE whose length covers the whole document.
        assert_eq!(der[0], 0x30);
        let (len, hdr) = match der[1] {
            0x82 => ((usize::from(der[2]) << 8) | usize::from(der[3]), 4),
            0x81 => (usize::from(der[2]), 3),
            n => (usize::from(n), 2),
        };
        assert_eq!(hdr + len, der.len());
        let hay = der.windows(4).any(|w| w == [10, 1, 2, 3]);
        assert!(hay, "the listen address is a subject name");
    }
}

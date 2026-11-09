//! A minimal in-process LDAPv3 directory: simple bind, search (with real
//! filter evaluation), StartTLS and LDAPS, built on the crate's own BER and
//! filter code. Like real servers it accepts an "unauthenticated bind" (a
//! DN with an empty password), so the client must refuse empty passwords.
#![allow(dead_code, clippy::unwrap_used)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use forgebase_auth::ldap::ber::{self, Reader, SEQUENCE};
use forgebase_auth::ldap::filter::Filter;
use forgebase_auth::ldap::{message, parse_message};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpListener;

pub struct Entry {
    pub dn: String,
    pub password: String,
    pub attrs: HashMap<String, Vec<Vec<u8>>>,
}

impl Entry {
    pub fn person(uid: &str, password: &str, mail: Option<&str>) -> Self {
        let mut attrs = HashMap::new();
        attrs.insert("uid".to_owned(), vec![uid.as_bytes().to_vec()]);
        attrs.insert("objectclass".to_owned(), vec![b"person".to_vec()]);
        attrs.insert("cn".to_owned(), vec![format!("{uid} Example").into_bytes()]);
        attrs.insert("entryuuid".to_owned(), vec![format!("uuid-{uid}").into_bytes()]);
        if let Some(m) = mail {
            attrs.insert("mail".to_owned(), vec![m.as_bytes().to_vec()]);
        }
        Self {
            dn: format!("uid={uid},ou=people,dc=example,dc=com"),
            password: password.to_owned(),
            attrs,
        }
    }
}

#[derive(Clone)]
pub enum Tls {
    None,
    StartTls(tokio_rustls::TlsAcceptor),
    Ldaps(tokio_rustls::TlsAcceptor),
}

pub struct Directory {
    pub entries: Vec<Entry>,
    /// Service account (dn, password) required before searching.
    pub service: Option<(String, String)>,
}

pub struct MockLdap {
    pub port: u16,
    /// `bind <dn>`, `search <filter>`, `starttls` lines, in order.
    pub log: Arc<Mutex<Vec<String>>>,
}

impl MockLdap {
    pub fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }
}

pub async fn start(dir: Directory, tls: Tls) -> MockLdap {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let dir = Arc::new(dir);
    let l2 = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((tcp, _)) = listener.accept().await else { return };
            let (dir, log, tls) = (dir.clone(), l2.clone(), tls.clone());
            tokio::spawn(async move {
                match tls {
                    Tls::Ldaps(acc) => {
                        if let Ok(mut s) = acc.accept(tcp).await {
                            let _ = serve(&mut s, &dir, &log, false).await;
                        }
                    }
                    Tls::StartTls(acc) => {
                        let mut tcp = tcp;
                        if let Ok(true) = serve(&mut tcp, &dir, &log, true).await
                            && let Ok(mut s) = acc.accept(tcp).await
                        {
                            let _ = serve(&mut s, &dir, &log, false).await;
                        }
                    }
                    Tls::None => {
                        let mut tcp = tcp;
                        let _ = serve(&mut tcp, &dir, &log, false).await;
                    }
                }
            });
        }
    });
    MockLdap { port, log }
}

fn result(tag: u8, code: i64) -> Vec<u8> {
    ber::constructed(tag, &[ber::enumerated(code), ber::octets(b""), ber::octets(b"")])
}

/// Serves messages; returns `true` when the client asked for StartTLS (the
/// success response is already sent).
async fn serve<S: AsyncRead + AsyncWrite + Unpin>(
    s: &mut S,
    dir: &Directory,
    log: &Mutex<Vec<String>>,
    allow_starttls: bool,
) -> std::io::Result<bool> {
    let mut buf = Vec::new();
    let mut bound: Option<String> = None;
    loop {
        let n = match ber::frame_len(&buf) {
            Ok(Some(n)) if buf.len() >= n => n,
            Ok(_) => {
                let mut chunk = [0u8; 4096];
                let r = s.read(&mut chunk).await?;
                if r == 0 {
                    return Ok(false);
                }
                buf.extend_from_slice(&chunk[..r]);
                continue;
            }
            Err(_) => return Ok(false),
        };
        let frame: Vec<u8> = buf.drain(..n).collect();
        let Ok(m) = parse_message(&frame) else { return Ok(false) };
        match m.tag {
            0x60 => {
                let mut r = Reader::new(&m.body);
                let _version = r.integer().unwrap();
                let dn = String::from_utf8(r.octets().unwrap().to_vec()).unwrap();
                let pw = String::from_utf8(r.expect(0x80).unwrap().to_vec()).unwrap();
                log.lock().unwrap().push(format!("bind {dn}"));
                let ok = if pw.is_empty() {
                    true // anonymous or unauthenticated bind
                } else if dir.service.as_ref().is_some_and(|(d, p)| *d == dn && *p == pw) {
                    true
                } else {
                    dir.entries
                        .iter()
                        .any(|e| e.dn.eq_ignore_ascii_case(&dn) && e.password == pw)
                };
                if ok && !pw.is_empty() {
                    bound = Some(dn);
                }
                s.write_all(&message(m.id, result(0x61, if ok { 0 } else { 49 }))).await?;
            }
            0x63 => {
                let mut r = Reader::new(&m.body);
                let _base = r.octets().unwrap();
                let _scope = r.enumerated().unwrap();
                let _deref = r.enumerated().unwrap();
                let size = r.integer().unwrap();
                let _time = r.integer().unwrap();
                let _types = r.next().unwrap();
                let (tag, content) = r.next().unwrap();
                let filter = Filter::decode(tag, content).unwrap();
                log.lock().unwrap().push(format!("search {filter:?}"));
                if dir.service.as_ref().is_some_and(|(d, _)| bound.as_deref() != Some(d.as_str())) {
                    s.write_all(&message(m.id, result(0x65, 50))).await?;
                    continue;
                }
                let hits: Vec<&Entry> = dir.entries.iter().filter(|e| filter.matches(&e.attrs)).collect();
                for e in hits.iter().take(size.max(1) as usize) {
                    let attrs: Vec<Vec<u8>> = e
                        .attrs
                        .iter()
                        .map(|(k, vs)| {
                            ber::constructed(
                                SEQUENCE,
                                &[
                                    ber::octets(k.as_bytes()),
                                    ber::constructed(ber::SET, &vs.iter().map(|v| ber::octets(v)).collect::<Vec<_>>()),
                                ],
                            )
                        })
                        .collect();
                    let entry = ber::constructed(
                        0x64,
                        &[ber::octets(e.dn.as_bytes()), ber::constructed(SEQUENCE, &attrs)],
                    );
                    s.write_all(&message(m.id, entry)).await?;
                }
                let code = if hits.len() as i64 > size.max(1) { 4 } else { 0 };
                s.write_all(&message(m.id, result(0x65, code))).await?;
            }
            0x77 if allow_starttls => {
                log.lock().unwrap().push("starttls".into());
                s.write_all(&message(m.id, result(0x78, 0))).await?;
                s.flush().await?;
                return Ok(true);
            }
            0x77 => s.write_all(&message(m.id, result(0x78, 2))).await?,
            0x42 => return Ok(false),
            _ => return Ok(false),
        }
        s.flush().await?;
    }
}

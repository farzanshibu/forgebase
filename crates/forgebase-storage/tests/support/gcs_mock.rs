//! A mock of the Google Cloud Storage JSON API, the OAuth token endpoint and
//! the GCE metadata server. It checks what the driver sends (bearer tokens,
//! JWT assertions, Content-Range of resumable chunks) and records every
//! protocol violation in `bad`. It also serves path-style XML API downloads
//! (`GET /<bucket>/<object>`) through V4 signed URLs, verifying the
//! signature with the service account's public key from code written from
//! Google's documentation, and honouring the signed `response-*` overrides.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use ring::signature::{self, RsaKeyPair};
use serde_json::{Value as Json, json};

use super::http_mock::{Req, Resp, Server};

pub const BUCKET: &str = "test-bucket";
pub const SA_JSON: &str = concat!(
    include_str!("../fixtures/gcs_sa_head.part"),
    include_str!("../fixtures/gcs_sa_key.part"),
    include_str!("../fixtures/gcs_sa_tail.part")
);

/// A scheduled failure: the next `left` requests whose `METHOD path` contains
/// `needle` get `status`.
pub struct Fault {
    pub needle: String,
    pub status: u16,
    pub left: u32,
}

#[derive(Default)]
pub struct GcsState {
    /// Full object names (prefix included) and their bytes.
    pub objects: BTreeMap<String, Vec<u8>>,
    sessions: HashMap<String, (String, Vec<u8>)>,
    /// Bearer tokens the storage API accepts.
    pub accepted: HashSet<String>,
    pub tokens_issued: u32,
    pub token_ttl: i64,
    /// JWT assertions received by the token endpoint (claims).
    pub assertions: Vec<Json>,
    /// Protocol violations seen.
    pub bad: Vec<String>,
    pub faults: Vec<Fault>,
    /// Objects per list page (0 = all).
    pub list_page: usize,
    /// Persist only 256 KiB of the next non-final chunk (a partial write).
    pub partial_once: bool,
    pub cancelled: u32,
    pub require_auth: bool,
    rewrites: HashSet<String>,
    next_session: u32,
    /// Signed-URL requests refused, and why.
    pub refused: Vec<String>,
}

pub struct GcsMock {
    pub server: Server,
    pub st: Arc<Mutex<GcsState>>,
}

fn public_key() -> Vec<u8> {
    let sa: Json = serde_json::from_str(SA_JSON).unwrap();
    let der = forgebase_storage::cloud::pem_der(sa["private_key"].as_str().unwrap(), "PRIVATE KEY").unwrap();
    RsaKeyPair::from_pkcs8(&der).unwrap().public().as_ref().to_vec()
}

fn b64url_decode(s: &str) -> Vec<u8> {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.decode(s).unwrap()
}

fn err(status: u16, msg: &str) -> Resp {
    Resp::json(status, &json!({ "error": { "code": status, "message": msg } }))
}

fn meta(name: &str, data: &[u8]) -> Json {
    json!({ "name": name, "size": data.len().to_string(), "updated": "2026-11-09T12:00:00.000Z" })
}

impl GcsMock {
    pub async fn start(list_page: usize) -> Self {
        let st = Arc::new(Mutex::new(GcsState {
            list_page,
            token_ttl: 3600,
            require_auth: true,
            ..GcsState::default()
        }));
        let state = st.clone();
        let addr_cell: Arc<Mutex<String>> = Arc::new(Mutex::new(String::new()));
        let cell = addr_cell.clone();
        let server = Server::start(Arc::new(move |req: &Req| {
            let base = cell.lock().unwrap().clone();
            handle(&state, &base, req)
        }))
        .await;
        *addr_cell.lock().unwrap() = server.url();
        Self { server, st }
    }

    pub fn url(&self) -> String {
        self.server.url()
    }

    pub fn fail(&self, needle: &str, status: u16, times: u32) {
        self.st.lock().unwrap().faults.push(Fault {
            needle: needle.to_owned(),
            status,
            left: times,
        });
    }

    pub fn names(&self) -> Vec<String> {
        self.st.lock().unwrap().objects.keys().cloned().collect()
    }

    pub fn object(&self, name: &str) -> Option<Vec<u8>> {
        self.st.lock().unwrap().objects.get(name).cloned()
    }

    pub fn bad(&self) -> Vec<String> {
        self.st.lock().unwrap().bad.clone()
    }
}

fn handle(st: &Arc<Mutex<GcsState>>, base: &str, req: &Req) -> Resp {
    let mut s = st.lock().unwrap();
    let id = format!("{} {}", req.method, req.path);
    if let Some(f) = s.faults.iter_mut().find(|f| f.left > 0 && id.contains(&f.needle)) {
        f.left -= 1;
        let status = f.status;
        return err(status, "injected").header("Retry-After", "0");
    }

    // OAuth token endpoint: the JWT bearer grant.
    if req.path == "/token" && req.method == "POST" {
        let form: HashMap<String, String> = url::form_urlencoded::parse(&req.body).into_owned().collect();
        if form.get("grant_type").map(String::as_str) != Some("urn:ietf:params:oauth:grant-type:jwt-bearer") {
            return Resp::json(400, &json!({ "error": "unsupported_grant_type" }));
        }
        let jwt = form.get("assertion").cloned().unwrap_or_default();
        let parts: Vec<&str> = jwt.split('.').collect();
        if parts.len() != 3 {
            return Resp::json(400, &json!({ "error": "invalid_grant" }));
        }
        let ok = signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, public_key())
            .verify(
                format!("{}.{}", parts[0], parts[1]).as_bytes(),
                &b64url_decode(parts[2]),
            )
            .is_ok();
        if !ok {
            return Resp::json(
                400,
                &json!({ "error": "invalid_grant", "error_description": "bad signature" }),
            );
        }
        let header: Json = serde_json::from_slice(&b64url_decode(parts[0])).unwrap();
        let claims: Json = serde_json::from_slice(&b64url_decode(parts[1])).unwrap();
        if header["alg"] != "RS256" || header["kid"] != "ffffffffffffffffffffffffffffffffffffffff" {
            s.bad.push(format!("jwt header {header}"));
        }
        s.assertions.push(claims);
        s.tokens_issued += 1;
        let tok = format!("tok-{}", s.tokens_issued);
        s.accepted.insert(tok.clone());
        let ttl = s.token_ttl;
        return Resp::json(
            200,
            &json!({ "access_token": tok, "expires_in": ttl, "token_type": "Bearer" }),
        );
    }
    // The GCE metadata server.
    if req.path == "/computeMetadata/v1/instance/service-accounts/default/token" {
        if req.h("metadata-flavor") != Some("Google") {
            return Resp::new(403).body("missing Metadata-Flavor");
        }
        s.tokens_issued += 1;
        let tok = format!("meta-{}", s.tokens_issued);
        s.accepted.insert(tok.clone());
        let ttl = s.token_ttl;
        return Resp::json(
            200,
            &json!({ "access_token": tok, "expires_in": ttl, "token_type": "Bearer" }),
        );
    }

    // Resumable upload sessions are pre-authorised by their URI.
    if let Some(sid) = req.path.strip_prefix("/session/") {
        return session(&mut s, sid, req);
    }

    // XML API downloads through V4 signed URLs: no bearer token.
    if req.q("X-Goog-Signature").is_some() {
        let host = base.trim_start_matches("http://");
        return match verify_v4(req, host) {
            Ok(()) => signed_get(&s, req),
            Err(why) => {
                s.refused.push(why.clone());
                err(403, &why)
            }
        };
    }

    if s.require_auth {
        let ok = req
            .h("authorization")
            .and_then(|a| a.strip_prefix("Bearer "))
            .is_some_and(|t| s.accepted.contains(t));
        if !ok {
            return err(401, "Invalid Credentials");
        }
    }

    let upload_prefix = format!("/upload/storage/v1/b/{BUCKET}/o");
    let api_prefix = format!("/storage/v1/b/{BUCKET}/o");
    if req.path == upload_prefix && req.method == "POST" {
        let name = req.q("name").unwrap_or_default();
        match req.q("uploadType").as_deref() {
            Some("media") => {
                s.objects.insert(name.clone(), req.body.clone());
                return Resp::json(200, &meta(&name, &req.body));
            }
            Some("resumable") => {
                if req.h("x-upload-content-type").is_none() {
                    s.bad.push("resumable start without X-Upload-Content-Type".into());
                }
                s.next_session += 1;
                let sid = format!("sess{}", s.next_session);
                s.sessions.insert(sid.clone(), (name, Vec::new()));
                return Resp::new(200).header("Location", format!("{base}/session/{sid}"));
            }
            other => return err(400, &format!("uploadType {other:?}")),
        }
    }
    if req.path == api_prefix && req.method == "GET" {
        return list(&s, req);
    }
    if let Some(rest) = req.raw_path.strip_prefix(&format!("{api_prefix}/")) {
        let mut segs = rest.split('/');
        let name = pct(segs.next().unwrap_or(""));
        let tail: Vec<&str> = segs.collect();
        return object(&mut s, req, &name, &tail);
    }
    err(400, &format!("unsupported {id}"))
}

fn pct(s: &str) -> String {
    url::form_urlencoded::parse(format!("x={}", s.replace('+', "%2B")).as_bytes())
        .next()
        .map(|(_, v)| v.into_owned())
        .unwrap_or_default()
}

fn object(s: &mut GcsState, req: &Req, name: &str, tail: &[&str]) -> Resp {
    match (req.method.as_str(), tail) {
        ("GET", []) => {
            let Some(data) = s.objects.get(name) else {
                return err(404, "No such object");
            };
            if req.q("alt").as_deref() == Some("media") {
                match req
                    .h("range")
                    .and_then(|r| r.strip_prefix("bytes="))
                    .and_then(|r| r.split_once('-'))
                {
                    Some((a, b)) => {
                        let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                        if a >= data.len() {
                            return err(416, "range");
                        }
                        Resp::new(206).body(data[a..=b.min(data.len() - 1)].to_vec())
                    }
                    None => Resp::new(200).body(data.clone()),
                }
            } else {
                Resp::json(200, &meta(name, data))
            }
        }
        ("DELETE", []) => {
            if s.objects.remove(name).is_some() {
                Resp::new(204)
            } else {
                err(404, "No such object")
            }
        }
        ("POST", ["rewriteTo", "b", bucket, "o", dst]) => {
            let dst = pct(dst);
            if *bucket != BUCKET {
                return err(404, "bucket");
            }
            let Some(data) = s.objects.get(name).cloned() else {
                return err(404, "No such object");
            };
            // Every rewrite takes two calls, like a large cross-location one.
            let key = format!("{name}->{dst}");
            if req.q("rewriteToken").is_some() && s.rewrites.remove(&key) {
                s.objects.insert(dst.clone(), data.clone());
                Resp::json(200, &json!({ "done": true, "resource": meta(&dst, &data) }))
            } else {
                s.rewrites.insert(key);
                Resp::json(
                    200,
                    &json!({ "done": false, "rewriteToken": "rt-1", "totalBytesRewritten": "1" }),
                )
            }
        }
        ("POST", ["compose"]) => {
            let body: Json = serde_json::from_slice(&req.body).unwrap();
            let names: Vec<String> = body["sourceObjects"]
                .as_array()
                .unwrap()
                .iter()
                .map(|o| o["name"].as_str().unwrap().to_owned())
                .collect();
            if names.len() > 32 {
                s.bad.push("compose with more than 32 sources".into());
                return err(400, "too many sources");
            }
            let mut out = Vec::new();
            for n in &names {
                match s.objects.get(n) {
                    Some(d) => out.extend_from_slice(d),
                    None => return err(404, "No such object"),
                }
            }
            let resource = meta(name, &out);
            s.objects.insert(name.to_owned(), out);
            Resp::json(200, &resource)
        }
        _ => err(400, "unsupported object call"),
    }
}

fn list(s: &GcsState, req: &Req) -> Resp {
    let prefix = req.q("prefix").unwrap_or_default();
    let delimiter = req.q("delimiter");
    let after = req.q("pageToken").unwrap_or_default();
    let max = req
        .q("maxResults")
        .and_then(|m| m.parse::<usize>().ok())
        .unwrap_or(1000);
    let page = if s.list_page == 0 { max } else { s.list_page.min(max) };
    let mut items = Vec::new();
    let mut prefixes: Vec<String> = Vec::new();
    for (k, v) in &s.objects {
        if !k.starts_with(&prefix) || k.as_str() <= after.as_str() {
            continue;
        }
        if let Some(d) = &delimiter
            && let Some(i) = k[prefix.len()..].find(d.as_str())
        {
            let p = format!("{}{}", &k[..prefix.len()], &k[prefix.len()..prefix.len() + i + d.len()]);
            if !prefixes.contains(&p) {
                prefixes.push(p);
            }
            continue;
        }
        items.push((k.clone(), v.len()));
    }
    let more = items.len() > page;
    let shown: Vec<Json> = items
        .iter()
        .take(page)
        .map(|(k, n)| json!({ "name": k, "size": n.to_string(), "updated": "2026-11-09T12:00:00.000Z" }))
        .collect();
    let mut out = json!({ "items": shown });
    if !prefixes.is_empty() {
        out["prefixes"] = json!(prefixes);
    }
    if more {
        out["nextPageToken"] = json!(items[page - 1].0);
    }
    Resp::json(200, &out)
}

fn session(s: &mut GcsState, sid: &str, req: &Req) -> Resp {
    if req.method == "DELETE" {
        s.sessions.remove(sid);
        s.cancelled += 1;
        return Resp::new(499);
    }
    let Some((name, data)) = s.sessions.get(sid).cloned() else {
        return err(404, "no such session");
    };
    let range = req.h("content-range").unwrap_or_default().to_owned();
    let Some(spec) = range.strip_prefix("bytes ") else {
        s.bad.push(format!("content-range {range}"));
        return err(400, "content-range");
    };
    let (span, total) = spec.split_once('/').unwrap_or((spec, "*"));
    let total_n: Option<usize> = total.parse().ok();
    let status = |len: usize| {
        let r = Resp::new(308);
        if len > 0 {
            r.header("Range", format!("bytes=0-{}", len - 1))
        } else {
            r
        }
    };
    if span == "*" {
        // A status query, or the empty last chunk of an upload.
        return match total_n {
            Some(t) if t == data.len() => {
                s.sessions.remove(sid);
                s.objects.insert(name.clone(), data.clone());
                Resp::json(200, &meta(&name, &data))
            }
            _ => status(data.len()),
        };
    }
    let (a, b) = span.split_once('-').unwrap();
    let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
    if a != data.len() {
        s.bad.push(format!("chunk at {a} but the session has {}", data.len()));
        return status(data.len());
    }
    if req.body.len() != b - a + 1 {
        s.bad.push(format!("chunk {range} carries {} bytes", req.body.len()));
        return err(400, "length");
    }
    let mut data = data;
    match total_n {
        None => {
            if !req.body.len().is_multiple_of(256 * 1024) {
                s.bad.push(format!("non-final chunk of {} bytes", req.body.len()));
            }
            let keep = if s.partial_once && req.body.len() > 256 * 1024 {
                s.partial_once = false;
                256 * 1024
            } else {
                req.body.len()
            };
            data.extend_from_slice(&req.body[..keep]);
            let len = data.len();
            s.sessions.get_mut(sid).unwrap().1 = data;
            status(len)
        }
        Some(t) => {
            data.extend_from_slice(&req.body);
            if data.len() != t {
                s.bad
                    .push(format!("final chunk says total {t} but {} arrived", data.len()));
                return err(400, "total");
            }
            s.sessions.remove(sid);
            s.objects.insert(name.clone(), data.clone());
            Resp::json(200, &meta(&name, &data))
        }
    }
}

/// RFC 3986 encoding (unreserved characters kept), as V4 signing wants it.
fn uri_encode(s: &str, keep_slash: bool) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => char::from(b).to_string(),
            b'/' if keep_slash => "/".to_owned(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Checks a V4 signed request as Google documents it ("Signature V4 signing
/// process"): canonical request of the method, the path, every query
/// parameter but `X-Goog-Signature` (encoded, sorted), the `host` header and
/// `UNSIGNED-PAYLOAD`; RSA-SHA256 over the string to sign, verified with the
/// public key of the test service account.
fn verify_v4(req: &Req, host: &str) -> Result<(), String> {
    let sa: Json = serde_json::from_str(SA_JSON).unwrap();
    let email = sa["client_email"].as_str().unwrap();
    if req.q("X-Goog-Algorithm").as_deref() != Some("GOOG4-RSA-SHA256") {
        return Err("algorithm".into());
    }
    if req.q("X-Goog-SignedHeaders").as_deref() != Some("host") {
        return Err("signed headers".into());
    }
    let expires: u64 = req.q("X-Goog-Expires").and_then(|e| e.parse().ok()).ok_or("expires")?;
    if !(1..=604_800).contains(&expires) {
        return Err("X-Goog-Expires out of range".into());
    }
    let credential = req.q("X-Goog-Credential").unwrap_or_default();
    let Some(scope) = credential.strip_prefix(&format!("{email}/")) else {
        return Err(format!("credential {credential}"));
    };
    let date = req.q("X-Goog-Date").unwrap_or_default();
    if !scope.ends_with("/auto/storage/goog4_request") || !date.starts_with(&scope[..8]) {
        return Err(format!("scope {scope}"));
    }
    let mut pairs: Vec<(String, String)> = req
        .query
        .iter()
        .filter(|(k, _)| k != "X-Goog-Signature")
        .map(|(k, v)| (uri_encode(k, false), uri_encode(v, false)))
        .collect();
    pairs.sort();
    let query = pairs
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    let canonical = format!(
        "{}\n{}\n{query}\nhost:{host}\n\nhost\nUNSIGNED-PAYLOAD",
        req.method,
        uri_encode(&req.path, true)
    );
    let to_sign = format!(
        "GOOG4-RSA-SHA256\n{date}\n{scope}\n{}",
        forgebase_storage::util::sha256_hex(canonical.as_bytes())
    );
    let sig = forgebase_storage::util::unhex(&req.q("X-Goog-Signature").unwrap_or_default()).ok_or("signature hex")?;
    signature::UnparsedPublicKey::new(&signature::RSA_PKCS1_2048_8192_SHA256, public_key())
        .verify(to_sign.as_bytes(), &sig)
        .map_err(|_| "SignatureDoesNotMatch".to_owned())
}

/// `GET /<bucket>/<object>` of the XML API, with the signed overrides.
fn signed_get(s: &GcsState, req: &Req) -> Resp {
    if req.method != "GET" {
        return err(405, "signed GETs only");
    }
    let Some(name) = req.path.strip_prefix(&format!("/{BUCKET}/")) else {
        return err(404, "No such bucket");
    };
    let Some(data) = s.objects.get(name) else {
        return err(404, "No such object");
    };
    let mut r = Resp::new(200).body(data.clone());
    for (param, header) in [
        ("response-content-type", "Content-Type"),
        ("response-content-disposition", "Content-Disposition"),
    ] {
        if let Some(v) = req.q(param) {
            r = r.header(header, v);
        }
    }
    r
}

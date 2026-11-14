//! A mock of Azure Blob Storage, Microsoft Entra's token endpoint and IMDS.
//! `SharedKey` signatures are recomputed from what arrived, with code written
//! from Microsoft's documentation (not the driver's); protocol violations are
//! recorded in `bad`.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::json;
use sha2::Sha256;

use super::http_mock::{Req, Resp, Server};

pub const ACCOUNT: &str = "devstoreaccount1";
/// Azurite's well-known development account key.
pub const KEY: &str = "Eby8vdM02xNOcqFlqUwJPLlmEtlCDXJ1OUzFT50uSRZ6IFsuFq2UVErCz4I6tq/K1SZFPTOtr/KBHBeksoGMGw==";
pub const CONTAINER: &str = "blobs";
pub const SAS: &str = "sv=2021-08-06&ss=b&srt=co&sp=rwdl&se=2030-01-01T00%3A00%3A00Z&sig=c2lnbmF0dXJl";

#[derive(Clone)]
pub enum Mode {
    SharedKey,
    Sas,
    /// Bearer tokens from the Entra or IMDS endpoints below.
    Bearer,
}

pub struct Fault {
    pub needle: String,
    pub status: u16,
    pub left: u32,
}

pub struct AzState {
    pub mode: Mode,
    /// The account is part of the URL path (Azurite style).
    pub path_style: bool,
    pub blobs: BTreeMap<String, Vec<u8>>,
    staged: HashMap<String, BTreeMap<String, Vec<u8>>>,
    pub accepted: HashSet<String>,
    pub tokens_issued: u32,
    pub token_ttl: i64,
    pub bad: Vec<String>,
    pub faults: Vec<Fault>,
    pub list_page: usize,
    /// Copies answer `pending` and finish after this many `HEAD`s.
    pub pending_copies: u32,
    copying: HashMap<String, u32>,
    /// The `x-ms-date` the driver must send (its clock is fixed in tests).
    pub expect_date: Option<String>,
    pub token_requests: Vec<String>,
    pub deleted_with: Vec<String>,
}

pub struct AzureMock {
    pub server: Server,
    pub st: Arc<Mutex<AzState>>,
}

fn xml_err(status: u16, code: &str) -> Resp {
    Resp::new(status)
        .header("x-ms-error-code", code)
        .header("Content-Type", "application/xml")
        .body(format!(
            "<?xml version=\"1.0\"?><Error><Code>{code}</Code><Message>m</Message></Error>"
        ))
}

fn esc(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// The expected `Authorization`, from Microsoft's "Authorize with Shared Key".
fn expected_shared_key(req: &Req) -> String {
    let h = |n: &str| req.headers.get(n).cloned().unwrap_or_default();
    let len = req.body.len();
    let mut xms: Vec<(String, String)> = req
        .headers
        .iter()
        .filter(|(k, _)| k.starts_with("x-ms-"))
        .map(|(k, v)| (k.clone(), v.trim().to_owned()))
        .collect();
    xms.sort();
    let headers: String = xms.iter().map(|(k, v)| format!("{k}:{v}\n")).collect();
    let mut names: Vec<String> = req.query.iter().map(|(k, _)| k.to_ascii_lowercase()).collect();
    names.sort();
    names.dedup();
    let mut resource = format!("/{ACCOUNT}{}", req.raw_path);
    for n in names {
        let mut vs: Vec<String> = req
            .query
            .iter()
            .filter(|(k, _)| k.to_ascii_lowercase() == n)
            .map(|(_, v)| v.clone())
            .collect();
        vs.sort();
        resource.push_str(&format!("\n{n}:{}", vs.join(",")));
    }
    let string_to_sign = [
        req.method.clone(),
        h("content-encoding"),
        h("content-language"),
        if len == 0 { String::new() } else { len.to_string() },
        h("content-md5"),
        h("content-type"),
        String::new(), // Date: x-ms-date is used instead
        h("if-modified-since"),
        h("if-match"),
        h("if-none-match"),
        h("if-unmodified-since"),
        h("range"),
    ]
    .join("\n")
        + "\n"
        + &headers
        + &resource;
    let key = base64::engine::general_purpose::STANDARD.decode(KEY).unwrap();
    let mut mac = <Hmac<Sha256> as KeyInit>::new_from_slice(&key).unwrap();
    mac.update(string_to_sign.as_bytes());
    format!(
        "SharedKey {ACCOUNT}:{}",
        base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
    )
}

impl AzureMock {
    pub async fn start(mode: Mode, path_style: bool, list_page: usize) -> Self {
        let st = Arc::new(Mutex::new(AzState {
            mode,
            path_style,
            blobs: BTreeMap::new(),
            staged: HashMap::new(),
            accepted: HashSet::new(),
            tokens_issued: 0,
            token_ttl: 3600,
            bad: Vec::new(),
            faults: Vec::new(),
            list_page,
            pending_copies: 0,
            copying: HashMap::new(),
            expect_date: None,
            token_requests: Vec::new(),
            deleted_with: Vec::new(),
        }));
        let state = st.clone();
        let server = Server::start(Arc::new(move |req: &Req| handle(&state, req))).await;
        Self { server, st }
    }

    /// The account URL the driver is configured with.
    pub fn endpoint(&self) -> String {
        let st = self.st.lock().unwrap();
        if st.path_style {
            format!("{}/{ACCOUNT}", self.server.url())
        } else {
            self.server.url()
        }
    }

    pub fn fail(&self, needle: &str, status: u16, times: u32) {
        self.st.lock().unwrap().faults.push(Fault {
            needle: needle.to_owned(),
            status,
            left: times,
        });
    }

    pub fn names(&self) -> Vec<String> {
        self.st.lock().unwrap().blobs.keys().cloned().collect()
    }

    pub fn blob(&self, name: &str) -> Option<Vec<u8>> {
        self.st.lock().unwrap().blobs.get(name).cloned()
    }

    pub fn bad(&self) -> Vec<String> {
        self.st.lock().unwrap().bad.clone()
    }
}

fn handle(st: &Arc<Mutex<AzState>>, req: &Req) -> Resp {
    let mut s = st.lock().unwrap();
    let id = format!("{} {}", req.method, req.path);
    if let Some(f) = s.faults.iter_mut().find(|f| f.left > 0 && id.contains(&f.needle)) {
        f.left -= 1;
        return xml_err(f.status, "ServerBusy").header("Retry-After", "0");
    }

    // Microsoft Entra: client credentials.
    if req.method == "POST" && req.path.ends_with("/oauth2/v2.0/token") {
        let form: HashMap<String, String> = url::form_urlencoded::parse(&req.body).into_owned().collect();
        if form.get("grant_type").map(String::as_str) != Some("client_credentials")
            || form.get("scope").map(String::as_str) != Some("https://storage.azure.com/.default")
            || form.get("client_secret").map(String::as_str) != Some("shh")
        {
            return Resp::json(400, &json!({ "error": "invalid_client" }));
        }
        s.token_requests.push(format!("entra {}", req.path));
        return issue(&mut s);
    }
    // IMDS / App Service identity.
    if req.method == "GET" && req.path == "/metadata/identity/oauth2/token" {
        if req.h("metadata") != Some("true") && req.h("x-identity-header").is_none() {
            return Resp::new(400).body("missing Metadata header");
        }
        if req.q("resource").as_deref() != Some("https://storage.azure.com/") {
            return Resp::new(400).body("bad resource");
        }
        s.token_requests
            .push(format!("imds {}", req.q("client_id").unwrap_or_default()));
        return issue(&mut s);
    }

    if req.h("x-ms-version").is_none() {
        s.bad.push("no x-ms-version".into());
    }
    match s.mode.clone() {
        Mode::SharedKey => {
            if req.h("authorization") != Some(expected_shared_key(req).as_str()) {
                s.bad.push(format!("bad SharedKey signature on {id}"));
                return xml_err(403, "AuthenticationFailed");
            }
            if let Some(d) = &s.expect_date
                && req.h("x-ms-date") != Some(d.as_str())
            {
                s.bad.push(format!("x-ms-date {:?}", req.h("x-ms-date")));
            }
        }
        Mode::Sas => {
            if req.h("authorization").is_some() || req.q("sig").as_deref() != Some("c2lnbmF0dXJl") {
                s.bad.push(format!("SAS expected on {id}"));
                return xml_err(403, "AuthenticationFailed");
            }
        }
        Mode::Bearer => {
            let ok = req
                .h("authorization")
                .and_then(|a| a.strip_prefix("Bearer "))
                .is_some_and(|t| s.accepted.contains(t));
            if !ok {
                return xml_err(401, "InvalidAuthenticationInfo");
            }
        }
    }

    let mut path = req.path.clone();
    if s.path_style {
        match path.strip_prefix(&format!("/{ACCOUNT}")) {
            Some(p) => path = p.to_owned(),
            None => return xml_err(404, "InvalidUri"),
        }
    }
    let Some(rest) = path.strip_prefix(&format!("/{CONTAINER}")) else {
        return xml_err(404, "ContainerNotFound");
    };
    if rest.is_empty() {
        return list(&s, req);
    }
    let name = rest.trim_start_matches('/').to_owned();
    blob(&mut s, req, &name)
}

fn issue(s: &mut AzState) -> Resp {
    s.tokens_issued += 1;
    let tok = format!("az-tok-{}", s.tokens_issued);
    s.accepted.insert(tok.clone());
    // IMDS sends expires_in as a string.
    Resp::json(
        200,
        &json!({ "access_token": tok, "expires_in": s.token_ttl.to_string(), "token_type": "Bearer" }),
    )
}

fn list(s: &AzState, req: &Req) -> Resp {
    if req.q("restype").as_deref() != Some("container") || req.q("comp").as_deref() != Some("list") {
        return xml_err(400, "InvalidQueryParameterValue");
    }
    let prefix = req.q("prefix").unwrap_or_default();
    let delimiter = req.q("delimiter");
    let marker = req.q("marker").unwrap_or_default();
    let max = req
        .q("maxresults")
        .and_then(|m| m.parse::<usize>().ok())
        .unwrap_or(5000);
    let page = if s.list_page == 0 { max } else { s.list_page.min(max) };
    let mut blobs = Vec::new();
    let mut prefixes: Vec<String> = Vec::new();
    for (k, v) in &s.blobs {
        if !k.starts_with(&prefix) || k.as_str() <= marker.as_str() {
            continue;
        }
        if let Some(d) = &delimiter
            && let Some(i) = k[prefix.len()..].find(d.as_str())
        {
            let p = k[..prefix.len() + i + d.len()].to_owned();
            if !prefixes.contains(&p) {
                prefixes.push(p);
            }
            continue;
        }
        blobs.push((k.clone(), v.len()));
    }
    let mut xml = String::from("<?xml version=\"1.0\" encoding=\"utf-8\"?><EnumerationResults><Blobs>");
    for (k, n) in blobs.iter().take(page) {
        xml.push_str(&format!(
            "<Blob><Name>{}</Name><Properties><Last-Modified>Mon, 09 Nov 2026 12:00:00 GMT</Last-Modified><Content-Length>{n}</Content-Length></Properties></Blob>",
            esc(k)
        ));
    }
    for p in &prefixes {
        xml.push_str(&format!("<BlobPrefix><Name>{}</Name></BlobPrefix>", esc(p)));
    }
    xml.push_str("</Blobs>");
    if blobs.len() > page {
        xml.push_str(&format!("<NextMarker>{}</NextMarker>", esc(&blobs[page - 1].0)));
    } else {
        xml.push_str("<NextMarker />");
    }
    xml.push_str("</EnumerationResults>");
    Resp::new(200).header("Content-Type", "application/xml").body(xml)
}

fn blob(s: &mut AzState, req: &Req, name: &str) -> Resp {
    match req.method.as_str() {
        "PUT" => {
            if let Some(src) = req.h("x-ms-copy-source") {
                let u = url::Url::parse(src).unwrap();
                let mut p = u.path().to_owned();
                if s.path_style {
                    p = p.strip_prefix(&format!("/{ACCOUNT}")).unwrap_or(&p).to_owned();
                }
                let src_name = p.trim_start_matches(&format!("/{CONTAINER}/")).to_owned();
                let src_name = url::form_urlencoded::parse(format!("x={}", src_name.replace('+', "%2B")).as_bytes())
                    .next()
                    .map(|(_, v)| v.into_owned())
                    .unwrap_or_default();
                let Some(data) = s.blobs.get(&src_name).cloned() else {
                    return xml_err(404, "CannotVerifyCopySource");
                };
                s.blobs.insert(name.to_owned(), data);
                if s.pending_copies > 0 {
                    let n = s.pending_copies;
                    s.copying.insert(name.to_owned(), n);
                    return Resp::new(202)
                        .header("x-ms-copy-status", "pending")
                        .header("x-ms-copy-id", "c1");
                }
                return Resp::new(202)
                    .header("x-ms-copy-status", "success")
                    .header("x-ms-copy-id", "c1");
            }
            match req.q("comp").as_deref() {
                Some("block") => {
                    let id = req.q("blockid").unwrap_or_default();
                    let staged = s.staged.entry(name.to_owned()).or_default();
                    if staged.keys().any(|k| k.len() != id.len()) {
                        s.bad.push("block ids of different lengths".into());
                        return xml_err(400, "InvalidBlobOrBlock");
                    }
                    staged.insert(id, req.body.clone());
                    Resp::new(201)
                }
                Some("blocklist") => {
                    let xml = String::from_utf8_lossy(&req.body).into_owned();
                    let ids: Vec<String> = xml
                        .split("<Latest>")
                        .skip(1)
                        .map(|p| p.split("</Latest>").next().unwrap().to_owned())
                        .collect();
                    let staged = s.staged.remove(name).unwrap_or_default();
                    let mut out = Vec::new();
                    for id in &ids {
                        match staged.get(id) {
                            Some(d) => out.extend_from_slice(d),
                            None => {
                                s.bad.push(format!("block list names unknown block {id}"));
                                return xml_err(400, "InvalidBlockList");
                            }
                        }
                    }
                    s.blobs.insert(name.to_owned(), out);
                    Resp::new(201)
                }
                None => {
                    if req.h("x-ms-blob-type") != Some("BlockBlob") {
                        s.bad.push("Put Blob without x-ms-blob-type".into());
                        return xml_err(400, "MissingRequiredHeader");
                    }
                    s.blobs.insert(name.to_owned(), req.body.clone());
                    Resp::new(201)
                }
                other => xml_err(400, &format!("unsupported comp {other:?}")),
            }
        }
        "GET" | "HEAD" => {
            let Some(data) = s.blobs.get(name).cloned() else {
                return xml_err(404, "BlobNotFound");
            };
            let mut r = Resp::new(200).header("Last-Modified", "Mon, 09 Nov 2026 12:00:00 GMT");
            if let Some(left) = s.copying.get_mut(name) {
                if *left == 0 {
                    s.copying.remove(name);
                    r = r.header("x-ms-copy-status", "success");
                } else {
                    *left -= 1;
                    r = r.header("x-ms-copy-status", "pending");
                }
            }
            if req.method == "HEAD" {
                return r.header("Content-Length", data.len().to_string());
            }
            match req
                .h("x-ms-range")
                .and_then(|r| r.strip_prefix("bytes="))
                .and_then(|r| r.split_once('-'))
            {
                Some((a, b)) => {
                    let (a, b): (usize, usize) = (a.parse().unwrap(), b.parse().unwrap());
                    if a >= data.len() {
                        return xml_err(416, "InvalidRange");
                    }
                    let mut r = r;
                    r.status = 206;
                    r.body = data[a..=b.min(data.len() - 1)].to_vec();
                    r
                }
                None => r.body(data),
            }
        }
        "DELETE" => {
            s.deleted_with
                .push(req.h("x-ms-delete-snapshots").unwrap_or_default().to_owned());
            if s.blobs.remove(name).is_some() {
                Resp::new(202)
            } else {
                xml_err(404, "BlobNotFound")
            }
        }
        _ => xml_err(400, "unsupported"),
    }
}

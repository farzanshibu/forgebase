//! One client connection: decoding, authentication gate, command dispatch,
//! pub/sub delivery, timeouts and graceful close.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use forgebase_auth::scopes::Access;
use forgebase_kv::{Class, KvError, Reply, Subscription, glob_matches, spec};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::sync::broadcast::error::RecvError;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::{Duration, Instant, sleep, sleep_until, timeout};

use crate::auth::{AuthFailure, AuthLimiter, Authenticator, Identity};
use crate::backend::KvBackend;
use crate::compat::{self, Support};
use crate::config::{PREAUTH_MAX_ARGS, PREAUTH_MAX_BULK, RespConfig};
use crate::errors::{kv_error, unknown_command, wrong_arity};
use crate::protocol::{
    Decoder, Limits, Proto, put_array, put_bulk, put_error, put_int, put_map, put_null, put_push, put_reply,
    put_reply_as_map, put_reply_as_set, put_status,
};

/// State shared by the accept loop and every connection.
#[derive(Debug)]
pub(crate) struct Shared {
    pub cfg: RespConfig,
    pub backend: Arc<dyn KvBackend>,
    pub auth: Arc<dyn Authenticator>,
    pub limiter: AuthLimiter,
    pub active: AtomicUsize,
    pub next_id: AtomicU64,
    pub started: Instant,
}

/// Decrements the connection count when a connection task ends, however it ends.
pub(crate) struct ActiveGuard(pub Arc<Shared>);

impl Drop for ActiveGuard {
    fn drop(&mut self) {
        let n = self.0.active.fetch_sub(1, Ordering::SeqCst).saturating_sub(1);
        metrics::gauge!("forgebase_resp_connections").set(n as f64);
    }
}

#[derive(Debug)]
struct PubMsg {
    pattern: Option<String>,
    channel: String,
    payload: Vec<u8>,
}

const MSG_QUEUE: usize = 1024;
const FLUSH_AT: usize = 1 << 20;
const SERVER_NAME: &str = "forgebase";
/// Redis version reported for client feature detection (see ADR-0013).
const REDIS_COMPAT_VERSION: &str = "7.0.0";

struct Conn {
    sh: Arc<Shared>,
    id: u64,
    peer: SocketAddr,
    proto: Proto,
    identity: Option<Identity>,
    ns: String,
    name: Option<String>,
    chans: HashMap<String, JoinHandle<()>>,
    pats: HashMap<String, JoinHandle<()>>,
    msg_tx: mpsc::Sender<PubMsg>,
    close: bool,
    failures: u32,
    last_check: Instant,
}

fn upper(b: &[u8]) -> String {
    String::from_utf8_lossy(b).to_ascii_uppercase()
}

async fn until(d: Option<Instant>) {
    match d {
        Some(d) => sleep_until(d).await,
        None => std::future::pending::<()>().await,
    }
}

/// Serves one accepted connection until it ends.
pub(crate) async fn serve(
    sh: Arc<Shared>,
    mut sock: TcpStream,
    peer: SocketAddr,
    id: u64,
    mut shutdown: watch::Receiver<bool>,
) {
    let _guard = ActiveGuard(Arc::clone(&sh));
    let _ = sock.set_nodelay(true);
    let (msg_tx, mut msg_rx) = mpsc::channel(MSG_QUEUE);
    let started = Instant::now();
    let mut c = Conn {
        sh: Arc::clone(&sh),
        id,
        peer,
        proto: Proto::V2,
        identity: None,
        ns: "default".into(),
        name: None,
        chans: HashMap::new(),
        pats: HashMap::new(),
        msg_tx,
        close: false,
        failures: 0,
        last_check: started,
    };
    let mut dec = Decoder::default();
    let mut out: Vec<u8> = Vec::with_capacity(4096);
    let mut last_read = started;
    tracing::debug!(%peer, id, "resp connection opened");

    'conn: loop {
        if *shutdown.borrow() {
            break;
        }
        dec.compact();
        dec.buf.reserve(16 * 1024);
        let idle = c.idle_deadline(started, last_read);
        let reval = c.reval_deadline();
        tokio::select! {
            biased;
            _ = shutdown.changed() => break 'conn,
            () = until(idle) => {
                tracing::debug!(%peer, id, "resp connection timed out");
                break 'conn;
            }
            () = until(reval) => {
                if !c.revalidate().await {
                    put_error(&mut out, "NOAUTH credential is no longer valid");
                    c.close = true;
                }
            }
            Some(m) = msg_rx.recv() => c.put_message(&m, &mut out),
            r = sock.read_buf(&mut dec.buf) => match r {
                Ok(0) | Err(_) => break 'conn,
                Ok(_) => last_read = Instant::now(),
            },
        }
        while !c.close {
            match dec.next(&c.limits()) {
                Ok(Some(args)) => {
                    c.handle(args, &mut out).await;
                    if out.len() >= FLUSH_AT && !flush(&mut sock, &mut out, sh.cfg.write_timeout).await {
                        break 'conn;
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    put_error(&mut out, &format!("ERR Protocol error: {e}"));
                    c.close = true;
                }
            }
        }
        if !flush(&mut sock, &mut out, sh.cfg.write_timeout).await || c.close {
            break;
        }
    }
    c.drop_subscriptions();
    let _ = sock.shutdown().await;
    tracing::debug!(%peer, id, "resp connection closed");
}

async fn flush(sock: &mut TcpStream, out: &mut Vec<u8>, wt: Duration) -> bool {
    if out.is_empty() {
        return true;
    }
    let ok = if wt.is_zero() {
        sock.write_all(out).await.is_ok()
    } else {
        matches!(timeout(wt, sock.write_all(out)).await, Ok(Ok(())))
    };
    out.clear();
    ok
}

impl Conn {
    fn limits(&self) -> Limits {
        let c = &self.sh.cfg;
        if self.identity.is_some() {
            Limits {
                max_args: c.max_args,
                max_bulk: c.max_bulk_bytes,
                max_request: c.max_request_bytes,
            }
        } else {
            Limits {
                max_args: c.max_args.min(PREAUTH_MAX_ARGS),
                max_bulk: c.max_bulk_bytes.min(PREAUTH_MAX_BULK),
                max_request: c.max_request_bytes.min(PREAUTH_MAX_BULK * PREAUTH_MAX_ARGS),
            }
        }
    }

    fn sub_count(&self) -> usize {
        self.chans.len() + self.pats.len()
    }

    fn idle_deadline(&self, started: Instant, last_read: Instant) -> Option<Instant> {
        let c = &self.sh.cfg;
        if self.identity.is_none() {
            return (!c.auth_timeout.is_zero()).then(|| started + c.auth_timeout);
        }
        if self.sub_count() > 0 || c.idle_timeout.is_zero() {
            return None;
        }
        Some(last_read + c.idle_timeout)
    }

    /// Subscribed connections send no commands, so their credential is
    /// re-checked on a timer.
    fn reval_deadline(&self) -> Option<Instant> {
        let every = self.sh.cfg.revalidate_interval;
        (self.identity.is_some() && self.sub_count() > 0 && !every.is_zero()).then(|| self.last_check + every)
    }

    async fn revalidate(&mut self) -> bool {
        let Some(id) = &self.identity else { return true };
        let ok = self.sh.auth.still_valid(id).await;
        self.last_check = Instant::now();
        ok
    }

    async fn revalidate_if_due(&mut self) -> bool {
        let every = self.sh.cfg.revalidate_interval;
        if every.is_zero() || self.last_check.elapsed() < every {
            return true;
        }
        self.revalidate().await
    }

    fn drop_subscriptions(&mut self) {
        for (_, h) in self.chans.drain().chain(self.pats.drain()) {
            h.abort();
        }
    }

    fn put_message(&self, m: &PubMsg, out: &mut Vec<u8>) {
        match &m.pattern {
            None => {
                put_push(out, 3, self.proto);
                put_bulk(out, b"message");
            }
            Some(p) => {
                put_push(out, 4, self.proto);
                put_bulk(out, b"pmessage");
                put_bulk(out, p.as_bytes());
            }
        }
        put_bulk(out, m.channel.as_bytes());
        put_bulk(out, &m.payload);
    }

    // ---- dispatch ----------------------------------------------------------

    async fn handle(&mut self, args: Vec<Vec<u8>>, out: &mut Vec<u8>) {
        metrics::counter!("forgebase_resp_commands_total").increment(1);
        let name = upper(&args[0]);
        if self.proto == Proto::V2
            && self.sub_count() > 0
            && !matches!(
                name.as_str(),
                "SUBSCRIBE" | "UNSUBSCRIBE" | "PSUBSCRIBE" | "PUNSUBSCRIBE" | "PING" | "QUIT" | "RESET"
            )
        {
            put_error(
                out,
                &format!(
                    "ERR Can't execute '{}': only (P)SUBSCRIBE / (P)UNSUBSCRIBE / PING / QUIT / RESET are allowed in this context",
                    name.to_ascii_lowercase()
                ),
            );
            return;
        }
        if self.identity.is_none() && !matches!(name.as_str(), "PING" | "HELLO" | "AUTH" | "QUIT") {
            put_error(out, "NOAUTH Authentication required.");
            return;
        }
        match name.as_str() {
            "PING" => self.cmd_ping(&args, out),
            "ECHO" => match args.as_slice() {
                [_, msg] => put_bulk(out, msg),
                _ => put_error(out, &wrong_arity("echo")),
            },
            "QUIT" => {
                put_status(out, "OK");
                self.close = true;
            }
            "AUTH" => self.cmd_auth(&args, out).await,
            "HELLO" => self.cmd_hello(&args, out).await,
            "SELECT" => self.cmd_select(&args, out),
            "RESET" => {
                self.drop_subscriptions();
                self.ns = "default".into();
                self.proto = Proto::V2;
                self.identity = None;
                self.name = None;
                put_status(out, "RESET");
            }
            "CLIENT" => self.cmd_client(&args, out),
            "COMMAND" => self.cmd_command(&args, out),
            "INFO" => self.cmd_info(out).await,
            "TIME" => {
                let d = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default();
                put_array(out, 2);
                put_bulk(out, d.as_secs().to_string().as_bytes());
                put_bulk(out, d.subsec_micros().to_string().as_bytes());
            }
            "CONFIG" => match args.get(1).map(|a| upper(a)).as_deref() {
                None => put_error(out, &wrong_arity("config")),
                Some("GET") if args.len() == 3 => put_map(out, 0, self.proto),
                Some(sub) => put_error(out, &format!("ERR CONFIG {sub} is not supported")),
            },
            "SUBSCRIBE" => self.cmd_subscribe(&args, false, out).await,
            "PSUBSCRIBE" => self.cmd_subscribe(&args, true, out).await,
            "UNSUBSCRIBE" => self.cmd_unsubscribe(&args, false, out),
            "PUNSUBSCRIBE" => self.cmd_unsubscribe(&args, true, out),
            _ => self.cmd_data(&name, args, out).await,
        }
    }

    fn cmd_ping(&self, args: &[Vec<u8>], out: &mut Vec<u8>) {
        if args.len() > 2 {
            put_error(out, &wrong_arity("ping"));
        } else if self.proto == Proto::V2 && self.sub_count() > 0 {
            put_array(out, 2);
            put_bulk(out, b"pong");
            put_bulk(out, args.get(1).map_or(&[][..], Vec::as_slice));
        } else if let Some(m) = args.get(1) {
            put_bulk(out, m);
        } else {
            put_status(out, "PONG");
        }
    }

    // ---- authentication ------------------------------------------------------

    /// Checks the credential; `Err` is the error line to send.
    async fn authenticate(&mut self, user: Option<&str>, pass: &[u8]) -> Result<(), String> {
        let ip = self.peer.ip();
        if self.sh.limiter.blocked(ip) {
            metrics::counter!("forgebase_resp_auth_failures_total").increment(1);
            self.close = true;
            return Err("ERR too many failed authentication attempts; try again later".into());
        }
        let result = match std::str::from_utf8(pass) {
            Ok(p) => self.sh.auth.authenticate(user, p).await,
            Err(_) => Err(AuthFailure::Invalid),
        };
        match result {
            Ok(id) => {
                self.identity = Some(id);
                self.last_check = Instant::now();
                self.failures = 0;
                Ok(())
            }
            Err(AuthFailure::Invalid) => {
                metrics::counter!("forgebase_resp_auth_failures_total").increment(1);
                self.sh.limiter.record_failure(ip);
                self.failures += 1;
                if self.failures >= self.sh.cfg.max_auth_failures {
                    self.close = true;
                }
                let d = self.sh.cfg.auth_failure_delay;
                if !d.is_zero() {
                    sleep(d).await;
                }
                Err("WRONGPASS invalid username-password pair or user is disabled.".into())
            }
            Err(AuthFailure::Unavailable(m)) => {
                tracing::warn!(error = %m, "resp authentication unavailable");
                Err("ERR authentication is temporarily unavailable".into())
            }
        }
    }

    async fn cmd_auth(&mut self, args: &[Vec<u8>], out: &mut Vec<u8>) {
        let (user, pass) = match args {
            [_, pass] => (None, pass),
            [_, user, pass] => (Some(String::from_utf8_lossy(user).into_owned()), pass),
            _ => return put_error(out, &wrong_arity("auth")),
        };
        match self.authenticate(user.as_deref(), pass).await {
            Ok(()) => put_status(out, "OK"),
            Err(e) => put_error(out, &e),
        }
    }

    async fn cmd_hello(&mut self, args: &[Vec<u8>], out: &mut Vec<u8>) {
        let mut proto = self.proto;
        let mut i = 1;
        if let Some(v) = args.get(1) {
            proto = match v.as_slice() {
                b"2" => Proto::V2,
                b"3" => Proto::V3,
                other if std::str::from_utf8(other).is_ok_and(|s| s.parse::<i64>().is_ok()) => {
                    return put_error(out, "NOPROTO sorry, this protocol version is not supported");
                }
                _ => return put_error(out, "ERR Protocol version is not an integer or out of range"),
            };
            i = 2;
        }
        let mut creds: Option<(String, Vec<u8>)> = None;
        let mut setname = None;
        while i < args.len() {
            match upper(&args[i]).as_str() {
                "AUTH" if i + 2 < args.len() => {
                    creds = Some((String::from_utf8_lossy(&args[i + 1]).into_owned(), args[i + 2].clone()));
                    i += 3;
                }
                "SETNAME" if i + 1 < args.len() => {
                    setname = Some(args[i + 1].clone());
                    i += 2;
                }
                _ => {
                    return put_error(
                        out,
                        &format!(
                            "ERR Syntax error in HELLO option '{}'",
                            String::from_utf8_lossy(&args[i])
                        ),
                    );
                }
            }
        }
        if let Some(n) = &setname
            && !valid_client_name(n)
        {
            return put_error(
                out,
                "ERR Client names cannot contain spaces, newlines or special characters.",
            );
        }
        if let Some((user, pass)) = creds
            && let Err(e) = self.authenticate(Some(&user), &pass).await
        {
            return put_error(out, &e);
        }
        if self.identity.is_none() {
            return put_error(
                out,
                "NOAUTH HELLO must be called with the client already authenticated, otherwise the HELLO <proto> AUTH <user> <pass> option can be used to authenticate the client and select the RESP protocol version at the same time",
            );
        }
        if let Some(n) = setname {
            self.name = Some(String::from_utf8_lossy(&n).into_owned());
        }
        self.proto = proto;
        put_map(out, 7, proto);
        put_bulk(out, b"server");
        put_bulk(out, SERVER_NAME.as_bytes());
        put_bulk(out, b"version");
        put_bulk(out, env!("CARGO_PKG_VERSION").as_bytes());
        put_bulk(out, b"proto");
        put_int(out, if proto == Proto::V3 { 3 } else { 2 });
        put_bulk(out, b"id");
        put_int(out, i64::try_from(self.id).unwrap_or(i64::MAX));
        put_bulk(out, b"mode");
        put_bulk(out, b"standalone");
        put_bulk(out, b"role");
        put_bulk(out, b"master");
        put_bulk(out, b"modules");
        put_array(out, 0);
    }

    // ---- connection state ------------------------------------------------------

    fn cmd_select(&mut self, args: &[Vec<u8>], out: &mut Vec<u8>) {
        let [_, arg] = args else {
            return put_error(out, &wrong_arity("select"));
        };
        match namespace_for(arg) {
            Ok(ns) => {
                self.ns = ns;
                put_status(out, "OK");
            }
            Err(e) => put_error(out, e),
        }
    }

    fn cmd_client(&mut self, args: &[Vec<u8>], out: &mut Vec<u8>) {
        let Some(sub) = args.get(1).map(|a| upper(a)) else {
            return put_error(out, &wrong_arity("client"));
        };
        match (sub.as_str(), args.len()) {
            ("ID", 2) => put_int(out, i64::try_from(self.id).unwrap_or(i64::MAX)),
            ("GETNAME", 2) => match &self.name {
                Some(n) => put_bulk(out, n.as_bytes()),
                None => put_null(out, self.proto),
            },
            ("SETNAME", 3) => {
                if valid_client_name(&args[2]) {
                    self.name = Some(String::from_utf8_lossy(&args[2]).into_owned()).filter(|n| !n.is_empty());
                    put_status(out, "OK");
                } else {
                    put_error(
                        out,
                        "ERR Client names cannot contain spaces, newlines or special characters.",
                    );
                }
            }
            ("SETINFO", 4) => put_status(out, "OK"),
            ("INFO", 2) => {
                let line = format!(
                    "id={} addr={} name={} db={} resp={}\n",
                    self.id,
                    self.peer,
                    self.name.as_deref().unwrap_or(""),
                    self.ns,
                    if self.proto == Proto::V3 { 3 } else { 2 }
                );
                put_bulk(out, line.as_bytes());
            }
            ("ID" | "GETNAME" | "SETNAME" | "SETINFO" | "INFO", _) => put_error(out, &wrong_arity("client")),
            _ => put_error(out, &format!("ERR CLIENT {sub} is not supported")),
        }
    }

    fn cmd_command(&self, args: &[Vec<u8>], out: &mut Vec<u8>) {
        match args.get(1).map(|a| upper(a)).as_deref() {
            None | Some("DOCS" | "INFO") => put_map(out, 0, self.proto),
            Some("COUNT") => put_int(
                out,
                i64::try_from(compat::CONNECTION_COMMANDS.len() + compat::DATA_COMMANDS.len()).unwrap_or(i64::MAX),
            ),
            Some("LIST") => {
                let all: Vec<&str> = compat::CONNECTION_COMMANDS
                    .iter()
                    .chain(compat::DATA_COMMANDS)
                    .copied()
                    .collect();
                put_array(out, all.len());
                for n in all {
                    put_bulk(out, n.to_ascii_lowercase().as_bytes());
                }
            }
            Some(other) => put_error(out, &format!("ERR COMMAND {other} is not supported")),
        }
    }

    async fn cmd_info(&self, out: &mut Vec<u8>) {
        let mut keys = 0i64;
        if let Some(id) = &self.identity
            && id.allows(&self.ns, Access::Read)
            && let Ok(Reply::Int(n)) = self.sh.backend.exec(&id.ctx, &self.ns, &[b"DBSIZE".to_vec()]).await
        {
            keys = n;
        }
        let text = format!(
            "# Server\r\nredis_version:{REDIS_COMPAT_VERSION}\r\nforgebase_version:{}\r\nredis_mode:standalone\r\nprocess_id:{}\r\nuptime_in_seconds:{}\r\n\r\n# Clients\r\nconnected_clients:{}\r\n\r\n# Keyspace\r\n{}\r\n",
            env!("CARGO_PKG_VERSION"),
            std::process::id(),
            self.sh.started.elapsed().as_secs(),
            self.sh.active.load(Ordering::SeqCst),
            if keys > 0 {
                format!("namespace={},keys={keys},expires=0,avg_ttl=0", self.ns)
            } else {
                String::new()
            },
        );
        put_bulk(out, text.as_bytes());
    }

    // ---- data commands ----------------------------------------------------------

    async fn cmd_data(&mut self, name: &str, mut args: Vec<Vec<u8>>, out: &mut Vec<u8>) {
        match compat::classify(name) {
            Support::Refused(msg) => return put_error(out, &compat::refusal(name, msg)),
            Support::Unknown => return put_error(out, &unknown_command(&args)),
            Support::Supported => {}
        }
        let Some(sp) = spec(name) else {
            return put_error(out, &unknown_command(&args));
        };
        if !self.revalidate_if_due().await {
            put_error(out, "NOAUTH credential is no longer valid");
            self.close = true;
            return;
        }
        let Some(id) = &self.identity else {
            return put_error(out, "NOAUTH Authentication required.");
        };
        let access = if sp.class == Class::Read {
            Access::Read
        } else {
            Access::Write
        };
        if !id.allows(&self.ns, access) {
            return put_error(
                out,
                &format!(
                    "NOPERM this API key lacks the scope {} needed for '{}'",
                    Identity::needed(&self.ns, access),
                    name.to_ascii_lowercase()
                ),
            );
        }
        // FLUSHDB [ASYNC|SYNC]: the engine takes no options; both are the same here.
        if name == "FLUSHDB" && args.len() == 2 && matches!(upper(&args[1]).as_str(), "ASYNC" | "SYNC") {
            args.truncate(1);
        }
        match self.sh.backend.exec(&id.ctx, &self.ns, &args).await {
            Ok(reply) => match name {
                "FLUSHDB" => put_status(out, "OK"),
                "HGETALL" => put_reply_as_map(out, &reply, self.proto),
                "SMEMBERS" | "SUNION" | "SINTER" | "SDIFF" => put_reply_as_set(out, &reply, self.proto),
                _ => put_reply(out, &reply, self.proto),
            },
            Err(e) => put_error(out, &kv_error(&e, &args)),
        }
    }

    // ---- pub/sub -----------------------------------------------------------------

    async fn cmd_subscribe(&mut self, args: &[Vec<u8>], pattern: bool, out: &mut Vec<u8>) {
        let cmd = if pattern { "psubscribe" } else { "subscribe" };
        if args.len() < 2 {
            return put_error(out, &wrong_arity(cmd));
        }
        if !self.revalidate_if_due().await {
            put_error(out, "NOAUTH credential is no longer valid");
            self.close = true;
            return;
        }
        let Some(id) = self.identity.clone() else {
            return put_error(out, "NOAUTH Authentication required.");
        };
        if !id.allows(&self.ns, Access::Read) {
            return put_error(
                out,
                &format!(
                    "NOPERM this API key lacks the scope {} needed for '{cmd}'",
                    Identity::needed(&self.ns, Access::Read)
                ),
            );
        }
        for raw in &args[1..] {
            let Ok(ch) = std::str::from_utf8(raw) else {
                put_error(out, "ERR channel names and patterns must be valid UTF-8");
                continue;
            };
            let known = if pattern {
                self.pats.contains_key(ch)
            } else {
                self.chans.contains_key(ch)
            };
            if !known {
                if self.sub_count() >= self.sh.cfg.max_subscriptions {
                    put_error(out, "ERR too many subscriptions on this connection");
                    break;
                }
                let sub = if pattern {
                    self.sh.backend.subscribe_all(&id.ctx, &self.ns)
                } else {
                    self.sh.backend.subscribe(&id.ctx, &self.ns, ch)
                };
                match sub {
                    Ok(rx) => {
                        let h = forward(rx, self.msg_tx.clone(), pattern.then(|| ch.to_owned()));
                        if pattern {
                            self.pats.insert(ch.to_owned(), h);
                        } else {
                            self.chans.insert(ch.to_owned(), h);
                        }
                    }
                    Err(e) => {
                        put_error(out, &kv_error(&e, args));
                        continue;
                    }
                }
            }
            put_push(out, 3, self.proto);
            put_bulk(out, cmd.as_bytes());
            put_bulk(out, raw);
            put_int(out, i64::try_from(self.sub_count()).unwrap_or(i64::MAX));
        }
    }

    fn cmd_unsubscribe(&mut self, args: &[Vec<u8>], pattern: bool, out: &mut Vec<u8>) {
        let cmd = if pattern { "punsubscribe" } else { "unsubscribe" };
        let targets: Vec<String> = if args.len() > 1 {
            args[1..]
                .iter()
                .map(|a| String::from_utf8_lossy(a).into_owned())
                .collect()
        } else if pattern {
            self.pats.keys().cloned().collect()
        } else {
            self.chans.keys().cloned().collect()
        };
        if targets.is_empty() {
            put_push(out, 3, self.proto);
            put_bulk(out, cmd.as_bytes());
            put_null(out, self.proto);
            put_int(out, i64::try_from(self.sub_count()).unwrap_or(i64::MAX));
            return;
        }
        for t in targets {
            let map = if pattern { &mut self.pats } else { &mut self.chans };
            if let Some(h) = map.remove(&t) {
                h.abort();
            }
            put_push(out, 3, self.proto);
            put_bulk(out, cmd.as_bytes());
            put_bulk(out, t.as_bytes());
            put_int(out, i64::try_from(self.sub_count()).unwrap_or(i64::MAX));
        }
    }
}

/// Moves messages from an engine subscription into the connection's queue.
/// A subscriber that falls behind loses the oldest messages (at-most-once
/// delivery, like Redis pub/sub); the loss is counted.
fn forward(mut rx: Subscription, tx: mpsc::Sender<PubMsg>, pattern: Option<String>) -> JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            match rx.recv().await {
                Ok(m) => {
                    if let Some(p) = &pattern
                        && !glob_matches(p, &m.channel)
                    {
                        continue;
                    }
                    let msg = PubMsg {
                        pattern: pattern.clone(),
                        channel: m.channel,
                        payload: m.payload,
                    };
                    if tx.send(msg).await.is_err() {
                        break;
                    }
                }
                Err(RecvError::Lagged(n)) => {
                    metrics::counter!("forgebase_resp_pubsub_dropped_total").increment(n);
                }
                Err(RecvError::Closed) => break,
            }
        }
    })
}

fn valid_client_name(n: &[u8]) -> bool {
    n.iter().all(|b| (b'!'..=b'~').contains(b))
}

/// Largest `SELECT` index.
const MAX_DB_INDEX: u32 = 255;

/// Maps a `SELECT` argument to a KV namespace: `0` is `default`, `1..=255`
/// are `db1..db255`, and a valid namespace name is used as it is.
fn namespace_for(arg: &[u8]) -> Result<String, &'static str> {
    let s = std::str::from_utf8(arg).map_err(|_| "ERR invalid DB index")?;
    if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) {
        return match s.parse::<u32>() {
            Ok(0) => Ok("default".into()),
            Ok(n) if n <= MAX_DB_INDEX => Ok(format!("db{n}")),
            _ => Err("ERR DB index is out of range"),
        };
    }
    forgebase_kv::validate_namespace(s)
        .map(|()| s.to_owned())
        .map_err(|_: KvError| "ERR invalid DB index or namespace name")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn select_maps_indexes_and_names_to_namespaces() {
        assert_eq!(namespace_for(b"0").as_deref(), Ok("default"));
        assert_eq!(namespace_for(b"7").as_deref(), Ok("db7"));
        assert_eq!(namespace_for(b"255").as_deref(), Ok("db255"));
        assert!(namespace_for(b"256").is_err());
        assert!(namespace_for(b"99999999999999999999").is_err());
        assert_eq!(namespace_for(b"cache").as_deref(), Ok("cache"));
        assert_eq!(namespace_for(b"a.b-c:d").as_deref(), Ok("a.b-c:d"));
        assert!(namespace_for(b"").is_err());
        assert!(namespace_for(b"_hidden").is_err());
        assert!(namespace_for(b"has space").is_err());
        assert!(namespace_for(&[0xff, 0xfe]).is_err());
    }

    #[test]
    fn client_names_are_printable_without_spaces() {
        assert!(valid_client_name(b"my-app_1"));
        assert!(valid_client_name(b""));
        assert!(!valid_client_name(b"a b"));
        assert!(!valid_client_name(b"a\nb"));
    }
}

//! Cross-instance fan-out of channel messages and presence (optional).
//!
//! Record events need no fan-out: every instance tails the shared outbox of
//! the shared database. Channel messages and presence changes live only in
//! memory, so instances exchange them through a [`FanOut`]. One instance
//! needs none. [`RedisFanOut`] speaks the Redis/Valkey pub/sub protocol
//! (RESP2 `PUBLISH` / `SUBSCRIBE`) over plain TCP, without a client library.

use std::fmt;
use std::time::Duration;

use forgebase_data::db::BoxFuture;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpStream;
use tokio::sync::{Mutex, mpsc};

use crate::RealtimeError;

/// Moves opaque payloads (JSON text) between instances.
pub trait FanOut: Send + Sync + fmt::Debug {
    /// Publishes one payload to every instance (this one included).
    fn publish(&self, payload: String) -> BoxFuture<'_, Result<(), RealtimeError>>;

    /// Starts receiving. Every payload published by any instance arrives on
    /// the returned channel until it is dropped. Implementations reconnect by
    /// themselves; payloads published while disconnected may be lost.
    fn subscribe(&self) -> BoxFuture<'_, Result<mpsc::Receiver<String>, RealtimeError>>;
}

/// Largest payload accepted from the broker.
const MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// Redis/Valkey pub/sub fan-out over one channel.
pub struct RedisFanOut {
    addr: String,
    auth: Option<(Option<String>, String)>,
    channel: String,
    publisher: Mutex<Option<BufReader<TcpStream>>>,
}

impl fmt::Debug for RedisFanOut {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RedisFanOut")
            .field("addr", &self.addr)
            .field("channel", &self.channel)
            .field("auth", &self.auth.as_ref().map(|_| "<redacted>"))
            .finish_non_exhaustive()
    }
}

impl RedisFanOut {
    /// `url` is `redis://[[user]:password@]host[:port]` (TLS is not
    /// supported; use a local proxy); `channel` is the Redis channel shared by
    /// all instances of one deployment.
    pub fn new(url: &str, channel: impl Into<String>) -> Result<Self, RealtimeError> {
        let rest = url
            .strip_prefix("redis://")
            .ok_or_else(|| RealtimeError::invalid("invalid_redis_url", "expected redis://[[user]:password@]host[:port]"))?;
        let (auth, host) = match rest.rsplit_once('@') {
            Some((cred, host)) => {
                let (user, pass) = match cred.split_once(':') {
                    Some((u, p)) => ((!u.is_empty()).then(|| u.to_owned()), p.to_owned()),
                    None => (None, cred.to_owned()),
                };
                (Some((user, pass)), host)
            }
            None => (None, rest),
        };
        let host = host.trim_end_matches('/');
        if host.is_empty() {
            return Err(RealtimeError::invalid("invalid_redis_url", "missing host"));
        }
        let addr = if host.rsplit_once(':').is_some_and(|(_, p)| p.parse::<u16>().is_ok()) {
            host.to_owned()
        } else {
            format!("{host}:6379")
        };
        Ok(Self {
            addr,
            auth,
            channel: channel.into(),
            publisher: Mutex::new(None),
        })
    }

    async fn connect(&self) -> Result<BufReader<TcpStream>, RealtimeError> {
        let io = |e: std::io::Error| RealtimeError::FanOut(e.to_string());
        let s = tokio::time::timeout(Duration::from_secs(5), TcpStream::connect(&self.addr))
            .await
            .map_err(|_| RealtimeError::FanOut(format!("connect to {} timed out", self.addr)))?
            .map_err(io)?;
        s.set_nodelay(true).map_err(io)?;
        let mut conn = BufReader::new(s);
        if let Some((user, pass)) = &self.auth {
            let mut args = vec!["AUTH"];
            if let Some(u) = user {
                args.push(u);
            }
            args.push(pass);
            command(&mut conn, &args).await?;
            match read_frame(&mut conn).await? {
                Frame::Simple(_) => {}
                Frame::Error(e) => return Err(RealtimeError::FanOut(format!("AUTH failed: {e}"))),
                other => return Err(RealtimeError::FanOut(format!("unexpected AUTH reply {other:?}"))),
            }
        }
        Ok(conn)
    }

    async fn publish_once(&self, payload: &str) -> Result<(), RealtimeError> {
        let mut guard = self.publisher.lock().await;
        if guard.is_none() {
            *guard = Some(self.connect().await?);
        }
        let Some(conn) = guard.as_mut() else {
            return Err(RealtimeError::FanOut("not connected".into()));
        };
        let result = async {
            command(conn, &["PUBLISH", &self.channel, payload]).await?;
            match read_frame(conn).await? {
                Frame::Integer(_) => Ok(()),
                Frame::Error(e) => Err(RealtimeError::FanOut(e)),
                other => Err(RealtimeError::FanOut(format!("unexpected PUBLISH reply {other:?}"))),
            }
        }
        .await;
        if result.is_err() {
            *guard = None;
        }
        result
    }

    async fn subscriber_loop(&self, tx: mpsc::Sender<String>) {
        let mut backoff = Duration::from_millis(200);
        while !tx.is_closed() {
            match self.subscribe_once(&tx).await {
                Ok(()) => return,
                Err(e) => {
                    tracing::warn!(error = %e, addr = %self.addr, "redis fan-out subscriber disconnected; reconnecting");
                }
            }
            tokio::select! {
                () = tokio::time::sleep(backoff) => {}
                () = tx.closed() => return,
            }
            backoff = (backoff * 2).min(Duration::from_secs(30));
        }
    }

    async fn subscribe_once(&self, tx: &mpsc::Sender<String>) -> Result<(), RealtimeError> {
        let mut conn = self.connect().await?;
        command(&mut conn, &["SUBSCRIBE", &self.channel]).await?;
        loop {
            let frame = tokio::select! {
                f = read_frame(&mut conn) => f?,
                () = tx.closed() => return Ok(()),
            };
            if let Frame::Array(items) = frame
                && let [Frame::Bulk(Some(kind)), Frame::Bulk(Some(_chan)), Frame::Bulk(Some(payload))] = items.as_slice()
                && kind == b"message"
            {
                let text = String::from_utf8_lossy(payload).into_owned();
                if tx.send(text).await.is_err() {
                    return Ok(());
                }
            }
        }
    }
}

impl FanOut for RedisFanOut {
    fn publish(&self, payload: String) -> BoxFuture<'_, Result<(), RealtimeError>> {
        Box::pin(async move {
            match self.publish_once(&payload).await {
                Ok(()) => Ok(()),
                // One retry on a fresh connection (the old one may be stale).
                Err(_) => self.publish_once(&payload).await,
            }
        })
    }

    fn subscribe(&self) -> BoxFuture<'_, Result<mpsc::Receiver<String>, RealtimeError>> {
        Box::pin(async move {
            let (tx, rx) = mpsc::channel(1024);
            let me = Self {
                addr: self.addr.clone(),
                auth: self.auth.clone(),
                channel: self.channel.clone(),
                publisher: Mutex::new(None),
            };
            tokio::spawn(async move { me.subscriber_loop(tx).await });
            Ok(rx)
        })
    }
}

/// A RESP2 reply.
#[derive(Debug, PartialEq)]
enum Frame {
    Simple(String),
    Error(String),
    Integer(i64),
    Bulk(Option<Vec<u8>>),
    Array(Vec<Frame>),
}

async fn command(conn: &mut BufReader<TcpStream>, args: &[&str]) -> Result<(), RealtimeError> {
    let mut buf = format!("*{}\r\n", args.len()).into_bytes();
    for a in args {
        buf.extend_from_slice(format!("${}\r\n", a.len()).as_bytes());
        buf.extend_from_slice(a.as_bytes());
        buf.extend_from_slice(b"\r\n");
    }
    conn.get_mut()
        .write_all(&buf)
        .await
        .map_err(|e| RealtimeError::FanOut(e.to_string()))
}

async fn read_line(conn: &mut BufReader<TcpStream>) -> Result<String, RealtimeError> {
    let mut line = Vec::new();
    let n = (&mut *conn)
        .take(64 * 1024)
        .read_until(b'\n', &mut line)
        .await
        .map_err(|e| RealtimeError::FanOut(e.to_string()))?;
    if n == 0 {
        return Err(RealtimeError::FanOut("connection closed".into()));
    }
    if !line.ends_with(b"\r\n") {
        return Err(RealtimeError::FanOut("malformed reply line".into()));
    }
    line.truncate(line.len() - 2);
    String::from_utf8(line).map_err(|_| RealtimeError::FanOut("reply line is not UTF-8".into()))
}

async fn read_scalar(conn: &mut BufReader<TcpStream>, line: &str) -> Result<Frame, RealtimeError> {
    let bad = || RealtimeError::FanOut(format!("malformed reply {line:?}"));
    let (t, rest) = line.split_at_checked(1).ok_or_else(bad)?;
    Ok(match t {
        "+" => Frame::Simple(rest.to_owned()),
        "-" => Frame::Error(rest.to_owned()),
        ":" => Frame::Integer(rest.parse().map_err(|_| bad())?),
        "$" => {
            let len: i64 = rest.parse().map_err(|_| bad())?;
            if len < 0 {
                Frame::Bulk(None)
            } else {
                let len = usize::try_from(len).map_err(|_| bad())?;
                if len > MAX_PAYLOAD {
                    return Err(RealtimeError::FanOut("payload too large".into()));
                }
                let mut data = vec![0; len + 2];
                conn.read_exact(&mut data)
                    .await
                    .map_err(|e| RealtimeError::FanOut(e.to_string()))?;
                data.truncate(len);
                Frame::Bulk(Some(data))
            }
        }
        _ => return Err(bad()),
    })
}

/// Reads one reply; arrays may hold scalars only (all pub/sub needs).
async fn read_frame(conn: &mut BufReader<TcpStream>) -> Result<Frame, RealtimeError> {
    let line = read_line(conn).await?;
    if let Some(n) = line.strip_prefix('*') {
        let n: i64 = n
            .parse()
            .map_err(|_| RealtimeError::FanOut(format!("malformed reply {line:?}")))?;
        if !(0..=64).contains(&n) {
            return Err(RealtimeError::FanOut("unexpected array size".into()));
        }
        let mut items = Vec::with_capacity(n as usize);
        for _ in 0..n {
            let l = read_line(conn).await?;
            items.push(read_scalar(conn, &l).await?);
        }
        return Ok(Frame::Array(items));
    }
    read_scalar(conn, &line).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_parse() {
        let r = RedisFanOut::new("redis://localhost", "c").unwrap();
        assert_eq!(r.addr, "localhost:6379");
        assert!(r.auth.is_none());
        let r = RedisFanOut::new("redis://:secret@10.0.0.1:6380/", "c").unwrap();
        assert_eq!(r.addr, "10.0.0.1:6380");
        assert_eq!(r.auth, Some((None, "secret".into())));
        let r = RedisFanOut::new("redis://bob:pw@h:1", "c").unwrap();
        assert_eq!(r.auth, Some((Some("bob".into()), "pw".into())));
        assert!(!format!("{r:?}").contains("pw"), "debug output hides the password");
        assert!(RedisFanOut::new("rediss://h", "c").is_err());
        assert!(RedisFanOut::new("redis://", "c").is_err());
    }
}

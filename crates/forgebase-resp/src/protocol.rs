//! RESP wire format: a resumable request decoder and the reply encoder.
//!
//! Requests are arrays of bulk strings (`*2\r\n$3\r\nGET\r\n$1\r\nk\r\n`) or
//! inline lines (`GET k\r\n`, what `telnet` and `nc` send). Replies use RESP2
//! types, plus null, map, set and push types after `HELLO 3`.

use std::io::Write;

use forgebase_kv::Reply;

use crate::config::MAX_INLINE;

/// Protocol version a connection speaks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Proto {
    /// RESP2 (the default).
    V2,
    /// RESP3 (after `HELLO 3`).
    V3,
}

/// Size limits applied while decoding one command.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Limits {
    pub max_args: usize,
    pub max_bulk: usize,
    pub max_request: usize,
}

#[derive(Debug)]
struct Cur {
    remaining: usize,
    args: Vec<Vec<u8>>,
    total: usize,
    bulk: Option<usize>,
}

/// Incremental decoder. Bytes are appended to [`Decoder::buf`] (by the socket
/// read); [`Decoder::next`] yields complete commands and keeps partial state,
/// so a command arriving in many small reads is parsed once.
#[derive(Debug, Default)]
pub(crate) struct Decoder {
    pub(crate) buf: Vec<u8>,
    pos: usize,
    cur: Option<Cur>,
}

fn find_crlf(buf: &[u8], from: usize) -> Option<usize> {
    buf[from..].windows(2).position(|w| w == b"\r\n").map(|i| from + i)
}

fn parse_len(b: &[u8]) -> Option<i64> {
    std::str::from_utf8(b).ok()?.parse().ok()
}

impl Decoder {
    /// Drops consumed bytes. Call before reading more.
    pub(crate) fn compact(&mut self) {
        if self.pos >= self.buf.len() {
            self.buf.clear();
            self.pos = 0;
        } else if self.pos > 0 {
            self.buf.drain(..self.pos);
            self.pos = 0;
        }
    }

    /// Bytes received but not yet consumed.
    #[cfg(test)]
    pub(crate) fn pending(&self) -> usize {
        self.buf.len() - self.pos
    }

    /// The next complete command, `Ok(None)` when more bytes are needed, or
    /// the text of a protocol error (after which the connection must close).
    pub(crate) fn next(&mut self, lim: &Limits) -> Result<Option<Vec<Vec<u8>>>, String> {
        loop {
            let mut cur = match self.cur.take() {
                Some(c) => c,
                None => {
                    if self.pos >= self.buf.len() {
                        return Ok(None);
                    }
                    if self.buf[self.pos] != b'*' {
                        match self.inline(lim)? {
                            Some(args) if args.is_empty() => continue,
                            other => return Ok(other),
                        }
                    }
                    let Some(end) = find_crlf(&self.buf, self.pos) else {
                        if self.buf.len() - self.pos > 1024 {
                            return Err("too big mbulk count string".into());
                        }
                        return Ok(None);
                    };
                    let n = parse_len(&self.buf[self.pos + 1..end]).ok_or("invalid multibulk length")?;
                    self.pos = end + 2;
                    if n <= 0 {
                        continue; // an empty request is ignored
                    }
                    let n = usize::try_from(n).map_err(|_| "invalid multibulk length")?;
                    if n > lim.max_args {
                        return Err("invalid multibulk length".into());
                    }
                    Cur {
                        remaining: n,
                        args: Vec::with_capacity(n.min(1024)),
                        total: 0,
                        bulk: None,
                    }
                }
            };
            while cur.remaining > 0 {
                let len = match cur.bulk {
                    Some(len) => len,
                    None => {
                        if self.pos >= self.buf.len() {
                            self.cur = Some(cur);
                            return Ok(None);
                        }
                        if self.buf[self.pos] != b'$' {
                            return Err(format!("expected '$', got '{}'", char::from(self.buf[self.pos])));
                        }
                        let Some(end) = find_crlf(&self.buf, self.pos) else {
                            if self.buf.len() - self.pos > 1024 {
                                return Err("too big bulk count string".into());
                            }
                            self.cur = Some(cur);
                            return Ok(None);
                        };
                        let len = parse_len(&self.buf[self.pos + 1..end])
                            .and_then(|l| usize::try_from(l).ok())
                            .filter(|l| *l <= lim.max_bulk)
                            .ok_or("invalid bulk length")?;
                        if cur.total.saturating_add(len) > lim.max_request {
                            return Err("request too large".into());
                        }
                        self.pos = end + 2;
                        cur.bulk = Some(len);
                        len
                    }
                };
                if self.buf.len() - self.pos < len + 2 {
                    self.cur = Some(cur);
                    return Ok(None);
                }
                if &self.buf[self.pos + len..self.pos + len + 2] != b"\r\n" {
                    return Err("bulk string not terminated by CRLF".into());
                }
                cur.args.push(self.buf[self.pos..self.pos + len].to_vec());
                cur.total += len;
                self.pos += len + 2;
                cur.remaining -= 1;
                cur.bulk = None;
            }
            return Ok(Some(cur.args));
        }
    }

    fn inline(&mut self, lim: &Limits) -> Result<Option<Vec<Vec<u8>>>, String> {
        let Some(nl) = self.buf[self.pos..]
            .iter()
            .position(|b| *b == b'\n')
            .map(|i| self.pos + i)
        else {
            if self.buf.len() - self.pos > MAX_INLINE {
                return Err("too big inline request".into());
            }
            return Ok(None);
        };
        let mut line = &self.buf[self.pos..nl];
        if line.last() == Some(&b'\r') {
            line = &line[..line.len() - 1];
        }
        let args = split_inline(line).ok_or("unbalanced quotes in request")?;
        self.pos = nl + 1;
        if args.len() > lim.max_args || args.iter().any(|a| a.len() > lim.max_bulk) {
            return Err("invalid multibulk length".into());
        }
        Ok(Some(args))
    }
}

/// Splits an inline command line into arguments like Redis: whitespace
/// separated, `"double"` quotes with `\n \r \t \b \a \xHH \\ \"` escapes,
/// `'single'` quotes with `\'`.
pub(crate) fn split_inline(line: &[u8]) -> Option<Vec<Vec<u8>>> {
    let mut out = Vec::new();
    let mut i = 0;
    let n = line.len();
    while i < n {
        while i < n && line[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= n {
            break;
        }
        let mut tok = Vec::new();
        match line[i] {
            b'"' => {
                i += 1;
                loop {
                    let c = *line.get(i)?;
                    if c == b'\\' && i + 1 < n {
                        let e = line[i + 1];
                        if e == b'x' && i + 3 < n && line[i + 2].is_ascii_hexdigit() && line[i + 3].is_ascii_hexdigit()
                        {
                            let h = std::str::from_utf8(&line[i + 2..i + 4]).ok()?;
                            tok.push(u8::from_str_radix(h, 16).ok()?);
                            i += 4;
                            continue;
                        }
                        tok.push(match e {
                            b'n' => b'\n',
                            b'r' => b'\r',
                            b't' => b'\t',
                            b'b' => 8,
                            b'a' => 7,
                            other => other,
                        });
                        i += 2;
                    } else if c == b'"' {
                        i += 1;
                        if i < n && !line[i].is_ascii_whitespace() {
                            return None;
                        }
                        break;
                    } else {
                        tok.push(c);
                        i += 1;
                    }
                }
            }
            b'\'' => {
                i += 1;
                loop {
                    let c = *line.get(i)?;
                    if c == b'\\' && line.get(i + 1) == Some(&b'\'') {
                        tok.push(b'\'');
                        i += 2;
                    } else if c == b'\'' {
                        i += 1;
                        if i < n && !line[i].is_ascii_whitespace() {
                            return None;
                        }
                        break;
                    } else {
                        tok.push(c);
                        i += 1;
                    }
                }
            }
            _ => {
                while i < n && !line[i].is_ascii_whitespace() {
                    tok.push(line[i]);
                    i += 1;
                }
            }
        }
        out.push(tok);
    }
    Some(out)
}

// ---- encoder ---------------------------------------------------------------

fn clean(s: &str) -> String {
    s.replace(['\r', '\n'], " ")
}

/// `+status`.
pub(crate) fn put_status(out: &mut Vec<u8>, s: &str) {
    let _ = write!(out, "+{}\r\n", clean(s));
}

/// `-error` (`msg` starts with the error code, e.g. `ERR ...`).
pub(crate) fn put_error(out: &mut Vec<u8>, msg: &str) {
    let _ = write!(out, "-{}\r\n", clean(msg));
}

/// `:n`.
pub(crate) fn put_int(out: &mut Vec<u8>, n: i64) {
    let _ = write!(out, ":{n}\r\n");
}

/// `$len` bulk string.
pub(crate) fn put_bulk(out: &mut Vec<u8>, b: &[u8]) {
    let _ = write!(out, "${}\r\n", b.len());
    out.extend_from_slice(b);
    out.extend_from_slice(b"\r\n");
}

/// Null: `_` in RESP3, a null bulk string in RESP2.
pub(crate) fn put_null(out: &mut Vec<u8>, p: Proto) {
    out.extend_from_slice(if p == Proto::V3 { b"_\r\n" } else { b"$-1\r\n" });
}

/// Array header.
pub(crate) fn put_array(out: &mut Vec<u8>, n: usize) {
    let _ = write!(out, "*{n}\r\n");
}

/// Header of a map of `pairs` entries: `%` in RESP3, a flat array in RESP2.
pub(crate) fn put_map(out: &mut Vec<u8>, pairs: usize, p: Proto) {
    if p == Proto::V3 {
        let _ = write!(out, "%{pairs}\r\n");
    } else {
        put_array(out, pairs * 2);
    }
}

/// Header of a set of `n` members: `~` in RESP3, an array in RESP2.
pub(crate) fn put_set(out: &mut Vec<u8>, n: usize, p: Proto) {
    if p == Proto::V3 {
        let _ = write!(out, "~{n}\r\n");
    } else {
        put_array(out, n);
    }
}

/// Header of an out-of-band push of `n` elements: `>` in RESP3, an array in RESP2.
pub(crate) fn put_push(out: &mut Vec<u8>, n: usize, p: Proto) {
    if p == Proto::V3 {
        let _ = write!(out, ">{n}\r\n");
    } else {
        put_array(out, n);
    }
}

/// Encodes an engine reply.
pub(crate) fn put_reply(out: &mut Vec<u8>, r: &Reply, p: Proto) {
    match r {
        Reply::Nil => put_null(out, p),
        Reply::Status(s) => put_status(out, s),
        Reply::Int(n) => put_int(out, *n),
        Reply::Bulk(b) => put_bulk(out, b),
        Reply::Array(items) => {
            put_array(out, items.len());
            for i in items {
                put_reply(out, i, p);
            }
        }
    }
}

/// Encodes an array reply as a map (`HGETALL`) when the protocol has one.
pub(crate) fn put_reply_as_map(out: &mut Vec<u8>, r: &Reply, p: Proto) {
    match r {
        Reply::Array(items) if p == Proto::V3 && items.len() % 2 == 0 => {
            put_map(out, items.len() / 2, p);
            for i in items {
                put_reply(out, i, p);
            }
        }
        _ => put_reply(out, r, p),
    }
}

/// Encodes an array reply as a set (`SMEMBERS`) when the protocol has one.
pub(crate) fn put_reply_as_set(out: &mut Vec<u8>, r: &Reply, p: Proto) {
    match r {
        Reply::Array(items) if p == Proto::V3 => {
            put_set(out, items.len(), p);
            for i in items {
                put_reply(out, i, p);
            }
        }
        _ => put_reply(out, r, p),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    const LIM: Limits = Limits {
        max_args: 100,
        max_bulk: 1000,
        max_request: 5000,
    };

    fn feed(d: &mut Decoder, bytes: &[u8]) {
        d.buf.extend_from_slice(bytes);
    }

    fn cmd(args: &[&str]) -> Vec<Vec<u8>> {
        args.iter().map(|a| a.as_bytes().to_vec()).collect()
    }

    #[test]
    fn decodes_pipelined_commands_in_one_buffer() {
        let mut d = Decoder::default();
        feed(&mut d, b"*2\r\n$3\r\nGET\r\n$1\r\nk\r\n*1\r\n$4\r\nPING\r\n");
        assert_eq!(d.next(&LIM).unwrap(), Some(cmd(&["GET", "k"])));
        assert_eq!(d.next(&LIM).unwrap(), Some(cmd(&["PING"])));
        assert_eq!(d.next(&LIM).unwrap(), None);
    }

    #[test]
    fn decodes_a_command_split_at_every_byte() {
        let wire = b"*3\r\n$3\r\nSET\r\n$3\r\nkey\r\n$5\r\nva\r\nl\r\n";
        let mut d = Decoder::default();
        let mut got = None;
        for b in wire {
            assert!(got.is_none());
            feed(&mut d, &[*b]);
            got = d.next(&LIM).unwrap();
        }
        assert_eq!(
            got,
            Some(cmd(&["SET", "key", "va\r\nl"])),
            "binary safe, including CRLF in a value"
        );
        assert_eq!(d.pending(), 0);
    }

    #[test]
    fn empty_requests_are_skipped_and_inline_commands_work() {
        let mut d = Decoder::default();
        feed(&mut d, b"*0\r\n*-1\r\n\r\nPING hello\r\nSET \"a b\" 'c d'\r\n");
        assert_eq!(d.next(&LIM).unwrap(), Some(cmd(&["PING", "hello"])));
        assert_eq!(d.next(&LIM).unwrap(), Some(cmd(&["SET", "a b", "c d"])));
        assert_eq!(d.next(&LIM).unwrap(), None);
    }

    #[test]
    fn inline_quotes_and_escapes() {
        assert_eq!(
            split_inline(br#"set "a\x41\n" 'it\'s'"#).unwrap(),
            cmd(&["set", "aA\n", "it's"])
        );
        assert!(split_inline(b"set \"open").is_none());
        assert!(split_inline(br#"set "a"b"#).is_none(), "text glued to a closing quote");
    }

    #[test]
    fn limits_and_malformed_input_are_protocol_errors() {
        let mut d = Decoder::default();
        feed(&mut d, b"*101\r\n");
        assert!(d.next(&LIM).unwrap_err().contains("multibulk"));
        let mut d = Decoder::default();
        feed(&mut d, b"*1\r\n$1001\r\n");
        assert!(d.next(&LIM).unwrap_err().contains("bulk length"));
        let mut d = Decoder::default();
        feed(&mut d, b"*1\r\n$-5\r\n");
        assert!(d.next(&LIM).unwrap_err().contains("bulk length"));
        let mut d = Decoder::default();
        feed(&mut d, b"*2\r\n:1\r\n");
        assert!(d.next(&LIM).unwrap_err().contains("expected '$'"));
        let mut d = Decoder::default();
        feed(&mut d, b"*1\r\n$2\r\nabXY");
        assert!(d.next(&LIM).unwrap_err().contains("CRLF"));
        let mut d = Decoder::default();
        feed(&mut d, b"*x\r\n");
        assert!(d.next(&LIM).is_err());
        // total request size
        let tight = Limits { max_request: 10, ..LIM };
        let mut d = Decoder::default();
        feed(&mut d, b"*2\r\n$6\r\nabcdef\r\n$6\r\n");
        assert!(d.next(&tight).unwrap_err().contains("too large"));
        // endless header and endless inline line
        let mut d = Decoder::default();
        feed(&mut d, &vec![b'*'; 2000]);
        assert!(d.next(&LIM).is_err());
        let mut d = Decoder::default();
        feed(&mut d, &vec![b'a'; MAX_INLINE + 1]);
        assert!(d.next(&LIM).unwrap_err().contains("inline"));
    }

    #[test]
    fn compact_keeps_unread_bytes() {
        let mut d = Decoder::default();
        feed(&mut d, b"*1\r\n$4\r\nPING\r\n*1\r\n$4\r\nPI");
        assert!(d.next(&LIM).unwrap().is_some());
        assert_eq!(d.next(&LIM).unwrap(), None);
        d.compact();
        feed(&mut d, b"NG\r\n");
        assert_eq!(d.next(&LIM).unwrap(), Some(cmd(&["PING"])));
    }

    #[test]
    fn encodes_resp2_and_resp3() {
        let mut o = Vec::new();
        put_reply(
            &mut o,
            &Reply::Array(vec![Reply::Nil, Reply::Int(-3), Reply::bulk("hi"), Reply::ok()]),
            Proto::V2,
        );
        assert_eq!(o, b"*4\r\n$-1\r\n:-3\r\n$2\r\nhi\r\n+OK\r\n");
        let mut o = Vec::new();
        put_reply(&mut o, &Reply::Nil, Proto::V3);
        put_reply_as_map(
            &mut o,
            &Reply::Array(vec![Reply::bulk("a"), Reply::bulk("1")]),
            Proto::V3,
        );
        put_reply_as_set(&mut o, &Reply::Array(vec![Reply::bulk("a")]), Proto::V3);
        put_push(&mut o, 2, Proto::V3);
        assert_eq!(o, b"_\r\n%1\r\n$1\r\na\r\n$1\r\n1\r\n~1\r\n$1\r\na\r\n>2\r\n");
        let mut o = Vec::new();
        put_reply_as_map(
            &mut o,
            &Reply::Array(vec![Reply::bulk("a"), Reply::bulk("1")]),
            Proto::V2,
        );
        assert_eq!(o, b"*2\r\n$1\r\na\r\n$1\r\n1\r\n");
        let mut o = Vec::new();
        put_error(&mut o, "ERR a\r\nb");
        assert_eq!(o, b"-ERR a  b\r\n", "line breaks cannot smuggle a second reply");
    }
}

//! PostgreSQL's `COPY` text format: rows of tab-separated fields, `\N` for
//! NULL, backslash escapes for the special bytes.

use forgebase_db::Value;

use crate::canon::{Kind, bytes_of, float_of, int_of, json_text_of, real_text, text_of};

/// Appends one field for `COPY … FROM STDIN` (text format) to `out`.
///
/// # Errors
/// A value that cannot go into a column of `kind`.
pub(crate) fn encode_field(out: &mut Vec<u8>, kind: Kind, v: &Value) -> Result<(), String> {
    if v.is_null() {
        out.extend_from_slice(b"\\N");
        return Ok(());
    }
    match kind {
        Kind::Int | Kind::Bool => out.extend_from_slice(int_of(v)?.to_string().as_bytes()),
        Kind::Float => out.extend_from_slice(real_text(float_of(v)?).as_bytes()),
        Kind::Text | Kind::Other => escape(out, text_of(v)?.as_bytes()),
        Kind::Json => escape(out, json_text_of(v)?.as_bytes()),
        Kind::Bytes => {
            // bytea hex input `\x…`, its backslash escaped for COPY.
            out.extend_from_slice(b"\\\\x");
            for b in bytes_of(v)?.iter() {
                out.extend_from_slice(&HEX[usize::from(*b >> 4)..=usize::from(*b >> 4)]);
                out.extend_from_slice(&HEX[usize::from(*b & 15)..=usize::from(*b & 15)]);
            }
        }
    }
    Ok(())
}

const HEX: &[u8; 16] = b"0123456789abcdef";

fn escape(out: &mut Vec<u8>, bytes: &[u8]) {
    for &b in bytes {
        match b {
            b'\\' => out.extend_from_slice(b"\\\\"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            _ => out.push(b),
        }
    }
}

/// Splits `COPY … TO STDOUT` output (text format) into rows, across chunk
/// boundaries.
#[derive(Debug, Default)]
pub(crate) struct RowReader {
    pending: Vec<u8>,
}

impl RowReader {
    /// Feeds a chunk; calls `row` for every complete row.
    pub(crate) fn feed<E>(
        &mut self,
        chunk: &[u8],
        mut row: impl FnMut(Vec<Option<Vec<u8>>>) -> Result<(), E>,
    ) -> Result<(), E> {
        self.pending.extend_from_slice(chunk);
        let mut start = 0;
        while let Some(pos) = self.pending[start..].iter().position(|&b| b == b'\n') {
            let line = &self.pending[start..start + pos];
            row(split_fields(line))?;
            start += pos + 1;
        }
        self.pending.drain(..start);
        Ok(())
    }

    /// Whether a partial row is left over (a truncated stream).
    pub(crate) fn has_partial(&self) -> bool {
        !self.pending.is_empty()
    }
}

fn split_fields(line: &[u8]) -> Vec<Option<Vec<u8>>> {
    line.split(|&b| b == b'\t')
        .map(|f| if f == b"\\N" { None } else { Some(unescape(f)) })
        .collect()
}

fn unescape(f: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(f.len());
    let mut i = 0;
    while i < f.len() {
        let b = f[i];
        if b != b'\\' || i + 1 >= f.len() {
            out.push(b);
            i += 1;
            continue;
        }
        let c = f[i + 1];
        i += 2;
        match c {
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'0'..=b'7' => {
                let mut v = u32::from(c - b'0');
                let mut n = 1;
                while n < 3 && i < f.len() && (b'0'..=b'7').contains(&f[i]) {
                    v = v * 8 + u32::from(f[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push((v & 0xff) as u8);
            }
            b'x' if i < f.len() && f[i].is_ascii_hexdigit() => {
                let mut v = 0u32;
                let mut n = 0;
                while n < 2 && i < f.len() && f[i].is_ascii_hexdigit() {
                    v = v * 16 + u32::from(hexval(f[i]));
                    i += 1;
                    n += 1;
                }
                out.push((v & 0xff) as u8);
            }
            other => out.push(other),
        }
    }
    out
}

fn hexval(b: u8) -> u8 {
    match b {
        b'0'..=b'9' => b - b'0',
        b'a'..=b'f' => b - b'a' + 10,
        b'A'..=b'F' => b - b'A' + 10,
        _ => 0,
    }
}

/// A field of `COPY … TO STDOUT` output as the value SQLite stores for a
/// column of `kind` (`jsonb` text compacted, `bytea` hex decoded, booleans
/// as 0/1).
///
/// # Errors
/// Text that does not read as the column's kind.
pub(crate) fn decode_field(kind: Kind, field: Option<Vec<u8>>) -> Result<Value, String> {
    let Some(bytes) = field else {
        return Ok(Value::Null);
    };
    let text = String::from_utf8(bytes).map_err(|_| "the server sent text that is not UTF-8".to_owned())?;
    Ok(match kind {
        Kind::Int => Value::Integer(text.parse().map_err(|_| format!("{text:?} is not an integer"))?),
        Kind::Bool => Value::Integer(match text.as_str() {
            "t" | "true" | "1" => 1,
            "f" | "false" | "0" => 0,
            other => other.parse().map_err(|_| format!("{text:?} is not a boolean"))?,
        }),
        Kind::Float => Value::Real(float_of(&Value::Text(text))?),
        Kind::Text | Kind::Other => Value::Text(text),
        Kind::Json => Value::Text(crate::canon::compact_json(&text)),
        Kind::Bytes => {
            let hex = text
                .strip_prefix("\\x")
                .ok_or_else(|| "bytea is not in hex form".to_owned())?;
            if hex.len() % 2 != 0 {
                return Err("odd bytea hex".into());
            }
            let b = hex.as_bytes();
            let mut out = Vec::with_capacity(b.len() / 2);
            for pair in b.chunks(2) {
                if !pair.iter().all(u8::is_ascii_hexdigit) {
                    return Err("bad bytea hex".into());
                }
                out.push(hexval(pair[0]) * 16 + hexval(pair[1]));
            }
            Value::Blob(out)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fields_round_trip_through_the_text_format() {
        let mut line = Vec::new();
        encode_field(&mut line, Kind::Text, &Value::Text("a\tb\nc\\d\re".into())).unwrap();
        line.push(b'\t');
        encode_field(&mut line, Kind::Bytes, &Value::Blob(vec![0, 0x5c, 0xff])).unwrap();
        line.push(b'\t');
        encode_field(&mut line, Kind::Int, &Value::Null).unwrap();
        line.push(b'\n');
        let mut rows = Vec::new();
        let mut r = RowReader::default();
        // Split mid-row to cover chunk boundaries.
        let (a, b) = line.split_at(5);
        r.feed(a, |row| {
            rows.push(row);
            Ok::<_, ()>(())
        })
        .unwrap();
        r.feed(b, |row| {
            rows.push(row);
            Ok::<_, ()>(())
        })
        .unwrap();
        assert!(!r.has_partial());
        assert_eq!(rows.len(), 1);
        let row = rows.pop().unwrap();
        assert_eq!(
            decode_field(Kind::Text, row[0].clone()).unwrap(),
            Value::Text("a\tb\nc\\d\re".into())
        );
        // COPY input `\\x005cff` arrives at the server as `\x005cff`, which is
        // what the server sends back for that bytea.
        assert_eq!(row[1].as_deref(), Some(&b"\\x005cff"[..]));
        assert_eq!(
            decode_field(Kind::Bytes, row[1].clone()).unwrap(),
            Value::Blob(vec![0, 0x5c, 0xff])
        );
        assert_eq!(row[2], None);
    }

    #[test]
    fn server_escapes_decode() {
        assert_eq!(unescape(br"\101\x41\b\f\v"), b"AA\x08\x0c\x0b".to_vec());
        assert_eq!(
            decode_field(Kind::Json, Some(br#"{"a": [1, 2]}"#.to_vec())).unwrap(),
            Value::Text(r#"{"a":[1,2]}"#.into())
        );
        assert_eq!(
            decode_field(Kind::Bool, Some(b"t".to_vec())).unwrap(),
            Value::Integer(1)
        );
        assert_eq!(
            decode_field(Kind::Float, Some(b"-Infinity".to_vec())).unwrap(),
            Value::Real(f64::NEG_INFINITY)
        );
    }
}

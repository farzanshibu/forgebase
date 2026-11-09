//! A small, strict CBOR (RFC 8949) reader: just what WebAuthn needs
//! (attestation objects and COSE keys). Definite lengths only, bounded depth
//! and size, no tags, no floats.

/// A decoded CBOR item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Cbor {
    /// Major type 0 and 1 (as i128 so the full range fits).
    Int(i128),
    /// Byte string.
    Bytes(Vec<u8>),
    /// Text string.
    Text(String),
    /// Array.
    Array(Vec<Cbor>),
    /// Map (order kept).
    Map(Vec<(Cbor, Cbor)>),
    /// `true` / `false`.
    Bool(bool),
    /// `null`.
    Null,
}

const MAX_DEPTH: usize = 8;
const MAX_ITEMS: usize = 256;

impl Cbor {
    /// Decodes one item; returns it and the number of bytes consumed.
    pub fn decode(input: &[u8]) -> Option<(Cbor, usize)> {
        let mut r = Reader { b: input, pos: 0 };
        let v = r.item(0)?;
        Some((v, r.pos))
    }

    /// Map lookup by integer key.
    pub fn get_int(&self, key: i128) -> Option<&Cbor> {
        match self {
            Cbor::Map(m) => m.iter().find(|(k, _)| *k == Cbor::Int(key)).map(|(_, v)| v),
            _ => None,
        }
    }

    /// Map lookup by text key.
    pub fn get_text(&self, key: &str) -> Option<&Cbor> {
        match self {
            Cbor::Map(m) => m
                .iter()
                .find(|(k, _)| matches!(k, Cbor::Text(t) if t == key))
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// The integer, if this is one.
    pub fn as_int(&self) -> Option<i128> {
        match self {
            Cbor::Int(i) => Some(*i),
            _ => None,
        }
    }

    /// The bytes, if this is a byte string.
    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Cbor::Bytes(b) => Some(b),
            _ => None,
        }
    }

    /// The text, if this is a text string.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Cbor::Text(t) => Some(t),
            _ => None,
        }
    }
}

struct Reader<'a> {
    b: &'a [u8],
    pos: usize,
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Option<&[u8]> {
        let end = self.pos.checked_add(n)?;
        let s = self.b.get(self.pos..end)?;
        self.pos = end;
        Some(s)
    }

    fn arg(&mut self, info: u8) -> Option<u64> {
        Some(match info {
            0..=23 => u64::from(info),
            24 => u64::from(*self.take(1)?.first()?),
            25 => u64::from(u16::from_be_bytes(self.take(2)?.try_into().ok()?)),
            26 => u64::from(u32::from_be_bytes(self.take(4)?.try_into().ok()?)),
            27 => u64::from_be_bytes(self.take(8)?.try_into().ok()?),
            _ => return None, // indefinite lengths and reserved values
        })
    }

    fn item(&mut self, depth: usize) -> Option<Cbor> {
        if depth > MAX_DEPTH {
            return None;
        }
        let head = *self.take(1)?.first()?;
        let (major, info) = (head >> 5, head & 31);
        if major == 7 {
            return match info {
                20 => Some(Cbor::Bool(false)),
                21 => Some(Cbor::Bool(true)),
                22 => Some(Cbor::Null),
                _ => None,
            };
        }
        let n = self.arg(info)?;
        match major {
            0 => Some(Cbor::Int(i128::from(n))),
            1 => Some(Cbor::Int(-1 - i128::from(n))),
            2 => Some(Cbor::Bytes(self.take(usize::try_from(n).ok()?)?.to_vec())),
            3 => Some(Cbor::Text(
                String::from_utf8(self.take(usize::try_from(n).ok()?)?.to_vec()).ok()?,
            )),
            4 => {
                let n = usize::try_from(n).ok().filter(|n| *n <= MAX_ITEMS)?;
                (0..n)
                    .map(|_| self.item(depth + 1))
                    .collect::<Option<Vec<_>>>()
                    .map(Cbor::Array)
            }
            5 => {
                let n = usize::try_from(n).ok().filter(|n| *n <= MAX_ITEMS)?;
                (0..n)
                    .map(|_| Some((self.item(depth + 1)?, self.item(depth + 1)?)))
                    .collect::<Option<Vec<_>>>()
                    .map(Cbor::Map)
            }
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_scalars_maps_and_rejects_junk() {
        // {1: 2, -1: h'0a0b', "k": [true, null]}
        let b = [0xa3, 0x01, 0x02, 0x20, 0x42, 0x0a, 0x0b, 0x61, b'k', 0x82, 0xf5, 0xf6];
        let (v, n) = Cbor::decode(&b).unwrap();
        assert_eq!(n, b.len());
        assert_eq!(v.get_int(1).unwrap().as_int(), Some(2));
        assert_eq!(v.get_int(-1).unwrap().as_bytes(), Some(&[0x0a, 0x0b][..]));
        assert_eq!(v.get_text("k"), Some(&Cbor::Array(vec![Cbor::Bool(true), Cbor::Null])));
        assert!(Cbor::decode(&[0x5f]).is_none(), "indefinite length");
        assert!(Cbor::decode(&[0x42, 0x01]).is_none(), "truncated");
        assert!(Cbor::decode(&[0xf9, 0, 0]).is_none(), "float");
        let deep = vec![0x81; 20];
        assert!(Cbor::decode(&deep).is_none(), "too deep");
    }
}

//! The BER subset LDAPv3 uses (RFC 4511 §5.1): definite lengths, low tag
//! numbers, INTEGER, ENUMERATED, BOOLEAN, OCTET STRING, SEQUENCE, SET and
//! context/application tags. Public so tests can build a mock server.

/// Universal BOOLEAN.
pub const BOOLEAN: u8 = 0x01;
/// Universal INTEGER.
pub const INTEGER: u8 = 0x02;
/// Universal OCTET STRING.
pub const OCTET_STRING: u8 = 0x04;
/// Universal ENUMERATED.
pub const ENUMERATED: u8 = 0x0a;
/// Universal SEQUENCE (constructed).
pub const SEQUENCE: u8 = 0x30;
/// Universal SET (constructed).
pub const SET: u8 = 0x31;

/// Largest message accepted (1 MiB).
pub const MAX_MESSAGE: usize = 1 << 20;

/// A malformed or oversized encoding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BerError;

/// Encodes a TLV.
pub fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(content.len() + 6);
    out.push(tag);
    let n = content.len();
    if n < 0x80 {
        out.push(n as u8);
    } else {
        let bytes = (n as u64).to_be_bytes();
        let skip = bytes.iter().take_while(|b| **b == 0).count();
        out.push(0x80 | (8 - skip) as u8);
        out.extend_from_slice(&bytes[skip..]);
    }
    out.extend_from_slice(content);
    out
}

/// Encodes an INTEGER-like value (minimal two's complement) under `tag`.
pub fn int_with(tag: u8, v: i64) -> Vec<u8> {
    let bytes = v.to_be_bytes();
    let mut start = 0;
    while start < 7 {
        let (b, next) = (bytes[start], bytes[start + 1]);
        if (b == 0x00 && next & 0x80 == 0) || (b == 0xff && next & 0x80 != 0) {
            start += 1;
        } else {
            break;
        }
    }
    tlv(tag, &bytes[start..])
}

/// INTEGER.
pub fn integer(v: i64) -> Vec<u8> {
    int_with(INTEGER, v)
}

/// ENUMERATED.
pub fn enumerated(v: i64) -> Vec<u8> {
    int_with(ENUMERATED, v)
}

/// OCTET STRING.
pub fn octets(v: &[u8]) -> Vec<u8> {
    tlv(OCTET_STRING, v)
}

/// BOOLEAN.
pub fn boolean(v: bool) -> Vec<u8> {
    tlv(BOOLEAN, &[if v { 0xff } else { 0x00 }])
}

/// Concatenation of encoded parts, wrapped in `tag`.
pub fn constructed(tag: u8, parts: &[Vec<u8>]) -> Vec<u8> {
    tlv(tag, &parts.concat())
}

/// Total length of the first TLV in `b`, when its header is complete.
/// `Ok(None)`: more bytes needed.
pub fn frame_len(b: &[u8]) -> Result<Option<usize>, BerError> {
    if b.len() < 2 {
        return Ok(None);
    }
    if b[0] & 0x1f == 0x1f {
        return Err(BerError); // high tag numbers are not used by LDAP
    }
    let first = b[1];
    let (len, header) = if first < 0x80 {
        (first as usize, 2)
    } else {
        let n = (first & 0x7f) as usize;
        if n == 0 || n > 4 {
            return Err(BerError); // indefinite or absurd lengths
        }
        if b.len() < 2 + n {
            return Ok(None);
        }
        let len = b[2..2 + n].iter().fold(0usize, |acc, x| (acc << 8) | *x as usize);
        (len, 2 + n)
    };
    if len > MAX_MESSAGE {
        return Err(BerError);
    }
    Ok(Some(header + len))
}

/// Sequential reader over the contents of a constructed value.
#[derive(Debug, Clone, Copy)]
pub struct Reader<'a> {
    b: &'a [u8],
}

impl<'a> Reader<'a> {
    /// Reader over `b`.
    pub fn new(b: &'a [u8]) -> Self {
        Self { b }
    }

    /// Whether everything was read.
    pub fn is_empty(&self) -> bool {
        self.b.is_empty()
    }

    /// Tag of the next value.
    pub fn peek_tag(&self) -> Option<u8> {
        self.b.first().copied()
    }

    /// Next TLV: tag and contents.
    pub fn next(&mut self) -> Result<(u8, &'a [u8]), BerError> {
        let total = frame_len(self.b)?.ok_or(BerError)?;
        if total > self.b.len() {
            return Err(BerError);
        }
        let header = if self.b[1] < 0x80 {
            2
        } else {
            2 + (self.b[1] & 0x7f) as usize
        };
        let (tag, content) = (self.b[0], &self.b[header..total]);
        self.b = &self.b[total..];
        Ok((tag, content))
    }

    /// Next value, which must carry `tag`.
    pub fn expect(&mut self, tag: u8) -> Result<&'a [u8], BerError> {
        match self.next()? {
            (t, c) if t == tag => Ok(c),
            _ => Err(BerError),
        }
    }

    /// Next INTEGER-like value under `tag`.
    pub fn int_with(&mut self, tag: u8) -> Result<i64, BerError> {
        let c = self.expect(tag)?;
        if c.is_empty() || c.len() > 8 {
            return Err(BerError);
        }
        let neg = c[0] & 0x80 != 0;
        Ok(c.iter().fold(if neg { -1i64 } else { 0 }, |acc, x| (acc << 8) | *x as i64))
    }

    /// Next INTEGER.
    pub fn integer(&mut self) -> Result<i64, BerError> {
        self.int_with(INTEGER)
    }

    /// Next ENUMERATED.
    pub fn enumerated(&mut self) -> Result<i64, BerError> {
        self.int_with(ENUMERATED)
    }

    /// Next OCTET STRING.
    pub fn octets(&mut self) -> Result<&'a [u8], BerError> {
        self.expect(OCTET_STRING)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn integers_round_trip_minimally() {
        for (v, enc) in [
            (0i64, vec![0x02, 0x01, 0x00]),
            (127, vec![0x02, 0x01, 0x7f]),
            (128, vec![0x02, 0x02, 0x00, 0x80]),
            (-1, vec![0x02, 0x01, 0xff]),
            (-129, vec![0x02, 0x02, 0xff, 0x7f]),
            (65_536, vec![0x02, 0x03, 0x01, 0x00, 0x00]),
        ] {
            assert_eq!(integer(v), enc, "{v}");
            assert_eq!(Reader::new(&enc).integer().unwrap(), v);
        }
    }

    #[test]
    fn long_lengths_and_framing() {
        let big = octets(&[7u8; 300]);
        assert_eq!(&big[..4], &[0x04, 0x82, 0x01, 0x2c]);
        assert_eq!(frame_len(&big).unwrap(), Some(304));
        assert_eq!(frame_len(&big[..3]).unwrap(), None);
        assert!(frame_len(&[0x30, 0x80]).is_err(), "indefinite length");
        assert!(frame_len(&[0x30, 0x84, 0x7f, 0xff, 0xff, 0xff]).is_err(), "too large");
        let mut r = Reader::new(&big);
        assert_eq!(r.octets().unwrap().len(), 300);
        assert!(r.is_empty());
        assert!(Reader::new(&[0x04, 0x05, 0x01]).next().is_err(), "truncated");
    }
}

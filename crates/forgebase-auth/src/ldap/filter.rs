//! LDAP search filters: the RFC 4515 string form, its BER encoding
//! (RFC 4511 §4.5.1) and decoding, and evaluation against an entry (used by
//! test directories).
//!
//! User input never reaches the parser unescaped: [`escape`] turns `*`, `(`,
//! `)`, `\` and NUL into `\2a`, `\28`, `\29`, `\5c`, `\00` before a username
//! is substituted into the configured filter template.

use std::collections::HashMap;

use super::ber::{self, BerError, Reader};

/// A search filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Filter {
    /// `(&...)`
    And(Vec<Filter>),
    /// `(|...)`
    Or(Vec<Filter>),
    /// `(!...)`
    Not(Box<Filter>),
    /// `(attr=value)`
    Equal(String, Vec<u8>),
    /// `(attr=ini*any*fin)`
    Substrings {
        /// Attribute.
        attr: String,
        /// Leading part.
        initial: Option<Vec<u8>>,
        /// Middle parts.
        any: Vec<Vec<u8>>,
        /// Trailing part.
        last: Option<Vec<u8>>,
    },
    /// `(attr>=value)`
    GreaterOrEqual(String, Vec<u8>),
    /// `(attr<=value)`
    LessOrEqual(String, Vec<u8>),
    /// `(attr=*)`
    Present(String),
    /// `(attr~=value)`
    Approx(String, Vec<u8>),
}

/// RFC 4515 escaping of an assertion value.
pub fn escape(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for c in v.chars() {
        match c {
            '*' => out.push_str("\\2a"),
            '(' => out.push_str("\\28"),
            ')' => out.push_str("\\29"),
            '\\' => out.push_str("\\5c"),
            '\0' => out.push_str("\\00"),
            c => out.push(c),
        }
    }
    out
}

/// A filter that does not parse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilterError(pub String);

impl std::fmt::Display for FilterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid LDAP filter: {}", self.0)
    }
}

struct Parser<'a> {
    s: &'a [u8],
    pos: usize,
    depth: usize,
}

impl Parser<'_> {
    fn err(&self, m: &str) -> FilterError {
        FilterError(format!("{m} at offset {}", self.pos))
    }

    fn eat(&mut self, c: u8) -> Result<(), FilterError> {
        if self.s.get(self.pos) == Some(&c) {
            self.pos += 1;
            Ok(())
        } else {
            Err(self.err(&format!("expected {:?}", c as char)))
        }
    }

    fn filter(&mut self) -> Result<Filter, FilterError> {
        self.depth += 1;
        if self.depth > 32 {
            return Err(self.err("nested too deeply"));
        }
        self.eat(b'(')?;
        let f = match self.s.get(self.pos) {
            Some(b'&') => {
                self.pos += 1;
                Filter::And(self.list()?)
            }
            Some(b'|') => {
                self.pos += 1;
                Filter::Or(self.list()?)
            }
            Some(b'!') => {
                self.pos += 1;
                Filter::Not(Box::new(self.filter()?))
            }
            _ => self.item()?,
        };
        self.eat(b')')?;
        self.depth -= 1;
        Ok(f)
    }

    fn list(&mut self) -> Result<Vec<Filter>, FilterError> {
        let mut v = Vec::new();
        while self.s.get(self.pos) == Some(&b'(') {
            v.push(self.filter()?);
        }
        if v.is_empty() {
            return Err(self.err("empty filter list"));
        }
        Ok(v)
    }

    fn item(&mut self) -> Result<Filter, FilterError> {
        let start = self.pos;
        while self
            .s
            .get(self.pos)
            .is_some_and(|c| c.is_ascii_alphanumeric() || matches!(c, b'-' | b'.' | b';'))
        {
            self.pos += 1;
        }
        let attr = std::str::from_utf8(&self.s[start..self.pos])
            .map_err(|_| self.err("bad attribute"))?
            .to_owned();
        if attr.is_empty() {
            return Err(self.err("missing attribute"));
        }
        let op = match (self.s.get(self.pos), self.s.get(self.pos + 1)) {
            (Some(b'='), _) => {
                self.pos += 1;
                b'='
            }
            (Some(c @ (b'~' | b'>' | b'<')), Some(b'=')) => {
                self.pos += 2;
                *c
            }
            _ => return Err(self.err("expected =, ~=, >= or <=")),
        };
        // Value up to ')', split at unescaped '*'.
        let mut parts: Vec<Vec<u8>> = vec![Vec::new()];
        while let Some(&c) = self.s.get(self.pos) {
            match c {
                b')' => break,
                b'(' => return Err(self.err("unescaped '(' in value")),
                b'*' => {
                    parts.push(Vec::new());
                    self.pos += 1;
                }
                b'\\' => {
                    let hex = self
                        .s
                        .get(self.pos + 1..self.pos + 3)
                        .and_then(|h| std::str::from_utf8(h).ok())
                        .and_then(|h| u8::from_str_radix(h, 16).ok())
                        .ok_or_else(|| self.err("bad escape"))?;
                    if let Some(p) = parts.last_mut() {
                        p.push(hex);
                    }
                    self.pos += 3;
                }
                c => {
                    if let Some(p) = parts.last_mut() {
                        p.push(c);
                    }
                    self.pos += 1;
                }
            }
        }
        if parts.len() == 1 {
            let v = parts.pop().unwrap_or_default();
            return Ok(match op {
                b'=' => Filter::Equal(attr, v),
                b'~' => Filter::Approx(attr, v),
                b'>' => Filter::GreaterOrEqual(attr, v),
                _ => Filter::LessOrEqual(attr, v),
            });
        }
        if op != b'=' {
            return Err(self.err("wildcards only work with ="));
        }
        if parts.len() == 2 && parts.iter().all(Vec::is_empty) {
            return Ok(Filter::Present(attr));
        }
        let last = parts.pop().filter(|p| !p.is_empty());
        let initial = Some(parts.remove(0)).filter(|p| !p.is_empty());
        if parts.iter().any(Vec::is_empty) {
            return Err(self.err("empty substring"));
        }
        Ok(Filter::Substrings {
            attr,
            initial,
            any: parts,
            last,
        })
    }
}

impl Filter {
    /// Parses the RFC 4515 string form.
    pub fn parse(s: &str) -> Result<Self, FilterError> {
        let mut p = Parser {
            s: s.trim().as_bytes(),
            pos: 0,
            depth: 0,
        };
        let f = p.filter()?;
        if p.pos != p.s.len() {
            return Err(p.err("trailing characters"));
        }
        Ok(f)
    }

    /// BER encoding.
    pub fn encode(&self) -> Vec<u8> {
        let ava = |tag: u8, a: &str, v: &[u8]| ber::constructed(tag, &[ber::octets(a.as_bytes()), ber::octets(v)]);
        match self {
            Filter::And(v) => ber::constructed(0xa0, &v.iter().map(Filter::encode).collect::<Vec<_>>()),
            Filter::Or(v) => ber::constructed(0xa1, &v.iter().map(Filter::encode).collect::<Vec<_>>()),
            Filter::Not(f) => ber::constructed(0xa2, &[f.encode()]),
            Filter::Equal(a, v) => ava(0xa3, a, v),
            Filter::Substrings {
                attr,
                initial,
                any,
                last,
            } => {
                let mut subs = Vec::new();
                if let Some(i) = initial {
                    subs.push(ber::tlv(0x80, i));
                }
                subs.extend(any.iter().map(|a| ber::tlv(0x81, a)));
                if let Some(l) = last {
                    subs.push(ber::tlv(0x82, l));
                }
                ber::constructed(
                    0xa4,
                    &[ber::octets(attr.as_bytes()), ber::constructed(ber::SEQUENCE, &subs)],
                )
            }
            Filter::GreaterOrEqual(a, v) => ava(0xa5, a, v),
            Filter::LessOrEqual(a, v) => ava(0xa6, a, v),
            Filter::Present(a) => ber::tlv(0x87, a.as_bytes()),
            Filter::Approx(a, v) => ava(0xa8, a, v),
        }
    }

    /// Decodes one BER filter.
    pub fn decode(tag: u8, content: &[u8]) -> Result<Self, BerError> {
        let s = |b: &[u8]| String::from_utf8(b.to_vec()).map_err(|_| BerError);
        let list = |c: &[u8]| -> Result<Vec<Filter>, BerError> {
            let mut r = Reader::new(c);
            let mut v = Vec::new();
            while !r.is_empty() {
                let (t, c) = r.read_tlv()?;
                v.push(Filter::decode(t, c)?);
            }
            Ok(v)
        };
        let ava = |c: &[u8]| -> Result<(String, Vec<u8>), BerError> {
            let mut r = Reader::new(c);
            Ok((s(r.octets()?)?, r.octets()?.to_vec()))
        };
        Ok(match tag {
            0xa0 => Filter::And(list(content)?),
            0xa1 => Filter::Or(list(content)?),
            0xa2 => {
                let (t, c) = Reader::new(content).read_tlv()?;
                Filter::Not(Box::new(Filter::decode(t, c)?))
            }
            0xa3 => {
                let (a, v) = ava(content)?;
                Filter::Equal(a, v)
            }
            0xa4 => {
                let mut r = Reader::new(content);
                let attr = s(r.octets()?)?;
                let mut subs = Reader::new(r.expect(ber::SEQUENCE)?);
                let (mut initial, mut any, mut last) = (None, Vec::new(), None);
                while !subs.is_empty() {
                    match subs.read_tlv()? {
                        (0x80, v) => initial = Some(v.to_vec()),
                        (0x81, v) => any.push(v.to_vec()),
                        (0x82, v) => last = Some(v.to_vec()),
                        _ => return Err(BerError),
                    }
                }
                Filter::Substrings {
                    attr,
                    initial,
                    any,
                    last,
                }
            }
            0xa5 => {
                let (a, v) = ava(content)?;
                Filter::GreaterOrEqual(a, v)
            }
            0xa6 => {
                let (a, v) = ava(content)?;
                Filter::LessOrEqual(a, v)
            }
            0x87 => Filter::Present(s(content)?),
            0xa8 => {
                let (a, v) = ava(content)?;
                Filter::Approx(a, v)
            }
            _ => return Err(BerError),
        })
    }

    /// Evaluates the filter against an entry (attribute names lower-cased;
    /// values compared case-insensitively, as most user attributes are).
    pub fn matches(&self, entry: &HashMap<String, Vec<Vec<u8>>>) -> bool {
        let vals = |a: &str| entry.get(&a.to_ascii_lowercase()).map(Vec::as_slice).unwrap_or(&[]);
        let lc = |v: &[u8]| v.to_ascii_lowercase();
        match self {
            Filter::And(v) => v.iter().all(|f| f.matches(entry)),
            Filter::Or(v) => v.iter().any(|f| f.matches(entry)),
            Filter::Not(f) => !f.matches(entry),
            Filter::Equal(a, v) | Filter::Approx(a, v) => vals(a).iter().any(|x| lc(x) == lc(v)),
            Filter::GreaterOrEqual(a, v) => vals(a).iter().any(|x| lc(x) >= lc(v)),
            Filter::LessOrEqual(a, v) => vals(a).iter().any(|x| lc(x) <= lc(v)),
            Filter::Present(a) => !vals(a).is_empty(),
            Filter::Substrings {
                attr,
                initial,
                any,
                last,
            } => vals(attr).iter().any(|x| {
                let x = lc(x);
                let mut rest: &[u8] = &x;
                if let Some(i) = initial {
                    let i = lc(i);
                    if !rest.starts_with(&i) {
                        return false;
                    }
                    rest = &rest[i.len()..];
                }
                for a in any {
                    let a = lc(a);
                    match rest.windows(a.len()).position(|w| w == a.as_slice()) {
                        Some(p) => rest = &rest[p + a.len()..],
                        None => return false,
                    }
                }
                last.as_ref().is_none_or(|l| rest.ends_with(&lc(l)))
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_encodes_and_decodes() {
        for s in [
            "(uid=alice)",
            "(&(objectClass=person)(|(uid=al*ce)(mail=*@example.com))(!(disabled=TRUE)))",
            "(cn=*)",
            "(cn=a*b*c)",
            "(age>=21)",
            "(sn~=smith)",
            "(cn=\\28x\\29)",
        ] {
            let f = Filter::parse(s).unwrap();
            let enc = f.encode();
            let (tag, content) = Reader::new(&enc).read_tlv().unwrap();
            assert_eq!(Filter::decode(tag, content).unwrap(), f, "{s}");
        }
        assert_eq!(Filter::parse("(cn=*)").unwrap(), Filter::Present("cn".into()));
        assert_eq!(
            Filter::parse("(cn=\\28x\\29)").unwrap(),
            Filter::Equal("cn".into(), b"(x)".to_vec())
        );
    }

    #[test]
    fn rejects_bad_filters() {
        for s in [
            "",
            "uid=a",
            "(uid=a",
            "(=a)",
            "(&)",
            "(uid=a)(x=y)",
            "(uid=a(b)",
            "(uid>=a*)",
            "(uid=\\zz)",
        ] {
            assert!(Filter::parse(s).is_err(), "{s:?}");
        }
        let deep = format!("{}(a=b){}", "(!".repeat(40), ")".repeat(40));
        assert!(Filter::parse(&deep).is_err());
    }

    #[test]
    fn escaping_prevents_injection() {
        let user = "*)(uid=*";
        let f = Filter::parse(&format!("(uid={})", escape(user))).unwrap();
        assert_eq!(f, Filter::Equal("uid".into(), user.as_bytes().to_vec()));
        let mut entry = HashMap::new();
        entry.insert("uid".to_owned(), vec![b"admin".to_vec()]);
        assert!(!f.matches(&entry));
    }

    #[test]
    fn evaluates_against_entries() {
        let mut e = HashMap::new();
        e.insert("uid".to_owned(), vec![b"Alice".to_vec()]);
        e.insert("mail".to_owned(), vec![b"alice@example.com".to_vec()]);
        for (s, want) in [
            ("(uid=alice)", true),
            ("(&(uid=alice)(mail=*@example.com))", true),
            ("(uid=al*e)", true),
            ("(uid=b*)", false),
            ("(!(uid=alice))", false),
            ("(phone=*)", false),
        ] {
            assert_eq!(Filter::parse(s).unwrap().matches(&e), want, "{s}");
        }
    }
}

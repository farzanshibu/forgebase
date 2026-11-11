//! Reads AGE's `agtype` text output (`agtype_out`) into JSON (FB-163).
//!
//! `agtype` text is JSON plus type annotations (`::vertex`, `::edge`,
//! `::path`, `::numeric`, `::float`, `::integer`) and the floats `NaN`,
//! `Infinity` and `-Infinity`. Vertices and edges come back in ForgeBase's
//! shapes, with ForgeBase ids (the derived graph keeps them in `__id`,
//! `__source` and `__target`) rather than AGE's `graphid`s:
//!
//! * vertex: `{"id", "label", "properties"}` (as a node of the REST API);
//! * edge: `{"id", "type", "source", "target", "directed", "properties"}`;
//! * path: `{"nodes": [...], "edges": [...]}`;
//! * `NaN` and the infinities: the strings `"NaN"`, `"Infinity"`,
//!   `"-Infinity"` (JSON has no such numbers); a `numeric` that does not fit
//!   a JSON number: its text.

use serde_json::{Map, Number, Value as Json};

use super::{CATCH_EDGE, CATCH_NODE};

/// Properties of the derived graph that are ForgeBase's, not the user's.
const NODE_KEYS: &[&str] = &["__id", "__label"];
const EDGE_KEYS: &[&str] = &["__id", "__type", "__source", "__target", "__undirected"];

/// Parses one `agtype_out` text.
pub fn parse(text: &str) -> Result<Json, String> {
    let mut p = Parser {
        s: text.as_bytes(),
        i: 0,
        text,
    };
    let v = p.value()?;
    p.ws();
    if p.i != p.s.len() {
        return Err(format!("trailing characters at {}", p.i));
    }
    Ok(v)
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
    text: &'a str,
}

impl Parser<'_> {
    fn ws(&mut self) {
        while self.s.get(self.i).is_some_and(u8::is_ascii_whitespace) {
            self.i += 1;
        }
    }

    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.s.get(self.i) == Some(&c) {
            self.i += 1;
            true
        } else {
            false
        }
    }

    fn expect(&mut self, c: u8) -> Result<(), String> {
        if self.eat(c) {
            Ok(())
        } else {
            Err(format!("expected {:?} at {}", c as char, self.i))
        }
    }

    fn starts(&self, word: &str) -> bool {
        self.s[self.i..].starts_with(word.as_bytes())
    }

    fn value(&mut self) -> Result<Json, String> {
        self.ws();
        let raw = match self.s.get(self.i) {
            None => return Err("unexpected end".into()),
            Some(b'{') => self.object()?,
            Some(b'[') => self.array()?,
            Some(b'"') => Json::String(self.string()?),
            Some(_) => self.scalar()?,
        };
        // An annotation: `::vertex`, `::edge`, `::path`, `::numeric`, …
        if self.s[self.i..].starts_with(b"::") {
            self.i += 2;
            let start = self.i;
            while self.s.get(self.i).is_some_and(|c| c.is_ascii_alphanumeric() || *c == b'_') {
                self.i += 1;
            }
            let ty = &self.text[start..self.i];
            return Ok(match ty {
                "vertex" => vertex(raw),
                "edge" => edge(raw),
                "path" => path(raw),
                _ => raw,
            });
        }
        Ok(raw)
    }

    fn object(&mut self) -> Result<Json, String> {
        self.expect(b'{')?;
        let mut m = Map::new();
        if self.eat(b'}') {
            return Ok(Json::Object(m));
        }
        loop {
            self.ws();
            let k = self.string()?;
            self.expect(b':')?;
            let v = self.value()?;
            m.insert(k, v);
            if self.eat(b'}') {
                return Ok(Json::Object(m));
            }
            self.expect(b',')?;
        }
    }

    fn array(&mut self) -> Result<Json, String> {
        self.expect(b'[')?;
        let mut a = Vec::new();
        if self.eat(b']') {
            return Ok(Json::Array(a));
        }
        loop {
            a.push(self.value()?);
            if self.eat(b']') {
                return Ok(Json::Array(a));
            }
            self.expect(b',')?;
        }
    }

    fn string(&mut self) -> Result<String, String> {
        if self.s.get(self.i) != Some(&b'"') {
            return Err(format!("expected a string at {}", self.i));
        }
        let start = self.i;
        self.i += 1;
        loop {
            match self.s.get(self.i) {
                None => return Err("unterminated string".into()),
                Some(b'\\') => self.i += 2,
                Some(b'"') => {
                    self.i += 1;
                    break;
                }
                Some(_) => self.i += 1,
            }
        }
        serde_json::from_str(&self.text[start..self.i]).map_err(|e| e.to_string())
    }

    fn scalar(&mut self) -> Result<Json, String> {
        for (word, v) in [
            ("null", Json::Null),
            ("true", Json::Bool(true)),
            ("false", Json::Bool(false)),
            ("NaN", Json::String("NaN".into())),
            ("Infinity", Json::String("Infinity".into())),
            ("-Infinity", Json::String("-Infinity".into())),
        ] {
            if self.starts(word) {
                self.i += word.len();
                return Ok(v);
            }
        }
        let start = self.i;
        while self
            .s
            .get(self.i)
            .is_some_and(|c| c.is_ascii_digit() || matches!(c, b'-' | b'+' | b'.' | b'e' | b'E'))
        {
            self.i += 1;
        }
        let num = &self.text[start..self.i];
        if num.is_empty() {
            return Err(format!("unexpected character at {start}"));
        }
        Ok(match num.parse::<Number>() {
            Ok(n) => Json::Number(n),
            // A numeric beyond JSON numbers: keep its digits.
            Err(_) => Json::String(num.to_owned()),
        })
    }
}

fn take_props(raw: &mut Json) -> Map<String, Json> {
    match raw.get_mut("properties").map(Json::take) {
        Some(Json::Object(m)) => m,
        _ => Map::new(),
    }
}

fn text_of(m: &Map<String, Json>, key: &str) -> Json {
    m.get(key).cloned().unwrap_or(Json::Null)
}

fn vertex(mut raw: Json) -> Json {
    let props = take_props(&mut raw);
    let age_label = raw.get("label").and_then(Json::as_str).unwrap_or_default();
    let label = if age_label == CATCH_NODE {
        text_of(&props, "__label")
    } else {
        Json::String(age_label.to_owned())
    };
    let id = text_of(&props, "__id");
    let user: Map<String, Json> = props.into_iter().filter(|(k, _)| !NODE_KEYS.contains(&k.as_str())).collect();
    serde_json::json!({"id": id, "label": label, "properties": user})
}

fn edge(mut raw: Json) -> Json {
    let props = take_props(&mut raw);
    let age_label = raw.get("label").and_then(Json::as_str).unwrap_or_default();
    let ty = if age_label == CATCH_EDGE {
        text_of(&props, "__type")
    } else {
        Json::String(age_label.to_owned())
    };
    let directed = props.get("__undirected").and_then(Json::as_bool) != Some(true);
    let (id, source, target) = (
        text_of(&props, "__id"),
        text_of(&props, "__source"),
        text_of(&props, "__target"),
    );
    let user: Map<String, Json> = props.into_iter().filter(|(k, _)| !EDGE_KEYS.contains(&k.as_str())).collect();
    serde_json::json!({
        "id": id, "type": ty, "source": source, "target": target, "directed": directed, "properties": user,
    })
}

/// A path is `[vertex, edge, vertex, …]` (already converted).
fn path(raw: Json) -> Json {
    let mut nodes = Vec::new();
    let mut edges = Vec::new();
    if let Json::Array(items) = raw {
        for (i, item) in items.into_iter().enumerate() {
            if i % 2 == 0 { nodes.push(item) } else { edges.push(item) }
        }
    }
    serde_json::json!({"nodes": nodes, "edges": edges})
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn scalars_and_specials() {
        assert_eq!(parse("1").unwrap(), json!(1));
        assert_eq!(parse("-2.5").unwrap(), json!(-2.5));
        assert_eq!(parse("\"a\\\"b\"").unwrap(), json!("a\"b"));
        assert_eq!(parse("null").unwrap(), Json::Null);
        assert_eq!(
            parse("[9223372036854775807, 1.5::numeric, NaN, Infinity, -Infinity, -0.0]").unwrap(),
            json!([9_223_372_036_854_775_807_i64, 1.5, "NaN", "Infinity", "-Infinity", -0.0])
        );
        assert_eq!(parse("{\"k\": [true, {}]}").unwrap(), json!({"k": [true, {}]}));
        assert!(parse("[1,").is_err() && parse("1 2").is_err());
    }

    #[test]
    fn vertices_edges_and_paths_use_forgebase_ids() {
        let v = r#"{"id": 844424930131969, "label": "Person", "properties": {"__id": "a", "name": "Ann"}}::vertex"#;
        assert_eq!(parse(v).unwrap(), json!({"id": "a", "label": "Person", "properties": {"name": "Ann"}}));
        let c = r#"{"id": 1, "label": "_fbnode", "properties": {"__id": "x", "__label": "my-label"}}::vertex"#;
        assert_eq!(parse(c).unwrap()["label"], "my-label");
        let e = r#"{"id": 1407374883553281, "label": "KNOWS", "end_id": 2, "start_id": 1, "properties": {"__id": "e1", "__source": "a", "__target": "b", "__undirected": true, "w": 2}}::edge"#;
        assert_eq!(
            parse(e).unwrap(),
            json!({"id": "e1", "type": "KNOWS", "source": "a", "target": "b", "directed": false, "properties": {"w": 2}})
        );
        let p = format!("[{v}, {e}, {}]::path", v.replace("\"a\"", "\"b\""));
        let p = parse(&p).unwrap();
        assert_eq!(p["nodes"].as_array().unwrap().len(), 2);
        assert_eq!(p["edges"][0]["id"], "e1");
        assert_eq!(p["nodes"][1]["id"], "b");
    }
}

//! Email template rendering.
//!
//! Syntax: `{{ name }}` or `{{ a.b }}` substitutes a variable. There is no
//! raw (unescaped) form, no logic and no nesting, so a template author cannot
//! run code and a variable cannot inject markup:
//!
//! * **HTML body**: the value is HTML-escaped (`& < > " '`). A variable named
//!   `link`, `url` or ending in `_link` / `_url` is also required to be an
//!   `http(s)://` or `mailto:` URL, else it renders as `#` (no `javascript:`).
//! * **Text body**: the value is inserted as is, except for NUL and other
//!   control characters (kept: newline, tab).
//! * **Subject**: control characters (CR, LF, ...) become spaces, so a value
//!   cannot add mail headers.
//!
//! Missing variables render as the empty string and are reported.

use serde_json::Value as Json;

use crate::error::{CustomizeError, Result};

/// Largest subject template.
pub const MAX_SUBJECT: usize = 300;
/// Largest text body template.
pub const MAX_TEXT: usize = 64 * 1024;
/// Largest HTML body template.
pub const MAX_HTML: usize = 128 * 1024;

/// Where a rendered value ends up.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Subject line.
    Subject,
    /// Plain text body.
    Text,
    /// HTML body.
    Html,
}

/// One piece of a parsed template.
enum Part<'a> {
    Lit(&'a str),
    Var(&'a str),
}

fn valid_var(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && !name.starts_with('.')
        && !name.ends_with('.')
        && !name.contains("..")
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.')
}

fn parse(tpl: &str) -> Result<Vec<Part<'_>>> {
    let mut out = Vec::new();
    let mut rest = tpl;
    while let Some(start) = rest.find("{{") {
        if start > 0 {
            out.push(Part::Lit(&rest[..start]));
        }
        let after = &rest[start + 2..];
        let Some(end) = after.find("}}") else {
            return Err(CustomizeError::invalid(
                "invalid_template",
                "unclosed `{{`: a variable needs a closing `}}`",
            ));
        };
        let name = after[..end].trim();
        if !valid_var(name) {
            return Err(CustomizeError::invalid(
                "invalid_template",
                format!(
                    "{name:?} is not a variable name (letters, digits, `_` and `.`; only plain substitution is supported)"
                ),
            ));
        }
        out.push(Part::Var(name));
        rest = &after[end + 2..];
    }
    if rest.contains("}}") {
        return Err(CustomizeError::invalid("invalid_template", "stray `}}`"));
    }
    if !rest.is_empty() {
        out.push(Part::Lit(rest));
    }
    Ok(out)
}

/// Checks the syntax of a template.
pub fn validate(tpl: &str) -> Result<()> {
    parse(tpl).map(|_| ())
}

/// The variable names a template uses, sorted and unique.
pub fn variables(tpl: &str) -> Result<Vec<String>> {
    let mut v: Vec<String> = parse(tpl)?
        .into_iter()
        .filter_map(|p| match p {
            Part::Var(n) => Some(n.to_owned()),
            Part::Lit(_) => None,
        })
        .collect();
    v.sort();
    v.dedup();
    Ok(v)
}

/// HTML-escapes text for element content and quoted attribute values.
pub fn escape_html(s: &str) -> String {
    let mut o = String::with_capacity(s.len() + 8);
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            '\'' => o.push_str("&#39;"),
            '`' => o.push_str("&#96;"),
            c => o.push(c),
        }
    }
    o
}

fn is_url_var(name: &str) -> bool {
    let last = name.rsplit('.').next().unwrap_or(name);
    last == "link" || last == "url" || last.ends_with("_link") || last.ends_with("_url")
}

fn safe_url(v: &str) -> bool {
    let l = v.trim().to_ascii_lowercase();
    (l.starts_with("https://") || l.starts_with("http://") || l.starts_with("mailto:"))
        && !l.chars().any(|c| c.is_control() || c.is_whitespace())
}

fn lookup<'a>(vars: &'a Json, name: &str) -> Option<&'a Json> {
    let mut cur = vars;
    for seg in name.split('.') {
        cur = cur.get(seg)?;
    }
    Some(cur)
}

fn value_text(v: &Json) -> Option<String> {
    match v {
        Json::String(s) => Some(s.clone()),
        Json::Number(n) => Some(n.to_string()),
        Json::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

fn strip_controls(s: &str, keep_newline: bool) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() && !(keep_newline && (c == '\n' || c == '\t')) {
                ' '
            } else {
                c
            }
        })
        .collect()
}

/// Renders `tpl` with `vars` for `mode`. The second value lists the variables
/// the template uses that `vars` does not provide.
pub fn render(tpl: &str, vars: &Json, mode: Mode) -> Result<(String, Vec<String>)> {
    let mut out = String::with_capacity(tpl.len());
    let mut missing = Vec::new();
    for part in parse(tpl)? {
        match part {
            Part::Lit(l) => out.push_str(l),
            Part::Var(name) => {
                let Some(text) = lookup(vars, name).and_then(value_text) else {
                    if !missing.iter().any(|m| m == name) {
                        missing.push(name.to_owned());
                    }
                    if mode == Mode::Html && is_url_var(name) {
                        out.push('#');
                    }
                    continue;
                };
                match mode {
                    Mode::Subject => out.push_str(&strip_controls(&text, false)),
                    Mode::Text => out.push_str(&strip_controls(&text, true)),
                    Mode::Html => {
                        if is_url_var(name) && !safe_url(&text) {
                            out.push('#');
                        } else {
                            out.push_str(&escape_html(&text));
                        }
                    }
                }
            }
        }
    }
    Ok((out, missing))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use serde_json::json;

    use super::*;

    #[test]
    fn substitutes_and_reports_missing() {
        let (s, m) = render(
            "Hi {{ user.name }}, {{code}} {{nope}}",
            &json!({"user": {"name": "Ann"}, "code": 42}),
            Mode::Text,
        )
        .unwrap();
        assert_eq!(s, "Hi Ann, 42 ");
        assert_eq!(m, vec!["nope"]);
    }

    #[test]
    fn html_escapes_values_but_not_the_template() {
        let (s, _) = render(
            "<p>{{ name }}</p>",
            &json!({"name": "<script>alert(1)</script>\"'&"}),
            Mode::Html,
        )
        .unwrap();
        assert_eq!(s, "<p>&lt;script&gt;alert(1)&lt;/script&gt;&quot;&#39;&amp;</p>");
    }

    #[test]
    fn url_variables_refuse_script_schemes() {
        let a = render(
            r#"<a href="{{link}}">x</a>"#,
            &json!({"link": "javascript:alert(1)"}),
            Mode::Html,
        )
        .unwrap()
        .0;
        assert_eq!(a, r##"<a href="#">x</a>"##);
        let ok = render(
            r#"<a href="{{ reset_url }}">x</a>"#,
            &json!({"reset_url": "https://a.example/r?x=1&y=2"}),
            Mode::Html,
        )
        .unwrap()
        .0;
        assert_eq!(ok, r#"<a href="https://a.example/r?x=1&amp;y=2">x</a>"#);
        // a quote cannot break out of the attribute
        let q = render(
            r#"<a href="{{link}}">"#,
            &json!({"link": "https://a.example/\" onclick=\"x"}),
            Mode::Html,
        )
        .unwrap()
        .0;
        assert_eq!(q, r##"<a href="#">"##);
    }

    #[test]
    fn subject_cannot_inject_headers() {
        let (s, _) = render(
            "Hello {{n}}",
            &json!({"n": "x\r\nBcc: evil@example.com"}),
            Mode::Subject,
        )
        .unwrap();
        assert!(!s.contains('\r') && !s.contains('\n'));
    }

    #[test]
    fn rejects_malformed_templates() {
        for bad in [
            "{{ a",
            "a }}",
            "{{ }}",
            "{{ a b }}",
            "{{#if x}}",
            "{{ ..a }}",
            "{{ a-b }}",
        ] {
            assert!(validate(bad).is_err(), "{bad}");
        }
        assert_eq!(variables("{{b}} {{a}} {{b}}").unwrap(), vec!["a", "b"]);
    }

    #[test]
    fn nested_template_syntax_in_values_is_not_reexpanded() {
        let (s, _) = render("{{a}}", &json!({"a": "{{b}}", "b": "secret"}), Mode::Text).unwrap();
        assert_eq!(s, "{{b}}");
    }
}

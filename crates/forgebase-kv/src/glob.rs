//! Redis-style glob matching for `KEYS` and `SCAN MATCH`: `*`, `?`, `[abc]`,
//! `[a-z]`, `[^a]` and `\` escapes. Linear in pattern and text length (one
//! backtrack point), so a hostile pattern cannot blow up.

/// Whether `text` matches `pattern`.
pub(crate) fn matches(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        let step = if pi < p.len() {
            match p[pi] {
                '*' => {
                    star = Some((pi, ti));
                    pi += 1;
                    continue;
                }
                '?' => Some(pi + 1),
                '[' => class(&p, pi, t[ti]),
                '\\' if pi + 1 < p.len() => (p[pi + 1] == t[ti]).then_some(pi + 2),
                c => (c == t[ti]).then_some(pi + 1),
            }
        } else {
            None
        };
        match step {
            Some(next) => {
                pi = next;
                ti += 1;
            }
            None => match star {
                Some((sp, st)) => {
                    pi = sp + 1;
                    ti = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

/// Matches `c` against the class starting at `p[start] == '['`; returns the
/// index after the class when it matches.
fn class(p: &[char], start: usize, c: char) -> Option<usize> {
    let mut i = start + 1;
    let negate = p.get(i) == Some(&'^');
    if negate {
        i += 1;
    }
    let mut hit = false;
    let mut closed = false;
    while i < p.len() {
        match p[i] {
            ']' => {
                closed = true;
                i += 1;
                break;
            }
            '\\' if i + 1 < p.len() => {
                hit |= p[i + 1] == c;
                i += 2;
            }
            lo if i + 2 < p.len() && p[i + 1] == '-' && p[i + 2] != ']' => {
                let hi = p[i + 2];
                hit |= (lo.min(hi)..=lo.max(hi)).contains(&c);
                i += 3;
            }
            x => {
                hit |= x == c;
                i += 1;
            }
        }
    }
    // An unclosed class matches nothing.
    (closed && hit != negate).then_some(i)
}

#[cfg(test)]
mod tests {
    use super::matches;

    #[test]
    fn globs() {
        assert!(matches("*", "anything"));
        assert!(matches("user:*", "user:42"));
        assert!(!matches("user:*", "usr:42"));
        assert!(matches("h?llo", "hello"));
        assert!(matches("h[ae]llo", "hallo"));
        assert!(!matches("h[^ae]llo", "hello"));
        assert!(matches("h[a-c]llo", "hbllo"));
        assert!(matches("a\\*b", "a*b"));
        assert!(!matches("a\\*b", "axb"));
        assert!(matches("*a*b*c*", "xxaxxbxxcxx"));
        assert!(matches("", ""));
        assert!(!matches("", "x"));
        assert!(!matches("[ab", "a"));
        // pathological pattern stays fast
        let pat = "*a".repeat(40) + "b";
        assert!(!matches(&pat, &"a".repeat(2000)));
    }
}

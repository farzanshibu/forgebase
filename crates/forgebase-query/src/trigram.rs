//! Portable trigrams (ADR-0034): `pg_trgm`'s model of text similarity
//! without the extension, for SQLite and for PostgreSQL servers without
//! `pg_trgm` (or with `trigram = "off"`).
//!
//! The model is `pg_trgm`'s:
//!
//! * the text is lowercased and split into **words** (runs of letters and
//!   digits; everything else separates words);
//! * each word is padded with two spaces in front and one behind
//!   (`"  cat "`), and its **trigrams** are the three-character windows of
//!   the padded word (`"  c"`, `" ca"`, `"cat"`, `"at "`);
//! * a text's trigram **set** has every trigram once;
//! * **similarity** = |shared| / |union| = `s / (|a| + |b| - s)`, `0` when
//!   either set is empty; a match is a similarity of at least the threshold
//!   ([`DEFAULT_THRESHOLD`], `pg_trgm.similarity_threshold`'s default).
//!
//! On PostgreSQL the portable engine computes this in SQL with the server's
//! own `lower()` and `[[:alnum:]]` (migration 0056), so it classifies and
//! lowercases characters exactly as `pg_trgm` does on that server. SQLite's
//! `lower()` knows ASCII only, so this module defines the classification
//! used there, shared by the SQL that maintains the SQLite trigram tables
//! and by the Rust code that turns a query into trigrams:
//!
//! * ASCII: `0-9`, `a-z` are word characters, `A-Z` are lowercased, every
//!   other ASCII character separates words;
//! * the characters of [`FOLD_BLOCKS`] (Latin-1 to Hebrew, Latin Extended
//!   Additional, Greek Extended, general punctuation, currency signs, CJK
//!   punctuation, full-width forms): letters and digits are lowercased
//!   (Unicode simple lowercase), everything else separates words, as
//!   `pg_trgm` under the bundled engine's `C.UTF-8` locale;
//! * every other character is a word character, compared as it is (no case
//!   folding);
//! * only the first [`MAX_CHARS`] characters of a value are indexed and
//!   compared.
//!
//! The classification is a table generated once ([`fold_rows`]); the
//! migration seeds the same rows into `_forgebase_trgm_fold`.

use std::collections::BTreeSet;

use crate::trigram_fold::FOLD;

/// `pg_trgm.similarity_threshold`'s default: the match threshold when a
/// request names none.
pub const DEFAULT_THRESHOLD: f64 = 0.3;

/// Characters of a value that the SQLite engine indexes and compares
/// (`pg_trgm` and the PostgreSQL portable engine have no limit).
pub const MAX_CHARS: usize = 4096;

/// Rows of `_forgebase_trgm_positions` (`1..=POSITIONS`): enough to walk
/// [`MAX_CHARS`] characters and, two windows a row, the up to `2 ×
/// MAX_CHARS + 3` characters of the padded text.
pub const POSITIONS: usize = 4100;

/// Unicode blocks (inclusive code point ranges) whose characters the SQLite
/// engine classifies like `pg_trgm` under `C.UTF-8`.
pub const FOLD_BLOCKS: &[(u32, u32)] = &[
    (0x0080, 0x05FF),
    (0x1E00, 0x1FFF),
    (0x2000, 0x206F),
    (0x20A0, 0x20CF),
    (0x3000, 0x303F),
    (0xFF00, 0xFF65),
];

/// `(code point, lowercase code point)` for every non-ASCII character of
/// [`FOLD_BLOCKS`] that is not a word character as it is: `0` when it
/// separates words, else the character it lowercases to. Sorted by code
/// point; the seed rows of `_forgebase_trgm_fold`.
pub fn fold_rows() -> &'static [(u32, u32)] {
    FOLD
}

/// How the SQLite engine reads one character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    /// A word character (after lowercasing).
    Word(char),
    /// Separates words.
    Separator,
}

fn classify(c: char) -> Class {
    if c.is_ascii() {
        return if c.is_ascii_alphanumeric() {
            Class::Word(c.to_ascii_lowercase())
        } else {
            Class::Separator
        };
    }
    let cp = u32::from(c);
    match FOLD.binary_search_by_key(&cp, |&(k, _)| k) {
        Ok(i) => match FOLD[i].1 {
            0 => Class::Separator,
            l => char::from_u32(l).map_or(Class::Separator, Class::Word),
        },
        Err(_) => Class::Word(c),
    }
}

/// The padded text the SQLite engine takes windows of: `"  "`, then each
/// character (lowercased) or two spaces for a separator, then `" "`.
/// Empty for an empty text.
fn padded(text: &str) -> Vec<char> {
    let mut out = Vec::with_capacity(text.len() * 2 + 3);
    for c in text.chars().take(MAX_CHARS) {
        match classify(c) {
            Class::Word(w) => out.push(w),
            Class::Separator => out.extend([' ', ' ']),
        }
    }
    if out.is_empty() {
        return out;
    }
    let mut s = vec![' ', ' '];
    s.append(&mut out);
    s.push(' ');
    s
}

/// The trigram set of `text` under the SQLite engine's classification,
/// sorted. Windows that end in two spaces are not trigrams (they straddle
/// two words or lie between them); every other window is a trigram of one
/// padded word.
pub fn trigrams(text: &str) -> Vec<String> {
    let s = padded(text);
    let mut set = BTreeSet::new();
    if s.len() >= 3 {
        for w in s.windows(3) {
            if !(w[1] == ' ' && w[2] == ' ') {
                set.insert(w.iter().collect::<String>());
            }
        }
    }
    set.into_iter().collect()
}

/// `pg_trgm`'s similarity of two trigram sets (sorted, distinct): shared /
/// union, `0` when either is empty.
pub fn similarity(a: &[String], b: &[String]) -> f64 {
    if a.is_empty() || b.is_empty() {
        return 0.0;
    }
    let (mut i, mut j, mut shared) = (0, 0, 0usize);
    while i < a.len() && j < b.len() {
        match a[i].cmp(&b[j]) {
            std::cmp::Ordering::Less => i += 1,
            std::cmp::Ordering::Greater => j += 1,
            std::cmp::Ordering::Equal => {
                shared += 1;
                i += 1;
                j += 1;
            }
        }
    }
    shared as f64 / (a.len() + b.len() - shared) as f64
}

/// The fewest trigrams a stored value must share with a query of `query`
/// trigrams to reach `threshold`: similarity ≤ shared / |query|, so at least
/// `⌈threshold × |query|⌉` (at least 1; 0 for a threshold of 0, where every
/// value matches).
pub fn min_shared(query: usize, threshold: f64) -> usize {
    if threshold <= 0.0 {
        return 0;
    }
    // A hair below the exact product, so float rounding never drops a match.
    let need = (threshold * query as f64 - 1e-9).ceil();
    (need.max(1.0) as usize).min(query.max(1))
}

/// The `INSERT … VALUES` rows of `_forgebase_trgm_fold` as migration 0056
/// writes them: `(cp, lower_cp)` pairs, twelve a line.
pub fn fold_seed_values() -> String {
    let rows = fold_rows();
    let mut s = String::new();
    for (i, (c, l)) in rows.iter().enumerate() {
        if i % 12 == 0 {
            s.push_str("    ");
        }
        s.push_str(&format!("({c}, {l})"));
        if i + 1 < rows.len() {
            s.push(',');
        }
        if i % 12 == 11 {
            s.push('\n');
        } else {
            s.push(' ');
        }
    }
    let mut s = s.trim_end().to_owned();
    s.push(';');
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> Vec<String> {
        trigrams(s)
    }

    #[test]
    fn words_are_padded_like_pg_trgm() {
        // SELECT show_trgm('Cat');  {"  c"," ca","at ",cat}
        assert_eq!(t("Cat"), ["  c", " ca", "at ", "cat"]);
        // One-letter words: "  a", " a ".
        assert_eq!(t("a"), ["  a", " a "]);
        // SELECT show_trgm('foo-bar  x');
        assert_eq!(
            t("foo-bar  x"),
            ["  b", "  f", "  x", " ba", " fo", " x ", "ar ", "bar", "foo", "oo "]
        );
        assert!(t("").is_empty());
        assert!(t(" -- !! ").is_empty());
        // Duplicates count once.
        assert_eq!(t("aa aa"), t("aa"));
    }

    #[test]
    fn case_and_separators_follow_the_fold_table() {
        assert_eq!(t("ÉCOLE"), t("école"));
        assert_eq!(t("ΣΟΦΙΑ"), t("σοφια"));
        assert_eq!(t("МОСКВА"), t("москва"));
        // Non-breaking space, em dash and curly quotes separate words.
        assert_eq!(t("a\u{a0}b—c“d”"), t("a b c d"));
        // Characters outside the blocks are word characters, kept as they are.
        assert_eq!(t("日本"), ["  日", " 日本", "日本 "]);
        // Full-width Latin folds.
        assert_eq!(t("ＡＢ"), t("ａｂ"));
    }

    #[test]
    fn similarity_is_shared_over_union() {
        let a = t("word");
        let b = t("words");
        // pg_trgm: similarity('word', 'words') = 0.5714286 (4 / 7).
        assert!((similarity(&a, &b) - 4.0 / 7.0).abs() < 1e-12);
        assert_eq!(similarity(&a, &[]), 0.0);
        assert_eq!(similarity(&a, &a), 1.0);
    }

    #[test]
    fn min_shared_never_cuts_a_match() {
        assert_eq!(min_shared(10, 0.3), 3);
        assert_eq!(min_shared(10, 0.0), 0);
        assert_eq!(min_shared(3, 1.0), 3);
        assert_eq!(min_shared(7, 0.3), 3);
        // 0.3 × 10 = 3.0000000000000004 in floating point: still 3.
        assert_eq!(min_shared(10, 0.1 + 0.2), 3);
        assert_eq!(min_shared(0, 0.3), 1);
    }

    #[test]
    fn long_values_are_cut_at_max_chars() {
        let long = "ab ".repeat(MAX_CHARS);
        let short: String = long.chars().take(MAX_CHARS).collect();
        assert_eq!(t(&long), t(&short));
        assert!(padded(&long).len() <= 2 * POSITIONS);
    }

    #[test]
    fn fold_rows_are_sorted_and_in_their_blocks() {
        let rows = fold_rows();
        assert!(rows.windows(2).all(|w| w[0].0 < w[1].0));
        for &(c, _) in rows {
            assert!(FOLD_BLOCKS.iter().any(|&(a, b)| (a..=b).contains(&c)), "{c:#x}");
        }
    }

    #[test]
    fn fold_rows_match_the_migration() {
        for sql in [
            include_str!("../../../migrations/system/0056_portable_trigram.sql"),
            include_str!("../../../migrations/postgres/0056_portable_trigram.sql"),
        ] {
            assert!(
                sql.contains(&fold_seed_values()),
                "the seed rows of _forgebase_trgm_fold differ from trigram_fold.rs"
            );
            assert!(sql.contains(&format!("WHERE i < {POSITIONS}")));
        }
    }
}

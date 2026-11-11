//! Delivery history SQL (ADR-0022): `history` is a JSON array (text) of the
//! last [`MAX_ENTRIES`] attempts of a delivery, `{attempt, at, consumer,
//! outcome, error}`. Built with the dialect's JSON array functions so the
//! same statements run on SQLite and PostgreSQL (ADR-0028 §7.4, FB-146).

use forgebase_db::Dialect;

/// Attempts a delivery's history keeps (the oldest is dropped first).
pub(crate) const MAX_ENTRIES: i64 = 25;

fn postgres(d: &dyn Dialect) -> bool {
    d.name() == "postgres"
}

/// `history` with an entry for a new attempt appended, dropping the oldest
/// entry at [`MAX_ENTRIES`]. Reads `attempts` (before the increment) and
/// binds `?3` (time, ms) and `?4` (consumer).
pub(crate) fn push(d: &dyn Dialect) -> String {
    if postgres(d) {
        d.json_array_append(
            &format!(
                "CASE WHEN {len} >= {MAX_ENTRIES} THEN (history)::jsonb - 0 ELSE (history)::jsonb END",
                len = d.json_array_length("history")
            ),
            "jsonb_build_object('attempt', attempts + 1, 'at', CAST(?3 AS bigint), 'consumer', CAST(?4 AS text))",
        )
    } else {
        d.json_array_append(
            &format!(
                "CASE WHEN json_array_length(history) >= {MAX_ENTRIES} THEN json_remove(history, '$[0]') \
                 ELSE history END"
            ),
            "json_object('attempt', attempts + 1, 'at', ?3, 'consumer', ?4)",
        )
    }
}

/// `history` with the `outcome` of its last entry set to the SQL text
/// expression `outcome` (a literal or a `CAST(?n AS TEXT)`).
pub(crate) fn set_outcome(d: &dyn Dialect, outcome: &str) -> String {
    d.json_set_last("history", "outcome", outcome)
}

/// Like [`set_outcome`], also setting `error` of the last entry.
pub(crate) fn set_outcome_and_error(d: &dyn Dialect, outcome: &str, error: &str) -> String {
    d.json_set_last(&set_outcome(d, outcome), "error", error)
}

/// The condition "the history has an entry" (guards [`set_outcome`]).
pub(crate) fn not_empty(d: &dyn Dialect) -> String {
    format!("{} > 0", d.json_array_length("history"))
}

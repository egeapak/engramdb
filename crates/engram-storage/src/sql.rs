//! SQL string literals for LanceDB filter expressions.
//!
//! LanceDB takes its predicates as SQL text, so every string value spliced
//! into one has to be quoted. The rule is the SQL standard's and nothing
//! else: wrap in single quotes and double every single quote inside. Values
//! that reach a predicate come from our own writers, but also from files on
//! disk (a drained JSONL log, a restored backup, a hand-edited store), so the
//! quote is doubled rather than assumed absent. This is the one place it is
//! done; a new predicate builds its literals here.

/// `s` as a SQL string literal: `'…'` with each `'` doubled.
pub fn sql_str(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// The items of `values` as a comma-separated list of SQL string literals,
/// for `column IN (…)`. Empty input gives an empty string; the caller decides
/// what an empty set means (an `IN ()` is not valid SQL).
pub fn sql_in_list<S: AsRef<str>>(values: &[S]) -> String {
    values
        .iter()
        .map(|v| sql_str(v.as_ref()))
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn str_literal_doubles_quotes_only() {
        assert_eq!(sql_str("plain"), "'plain'");
        assert_eq!(sql_str(""), "''");
        assert_eq!(sql_str("it's"), "'it''s'");
        assert_eq!(sql_str("''"), "''''''");
        // Nothing but the single quote is touched: no backslash escaping, no
        // change to double quotes, `%`, `_`, newlines or non-ASCII.
        let other = "a\\b\"c%d_e\nf\u{e9}";
        assert_eq!(sql_str(other), format!("'{other}'"));
    }

    #[test]
    fn in_list_joins_quoted_literals() {
        assert_eq!(sql_in_list::<&str>(&[]), "");
        assert_eq!(sql_in_list(&["a"]), "'a'");
        assert_eq!(
            sql_in_list(&["a".to_string(), "b'c".to_string()]),
            "'a', 'b''c'"
        );
    }
}

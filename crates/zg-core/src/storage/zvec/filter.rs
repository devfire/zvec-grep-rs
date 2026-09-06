//! zvec filter-expression builders for FTS/vector recall.
//!
//! Ports `buildFilter`, `buildNonEmptyInFilter`, `buildInFilter`, and
//! `quoteFilterString` from `engine/storage/zvec.ts`.
//!
//! Divergence: TypeScript distinguishes an absent filter dimension from a
//! present-but-empty one (empty means "match nothing"). The Rust
//! [`StorageSearchFilter`] uses plain vectors, so an empty vector means
//! "no constraint"; callers that resolve a file set down to zero entries
//! must short-circuit before calling storage instead of passing the empty
//! set through.

use crate::storage::StorageSearchFilter;

/// Filter matching no document, used when a caller explicitly needs one.
pub const NO_MATCH_FILTER: &str = "file_id = '__zvec_grep_no_match__'";

/// Builds the zvec filter expression for `filter`, or `None` for
/// unfiltered recall.
pub fn build_filter(filter: Option<&StorageSearchFilter>) -> Option<String> {
    let filter = filter?;
    let mut clauses = Vec::new();
    push_in_clause(
        &mut clauses,
        "file_id",
        filter.file_ids.iter().map(|id| id.as_str()),
    );
    push_in_clause(
        &mut clauses,
        "group",
        filter.group_ids.iter().map(String::as_str),
    );
    push_in_clause(
        &mut clauses,
        "symbol_name",
        filter.symbol_names.iter().map(String::as_str),
    );
    push_in_clause(
        &mut clauses,
        "symbol_type",
        filter.symbol_types.iter().map(symbol_type_filter_value),
    );
    if clauses.is_empty() {
        return None;
    }
    Some(clauses.join(" AND "))
}

fn push_in_clause<'a>(
    clauses: &mut Vec<String>,
    field: &str,
    values: impl Iterator<Item = &'a str>,
) {
    let values: Vec<&'a str> = values.collect();
    if values.is_empty() {
        return;
    }
    clauses.push(build_in_filter(field, &values));
}

fn build_in_filter(field: &str, values: &[&str]) -> String {
    if values.len() == 1 {
        return format!("{field} = {}", quote_filter_string(values[0]));
    }
    let terms: Vec<String> = values
        .iter()
        .map(|value| format!("{field} = {}", quote_filter_string(value)))
        .collect();
    format!("({})", terms.join(" OR "))
}

/// Quotes a filter string literal, escaping backslashes and single quotes.
pub fn quote_filter_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('\'');
    for ch in value.chars() {
        if ch == '\\' || ch == '\'' {
            quoted.push('\\');
        }
        quoted.push(ch);
    }
    quoted.push('\'');
    quoted
}

fn symbol_type_filter_value(symbol_type: &crate::types::CodeSymbolType) -> &'static str {
    match symbol_type {
        crate::types::CodeSymbolType::Module => "module",
        crate::types::CodeSymbolType::Class => "class",
        crate::types::CodeSymbolType::Interface => "interface",
        crate::types::CodeSymbolType::Function => "function",
        crate::types::CodeSymbolType::Value => "value",
        crate::types::CodeSymbolType::Alias => "alias",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absent_filter_builds_no_expression() {
        assert_eq!(build_filter(None), None);
        assert_eq!(build_filter(Some(&StorageSearchFilter::default())), None);
    }

    #[test]
    fn single_value_uses_equality() {
        let filter = StorageSearchFilter {
            file_ids: vec![crate::ids::FileId::from_raw("abc".to_owned())],
            ..StorageSearchFilter::default()
        };
        assert_eq!(
            build_filter(Some(&filter)),
            Some("file_id = 'abc'".to_owned())
        );
    }

    #[test]
    fn multiple_values_join_with_or_and_clauses_with_and() {
        let filter = StorageSearchFilter {
            group_ids: vec!["g1".to_owned(), "g2".to_owned()],
            symbol_names: vec!["main".to_owned()],
            ..StorageSearchFilter::default()
        };
        assert_eq!(
            build_filter(Some(&filter)),
            Some("(group = 'g1' OR group = 'g2') AND symbol_name = 'main'".to_owned())
        );
    }

    #[test]
    fn quoting_escapes_backslash_and_quote() {
        assert_eq!(quote_filter_string("a'b\\c"), "'a\\'b\\\\c'");
    }
}

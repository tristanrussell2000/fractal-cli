//! Just enough of a caller's SELECT to explain SAP's refusal of it: the tables
//! it reads and the aliases it gives them. Anything it cannot read is skipped,
//! so the worst case is SAP's own message.

use std::sync::LazyLock;

use regex::Regex;

/// One table in the FROM clause.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct QuerySource {
    pub name: String,
    pub alias: Option<String>,
}

impl QuerySource {
    /// How a column of this table is qualified: by its alias, once it has one.
    pub fn reference(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.name)
    }

    fn is_called(&self, qualifier: &str) -> bool {
        self.reference().eq_ignore_ascii_case(qualifier)
    }
}

static SOURCE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\b(?:FROM|JOIN)\s+([A-Z/][A-Z0-9_/]*)(?:\s+AS\s+([A-Z][A-Z0-9_]*))?")
        .expect("query source regex is valid")
});

/// The tables a statement reads, in order, each once.
pub(super) fn query_sources(statement: &str) -> Vec<QuerySource> {
    let mut sources: Vec<QuerySource> = Vec::new();
    for captures in SOURCE.captures_iter(statement) {
        let source = QuerySource {
            name: captures[1].to_ascii_uppercase(),
            alias: captures
                .get(2)
                .map(|alias| alias.as_str().to_ascii_uppercase()),
        };
        if !sources.contains(&source) {
            sources.push(source);
        }
    }
    sources
}

/// The table or alias the statement qualifies `column` with, if any.
pub(super) fn column_qualifier(statement: &str, column: &str) -> Option<String> {
    let pattern = format!(r"(?i)\b([A-Z/][A-Z0-9_/]*)~{}\b", regex::escape(column));
    Regex::new(&pattern)
        .ok()?
        .captures(statement)
        .map(|captures| captures[1].to_ascii_uppercase())
}

/// The sources a column could belong to: the one it is qualified with, or all.
pub(super) fn candidate_sources<'a>(
    sources: &'a [QuerySource],
    qualifier: Option<&str>,
) -> Vec<&'a QuerySource> {
    sources
        .iter()
        .filter(|source| qualifier.is_none_or(|qualifier| source.is_called(qualifier)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_tables_and_aliases_from_from_and_join() {
        let sources = query_sources(
            "SELECT t~trkorr FROM e070 AS t INNER JOIN e071 ON e071~trkorr = t~trkorr LEFT OUTER JOIN /acme/log AS l ON l~id = t~trkorr",
        );
        assert_eq!(
            sources,
            vec![
                QuerySource {
                    name: "E070".to_owned(),
                    alias: Some("T".to_owned()),
                },
                QuerySource {
                    name: "E071".to_owned(),
                    alias: None,
                },
                QuerySource {
                    name: "/ACME/LOG".to_owned(),
                    alias: Some("L".to_owned()),
                },
            ]
        );
        assert_eq!(sources[0].reference(), "T");
        assert_eq!(sources[1].reference(), "E071");
    }

    #[test]
    fn finds_the_qualifier_of_a_column() {
        let statement = "SELECT t~foo, bar FROM tlock AS t";
        assert_eq!(column_qualifier(statement, "FOO").as_deref(), Some("T"));
        assert_eq!(column_qualifier(statement, "BAR"), None);
        let sources = query_sources(statement);
        assert_eq!(candidate_sources(&sources, Some("T")).len(), 1);
        assert!(candidate_sources(&sources, Some("X")).is_empty());
    }
}

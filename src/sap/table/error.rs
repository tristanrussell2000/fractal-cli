use std::{fmt, sync::LazyLock};

use regex::Regex;
use thiserror::Error;

use super::{TableColumn, TableDdlParseError};
use crate::sap::client::SapClientError;
use crate::sap::object_source::ObjectSourceError;
use crate::suggested_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableQueryErrorKind {
    UnknownColumn,
    InvalidSyntax,
    UnknownSource,
}

impl TableQueryErrorKind {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownColumn => "table_query_unknown_column",
            Self::InvalidSyntax => "table_query_invalid_syntax",
            Self::UnknownSource => "table_query_unknown_source",
        }
    }
}

/// Structured details extracted from a SAP `OpenSQL` error message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableQueryError {
    pub kind: TableQueryErrorKind,
    /// The one DDIC entity queried, when there is one. A complete
    /// caller-authored statement may join several sources, so no single
    /// entity can be named and field remedies are not derivable.
    pub entity: Option<String>,
    pub identifier: Option<String>,
    pub suggestions: Vec<String>,
    pub message: String,
}

impl fmt::Display for TableQueryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.kind, self.identifier.as_deref()) {
            (TableQueryErrorKind::UnknownColumn, Some(column)) => {
                write!(
                    formatter,
                    "unknown table column '{column}': {}",
                    self.message
                )
            }
            (TableQueryErrorKind::InvalidSyntax, Some(token)) => {
                write!(
                    formatter,
                    "invalid table query near '{token}': {}",
                    self.message
                )
            }
            (TableQueryErrorKind::UnknownSource, Some(source)) => {
                write!(
                    formatter,
                    "unknown table or view '{source}': {}",
                    self.message
                )
            }
            _ => formatter.write_str(&self.message),
        }
    }
}

#[derive(Debug, Error)]
pub enum TableError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error("could not fetch table DDL source: {0}")]
    DdlSource(#[source] ObjectSourceError),
    #[error("could not parse table DDL source: {0}")]
    DdlParse(#[from] TableDdlParseError),
    #[error("{query}")]
    Query {
        query: TableQueryError,
        #[source]
        source: Box<SapClientError>,
    },
    #[error("could not parse table data response: {0}")]
    Parse(String),
    #[error("SAP's table count response did not contain a numeric row count")]
    CountMissing,
    #[error("invalid DDIC entity name: {0}")]
    InvalidEntityName(String),
    #[error("invalid field name: {field}")]
    InvalidFieldName { entity: String, field: String },
    #[error("the where clause must not contain a semicolon")]
    WhereContainsSemicolon,
    #[error("a full table query is required")]
    QueryRequired,
    #[error("a full table query must begin with SELECT")]
    QueryMustBeSelect,
    #[error("a full table query must not contain a semicolon")]
    QueryContainsSemicolon,
    #[error(
        "the requested table-data range ends at row {requested}, above the maximum of {maximum}"
    )]
    PreviewRangeTooLarge { requested: usize, maximum: usize },
}

impl TableError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::DdlSource(error) => error.code(),
            Self::DdlParse(_) => "table_ddl_parse_error",
            Self::Query { query, .. } => query.kind.code(),
            Self::Parse(_) => "table_response_parse_error",
            Self::CountMissing => "table_count_response_error",
            Self::InvalidEntityName(_) => "invalid_table_entity_name",
            Self::InvalidFieldName { .. } => "invalid_table_field_name",
            Self::WhereContainsSemicolon => "where_contains_semicolon",
            Self::QueryRequired => "table_query_required",
            Self::QueryMustBeSelect => "table_query_must_be_select",
            Self::QueryContainsSemicolon => "table_query_contains_semicolon",
            Self::PreviewRangeTooLarge { .. } => "table_preview_range_too_large",
        }
    }

    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => Some(error.hint()),
            Self::DdlSource(error) => Some(error.hint()),
            Self::DdlParse(_) => Some(
                "The SAP table source did not match the expected `define table` DDL format."
                    .to_owned(),
            ),
            Self::Query { query, .. } => Some(query_hint(query)),
            Self::Parse(_) => Some(
                "The SAP table data response did not match the expected dataPreview format."
                    .to_owned(),
            ),
            Self::CountMissing => Some(
                "Retry the count; if it persists, inspect the freestyle dataPreview response."
                    .to_owned(),
            ),
            Self::InvalidEntityName(_) => Some(
                "Use a DDIC table, view, or CDS view name: letters, digits, underscore, and slash only."
                    .to_owned(),
            ),
            Self::InvalidFieldName { entity, .. } => Some(format!(
                "Field names may only contain letters, digits, and underscore. Run `{}` to list this entity's fields.",
                suggested_command::table_metadata(entity)
            )),
            Self::WhereContainsSemicolon => Some(
                "Semicolons are rejected to prevent multi-statement injection; express the filter as a single WHERE fragment, or use `fractal query` for a full statement."
                    .to_owned(),
            ),
            Self::QueryRequired => Some("Provide a complete SELECT statement.".to_owned()),
            Self::QueryMustBeSelect => Some(
                "Only SELECT statements are accepted by the table-data query mode.".to_owned(),
            ),
            Self::QueryContainsSemicolon => Some(
                "Remove the semicolon and provide exactly one SELECT statement.".to_owned(),
            ),
            Self::PreviewRangeTooLarge { maximum, .. } => Some(format!(
                "Reduce --offset or --limit so offset + limit is at most {maximum}."
            )),
        }
    }

    /// A read-only command that diagnoses this failure, if one exists.
    #[must_use]
    pub fn suggested_command(&self) -> Option<String> {
        match self {
            // The Levenshtein suggestions name likely columns; this names the
            // command that lists every valid one.
            Self::Query {
                query:
                    TableQueryError {
                        kind: TableQueryErrorKind::UnknownColumn,
                        entity: Some(entity),
                        ..
                    },
                ..
            }
            | Self::InvalidFieldName { entity, .. } => {
                Some(suggested_command::table_metadata(entity))
            }
            Self::Sap(error) => error.suggested_command(),
            Self::Query { source, .. } => source.suggested_command(),
            _ => None,
        }
    }

    #[must_use]
    pub fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) | Self::DdlSource(ObjectSourceError::Sap(error)) => Some(error),
            Self::Query { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

fn query_hint(error: &TableQueryError) -> String {
    match error.kind {
        TableQueryErrorKind::UnknownColumn if !error.suggestions.is_empty() => format!(
            "Check the column name. Closest columns: {}.",
            error.suggestions.join(", ")
        ),
        TableQueryErrorKind::UnknownColumn => {
            "Check the selected fields and the columns exposed by the table or view.".to_owned()
        }
        TableQueryErrorKind::InvalidSyntax => {
            "Check the OpenSQL syntax around the reported token or expression.".to_owned()
        }
        TableQueryErrorKind::UnknownSource => {
            "Check the table or view name and whether it is available in this SAP system."
                .to_owned()
        }
    }
}

struct QueryErrorPattern {
    kind: TableQueryErrorKind,
    regex: Regex,
}

static QUERY_ERROR_PATTERNS: LazyLock<[QueryErrorPattern; 3]> = LazyLock::new(|| {
    [
        QueryErrorPattern {
            kind: TableQueryErrorKind::UnknownColumn,
            regex: Regex::new(r#"(?i)^Unknown column name\s+"([^"]+)"\.?$"#)
                .expect("unknown-column regex is valid"),
        },
        QueryErrorPattern {
            kind: TableQueryErrorKind::InvalidSyntax,
            regex: Regex::new(r#"(?i)^"([^"]+)"\s+is invalid here\s+\(due to grammar\)\.?$"#)
                .expect("invalid-syntax regex is valid"),
        },
        QueryErrorPattern {
            kind: TableQueryErrorKind::UnknownSource,
            regex: Regex::new(r"(?i)^Cannot find\s+'([^']+)'\.?$")
                .expect("unknown-source regex is valid"),
        },
    ]
});

pub(super) fn classify_query_error(
    error: SapClientError,
    columns: &[TableColumn],
    entity: Option<&str>,
) -> TableError {
    let SapClientError::Http { message, .. } = &error else {
        return TableError::Sap(error);
    };
    let Some((kind, identifier)) = classify_query_message(message) else {
        return TableError::Sap(error);
    };

    let suggestions = if kind == TableQueryErrorKind::UnknownColumn {
        closest_columns(&identifier, columns)
    } else {
        Vec::new()
    };
    let query = TableQueryError {
        kind,
        entity: entity.map(str::to_owned),
        identifier: Some(identifier),
        suggestions,
        message: message.clone(),
    };
    TableError::Query {
        query,
        source: Box::new(error),
    }
}

fn classify_query_message(message: &str) -> Option<(TableQueryErrorKind, String)> {
    QUERY_ERROR_PATTERNS.iter().find_map(|pattern| {
        let captures = pattern.regex.captures(message)?;
        let identifier = captures.get(1)?.as_str().to_owned();
        Some((pattern.kind, identifier))
    })
}

fn closest_columns(target: &str, columns: &[TableColumn]) -> Vec<String> {
    let target = target.to_ascii_uppercase();
    let maximum_distance = (target.len() / 3).clamp(2, 5);
    let mut matches: Vec<(usize, &str)> = columns
        .iter()
        .map(|column| {
            (
                levenshtein(&target, &column.name.to_ascii_uppercase()),
                column.name.as_str(),
            )
        })
        .filter(|(distance, _)| *distance <= maximum_distance)
        .collect();
    matches.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(right.1)));
    matches
        .into_iter()
        .take(3)
        .map(|(_, name)| name.to_owned())
        .collect()
}

fn levenshtein(left: &str, right: &str) -> usize {
    let mut previous: Vec<usize> = (0..=right.len()).collect();
    let mut current = vec![0; right.len() + 1];

    for (left_index, left_byte) in left.bytes().enumerate() {
        current[0] = left_index + 1;
        for (right_index, right_byte) in right.bytes().enumerate() {
            let substitution = previous[right_index] + usize::from(left_byte != right_byte);
            let insertion = current[right_index] + 1;
            let deletion = previous[right_index + 1] + 1;
            current[right_index + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }

    previous[right.len()]
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;
    use crate::sap::client::SapHttpErrorKind;

    fn http_error(message: &str) -> SapClientError {
        SapClientError::Http {
            kind: SapHttpErrorKind::Other,
            status: StatusCode::BAD_REQUEST,
            url: "https://sap.example/sap/bc/adt/datapreview/freestyle".to_owned(),
            message: message.to_owned(),
        }
    }

    fn column(name: &str) -> TableColumn {
        TableColumn {
            name: name.to_owned(),
            sap_type: None,
            col_type: None,
            length: None,
            description: None,
        }
    }

    #[test]
    fn classifies_the_observed_query_error_formats_in_order() {
        let cases = [
            (
                "Unknown column name \"MISSING_FIELD\".",
                TableQueryErrorKind::UnknownColumn,
                "MISSING_FIELD",
                "table_query_unknown_column",
            ),
            (
                "\"WHERE\" is invalid here (due to grammar).",
                TableQueryErrorKind::InvalidSyntax,
                "WHERE",
                "table_query_invalid_syntax",
            ),
            (
                "Cannot find 'ZUNKNOWN_DATA'",
                TableQueryErrorKind::UnknownSource,
                "ZUNKNOWN_DATA",
                "table_query_unknown_source",
            ),
        ];

        for (message, expected_kind, expected_identifier, expected_code) in cases {
            let error = classify_query_error(http_error(message), &[], None);
            assert_eq!(error.code(), expected_code);
            assert!(matches!(
                &error,
                TableError::Query { query, .. }
                    if query.kind == expected_kind
                        && query.identifier.as_deref() == Some(expected_identifier)
                        && query.message == message
            ));
            assert!(matches!(
                error.sap_error(),
                Some(SapClientError::Http {
                    status: StatusCode::BAD_REQUEST,
                    ..
                })
            ));
        }
    }

    #[test]
    fn suggests_the_closest_columns_for_an_unknown_column() {
        let columns = [column("EVENT_ID"), column("EVENT_DATE"), column("STATUS")];
        let error = classify_query_error(
            http_error("Unknown column name \"EVNT_ID\"."),
            &columns,
            Some("ZDEMO_EVENT_LOG"),
        );

        let TableError::Query { query, .. } = &error else {
            panic!("expected a structured query error");
        };
        assert_eq!(query.suggestions, vec!["EVENT_ID"]);
        assert!(error.hint().unwrap().contains("EVENT_ID"));
    }

    #[test]
    fn preserves_unrecognized_sap_errors_without_reclassification() {
        let error = classify_query_error(http_error("Request could not be processed"), &[], None);
        assert!(matches!(
            error,
            TableError::Sap(SapClientError::Http { .. })
        ));
        assert_eq!(error.code(), "http_error");
    }

    #[test]
    fn computes_levenshtein_distance_with_linear_working_space() {
        assert_eq!(levenshtein("EVENT_ID", "EVNT_ID"), 1);
        assert_eq!(levenshtein("STATUS", "STATUS"), 0);
        assert_eq!(levenshtein("", "CLIENT"), 6);
    }
}

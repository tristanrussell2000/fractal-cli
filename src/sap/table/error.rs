use std::{fmt, sync::LazyLock};

use regex::Regex;
use thiserror::Error;

use super::TableColumn;
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::sap::client::SapClientError;
use crate::suggested_command;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableQueryErrorKind {
    UnknownColumn,
    InvalidSyntax,
    UnknownSource,
    AmbiguousColumn,
    ClientFieldInJoin,
    TypeMismatch,
    UnknownAlias,
    MissingGroupBy,
}

impl TableQueryErrorKind {
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::UnknownColumn => "table_query_unknown_column",
            Self::InvalidSyntax => "table_query_invalid_syntax",
            Self::UnknownSource => "table_query_unknown_source",
            Self::AmbiguousColumn => "table_query_ambiguous_column",
            Self::ClientFieldInJoin => "table_query_client_field_in_join",
            Self::TypeMismatch => "table_query_type_mismatch",
            Self::UnknownAlias => "table_query_unknown_alias",
            Self::MissingGroupBy => "table_query_missing_group_by",
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
    /// Names to use instead: the closest columns to an unknown one, the
    /// qualified forms of an ambiguous one, or the tables and aliases in scope.
    pub suggestions: Vec<String>,
    /// The column's own type, for a type mismatch, when it could be read.
    pub column_type: Option<String>,
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
            (TableQueryErrorKind::AmbiguousColumn, Some(column)) => {
                write!(formatter, "ambiguous column '{column}': {}", self.message)
            }
            (TableQueryErrorKind::UnknownAlias, Some(alias)) => {
                write!(
                    formatter,
                    "unknown table or alias '{alias}': {}",
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
    #[error("SAP records no active fields for {entity}")]
    EntityFieldsMissing { entity: String },
    #[error("{entity} has more fields than one read returns ({limit})")]
    FieldListTruncated { entity: String, limit: usize },
    #[error("SAP's field list for {entity} was missing a column it needs")]
    FieldListUnreadable { entity: String },
    #[error("{query}")]
    Query {
        query: Box<TableQueryError>,
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
    pub fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            Self::Query { source, .. } => Some(source.as_ref()),
            _ => None,
        }
    }
}

impl ReportableError for TableError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::EntityFieldsMissing { .. } => "table_fields_missing",
            Self::FieldListTruncated { .. } => "table_field_list_truncated",
            Self::FieldListUnreadable { .. } => "table_field_list_unreadable",
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

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::EntityFieldsMissing { .. } => Some(
                "Check the name exists and is active. Field definitions are read from DD03L."
                    .to_owned(),
            ),
            Self::FieldListTruncated { .. } => Some(
                "This entity has an unusually large field list; Fractal cannot describe it safely."
                    .to_owned(),
            ),
            Self::FieldListUnreadable { .. } => Some(
                "The DD03L read did not return the expected columns.".to_owned(),
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
    fn suggested_command(&self) -> Option<String> {
        match self {
            // The Levenshtein suggestions name likely columns; this names the
            // command that lists every valid one.
            Self::Query { query, source } if query.kind == TableQueryErrorKind::UnknownColumn => {
                query
                    .entity
                    .as_deref()
                    .map(suggested_command::table_metadata)
                    .or_else(|| source.suggested_command())
            }
            Self::InvalidFieldName { entity, .. } => {
                Some(suggested_command::table_metadata(entity))
            }
            Self::Sap(error) => error.suggested_command(),
            Self::Query { source, .. } => source.suggested_command(),
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
        TableQueryErrorKind::UnknownColumn => error.entity.as_ref().map_or_else(
            || "Check the selected fields and the columns exposed by the table or view.".to_owned(),
            |entity| {
                format!(
                    "{entity} has no column like that. Run `{}` for its columns.",
                    suggested_command::table_metadata(entity)
                )
            },
        ),
        TableQueryErrorKind::AmbiguousColumn if !error.suggestions.is_empty() => format!(
            "More than one joined table has this column; qualify it: {}.",
            error.suggestions.join(" or ")
        ),
        TableQueryErrorKind::AmbiguousColumn => {
            "More than one joined table has this column; qualify it as table~column or alias~column."
                .to_owned()
        }
        TableQueryErrorKind::ClientFieldInJoin => {
            "Leave the client field out of the ON condition: SAP joins on the logon client itself."
                .to_owned()
        }
        TableQueryErrorKind::TypeMismatch => format!(
            "{}Compare it with a value of that type: dates as 'YYYYMMDD', times as 'HHMMSS', numbers unquoted.",
            error.column_type.as_ref().map_or_else(String::new, |column_type| format!(
                "{} is {column_type}. ",
                error.identifier.as_deref().unwrap_or("The column")
            ))
        ),
        TableQueryErrorKind::UnknownAlias if !error.suggestions.is_empty() => format!(
            "Qualify columns with a table or alias from the FROM clause: {}. Once a table has an alias, only the alias works.",
            error.suggestions.join(", ")
        ),
        TableQueryErrorKind::UnknownAlias => {
            "Qualify columns with a table or alias from the FROM clause.".to_owned()
        }
        TableQueryErrorKind::MissingGroupBy => {
            "Add the field to GROUP BY, or wrap it in an aggregate such as MAX or COUNT.".to_owned()
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

static QUERY_ERROR_PATTERNS: LazyLock<[QueryErrorPattern; 8]> = LazyLock::new(|| {
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
        QueryErrorPattern {
            kind: TableQueryErrorKind::AmbiguousColumn,
            regex: Regex::new(r#"(?i)^The column name or association\s+"([^"]+)"\s+is ambiguous"#)
                .expect("ambiguous-column regex is valid"),
        },
        QueryErrorPattern {
            kind: TableQueryErrorKind::ClientFieldInJoin,
            regex: Regex::new(r#"(?i)^The client field\s+"([^"]+)"\s+cannot be specified in the ON condition"#)
                .expect("client-field regex is valid"),
        },
        QueryErrorPattern {
            kind: TableQueryErrorKind::TypeMismatch,
            regex: Regex::new(r#"(?i)^The type of\s+"([^"]+)".*\bare not compatible\b"#)
                .expect("type-mismatch regex is valid"),
        },
        QueryErrorPattern {
            kind: TableQueryErrorKind::UnknownAlias,
            regex: Regex::new(r#"(?i)^Incorrect table name or table alias name\s+"([^"]+)""#)
                .expect("unknown-alias regex is valid"),
        },
        QueryErrorPattern {
            kind: TableQueryErrorKind::MissingGroupBy,
            regex: Regex::new(r#"(?i)^The field\s+"([^"]+)"\s+from the SELECT list is missing in the GROUP BY clause"#)
                .expect("missing-group-by regex is valid"),
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
        closest_names(
            &identifier,
            columns
                .iter()
                .map(|column| (column.name.as_str(), column.name.clone())),
        )
    } else {
        Vec::new()
    };
    let query = TableQueryError {
        kind,
        entity: entity.map(str::to_owned),
        identifier: Some(identifier),
        suggestions,
        column_type: None,
        message: message.clone(),
    };
    TableError::Query {
        query: Box::new(query),
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

/// The labels of up to three candidates whose names are closest to `target`.
pub(super) fn closest_names<'a>(
    target: &str,
    candidates: impl IntoIterator<Item = (&'a str, String)>,
) -> Vec<String> {
    let target = target.to_ascii_uppercase();
    let maximum_distance = (target.len() / 3).clamp(2, 5);
    let mut matches: Vec<(usize, String)> = candidates
        .into_iter()
        .map(|(name, label)| (levenshtein(&target, &name.to_ascii_uppercase()), label))
        .filter(|(distance, _)| *distance <= maximum_distance)
        .collect();
    matches.sort();
    matches.dedup_by(|left, right| left.1 == right.1);
    matches
        .into_iter()
        .take(3)
        .map(|(_, label)| label)
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
            (
                "The column name or association \"TRKORR\" is ambiguous, which means it occurs in multiple tables.",
                TableQueryErrorKind::AmbiguousColumn,
                "TRKORR",
                "table_query_ambiguous_column",
            ),
            (
                "The client field \"MANDT\" cannot be specified in the ON condition. Client handling is performed by the compiler.",
                TableQueryErrorKind::ClientFieldInJoin,
                "MANDT",
                "table_query_client_field_in_join",
            ),
            (
                "The type of \"AS4DATE\" or the result type of the function \"AS4DATE\" and the type of \"'x'\" are not compatible for the operation in question.",
                TableQueryErrorKind::TypeMismatch,
                "AS4DATE",
                "table_query_type_mismatch",
            ),
            (
                "Incorrect table name or table alias name \"X\".",
                TableQueryErrorKind::UnknownAlias,
                "X",
                "table_query_unknown_alias",
            ),
            (
                "The field \"AS4USER\" from the SELECT list is missing in the GROUP BY clause.",
                TableQueryErrorKind::MissingGroupBy,
                "AS4USER",
                "table_query_missing_group_by",
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

    fn diagnosed(
        kind: TableQueryErrorKind,
        identifier: &str,
        suggestions: &[&str],
        column_type: Option<&str>,
    ) -> TableError {
        TableError::Query {
            query: Box::new(TableQueryError {
                kind,
                entity: None,
                identifier: Some(identifier.to_owned()),
                suggestions: suggestions.iter().map(|&s| s.to_owned()).collect(),
                column_type: column_type.map(str::to_owned),
                message: String::new(),
            }),
            source: Box::new(http_error("")),
        }
    }

    #[test]
    fn hints_use_what_the_diagnosis_found() {
        let ambiguous = diagnosed(
            TableQueryErrorKind::AmbiguousColumn,
            "TRKORR",
            &["E071~TRKORR", "E070~TRKORR"],
            None,
        );
        assert!(
            ambiguous
                .hint()
                .unwrap()
                .ends_with("qualify it: E071~TRKORR or E070~TRKORR.")
        );
        let mismatch = diagnosed(
            TableQueryErrorKind::TypeMismatch,
            "AS4DATE",
            &[],
            Some("DATS, length 8"),
        );
        assert!(
            mismatch
                .hint()
                .unwrap()
                .starts_with("AS4DATE is DATS, length 8.")
        );
        let alias = diagnosed(TableQueryErrorKind::UnknownAlias, "X", &["T", "E071"], None);
        assert!(alias.hint().unwrap().contains(": T, E071."));
    }

    #[test]
    fn an_unknown_column_in_a_known_table_names_its_metadata_command() {
        let TableError::Query { mut query, source } =
            diagnosed(TableQueryErrorKind::UnknownColumn, "ZZZ", &[], None)
        else {
            unreachable!()
        };
        query.entity = Some("ZSAMPLE_LOG".to_owned());
        let error = TableError::Query { query, source };
        assert!(
            error
                .hint()
                .unwrap()
                .starts_with("ZSAMPLE_LOG has no column like that")
        );
        assert_eq!(
            error.suggested_command().as_deref(),
            Some("fractal table metadata ZSAMPLE_LOG")
        );
    }

    #[test]
    fn closest_names_keep_their_labels_and_drop_far_ones() {
        let names = closest_names(
            "OBJNAME",
            [
                ("OBJ_NAME", "O~OBJ_NAME".to_owned()),
                ("TRKORR", "O~TRKORR".to_owned()),
            ],
        );
        assert_eq!(names, vec!["O~OBJ_NAME"]);
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

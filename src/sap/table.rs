mod fetch;
mod parse;

use thiserror::Error;

use super::client::SapError;

pub use fetch::{TableDataOptions, TableDataQuery, get_table_data};
pub use parse::parse_table_data;

#[derive(Debug, Error)]
pub enum TableError {
    #[error(transparent)]
    Sap(#[from] SapError),
    #[error("could not parse table data response: {0}")]
    Parse(String),
    #[error("invalid DDIC entity name: {0}")]
    InvalidEntityName(String),
    #[error("invalid field name: {0}")]
    InvalidFieldName(String),
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
            Self::Parse(_) => "table_response_parse_error",
            Self::InvalidEntityName(_) => "invalid_table_entity_name",
            Self::InvalidFieldName(_) => "invalid_table_field_name",
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
            Self::Sap(error) => Some(error.hint().to_owned()),
            Self::Parse(_) => Some(
                "The SAP table data response did not match the expected dataPreview format."
                    .to_owned(),
            ),
            Self::InvalidEntityName(_) => Some(
                "Use a DDIC table, view, or CDS view name: letters, digits, underscore, and slash only."
                    .to_owned(),
            ),
            Self::InvalidFieldName(_) => Some(
                "Field names may only contain letters, digits, and underscore.".to_owned(),
            ),
            Self::WhereContainsSemicolon => Some(
                "Semicolons are rejected to prevent multi-statement injection; express the filter as a single WHERE fragment, or use --query for a full statement."
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
}

/// One column's metadata from a `dataPreview` response.
///
/// `sap_type` is SAP's single-character ABAP type code (`C`, `X`, `N`, `P`, ...);
/// `col_type` is the friendlier name (`CHAR`, `RAW`, `NUMC`, `DEC`, ...). Both
/// this and `length`/`description` are frequently absent on the freestyle-query
/// path, which only echoes bare column names unless enriched from elsewhere —
/// `keyAttribute`/`isKeyFigure` are deliberately not modeled here: protocol
/// captures showed `keyAttribute="false"` on an actual DDIC primary key, so it
/// does not reflect SQL primary-key status (this codebase reads key fields from
/// a table's DDL source instead — see `object source`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableColumn {
    pub name: String,
    pub sap_type: Option<String>,
    pub col_type: Option<String>,
    pub length: Option<u32>,
    pub description: Option<String>,
}

/// A parsed `dataPreview:tableData` response, shared by both the cheap
/// (`datapreview/ddic`) and freestyle (`datapreview/freestyle`) ADT endpoints.
///
/// `entity` is only present on the cheap path; `executed_query` only on the
/// freestyle path. `total_rows` is present on both but is only trustworthy on
/// the freestyle path — protocol captures show that the cheap path's value
/// reflects the preview window rather than the table's real size. The
/// higher-level table fetch attempts to replace that value with a separate
/// `COUNT(*)` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDataResult {
    pub entity: Option<String>,
    pub executed_query: Option<String>,
    pub total_rows: Option<u64>,
    pub columns: Vec<TableColumn>,
    pub rows: Vec<Vec<String>>,
}

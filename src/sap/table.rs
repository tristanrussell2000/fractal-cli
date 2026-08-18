mod ddl;
mod error;
mod fetch;
mod parse;

pub use ddl::{TableDdl, TableDdlField, TableDdlParseError, parse_table_ddl};
pub use error::{TableError, TableQueryError, TableQueryErrorKind};
pub use fetch::{QueryOptions, TableDataOptions, get_table_data, get_table_ddl, run_query};
pub use parse::parse_table_data;

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

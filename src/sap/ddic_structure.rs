//! Inspection of DDIC objects that have a flat field list — structures,
//! tables and append structures.
//!
//! The DDL source is not the field list: an `include` names another structure
//! whose fields are spliced in but never shown, and an append does not appear
//! in the source at all. The recorded list in `DD03L` holds both kinds already
//! flattened, so that is what this reads.
//!
//! **What came back is not necessarily a structure.** SAP's `structures`
//! collection serves transparent tables and append structures through the same
//! route, and a transparent table is a table and nothing more — there is one
//! `DD02L` row for `MARA`, not a table plus a structure of the same name. It
//! is reachable here because ABAP treats a table name as a row type, so the
//! field list is a sensible answer to give; it is reported as a table because
//! that is what it is.
//!
//! **The document cannot be asked what it is.** ADT derives `adtcore:type`
//! from the collection in the URI, not from the object: the same `MARA` comes
//! back as `TABL/DS` through `ddic/structures` and `TABL/DT` through
//! `ddic/tables`. `DD02L-TABCLASS` is the only answer that does not depend on
//! how it was asked.
//!
//! The header — description, package, which layer arrived — still comes from
//! the object's own ADT document, so it reads the same way every other object
//! in Fractal does.

use serde::Serialize;
use thiserror::Error;

use super::{
    adt_response::{AdtResponseParseError, parse_adt_document},
    adt_version::AdtVersion,
    client::{SapClient, SapClientError},
    editable_source::validate_object_name,
    find_child, find_non_empty_attribute,
    metadata_document::declared_version,
    table::{QueryOptions, TableError, run_query},
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

const STRUCTURE_COLLECTION: &str = "/sap/bc/adt/ddic/structures";
/// Wider than any real structure, and a full page means fields may be missing.
const MAX_FIELDS: usize = 2_000;
/// The language field descriptions are read in.
const TEXT_LANGUAGE: &str = "E";

/// One structure, with every field an include or append contributed in place.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdicStructureInfo {
    pub name: String,
    /// What `DD02L` says this is — a structure, a table, an append structure.
    /// Never taken from the document, whose `adtcore:type` reflects the URI it
    /// was fetched through rather than the object.
    pub kind: String,
    pub uri: String,
    /// The layer that was asked for.
    pub requested_version: &'static str,
    /// The layer the document declared itself to be.
    pub version: Option<String>,
    pub description: Option<String>,
    pub package: Option<String>,
    pub field_count: usize,
    pub key_field_count: usize,
    pub fields: Vec<DdicStructureField>,
}

/// One field, as `DD03L` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdicStructureField {
    pub name: String,
    pub is_key: bool,
    /// The data element the field is typed with, absent when it is typed from
    /// a built-in type directly.
    pub data_element: Option<String>,
    pub domain: Option<String>,
    /// The DDIC type (`CHAR`, `CLNT`, `NUMC`, ...).
    pub col_type: Option<String>,
    /// The ABAP runtime type kind (`C`, `N`, `P`, ...).
    pub sap_type: Option<String>,
    pub length: Option<u32>,
    pub decimals: Option<u32>,
    pub not_null: bool,
    /// The table a foreign key checks against.
    pub check_table: Option<String>,
    pub description: Option<String>,
}

#[derive(Debug, Error)]
pub enum DdicStructureError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error(transparent)]
    Parse(#[from] AdtResponseParseError),
    #[error(transparent)]
    Fields(#[from] TableError),
    #[error("invalid DDIC object name '{0}'")]
    InvalidName(String),
    #[error("{name} records no active fields")]
    NoFields { name: String },
    #[error("{name} has more fields than one read returns ({limit})")]
    TooManyFields { name: String, limit: usize },
    #[error("SAP's field list for {name} was missing a column it needs")]
    Unreadable { name: String },
}

impl DdicStructureError {
    #[must_use]
    pub fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            Self::Fields(error) => error.sap_error(),
            _ => None,
        }
    }
}

impl ReportableError for DdicStructureError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(error) => error.code(),
            Self::Fields(error) => error.code(),
            Self::InvalidName(_) => "invalid_object_name",
            Self::NoFields { .. } => "ddic_structure_no_fields",
            Self::TooManyFields { .. } => "ddic_structure_too_many_fields",
            Self::Unreadable { .. } => "ddic_structure_field_list_unreadable",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(error) => error.hint(),
            Self::Fields(error) => error.hint(),
            Self::InvalidName(_) => {
                Some("Use a DDIC name: letters, digits, underscore and slash only.".to_owned())
            }
            Self::NoFields { .. } => {
                Some("Check the name exists and has been activated.".to_owned())
            }
            Self::TooManyFields { .. } => {
                Some("This object is too wide for Fractal to describe safely.".to_owned())
            }
            Self::Unreadable { .. } => {
                Some("The DD03L read did not return the expected columns.".to_owned())
            }
        }
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::NoFields { name } => Some(suggested_command::object_search("STRU", name)),
            _ => None,
        }
    }
}

/// Reads one structure's header and complete field list.
///
/// # Errors
///
/// Returns [`DdicStructureError`] when the name is invalid, SAP cannot serve
/// the document or the field list, or either response cannot be parsed.
pub async fn get_ddic_structure(
    sap: &mut SapClient,
    name: &str,
    version: AdtVersion,
) -> Result<DdicStructureInfo, DdicStructureError> {
    let name =
        validate_object_name(name).map_err(|_| DdicStructureError::InvalidName(name.to_owned()))?;
    let uri = structure_uri(&name);

    let xml = sap
        .get_text_with_query(&uri, &[("version", version.as_str())])
        .await?;
    let (kind, fields) = read_fields(sap, &name, version).await?;

    let document = parse_adt_document(&xml)?;
    let root = document.root_element();
    Ok(DdicStructureInfo {
        name: find_non_empty_attribute(root, "name").unwrap_or_else(|| name.clone()),
        kind,
        uri,
        requested_version: version.as_str(),
        version: declared_version(root),
        description: find_non_empty_attribute(root, "description"),
        package: find_child(root, "packageRef")
            .and_then(|node| find_non_empty_attribute(node, "name")),
        field_count: fields.len(),
        key_field_count: fields.iter().filter(|field| field.is_key).count(),
        fields,
    })
}

async fn read_fields(
    sap: &mut SapClient,
    name: &str,
    version: AdtVersion,
) -> Result<(String, Vec<DdicStructureField>), DdicStructureError> {
    let result = run_query(
        sap,
        &field_query(name, version),
        &QueryOptions {
            offset: 0,
            limit: MAX_FIELDS,
        },
    )
    .await?;

    if result.rows.len() >= MAX_FIELDS {
        return Err(DdicStructureError::TooManyFields {
            name: name.to_owned(),
            limit: MAX_FIELDS,
        });
    }

    let fields = parse_fields(name, &result.columns, &result.rows)?;
    if fields.is_empty() {
        return Err(DdicStructureError::NoFields {
            name: name.to_owned(),
        });
    }
    Ok((read_kind(&result.columns, &result.rows), fields))
}

/// `TABCLASS` is the same on every row, so the first one answers.
fn read_kind(columns: &[super::table::TableColumn], rows: &[Vec<String>]) -> String {
    let kind = columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case("TABCLASS"))
        .and_then(|index| rows.first()?.get(index))
        .map_or("", |value| value.trim());

    // Only the three classes this collection actually serves are named.
    // `VIEW` is not among them — `ddic/structures/<view>` is a 404 — and
    // pooled and cluster tables do not exist on S/4HANA at all. Anything else
    // is passed through as SAP spelled it rather than guessed at a label.
    match kind.to_ascii_uppercase().as_str() {
        "INTTAB" => "Structure".to_owned(),
        "TRANSP" => "Table".to_owned(),
        "APPEND" => "Append structure".to_owned(),
        "" => "Structure".to_owned(),
        other => other.to_owned(),
    }
}

/// The `DD03L` columns read, in the order the query asks for them.
const COLUMNS: [&str; 11] = [
    "POSITION",
    "FIELDNAME",
    "KEYFLAG",
    "ROLLNAME",
    "DOMNAME",
    "DATATYPE",
    "LENG",
    "DECIMALS",
    "INTTYPE",
    "NOTNULL",
    "CHECKTABLE",
];

/// `DD04T` supplies the description, which `DD03L` does not carry. The join is
/// outer because a field typed from a built-in type has no data element to
/// join to, and pinned to one language because `DD04T` holds one row per
/// installed language and an unpinned join multiplies the field list.
fn field_query(name: &str, version: AdtVersion) -> String {
    let selected = COLUMNS
        .iter()
        .map(|column| format!("f~{column}"))
        .collect::<Vec<_>>()
        .join(", ");
    let layer = field_layer(version);
    format!(
        "SELECT {selected}, t~ddtext, h~tabclass FROM dd03l AS f \
         INNER JOIN dd02l AS h \
         ON h~tabname = f~tabname AND h~as4local = f~as4local \
         LEFT OUTER JOIN dd04t AS t \
         ON t~rollname = f~rollname AND t~as4local = f~as4local \
         AND t~ddlanguage = '{TEXT_LANGUAGE}' \
         WHERE f~tabname = '{name}' AND f~as4local = '{layer}' ORDER BY f~position"
    )
}

/// `DD03L` names its layers `A` and `N`, not the words ADT uses.
fn field_layer(version: AdtVersion) -> &'static str {
    match version {
        AdtVersion::Inactive => "N",
        _ => "A",
    }
}

fn parse_fields(
    name: &str,
    columns: &[super::table::TableColumn],
    rows: &[Vec<String>],
) -> Result<Vec<DdicStructureField>, DdicStructureError> {
    let mut indexes = [0_usize; COLUMNS.len() + 1];
    for (slot, column) in indexes
        .iter_mut()
        .zip(COLUMNS.iter().copied().chain(["DDTEXT"]))
    {
        *slot = columns
            .iter()
            .position(|candidate| candidate.name.eq_ignore_ascii_case(column))
            .ok_or_else(|| DdicStructureError::Unreadable {
                name: name.to_owned(),
            })?;
    }

    Ok(rows
        .iter()
        .filter_map(|row| {
            let cell = |slot: usize| row.get(indexes[slot]).map_or("", |value| value.trim());
            let field = cell(1);
            // `.INCLUDE` and `.INCLU--AP` mark where a structure or an append
            // was spliced in. The fields themselves are already listed.
            if field.is_empty() || field.starts_with('.') {
                return None;
            }

            Some(DdicStructureField {
                name: field.to_ascii_lowercase(),
                is_key: cell(2).eq_ignore_ascii_case("X"),
                data_element: lowercased(cell(3)),
                domain: lowercased(cell(4)),
                col_type: (!cell(5).is_empty()).then(|| cell(5).to_ascii_uppercase()),
                sap_type: (!cell(8).is_empty()).then(|| cell(8).to_ascii_uppercase()),
                length: number(cell(6)),
                decimals: number(cell(7)),
                not_null: cell(9).eq_ignore_ascii_case("X"),
                // SAP writes `*` for "any table", which names nothing.
                check_table: lowercased(cell(10)).filter(|table| table != "*"),
                description: (!cell(11).is_empty()).then(|| cell(11).to_owned()),
            })
        })
        .collect())
}

fn lowercased(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_ascii_lowercase())
}

fn number(value: &str) -> Option<u32> {
    value.trim_start_matches('0').parse().ok()
}

fn structure_uri(name: &str) -> String {
    let path_name = name.to_ascii_lowercase().replace('/', "%2f");
    format!("{STRUCTURE_COLLECTION}/{path_name}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::table::TableColumn;

    fn columns() -> Vec<TableColumn> {
        COLUMNS
            .iter()
            .copied()
            .chain(["DDTEXT", "TABCLASS"])
            .map(|name| TableColumn {
                name: name.to_owned(),
                sap_type: None,
                col_type: None,
                length: None,
                description: None,
            })
            .collect()
    }

    fn rows(rows: Vec<Vec<&str>>) -> Vec<Vec<String>> {
        rows.into_iter()
            .map(|row| row.into_iter().map(str::to_owned).collect())
            .collect()
    }

    #[test]
    fn names_the_structure_and_pins_the_text_language() {
        let sql = field_query("ZSAMPLE_RECORD_S", AdtVersion::Active);
        assert!(sql.contains("f~tabname = 'ZSAMPLE_RECORD_S'"), "{sql}");
        assert!(sql.contains("f~as4local = 'A'"), "{sql}");
        assert!(sql.contains("t~ddlanguage = 'E'"), "{sql}");
        assert!(sql.contains("LEFT OUTER JOIN dd04t"), "{sql}");
    }

    #[test]
    fn an_inactive_read_asks_for_the_layer_dd03l_calls_n() {
        let sql = field_query("ZSAMPLE_RECORD_S", AdtVersion::Inactive);
        assert!(sql.contains("f~as4local = 'N'"), "{sql}");
    }

    /// The point of reading DD03L: what an include contributed is already
    /// listed, and only the marker row has to be dropped.
    #[test]
    fn drops_marker_rows_and_keeps_what_they_contributed() {
        let fields = parse_fields(
            "ZSAMPLE_RECORD_S",
            &columns(),
            &rows(vec![
                vec![
                    "0001", ".INCLUDE", "", "", "", "", "000000", "000000", "", "", "", "",
                    "INTTAB",
                ],
                vec![
                    "0002", "MANDT", "X", "MANDT", "MANDT", "CLNT", "000003", "000000", "C", "X",
                    "*", "Client", "INTTAB",
                ],
                vec![
                    "0003",
                    "STATUS",
                    "",
                    "ZSAMPLE_STATUS",
                    "ZSAMPLE_DOM",
                    "CHAR",
                    "000012",
                    "000000",
                    "C",
                    "",
                    "ZSAMPLE_VALUES",
                    "Status",
                    "INTTAB",
                ],
            ]),
        )
        .unwrap();

        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "mandt");
        assert!(fields[0].is_key);
        assert_eq!(fields[0].col_type.as_deref(), Some("CLNT"));
        assert_eq!(fields[0].sap_type.as_deref(), Some("C"));
        assert_eq!(fields[0].length, Some(3));
        assert!(fields[0].not_null);
        // `*` means "any table", so it names nothing worth reporting.
        assert_eq!(fields[0].check_table, None);
        assert_eq!(fields[0].description.as_deref(), Some("Client"));
        assert_eq!(fields[1].data_element.as_deref(), Some("zsample_status"));
        assert_eq!(fields[1].domain.as_deref(), Some("zsample_dom"));
        assert_eq!(fields[1].check_table.as_deref(), Some("zsample_values"));
    }

    /// A field typed from a built-in type has no data element, so the outer
    /// join contributes no text and the field still has to appear.
    #[test]
    fn keeps_a_field_the_text_join_did_not_match() {
        let fields = parse_fields(
            "ZSAMPLE_RECORD_S",
            &columns(),
            &rows(vec![vec![
                "0001", "COUNTER", "", "", "", "INT4", "000010", "000000", "I", "", "", "",
                "INTTAB",
            ]]),
        )
        .unwrap();

        assert_eq!(fields[0].name, "counter");
        assert_eq!(fields[0].data_element, None);
        assert_eq!(fields[0].description, None);
        assert_eq!(fields[0].col_type.as_deref(), Some("INT4"));
    }

    #[test]
    fn reports_a_response_missing_a_column_it_needs() {
        let mut columns = columns();
        columns.retain(|column| column.name != "KEYFLAG");

        let error = parse_fields("ZSAMPLE_RECORD_S", &columns, &[]).unwrap_err();
        assert!(matches!(error, DdicStructureError::Unreadable { .. }));
    }

    /// SAP serves tables through the structures collection, so the collection
    /// cannot be what decides the kind.
    #[test]
    fn reports_what_dd02l_says_it_is_rather_than_how_it_was_read() {
        let row = rows(vec![vec![
            "0001", "MANDT", "X", "MANDT", "MANDT", "CLNT", "000003", "000000", "C", "X", "*",
            "Client", "TRANSP",
        ]]);
        assert_eq!(read_kind(&columns(), &row), "Table");

        let row = rows(vec![vec![
            "0001", "MANDT", "X", "MANDT", "MANDT", "CLNT", "000003", "000000", "C", "X", "*",
            "Client", "INTTAB",
        ]]);
        assert_eq!(read_kind(&columns(), &row), "Structure");
    }

    /// A class this collection is not known to serve is reported as SAP
    /// spelled it, rather than relabelled on a guess.
    #[test]
    fn an_unrecognised_class_is_passed_through() {
        let row = rows(vec![vec![
            "0001",
            "MANDT",
            "X",
            "MANDT",
            "MANDT",
            "CLNT",
            "000003",
            "000000",
            "C",
            "X",
            "*",
            "Client",
            "SOMETHING_NEW",
        ]]);
        assert_eq!(read_kind(&columns(), &row), "SOMETHING_NEW");
    }

    #[test]
    fn an_append_structure_is_named_as_one() {
        let row = rows(vec![vec![
            "0001", "ZZFIELD", "", "ZZFIELD", "", "CHAR", "000010", "000000", "C", "", "", "Extra",
            "APPEND",
        ]]);
        assert_eq!(read_kind(&columns(), &row), "Append structure");
    }

    #[test]
    fn the_query_asks_dd02l_what_kind_of_object_this_is() {
        let sql = field_query("ZSAMPLE_RECORD_S", AdtVersion::Active);
        assert!(sql.contains("h~tabclass"), "{sql}");
        assert!(sql.contains("INNER JOIN dd02l"), "{sql}");
    }

    #[test]
    fn builds_uris_for_plain_and_namespaced_names() {
        assert_eq!(
            structure_uri("ZSAMPLE_RECORD_S"),
            "/sap/bc/adt/ddic/structures/zsample_record_s"
        );
        assert_eq!(
            structure_uri("/ACME/RECORD_S"),
            "/sap/bc/adt/ddic/structures/%2facme%2frecord_s"
        );
    }
}

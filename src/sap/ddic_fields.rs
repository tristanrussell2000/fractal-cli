//! The recorded field list of any DDIC object that has one — a structure, a
//! table, an append structure, a view.
//!
//! The DDL source is not the field list: an `include` names another structure
//! whose fields are spliced in but never shown, and an append does not appear
//! in the source at all. `DD03L` holds both kinds already flattened.
//!
//! Everything here comes from one query, so a caller pays a single round trip
//! for the fields, their texts, and what kind of object they belong to. No ADT
//! document is read, which is what lets this answer for objects that have no
//! document to read.

use std::fmt;

use serde::{Serialize, Serializer};
use thiserror::Error;

use super::{
    adt_version::AdtVersion,
    client::SapClient,
    table::{QueryOptions, TableColumn, TableError, run_query},
};

/// Wider than any real object, and a full page means fields may be missing.
pub const MAX_FIELDS: usize = 2_000;
/// The language field descriptions are read in.
const TEXT_LANGUAGE: &str = "E";

/// One field, as `DD03L` records it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DdicField {
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

/// A field list and what `DD02L` says the object holding it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DdicFieldList {
    pub class: DdicTableClass,
    pub fields: Vec<DdicField>,
}

/// What `DD02L-TABCLASS` says a DDIC object is.
///
/// Deliberately not [`super::repository_kind::RepositoryKind`]: that names ADT
/// object types used in URIs and `--type` arguments, has no append structure,
/// and folds anything unrecognized into a payload-free `Other`. This is the
/// storage class, a separate vocabulary, and the open variant keeps a class
/// SAP sends but this does not know readable instead of relabelling it.
///
/// Only the classes measured as reachable are named. Pooled and cluster tables
/// do not exist on S/4HANA at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DdicTableClass {
    Structure,
    Table,
    AppendStructure,
    View,
    Other(String),
}

impl DdicTableClass {
    fn parse(tabclass: &str) -> Self {
        match tabclass.trim().to_ascii_uppercase().as_str() {
            // A blank class means the query told us nothing, and a field list
            // with no table behind it is a structure.
            "INTTAB" | "" => Self::Structure,
            "TRANSP" => Self::Table,
            "APPEND" => Self::AppendStructure,
            "VIEW" => Self::View,
            other => Self::Other(other.to_owned()),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Structure => "Structure",
            Self::Table => "Table",
            Self::AppendStructure => "Append structure",
            Self::View => "View",
            Self::Other(class) => class,
        }
    }
}

impl fmt::Display for DdicTableClass {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Serialized as the name it displays, so the JSON stays one readable string
/// rather than a tagged union callers would have to unwrap.
impl Serialize for DdicTableClass {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

#[derive(Debug, Error)]
pub enum DdicFieldsError {
    #[error(transparent)]
    Query(#[from] TableError),
    #[error("{name} records no fields in the requested version")]
    NoFields { name: String },
    #[error("{name} has more fields than one read returns ({limit})")]
    TooManyFields { name: String, limit: usize },
    #[error("SAP's field list for {name} was missing a column it needs")]
    Unreadable { name: String },
}

/// Reads one object's complete field list, includes and appends flattened.
///
/// # Errors
///
/// Returns [`DdicFieldsError`] when SAP cannot serve the query, the response
/// is missing a column the mapping needs, or the object records no fields.
pub async fn read_field_list(
    sap: &mut SapClient,
    name: &str,
    version: AdtVersion,
) -> Result<DdicFieldList, DdicFieldsError> {
    let result = run_query(
        sap,
        &field_query(name, version),
        &QueryOptions {
            offset: 0,
            limit: MAX_FIELDS,
        },
    )
    .await?;

    // A capped read that came back full may have dropped fields, and a field
    // list silently missing entries is what this whole path exists to avoid.
    if result.rows.len() >= MAX_FIELDS {
        return Err(DdicFieldsError::TooManyFields {
            name: name.to_owned(),
            limit: MAX_FIELDS,
        });
    }

    let fields = parse_fields(name, &result.columns, &result.rows)?;
    if fields.is_empty() {
        return Err(DdicFieldsError::NoFields {
            name: name.to_owned(),
        });
    }

    Ok(DdicFieldList {
        class: read_class(&result.columns, &result.rows),
        fields,
    })
}

/// `TABCLASS` is the same on every row, so the first one answers.
fn read_class(columns: &[TableColumn], rows: &[Vec<String>]) -> DdicTableClass {
    let class = columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case("TABCLASS"))
        .and_then(|index| rows.first()?.get(index))
        .map_or("", |value| value.trim());

    DdicTableClass::parse(class)
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
///
/// `DD02L` supplies the object's class, because the collection a document was
/// read through does not say what it is.
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
    columns: &[TableColumn],
    rows: &[Vec<String>],
) -> Result<Vec<DdicField>, DdicFieldsError> {
    let mut indexes = [0_usize; COLUMNS.len() + 1];
    for (slot, column) in indexes
        .iter_mut()
        .zip(COLUMNS.iter().copied().chain(["DDTEXT"]))
    {
        *slot = columns
            .iter()
            .position(|candidate| candidate.name.eq_ignore_ascii_case(column))
            .ok_or_else(|| DdicFieldsError::Unreadable {
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

            Some(DdicField {
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn names_the_object_and_pins_the_text_language() {
        let sql = field_query("ZSAMPLE_RECORD_S", AdtVersion::Active);
        assert!(sql.contains("f~tabname = 'ZSAMPLE_RECORD_S'"), "{sql}");
        assert!(sql.contains("f~as4local = 'A'"), "{sql}");
        assert!(sql.contains("t~ddlanguage = 'E'"), "{sql}");
        assert!(sql.contains("LEFT OUTER JOIN dd04t"), "{sql}");
        assert!(sql.contains("INNER JOIN dd02l"), "{sql}");
        assert!(sql.contains("h~tabclass"), "{sql}");
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
        // `*` means "any table", so it names nothing worth reporting.
        assert_eq!(fields[0].check_table, None);
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
        assert!(matches!(error, DdicFieldsError::Unreadable { .. }));
    }

    /// SAP serves tables through the structures collection, so the collection
    /// cannot be what decides the kind.
    #[test]
    fn reports_what_dd02l_says_it_is_rather_than_how_it_was_read() {
        for (class, expected) in [
            ("TRANSP", DdicTableClass::Table),
            ("INTTAB", DdicTableClass::Structure),
            ("APPEND", DdicTableClass::AppendStructure),
            ("VIEW", DdicTableClass::View),
            ("", DdicTableClass::Structure),
            (
                "SOMETHING_NEW",
                DdicTableClass::Other("SOMETHING_NEW".to_owned()),
            ),
        ] {
            let row = rows(vec![vec![
                "0001", "MANDT", "X", "MANDT", "MANDT", "CLNT", "000003", "000000", "C", "X", "*",
                "Client", class,
            ]]);
            assert_eq!(read_class(&columns(), &row), expected, "{class}");
        }
    }

    /// The displayed name is also the serialized one, so JSON carries a plain
    /// string and an unknown class stays readable in it.
    #[test]
    fn displays_and_serializes_as_one_readable_name() {
        assert_eq!(
            DdicTableClass::AppendStructure.to_string(),
            "Append structure"
        );
        assert_eq!(
            serde_json::to_string(&DdicTableClass::Table).unwrap(),
            r#""Table""#
        );
        assert_eq!(
            serde_json::to_string(&DdicTableClass::Other("ODD".to_owned())).unwrap(),
            r#""ODD""#
        );
    }
}

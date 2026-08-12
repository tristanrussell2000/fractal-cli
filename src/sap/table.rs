use roxmltree::{Document, Node};
use thiserror::Error;

use super::client::SapError;
use super::{find_child, non_empty_attribute};

#[derive(Debug, Error)]
pub enum TableError {
    #[error(transparent)]
    Sap(#[from] SapError),
    #[error("could not parse table data response: {0}")]
    Parse(String),
}

impl TableError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(_) => "table_response_parse_error",
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
        }
    }
}

/// One column's metadata from a `dataPreview` response.
///
/// `sap_type` is SAP's single-character ABAP type code (`C`, `X`, `N`, `P`, ...);
/// `col_type` is the friendlier name (`CHAR`, `RAW`, `NUMC`, `DEC`, ...). Both
/// this and `length`/`description` are frequently absent on the freestyle-query
/// path, which only echoes bare column names unless enriched from elsewhere —
/// `keyAttribute`/`isKeyFigure` are deliberately not modeled here: real capture
/// against `de3` showed `keyAttribute="false"` on a table's actual DDIC primary
/// key, so it does not reflect SQL primary-key status (this codebase reads key
/// fields from a table's DDL source instead — see `object source`).
#[derive(Debug, Clone)]
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
/// the freestyle path — the cheap path's value reflects the preview window,
/// not the table's real size (confirmed against `de3`: reported `0` despite
/// real rows being returned). Callers needing an accurate total on the cheap
/// path should issue a separate `COUNT(*)` query.
#[derive(Debug, Clone)]
pub struct TableDataResult {
    pub entity: Option<String>,
    pub executed_query: Option<String>,
    pub total_rows: Option<u64>,
    pub columns: Vec<TableColumn>,
    pub rows: Vec<Vec<String>>,
}

/// Parses a `dataPreview:tableData` response into columns and row-major data.
///
/// The response is column-major (`<columns>` holds one `<metadata>` plus a
/// `<dataSet>` of `<data>` values for that column across all rows); this
/// transposes it into rows aligned positionally with `columns`. Missing cell
/// values (columns reporting fewer values than the widest column) are padded
/// with an empty string rather than causing an error, matching the response's
/// own tolerance for uneven data.
///
/// # Errors
///
/// Returns [`TableError::Parse`] when the response is not valid XML.
pub fn parse_table_data(xml: &str) -> Result<TableDataResult, TableError> {
    let document = Document::parse(xml).map_err(|error| TableError::Parse(error.to_string()))?;
    let root = document.root_element();

    let total_rows = child_text(root, "totalRows").and_then(|text| text.trim().parse().ok());
    let entity = non_empty_attribute(child_text(root, "name"));
    let executed_query = child_text(root, "executedQueryString").map(normalize_whitespace);

    let mut columns = Vec::new();
    let mut column_values: Vec<Vec<String>> = Vec::new();

    for columns_node in root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "columns")
    {
        let Some(metadata) = find_child(columns_node, "metadata") else {
            continue;
        };
        let Some(name) = metadata.attribute("name").filter(|value| !value.is_empty()) else {
            continue;
        };

        columns.push(TableColumn {
            name: name.to_owned(),
            sap_type: non_empty_attribute(metadata.attribute("type")),
            col_type: non_empty_attribute(metadata.attribute("colType")),
            length: metadata
                .attribute("length")
                .and_then(|value| value.parse().ok()),
            description: non_empty_attribute(metadata.attribute("description")),
        });

        let values = find_child(columns_node, "dataSet")
            .map(|data_set| {
                data_set
                    .children()
                    .filter(|node| node.is_element() && node.tag_name().name() == "data")
                    .map(|node| node.text().unwrap_or("").to_owned())
                    .collect()
            })
            .unwrap_or_default();
        column_values.push(values);
    }

    let row_count = column_values.iter().map(Vec::len).max().unwrap_or(0);
    let rows = (0..row_count)
        .map(|index| {
            column_values
                .iter()
                .map(|values| values.get(index).cloned().unwrap_or_default())
                .collect()
        })
        .collect();

    Ok(TableDataResult {
        entity,
        executed_query,
        total_rows,
        columns,
        rows,
    })
}

fn child_text<'a>(node: Node<'a, 'a>, name: &str) -> Option<&'a str> {
    find_child(node, name).and_then(|child| child.text())
}

/// SAP's `executedQueryString` echo carries irregular runs of whitespace
/// (leftover from its own lexer padding and the `INTO TABLE` wrapper concat,
/// e.g. `"PR'   INTO     TABLE"` observed verbatim against `de3`) — collapse
/// them so the echoed query is readable.
fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // Fixtures below are trimmed from real `dataPreview:tableData` responses
    // captured against a live DE3 system (cheap ddic path and freestyle path).

    #[test]
    fn parses_the_cheap_path_response_with_full_column_metadata() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>0</dataPreview:totalRows><dataPreview:name>ZDTLS_CHECK_IN</dataPreview:name><dataPreview:columns><dataPreview:metadata dataPreview:name="CLIENT" dataPreview:type="C" dataPreview:description="CLIENT" dataPreview:keyAttribute="false" dataPreview:colType="CLNT" dataPreview:isKeyFigure="false" dataPreview:length="3"/><dataPreview:dataSet><dataPreview:data>100</dataPreview:data><dataPreview:data>100</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="OBJECTID" dataPreview:type="N" dataPreview:description="Object ID" dataPreview:keyAttribute="false" dataPreview:colType="NUMC" dataPreview:isKeyFigure="false" dataPreview:length="10"/><dataPreview:dataSet><dataPreview:data>0000000009</dataPreview:data><dataPreview:data>0000000037</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#;

        let result = parse_table_data(xml).unwrap();
        assert_eq!(result.entity.as_deref(), Some("ZDTLS_CHECK_IN"));
        assert_eq!(result.executed_query, None);
        assert_eq!(result.total_rows, Some(0));
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.columns[0].name, "CLIENT");
        assert_eq!(result.columns[0].sap_type.as_deref(), Some("C"));
        assert_eq!(result.columns[0].col_type.as_deref(), Some("CLNT"));
        assert_eq!(result.columns[0].length, Some(3));
        assert_eq!(result.columns[0].description.as_deref(), Some("CLIENT"));
        assert_eq!(
            result.rows,
            vec![
                vec!["100".to_owned(), "0000000009".to_owned()],
                vec!["100".to_owned(), "0000000037".to_owned()],
            ]
        );
    }

    #[test]
    fn parses_the_freestyle_path_response_with_bare_columns_and_query_echo() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>12</dataPreview:totalRows><dataPreview:executedQueryString>SELECT OBJECTID, USERNAME FROM ZDTLS_CHECK_IN WHERE OBJECTTYPE = 'PR'   INTO     TABLE @DATA(LT_RESULT)   UP TO 3  ROWS   .</dataPreview:executedQueryString><dataPreview:columns><dataPreview:metadata dataPreview:name="OBJECTID" dataPreview:type="N" dataPreview:description="OBJECTID" dataPreview:keyAttribute="false" dataPreview:colType="" dataPreview:isKeyFigure="false"/><dataPreview:dataSet><dataPreview:data>0000000009</dataPreview:data><dataPreview:data>0000000037</dataPreview:data><dataPreview:data>0000000036</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="USERNAME" dataPreview:type="C" dataPreview:description="USERNAME" dataPreview:keyAttribute="false" dataPreview:colType="" dataPreview:isKeyFigure="false"/><dataPreview:dataSet><dataPreview:data>SLEE</dataPreview:data><dataPreview:data>DTLS_TEST_4</dataPreview:data><dataPreview:data>DTLS_TEST_9</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#;

        let result = parse_table_data(xml).unwrap();
        assert_eq!(result.entity, None);
        assert_eq!(result.total_rows, Some(12));
        assert_eq!(
            result.executed_query.as_deref(),
            Some(
                "SELECT OBJECTID, USERNAME FROM ZDTLS_CHECK_IN WHERE OBJECTTYPE = 'PR' INTO TABLE @DATA(LT_RESULT) UP TO 3 ROWS ."
            )
        );
        // colType="" is present-but-empty on the freestyle path; must be None, not Some("").
        assert_eq!(result.columns[0].col_type, None);
        assert_eq!(result.columns[0].length, None);
        assert_eq!(result.rows.len(), 3);
        assert_eq!(
            result.rows[0],
            vec!["0000000009".to_owned(), "SLEE".to_owned()]
        );
    }

    #[test]
    fn pads_missing_cell_values_with_empty_strings_when_columns_are_uneven() {
        let xml = r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:columns><dataPreview:metadata dataPreview:name="A"/><dataPreview:dataSet><dataPreview:data>1</dataPreview:data><dataPreview:data>2</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="B"/><dataPreview:dataSet><dataPreview:data>x</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#;

        let result = parse_table_data(xml).unwrap();
        assert_eq!(
            result.rows,
            vec![
                vec!["1".to_owned(), "x".to_owned()],
                vec!["2".to_owned(), String::new()],
            ]
        );
    }

    #[test]
    fn skips_columns_with_no_metadata_or_no_name() {
        let xml = r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:columns><dataPreview:dataSet><dataPreview:data>orphan</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name=""/></dataPreview:columns></dataPreview:tableData>"#;

        let result = parse_table_data(xml).unwrap();
        assert!(result.columns.is_empty());
        assert!(result.rows.is_empty());
    }

    #[test]
    fn returns_a_parse_error_for_malformed_table_data_xml() {
        let error = parse_table_data("<not-closed").unwrap_err();
        assert_eq!(error.code(), "table_response_parse_error");
        assert!(error.hint().is_some());
    }
}

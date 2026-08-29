use roxmltree::{Document, Node};

use super::{TableColumn, TableDataResult, TableError};
use crate::sap::{find_child, non_empty_attribute};

/// Parses a `dataPreview:tableData` response into columns and row-major data.
///
/// The response is column-major (`<columns>` holds one `<metadata>` plus a
/// `<dataSet>` of `<data>` values for that column across all rows); this
/// transposes it into rows aligned positionally with `columns`. Missing cell
/// values are padded with an empty string.
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

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::reportable_error::ReportableError;

    #[test]
    fn parses_the_cheap_path_response_with_full_column_metadata() {
        let xml = r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>0</dataPreview:totalRows><dataPreview:name>ZDEMO_EVENT_LOG</dataPreview:name><dataPreview:columns><dataPreview:metadata dataPreview:name="CLIENT" dataPreview:type="C" dataPreview:description="Client" dataPreview:keyAttribute="false" dataPreview:colType="CLNT" dataPreview:length="3"/><dataPreview:dataSet><dataPreview:data>100</dataPreview:data><dataPreview:data>100</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID" dataPreview:type="N" dataPreview:description="Event ID" dataPreview:colType="NUMC" dataPreview:length="10"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data><dataPreview:data>0000000002</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#;

        let result = parse_table_data(xml).unwrap();
        assert_eq!(result.entity.as_deref(), Some("ZDEMO_EVENT_LOG"));
        assert_eq!(result.executed_query, None);
        assert_eq!(result.total_rows, Some(0));
        assert_eq!(result.columns.len(), 2);
        assert_eq!(result.columns[0].name, "CLIENT");
        assert_eq!(result.columns[0].sap_type.as_deref(), Some("C"));
        assert_eq!(result.columns[0].col_type.as_deref(), Some("CLNT"));
        assert_eq!(result.columns[0].length, Some(3));
        assert_eq!(result.columns[0].description.as_deref(), Some("Client"));
        assert_eq!(
            result.rows,
            vec![
                vec!["100".to_owned(), "0000000001".to_owned()],
                vec!["100".to_owned(), "0000000002".to_owned()],
            ]
        );
    }

    #[test]
    fn parses_the_freestyle_path_response_with_bare_columns_and_query_echo() {
        let xml = r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>12</dataPreview:totalRows><dataPreview:executedQueryString>SELECT EVENT_ID   FROM ZDEMO_EVENT_LOG   UP TO 2 ROWS</dataPreview:executedQueryString><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID" dataPreview:type="N" dataPreview:description="EVENT_ID" dataPreview:colType=""/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data><dataPreview:data>0000000002</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#;

        let result = parse_table_data(xml).unwrap();
        assert_eq!(result.entity, None);
        assert_eq!(result.total_rows, Some(12));
        assert_eq!(
            result.executed_query.as_deref(),
            Some("SELECT EVENT_ID FROM ZDEMO_EVENT_LOG UP TO 2 ROWS")
        );
        assert_eq!(result.columns[0].col_type, None);
        assert_eq!(result.columns[0].length, None);
        assert_eq!(result.rows.len(), 2);
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

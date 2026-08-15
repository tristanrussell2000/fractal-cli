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
    #[error("invalid DDIC entity name: {0}")]
    InvalidEntityName(String),
    #[error("invalid field name: {0}")]
    InvalidFieldName(String),
    #[error("the where clause must not contain a semicolon")]
    WhereContainsSemicolon,
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
/// the freestyle path — protocol captures show that the cheap path's value
/// reflects the preview window rather than the table's real size. Callers
/// needing an accurate total on the cheap path should issue a separate
/// `COUNT(*)` query.
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

/// SAP's `executedQueryString` echo carries irregular runs of whitespace left
/// over from its own lexer padding and query-wrapper concatenation. Collapse
/// them so the echoed query is readable.
fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Composes `SELECT {fields|*} FROM {entity} [WHERE {where}]` for simple-mode
/// `table data` calls (`--fields`/`--where`, as opposed to `--query`'s whole
/// user-supplied statement). Deliberately does not support `ORDER BY`,
/// `GROUP BY`, `DISTINCT`, or aggregations — those require writing a full
/// query via `--query`.
///
/// # Errors
///
/// Returns [`TableError::InvalidEntityName`], [`TableError::InvalidFieldName`],
/// or [`TableError::WhereContainsSemicolon`] when an input fails validation.
// Not yet called outside tests: the async fetch that will use this to build a
// freestyle-endpoint request is a separate, later chunk. Remove once that lands.
#[allow(dead_code)]
fn build_simple_query(
    entity: &str,
    fields: &[String],
    where_clause: Option<&str>,
) -> Result<String, TableError> {
    let entity = validate_entity_name(entity)?;
    let fields = validate_field_names(fields)?;
    let select_list = if fields.is_empty() {
        "*".to_owned()
    } else {
        fields.join(", ")
    };

    let mut sql = format!("SELECT {select_list} FROM {entity}");
    if let Some(where_clause) = where_clause
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        if where_clause.contains(';') {
            return Err(TableError::WhereContainsSemicolon);
        }
        sql.push_str(" WHERE ");
        sql.push_str(&pad_abap_tokens(where_clause));
    }
    Ok(sql)
}

fn validate_entity_name(name: &str) -> Result<String, TableError> {
    let trimmed = name.trim();
    let valid = !trimmed.is_empty()
        && trimmed
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '/');
    if valid {
        Ok(trimmed.to_ascii_uppercase())
    } else {
        Err(TableError::InvalidEntityName(name.to_owned()))
    }
}

fn validate_field_names(fields: &[String]) -> Result<Vec<String>, TableError> {
    fields
        .iter()
        .map(String::as_str)
        .map(str::trim)
        .filter(|field| !field.is_empty())
        .map(|field| {
            if field.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                Ok(field.to_ascii_uppercase())
            } else {
                Err(TableError::InvalidFieldName(field.to_owned()))
            }
        })
        .collect()
}

/// ABAP Open SQL's lexer demands
/// whitespace around structural tokens that other SQL dialects let you mash
/// together — `STATUS='PE'` fails with a cryptic lexer error, `STATUS = 'PE'`
/// doesn't. String-literal-aware: content inside `'...'` is left untouched,
/// including an escaped quote (`''`). Grouping parentheses get padded while
/// function-call parentheses stay tight. Existing whitespace outside literals
/// is normalized, making the operation idempotent.
fn pad_abap_tokens(where_clause: &str) -> String {
    let mut out = String::with_capacity(where_clause.len() + 16);
    let mut in_quote = false;
    let mut chars = where_clause.chars().peekable();

    while let Some(ch) = chars.next() {
        if ch == '\'' {
            if in_quote && chars.peek() == Some(&'\'') {
                chars.next();
                out.push_str("''");
                continue;
            }
            in_quote = !in_quote;
            out.push('\'');
            continue;
        }

        if in_quote {
            out.push(ch);
            continue;
        }

        match ch {
            ch if ch.is_whitespace() => push_token_space(&mut out),
            '(' => {
                let previous_word = trailing_identifier(&out);
                let is_function_call =
                    !previous_word.is_empty() && !is_grouping_keyword(previous_word);
                if is_function_call {
                    trim_token_space(&mut out);
                } else {
                    push_token_space(&mut out);
                }
                out.push('(');
                push_token_space(&mut out);
            }
            ')' => {
                push_token_space(&mut out);
                out.push(')');
                push_token_space(&mut out);
            }
            '<' => {
                push_token_space(&mut out);
                out.push('<');
                if matches!(chars.peek(), Some('=' | '>'))
                    && let Some(next) = chars.next()
                {
                    out.push(next);
                }
                push_token_space(&mut out);
            }
            '>' => {
                push_token_space(&mut out);
                out.push('>');
                if chars.peek() == Some(&'=')
                    && let Some(next) = chars.next()
                {
                    out.push(next);
                }
                push_token_space(&mut out);
            }
            '=' => {
                push_token_space(&mut out);
                out.push('=');
                push_token_space(&mut out);
            }
            _ => out.push(ch),
        }
    }

    trim_token_space(&mut out);
    out
}

const fn is_identifier_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_'
}

fn trailing_identifier(value: &str) -> &str {
    let trimmed = value.trim_end();
    let start = trimmed
        .char_indices()
        .rev()
        .find_map(|(index, ch)| (!is_identifier_char(ch)).then_some(index + ch.len_utf8()))
        .unwrap_or(0);
    &trimmed[start..]
}

fn is_grouping_keyword(word: &str) -> bool {
    ["ALL", "AND", "ANY", "EXISTS", "IN", "NOT", "OR"]
        .iter()
        .any(|keyword| word.eq_ignore_ascii_case(keyword))
}

fn push_token_space(out: &mut String) {
    if !out.is_empty() && !out.ends_with(' ') {
        out.push(' ');
    }
}

fn trim_token_space(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_cheap_path_response_with_full_column_metadata() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>0</dataPreview:totalRows><dataPreview:name>ZDEMO_EVENT_LOG</dataPreview:name><dataPreview:columns><dataPreview:metadata dataPreview:name="CLIENT" dataPreview:type="C" dataPreview:description="Client" dataPreview:keyAttribute="false" dataPreview:colType="CLNT" dataPreview:isKeyFigure="false" dataPreview:length="3"/><dataPreview:dataSet><dataPreview:data>100</dataPreview:data><dataPreview:data>100</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID" dataPreview:type="N" dataPreview:description="Event ID" dataPreview:keyAttribute="false" dataPreview:colType="NUMC" dataPreview:isKeyFigure="false" dataPreview:length="10"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data><dataPreview:data>0000000002</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#;

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
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview"><dataPreview:totalRows>12</dataPreview:totalRows><dataPreview:executedQueryString>SELECT EVENT_ID, OWNER_NAME FROM ZDEMO_EVENT_LOG WHERE EVENT_TYPE = 'SAMPLE'   INTO     TABLE @DATA(LT_RESULT)   UP TO 3  ROWS   .</dataPreview:executedQueryString><dataPreview:columns><dataPreview:metadata dataPreview:name="EVENT_ID" dataPreview:type="N" dataPreview:description="EVENT_ID" dataPreview:keyAttribute="false" dataPreview:colType="" dataPreview:isKeyFigure="false"/><dataPreview:dataSet><dataPreview:data>0000000001</dataPreview:data><dataPreview:data>0000000002</dataPreview:data><dataPreview:data>0000000003</dataPreview:data></dataPreview:dataSet></dataPreview:columns><dataPreview:columns><dataPreview:metadata dataPreview:name="OWNER_NAME" dataPreview:type="C" dataPreview:description="OWNER_NAME" dataPreview:keyAttribute="false" dataPreview:colType="" dataPreview:isKeyFigure="false"/><dataPreview:dataSet><dataPreview:data>ALICE</dataPreview:data><dataPreview:data>BOB</dataPreview:data><dataPreview:data>CAROL</dataPreview:data></dataPreview:dataSet></dataPreview:columns></dataPreview:tableData>"#;

        let result = parse_table_data(xml).unwrap();
        assert_eq!(result.entity, None);
        assert_eq!(result.total_rows, Some(12));
        assert_eq!(
            result.executed_query.as_deref(),
            Some(
                "SELECT EVENT_ID, OWNER_NAME FROM ZDEMO_EVENT_LOG WHERE EVENT_TYPE = 'SAMPLE' INTO TABLE @DATA(LT_RESULT) UP TO 3 ROWS ."
            )
        );
        // colType="" is present-but-empty on the freestyle path; must be None, not Some("").
        assert_eq!(result.columns[0].col_type, None);
        assert_eq!(result.columns[0].length, None);
        assert_eq!(result.rows.len(), 3);
        assert_eq!(
            result.rows[0],
            vec!["0000000001".to_owned(), "ALICE".to_owned()]
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

    #[test]
    fn builds_a_star_query_with_no_fields_or_where() {
        let sql = build_simple_query("ztable", &[], None).unwrap();
        assert_eq!(sql, "SELECT * FROM ZTABLE");
    }

    #[test]
    fn builds_a_field_list_query_with_a_padded_where_clause() {
        let fields = vec!["id".to_owned(), "status".to_owned()];
        let sql = build_simple_query("ztable", &fields, Some("STATUS='PE'")).unwrap();
        assert_eq!(sql, "SELECT ID, STATUS FROM ZTABLE WHERE STATUS = 'PE'");
    }

    #[test]
    fn filters_out_blank_field_entries_from_a_stray_comma() {
        let fields = vec![
            "ID".to_owned(),
            String::new(),
            "  ".to_owned(),
            "STATUS".to_owned(),
        ];
        let sql = build_simple_query("ztable", &fields, None).unwrap();
        assert_eq!(sql, "SELECT ID, STATUS FROM ZTABLE");
    }

    #[test]
    fn treats_a_blank_where_clause_as_absent() {
        let sql = build_simple_query("ztable", &[], Some("   ")).unwrap();
        assert_eq!(sql, "SELECT * FROM ZTABLE");
    }

    #[test]
    fn rejects_invalid_entity_names() {
        let error = build_simple_query("ztable; drop", &[], None).unwrap_err();
        assert_eq!(error.code(), "invalid_table_entity_name");
        assert!(error.hint().is_some());
    }

    #[test]
    fn rejects_invalid_field_names() {
        let fields = vec!["ID; DROP TABLE".to_owned()];
        let error = build_simple_query("ztable", &fields, None).unwrap_err();
        assert_eq!(error.code(), "invalid_table_field_name");
    }

    #[test]
    fn rejects_where_clauses_containing_a_semicolon() {
        let error =
            build_simple_query("ztable", &[], Some("STATUS='PE'; DROP TABLE ZTABLE")).unwrap_err();
        assert_eq!(error.code(), "where_contains_semicolon");
        assert!(error.hint().unwrap().contains("--query"));
    }

    #[test]
    fn pads_a_simple_comparison() {
        assert_eq!(pad_abap_tokens("STATUS='PE'"), "STATUS = 'PE'");
    }

    #[test]
    fn pads_composite_comparison_operators_without_splitting_them() {
        assert_eq!(pad_abap_tokens("ID<>5"), "ID <> 5");
    }

    #[test]
    fn keeps_function_call_parens_tight() {
        assert_eq!(pad_abap_tokens("COUNT(x)"), "COUNT( x )");
    }

    #[test]
    fn treats_sql_keyword_parens_as_grouping_parens() {
        assert_eq!(pad_abap_tokens("ID IN(1,2)"), "ID IN ( 1,2 )");
    }

    #[test]
    fn separates_boolean_keywords_after_closing_parens() {
        assert_eq!(
            pad_abap_tokens("(STATUS='OPEN')AND(PRIORITY>=2)"),
            "( STATUS = 'OPEN' ) AND ( PRIORITY >= 2 )"
        );
    }

    #[test]
    fn normalizes_existing_token_whitespace_idempotently() {
        let once = pad_abap_tokens(" STATUS  =  'OPEN' ");
        assert_eq!(once, "STATUS = 'OPEN'");
        assert_eq!(pad_abap_tokens(&once), once);
    }

    #[test]
    fn handles_nested_function_and_grouping_parens() {
        assert_eq!(
            pad_abap_tokens("NOT(COALESCE(SCORE,0)<10)"),
            "NOT ( COALESCE( SCORE,0 ) < 10 )"
        );
    }

    #[test]
    fn leaves_content_inside_string_literals_untouched() {
        assert_eq!(pad_abap_tokens("NAME='(foo)'"), "NAME = '(foo)'");
        assert_eq!(pad_abap_tokens("NAME='O''BRIEN'"), "NAME = 'O''BRIEN'");
        assert_eq!(
            pad_abap_tokens("NAME='two  spaces'"),
            "NAME = 'two  spaces'"
        );
    }
}

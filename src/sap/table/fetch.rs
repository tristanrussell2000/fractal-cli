use reqwest::header::{HeaderMap, HeaderValue};

use super::{
    TableColumn, TableDataResult, TableError, error::classify_query_error, parse_table_data,
};
use crate::sap::client::{SapClient, SapError};

const DDIC_PREVIEW_PATH: &str = "/sap/bc/adt/datapreview/ddic";
const FREESTYLE_PREVIEW_PATH: &str = "/sap/bc/adt/datapreview/freestyle";
const MAX_PREVIEW_ROWS: usize = 5_000;

const SQL_BREAK_KEYWORDS: [&str; 13] = [
    "INNER JOIN",
    "LEFT OUTER JOIN",
    "LEFT JOIN",
    "RIGHT OUTER JOIN",
    "RIGHT JOIN",
    "FULL OUTER JOIN",
    "CROSS JOIN",
    "FROM",
    "WHERE",
    "GROUP BY",
    "HAVING",
    "ORDER BY",
    "UNION",
];

/// Selects how a table-data request is expressed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TableDataQuery {
    /// Builds a basic SELECT from a field list and optional WHERE fragment.
    Simple {
        fields: Vec<String>,
        where_clause: Option<String>,
    },
    /// Sends a complete caller-supplied SELECT statement.
    Full(String),
}

impl Default for TableDataQuery {
    fn default() -> Self {
        Self::Simple {
            fields: Vec::new(),
            where_clause: None,
        }
    }
}

/// Options for fetching a locally paged table-data preview.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDataOptions {
    pub query: TableDataQuery,
    pub offset: usize,
    pub limit: usize,
}

impl Default for TableDataOptions {
    fn default() -> Self {
        Self {
            query: TableDataQuery::default(),
            offset: 0,
            limit: 100,
        }
    }
}

/// Fetches table data through the appropriate SAP ADT preview endpoint.
///
/// An unfiltered simple request uses the inexpensive DDIC endpoint. A simple
/// request with fields or a WHERE clause, and every full query, uses the
/// freestyle endpoint. Because SAP has no offset parameter, this requests
/// `offset + limit` rows and applies the requested page locally.
///
/// The unfiltered path concurrently requests an accurate row count. Filtered
/// simple queries concurrently request DDIC column metadata. These enrichment
/// requests are best-effort and never replace an otherwise valid preview with
/// an error.
///
/// # Errors
///
/// Returns [`TableError`] when input validation, the SAP request, or XML
/// parsing fails.
pub async fn get_table_data(
    sap: &mut SapClient,
    entity: &str,
    options: &TableDataOptions,
) -> Result<TableDataResult, TableError> {
    let entity = validate_entity_name(entity)?;
    let row_count = preview_row_count(options)?;
    sap.establish_csrf_session().await?;

    let mut result = match &options.query {
        TableDataQuery::Simple {
            fields,
            where_clause,
        } if fields.is_empty()
            && where_clause
                .as_deref()
                .is_none_or(|value| value.trim().is_empty()) =>
        {
            let count_query =
                break_sql_lines(&format!("SELECT COUNT(*) AS ROW_COUNT FROM {entity}"));
            let (preview, count) = tokio::join!(
                post_ddic_preview(sap, &entity, row_count),
                post_freestyle_preview(sap, &count_query, 1),
            );

            let mut result = parse_table_data(&preview?)?;
            if let Ok(count_xml) = count
                && let Ok(count_result) = parse_table_data(&count_xml)
                && let Some(total_rows) = extract_count(&count_result)
            {
                result.total_rows = Some(total_rows);
            }
            result
        }
        TableDataQuery::Simple {
            fields,
            where_clause,
        } => {
            let query = build_simple_query(&entity, fields, where_clause.as_deref())?;
            let query = break_sql_lines(&query);
            let (preview, metadata) = tokio::join!(
                post_freestyle_preview(sap, &query, row_count),
                post_ddic_preview(sap, &entity, 1),
            );

            let metadata_result = metadata
                .ok()
                .and_then(|metadata_xml| parse_table_data(&metadata_xml).ok());
            let mut result = match preview {
                Ok(preview_xml) => parse_table_data(&preview_xml)?,
                Err(error) => {
                    let columns = metadata_result
                        .as_ref()
                        .map_or(&[][..], |metadata| metadata.columns.as_slice());
                    return Err(classify_query_error(error, columns));
                }
            };
            if let Some(metadata_result) = metadata_result {
                merge_column_metadata(&mut result.columns, &metadata_result.columns);
            }
            result
        }
        TableDataQuery::Full(query) => {
            let query = validate_full_query(query)?;
            let xml = post_freestyle_preview(sap, &break_sql_lines(query), row_count)
                .await
                .map_err(|error| classify_query_error(error, &[]))?;
            parse_table_data(&xml)?
        }
    };

    result.rows = result
        .rows
        .into_iter()
        .skip(options.offset)
        .take(options.limit)
        .collect();
    Ok(result)
}

async fn post_ddic_preview(
    sap: &SapClient,
    entity: &str,
    row_count: usize,
) -> Result<String, SapError> {
    let row_count = row_count.to_string();
    let query = [
        ("rowNumber", row_count.as_str()),
        ("ddicEntityName", entity),
    ];
    sap.post_text_read_only(DDIC_PREVIEW_PATH, &query, None, HeaderMap::new())
        .await
}

async fn post_freestyle_preview(
    sap: &SapClient,
    statement: &str,
    row_count: usize,
) -> Result<String, SapError> {
    let row_count = row_count.to_string();
    let query = [("rowNumber", row_count.as_str()), ("dataAging", "true")];
    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    sap.post_text_read_only(FREESTYLE_PREVIEW_PATH, &query, Some(statement), headers)
        .await
}

fn extract_count(result: &TableDataResult) -> Option<u64> {
    let count_index = result
        .columns
        .iter()
        .position(|column| column.name.eq_ignore_ascii_case("ROW_COUNT"))
        .or_else(|| (result.columns.len() == 1).then_some(0))?;
    result.rows.first()?.get(count_index)?.trim().parse().ok()
}

fn merge_column_metadata(columns: &mut [TableColumn], metadata: &[TableColumn]) {
    for column in columns {
        let Some(source) = metadata
            .iter()
            .find(|candidate| candidate.name.eq_ignore_ascii_case(&column.name))
        else {
            continue;
        };

        if source.sap_type.is_some() {
            column.sap_type.clone_from(&source.sap_type);
        }
        if source.col_type.is_some() {
            column.col_type.clone_from(&source.col_type);
        }
        if source.length.is_some() {
            column.length = source.length;
        }
        if source.description.is_some() {
            column.description.clone_from(&source.description);
        }
    }
}

fn preview_row_count(options: &TableDataOptions) -> Result<usize, TableError> {
    let requested = options.offset.saturating_add(options.limit);
    if requested > MAX_PREVIEW_ROWS {
        return Err(TableError::PreviewRangeTooLarge {
            requested,
            maximum: MAX_PREVIEW_ROWS,
        });
    }
    Ok(requested.max(1))
}

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
        && trimmed.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '_' || character == '/'
        });
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
            if field
                .chars()
                .all(|character| character.is_ascii_alphanumeric() || character == '_')
            {
                Ok(field.to_ascii_uppercase())
            } else {
                Err(TableError::InvalidFieldName(field.to_owned()))
            }
        })
        .collect()
}

fn validate_full_query(query: &str) -> Result<&str, TableError> {
    let trimmed = query.trim();
    if trimmed.is_empty() {
        return Err(TableError::QueryRequired);
    }
    if trimmed.contains(';') {
        return Err(TableError::QueryContainsSemicolon);
    }
    let Some(prefix) = trimmed.get(.."SELECT".len()) else {
        return Err(TableError::QueryMustBeSelect);
    };
    let remainder = &trimmed["SELECT".len()..];
    if !prefix.eq_ignore_ascii_case("SELECT")
        || remainder.trim().is_empty()
        || !remainder.chars().next().is_some_and(char::is_whitespace)
    {
        return Err(TableError::QueryMustBeSelect);
    }
    Ok(trimmed)
}

// This deliberately follows the established clause-boundary strategy. If
// protocol verification shows that individual clauses still exceed SAP's
// per-line limit, replace it with a quote-aware width wrapper that breaks at
// the nearest safe whitespace before a conservative limit and errors when one
// indivisible token or literal is too long.
fn break_sql_lines(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len() + 16);
    let mut index = 0;
    let mut in_quote = false;

    while index < sql.len() {
        let Some(character) = sql[index..].chars().next() else {
            break;
        };
        let character_len = character.len_utf8();

        if character == '\'' {
            out.push(character);
            index += character_len;
            if in_quote && sql[index..].starts_with('\'') {
                out.push('\'');
                index += '\''.len_utf8();
            } else {
                in_quote = !in_quote;
            }
            continue;
        }

        if !in_quote && character.is_whitespace() {
            let whitespace_start = index;
            index += character_len;
            while index < sql.len() {
                let Some(next) = sql[index..].chars().next() else {
                    break;
                };
                if !next.is_whitespace() {
                    break;
                }
                index += next.len_utf8();
            }

            if starts_with_break_keyword(&sql[index..]) {
                if !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
            } else {
                out.push_str(&sql[whitespace_start..index]);
            }
            continue;
        }

        out.push(character);
        index += character_len;
    }

    out
}

fn starts_with_break_keyword(value: &str) -> bool {
    SQL_BREAK_KEYWORDS
        .iter()
        .any(|keyword| matches_keyword(value, keyword))
}

fn matches_keyword(mut value: &str, keyword: &str) -> bool {
    for (index, word) in keyword.split(' ').enumerate() {
        if index > 0 {
            let trimmed = value.trim_start();
            if trimmed.len() == value.len() {
                return false;
            }
            value = trimmed;
        }

        let Some(candidate) = value.get(..word.len()) else {
            return false;
        };
        if !candidate.eq_ignore_ascii_case(word) {
            return false;
        }
        value = &value[word.len()..];
    }

    value
        .chars()
        .next()
        .is_none_or(|character| !is_identifier_char(character))
}

fn pad_abap_tokens(where_clause: &str) -> String {
    let mut out = String::with_capacity(where_clause.len() + 16);
    let mut in_quote = false;
    let mut chars = where_clause.chars().peekable();

    while let Some(character) = chars.next() {
        if character == '\'' {
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
            out.push(character);
            continue;
        }

        match character {
            character if character.is_whitespace() => push_token_space(&mut out),
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
            _ => out.push(character),
        }
    }

    trim_token_space(&mut out);
    out
}

const fn is_identifier_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || character == '_'
}

fn trailing_identifier(value: &str) -> &str {
    let trimmed = value.trim_end();
    let start = trimmed
        .char_indices()
        .rev()
        .find_map(|(index, character)| {
            (!is_identifier_char(character)).then_some(index + character.len_utf8())
        })
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
    fn builds_a_star_query_with_no_fields_or_where() {
        let sql = build_simple_query("ztable", &[], None).unwrap();
        assert_eq!(sql, "SELECT * FROM ZTABLE");
    }

    #[test]
    fn builds_a_field_list_query_with_a_padded_where_clause() {
        let fields = vec!["id".to_owned(), "status".to_owned()];
        let sql = build_simple_query("ztable", &fields, Some("STATUS='OPEN'")).unwrap();
        assert_eq!(sql, "SELECT ID, STATUS FROM ZTABLE WHERE STATUS = 'OPEN'");
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
    fn rejects_invalid_simple_query_inputs() {
        assert_eq!(
            build_simple_query("ztable; drop", &[], None)
                .unwrap_err()
                .code(),
            "invalid_table_entity_name"
        );
        assert_eq!(
            build_simple_query("ztable", &["ID; DROP TABLE".to_owned()], None)
                .unwrap_err()
                .code(),
            "invalid_table_field_name"
        );
        assert_eq!(
            build_simple_query("ztable", &[], Some("STATUS='OPEN'; SELECT *"))
                .unwrap_err()
                .code(),
            "where_contains_semicolon"
        );
    }

    #[test]
    fn validates_full_select_queries() {
        assert_eq!(
            validate_full_query("  select * from zdemo  ").unwrap(),
            "select * from zdemo"
        );
        assert_eq!(
            validate_full_query(" ").unwrap_err().code(),
            "table_query_required"
        );
        assert_eq!(
            validate_full_query("DELETE FROM zdemo").unwrap_err().code(),
            "table_query_must_be_select"
        );
        assert_eq!(
            validate_full_query("SELECT * FROM zdemo;")
                .unwrap_err()
                .code(),
            "table_query_contains_semicolon"
        );
    }

    #[test]
    fn breaks_sql_lines_before_clauses_but_not_inside_literals() {
        let sql = "SELECT ID FROM ZDEMO WHERE NOTE = 'FROM WHERE' ORDER   BY ID";
        assert_eq!(
            break_sql_lines(sql),
            "SELECT ID\nFROM ZDEMO\nWHERE NOTE = 'FROM WHERE'\nORDER   BY ID"
        );
    }

    #[test]
    fn rejects_preview_ranges_above_the_endpoint_limit() {
        let options = TableDataOptions {
            offset: 4_950,
            limit: 100,
            ..TableDataOptions::default()
        };
        assert_eq!(
            preview_row_count(&options).unwrap_err().code(),
            "table_preview_range_too_large"
        );
    }

    #[test]
    fn pads_abap_tokens_without_changing_literals() {
        assert_eq!(
            pad_abap_tokens("(STATUS='OPEN')AND(PRIORITY>=2)"),
            "( STATUS = 'OPEN' ) AND ( PRIORITY >= 2 )"
        );
        assert_eq!(pad_abap_tokens("NAME='O''BRIEN'"), "NAME = 'O''BRIEN'");
        assert_eq!(
            pad_abap_tokens("NAME='(two  spaces)'"),
            "NAME = '(two  spaces)'"
        );
    }

    #[test]
    fn keeps_function_calls_tight_and_keyword_parentheses_grouped() {
        assert_eq!(pad_abap_tokens("COUNT(x)"), "COUNT( x )");
        assert_eq!(pad_abap_tokens("ID IN(1,2)"), "ID IN ( 1,2 )");
        assert_eq!(
            pad_abap_tokens("NOT(COALESCE(SCORE,0)<10)"),
            "NOT ( COALESCE( SCORE,0 ) < 10 )"
        );
    }

    #[test]
    fn preserves_composite_comparison_operators() {
        assert_eq!(pad_abap_tokens("LOW<=VALUE"), "LOW <= VALUE");
        assert_eq!(pad_abap_tokens("HIGH>=VALUE"), "HIGH >= VALUE");
        assert_eq!(pad_abap_tokens("STATUS<>VALUE"), "STATUS <> VALUE");
    }

    #[test]
    fn token_padding_is_idempotent() {
        let once = pad_abap_tokens(" STATUS  =  'OPEN' ");
        assert_eq!(once, "STATUS = 'OPEN'");
        assert_eq!(pad_abap_tokens(&once), once);
    }

    #[test]
    fn merges_metadata_by_case_insensitive_column_name() {
        let mut columns = vec![TableColumn {
            name: "event_id".to_owned(),
            sap_type: None,
            col_type: None,
            length: None,
            description: Some("event_id".to_owned()),
        }];
        let metadata = vec![TableColumn {
            name: "EVENT_ID".to_owned(),
            sap_type: Some("N".to_owned()),
            col_type: Some("NUMC".to_owned()),
            length: Some(10),
            description: Some("Event ID".to_owned()),
        }];

        merge_column_metadata(&mut columns, &metadata);

        assert_eq!(columns[0].sap_type.as_deref(), Some("N"));
        assert_eq!(columns[0].col_type.as_deref(), Some("NUMC"));
        assert_eq!(columns[0].length, Some(10));
        assert_eq!(columns[0].description.as_deref(), Some("Event ID"));
    }
}

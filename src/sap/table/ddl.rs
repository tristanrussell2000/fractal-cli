use std::sync::LazyLock;

use regex::Regex;
use thiserror::Error;

static TABLE_DEFINITION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)\bdefine\s+table\s+([A-Za-z0-9_/]+)").expect("table-definition regex is valid")
});

static FIELD_DECLARATION: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?i)^(?:(key)\s+)?([A-Za-z_][A-Za-z0-9_]*)\s*:\s*(.+)$")
        .expect("field-declaration regex is valid")
});

/// The field metadata available directly from an ADT table DDL source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDdlField {
    pub name: String,
    pub declared_type: String,
    pub is_key: bool,
}

/// A table definition parsed from its ADT DDL source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableDdl {
    pub name: String,
    pub fields: Vec<TableDdlField>,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum TableDdlParseError {
    #[error("the source does not contain a 'define table' declaration")]
    MissingDefinition,

    #[error("table {table} does not have an opening body brace")]
    MissingBody { table: String },

    #[error("table {table} has an unterminated body")]
    UnterminatedBody { table: String },

    #[error("table {table} does not declare any fields")]
    NoFields { table: String },

    #[error("unterminated table field declaration: {declaration}")]
    UnterminatedDeclaration { declaration: String },

    #[error("invalid table field declaration: {declaration}")]
    InvalidFieldDeclaration { declaration: String },
}

/// Parse the field list from the DDL source returned for an ADT table object.
///
/// The declared type is intentionally left unresolved: it can be either a
/// built-in `abap.*` type or a DDIC data element/domain name. Technical type,
/// length, and description can be merged from DDIC preview metadata later.
///
/// # Errors
///
/// Returns [`TableDdlParseError`] when the source has no complete table body,
/// has no fields, or contains a declaration the parser cannot identify safely.
pub fn parse_table_ddl(source: &str) -> Result<TableDdl, TableDdlParseError> {
    let code = code_mask(source);
    let definition = TABLE_DEFINITION
        .captures(&code)
        .ok_or(TableDdlParseError::MissingDefinition)?;
    let table_match = definition
        .get(1)
        .ok_or(TableDdlParseError::MissingDefinition)?;
    let table = source[table_match.range()].to_owned();

    let after_name = &code[table_match.end()..];
    let opening_offset = after_name
        .find(|character: char| !character.is_whitespace())
        .ok_or_else(|| TableDdlParseError::MissingBody {
            table: table.clone(),
        })?;
    let opening_brace = table_match.end() + opening_offset;
    if code.as_bytes()[opening_brace] != b'{' {
        return Err(TableDdlParseError::MissingBody { table });
    }

    let closing_brace = find_closing_brace(&code, opening_brace).ok_or_else(|| {
        TableDdlParseError::UnterminatedBody {
            table: table.clone(),
        }
    })?;
    let body_start = opening_brace + 1;
    let body_code = &code[body_start..closing_brace];
    let fields = parse_fields(body_code)?;

    if fields.is_empty() {
        return Err(TableDdlParseError::NoFields { table });
    }

    Ok(TableDdl {
        name: table,
        fields,
    })
}

fn find_closing_brace(code: &str, opening_brace: usize) -> Option<usize> {
    let mut depth = 0_u32;

    for (offset, byte) in code.as_bytes()[opening_brace..].iter().enumerate() {
        match byte {
            b'{' => depth += 1,
            b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(opening_brace + offset);
                }
            }
            _ => {}
        }
    }

    None
}

fn parse_fields(body_code: &str) -> Result<Vec<TableDdlField>, TableDdlParseError> {
    let mut fields = Vec::new();
    let mut declaration_start = 0;

    for (semicolon, _) in body_code.match_indices(';') {
        let declaration_code = &body_code[declaration_start..semicolon];

        if !declaration_code.trim().is_empty() {
            fields.push(parse_field(declaration_code)?);
        }

        declaration_start = semicolon + 1;
    }

    let remainder = &body_code[declaration_start..];
    if !remainder.trim().is_empty() {
        return Err(TableDdlParseError::UnterminatedDeclaration {
            declaration: summarize_declaration(remainder),
        });
    }

    Ok(fields)
}

fn parse_field(declaration: &str) -> Result<TableDdlField, TableDdlParseError> {
    let declaration = strip_leading_annotations(declaration);
    let normalized = normalize_whitespace(declaration);
    let captures = FIELD_DECLARATION.captures(&normalized).ok_or_else(|| {
        TableDdlParseError::InvalidFieldDeclaration {
            declaration: summarize_declaration(&normalized),
        }
    })?;

    let name = captures
        .get(2)
        .expect("field-declaration regex always captures the field name")
        .as_str();
    let type_and_clauses = captures
        .get(3)
        .expect("field-declaration regex always captures the declared type")
        .as_str();
    let declared_type = take_declared_type(type_and_clauses).ok_or_else(|| {
        TableDdlParseError::InvalidFieldDeclaration {
            declaration: summarize_declaration(&normalized),
        }
    })?;

    Ok(TableDdlField {
        name: name.to_owned(),
        declared_type: declared_type.to_owned(),
        is_key: captures.get(1).is_some(),
    })
}

fn strip_leading_annotations(mut declaration: &str) -> &str {
    loop {
        declaration = declaration.trim_start();
        if !declaration.starts_with('@') {
            return declaration;
        }

        let mut nesting = 0_i32;
        let mut annotation_end = declaration.len();

        for (offset, byte) in declaration.bytes().enumerate() {
            match byte {
                b'(' | b'[' | b'{' => nesting += 1,
                b')' | b']' | b'}' => nesting -= 1,
                b'\n' if nesting == 0 => {
                    annotation_end = offset + 1;
                    break;
                }
                _ => {}
            }
        }

        declaration = &declaration[annotation_end..];
    }
}

fn take_declared_type(type_and_clauses: &str) -> Option<&str> {
    let mut parenthesis_depth = 0_u32;
    let mut end = type_and_clauses.len();

    for (offset, character) in type_and_clauses.char_indices() {
        match character {
            '(' => parenthesis_depth += 1,
            ')' => parenthesis_depth = parenthesis_depth.checked_sub(1)?,
            _ if character.is_whitespace() && parenthesis_depth == 0 => {
                end = offset;
                break;
            }
            _ => {}
        }
    }

    if parenthesis_depth != 0 || end == 0 {
        return None;
    }

    Some(type_and_clauses[..end].trim())
}

fn normalize_whitespace(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn summarize_declaration(value: &str) -> String {
    const MAX_LENGTH: usize = 120;

    let normalized = normalize_whitespace(value);
    if normalized.chars().count() <= MAX_LENGTH {
        return normalized;
    }

    let mut summary = normalized.chars().take(MAX_LENGTH).collect::<String>();
    summary.push_str("...");
    summary
}

/// Return an equally sized mask with quoted text and comments replaced by
/// ASCII spaces. Structural indices therefore still address the original.
fn code_mask(source: &str) -> String {
    #[derive(Clone, Copy)]
    enum State {
        Code,
        String,
        LineComment,
        BlockComment,
    }

    let input = source.as_bytes();
    let mut output = input.to_vec();
    let mut state = State::Code;
    let mut index = 0;

    while index < input.len() {
        match state {
            State::Code if input[index] == b'\'' => {
                output[index] = b' ';
                state = State::String;
            }
            State::Code
                if input[index] == b'/'
                    && input.get(index + 1).is_some_and(|next| *next == b'/') =>
            {
                output[index] = b' ';
                output[index + 1] = b' ';
                index += 1;
                state = State::LineComment;
            }
            State::Code
                if input[index] == b'/'
                    && input.get(index + 1).is_some_and(|next| *next == b'*') =>
            {
                output[index] = b' ';
                output[index + 1] = b' ';
                index += 1;
                state = State::BlockComment;
            }
            State::String if input[index] == b'\'' => {
                output[index] = b' ';
                if input.get(index + 1).is_some_and(|next| *next == b'\'') {
                    output[index + 1] = b' ';
                    index += 1;
                } else {
                    state = State::Code;
                }
            }
            State::LineComment if input[index] == b'\n' => state = State::Code,
            State::BlockComment
                if input[index] == b'*'
                    && input.get(index + 1).is_some_and(|next| *next == b'/') =>
            {
                output[index] = b' ';
                output[index + 1] = b' ';
                index += 1;
                state = State::Code;
            }
            State::Code => {}
            State::String | State::LineComment | State::BlockComment => {
                if input[index] != b'\n' && input[index] != b'\r' {
                    output[index] = b' ';
                }
            }
        }

        index += 1;
    }

    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_keys_builtin_types_and_declared_ddic_types() {
        let ddl = parse_table_ddl(
            r#"
@EndUserText.label: 'Synthetic résumé table'
define table zsample_record {
  key client    : abap.clnt not null;
  record_number : abap.numc(10);
  status        : zsample_status;
  amount        : abap.dec(15, 2);
}
"#,
        )
        .unwrap();

        assert_eq!(ddl.name, "zsample_record");
        assert_eq!(
            ddl.fields,
            vec![
                TableDdlField {
                    name: "client".to_owned(),
                    declared_type: "abap.clnt".to_owned(),
                    is_key: true,
                },
                TableDdlField {
                    name: "record_number".to_owned(),
                    declared_type: "abap.numc(10)".to_owned(),
                    is_key: false,
                },
                TableDdlField {
                    name: "status".to_owned(),
                    declared_type: "zsample_status".to_owned(),
                    is_key: false,
                },
                TableDdlField {
                    name: "amount".to_owned(),
                    declared_type: "abap.dec(15, 2)".to_owned(),
                    is_key: false,
                },
            ]
        );
    }

    #[test]
    fn ignores_stacked_and_structured_field_annotations() {
        let ddl = parse_table_ddl(
            r#"
define table zsample_annotation {
  @EndUserText.label: 'Identifier; not a declaration'
  @AbapCatalog.foreignKey.screenCheck: true
  @Consumption.valueHelpDefinition: [{ entity: { name: 'zsample_help' } }]
  key item_id : abap.char(12) not null;
}
"#,
        )
        .unwrap();

        assert_eq!(ddl.fields.len(), 1);
        assert_eq!(ddl.fields[0].name, "item_id");
        assert!(ddl.fields[0].is_key);
    }

    #[test]
    fn consumes_a_multiline_foreign_key_clause_as_one_field_declaration() {
        let ddl = parse_table_ddl(
            r#"
define table zsample_child {
  key client    : abap.clnt not null;
  key parent_id : zsample_parent_id
    with foreign key [0..*, 1] zsample_parent
      where client = zsample_child.client
        and parent_id = zsample_child.parent_id;
  note : abap.char(40);
}
"#,
        )
        .unwrap();

        assert_eq!(ddl.fields.len(), 3);
        assert_eq!(ddl.fields[1].name, "parent_id");
        assert_eq!(ddl.fields[1].declared_type, "zsample_parent_id");
        assert!(ddl.fields[1].is_key);
        assert_eq!(ddl.fields[2].name, "note");
    }

    #[test]
    fn ignores_braces_and_definition_words_in_comments_and_strings() {
        let ddl = parse_table_ddl(
            r#"
@EndUserText.label: 'define table decoy { ; }'
// define table another_decoy {
define table zsample_masking {
  /* A misleading closing brace: } */
  key id : abap.int4;
}
"#,
        )
        .unwrap();

        assert_eq!(ddl.name, "zsample_masking");
        assert_eq!(ddl.fields[0].name, "id");
    }

    #[test]
    fn rejects_an_unterminated_field_instead_of_returning_partial_metadata() {
        let error = parse_table_ddl(
            r#"
define table zsample_incomplete {
  key id : abap.int4;
  status : zsample_status
}
"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            TableDdlParseError::UnterminatedDeclaration {
                declaration: "status : zsample_status".to_owned(),
            }
        );
    }

    #[test]
    fn rejects_non_field_declarations_instead_of_returning_partial_metadata() {
        let error = parse_table_ddl(
            r#"
define table zsample_unrecognized {
  key id : abap.int4;
  include zsample_include;
}
"#,
        )
        .unwrap_err();

        assert_eq!(
            error,
            TableDdlParseError::InvalidFieldDeclaration {
                declaration: "include zsample_include".to_owned(),
            }
        );
    }

    #[test]
    fn reports_missing_and_unterminated_table_bodies() {
        assert_eq!(
            parse_table_ddl("define view zsample_view as select from zsample_source").unwrap_err(),
            TableDdlParseError::MissingDefinition
        );
        assert_eq!(
            parse_table_ddl("define table zsample_no_body").unwrap_err(),
            TableDdlParseError::MissingBody {
                table: "zsample_no_body".to_owned(),
            }
        );
        assert_eq!(
            parse_table_ddl("define table zsample_open { key id : abap.int4;").unwrap_err(),
            TableDdlParseError::UnterminatedBody {
                table: "zsample_open".to_owned(),
            }
        );
    }
}

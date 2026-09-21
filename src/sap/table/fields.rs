use super::{TableDataResult, TableError, TableFieldMetadata};

/// The `DD03L` columns read, in the order the query asks for them.
const COLUMNS: [&str; 6] = [
    "POSITION",
    "FIELDNAME",
    "KEYFLAG",
    "ROLLNAME",
    "DATATYPE",
    "LENG",
];

/// Reads the active field list SAP records for one DDIC entity.
///
/// `DD03L` is used rather than the entity's own DDL source because the DDL is
/// not a complete field list: an `include` names a structure whose fields are
/// spliced in but not shown, and an append structure does not appear in the
/// source at all. `DD03L` holds every field of both kinds, already flattened
/// and ordered by `POSITION`.
pub(super) fn field_query(entity: &str) -> String {
    format!(
        "SELECT {} FROM DD03L WHERE TABNAME = '{entity}' AND AS4LOCAL = 'A'",
        COLUMNS.join(", ")
    )
}

/// Turns a `DD03L` result into field metadata, in `POSITION` order.
///
/// # Errors
///
/// Returns [`TableError::FieldListUnreadable`] when the response is missing a
/// column the mapping needs.
pub(super) fn parse_fields(
    entity: &str,
    result: &TableDataResult,
) -> Result<Vec<TableFieldMetadata>, TableError> {
    let mut indexes = [0_usize; COLUMNS.len()];
    for (slot, name) in indexes.iter_mut().zip(COLUMNS) {
        *slot = result
            .columns
            .iter()
            .position(|column| column.name.eq_ignore_ascii_case(name))
            .ok_or_else(|| TableError::FieldListUnreadable {
                entity: entity.to_owned(),
            })?;
    }

    let mut rows: Vec<_> = result
        .rows
        .iter()
        .filter_map(|row| {
            let cell = |slot: usize| row.get(indexes[slot]).map_or("", |value| value.trim());
            let name = cell(1);
            // `.INCLUDE` and `.INCLU--AP` mark where a structure or an append
            // was spliced in. The fields themselves are already listed.
            if name.is_empty() || name.starts_with('.') {
                return None;
            }

            let col_type = cell(4);
            Some((
                cell(0).to_owned(),
                TableFieldMetadata {
                    name: name.to_ascii_lowercase(),
                    declared_type: declared_type(cell(3), col_type),
                    is_key: cell(2).eq_ignore_ascii_case("X"),
                    sap_type: None,
                    col_type: (!col_type.is_empty()).then(|| col_type.to_ascii_uppercase()),
                    length: parse_length(cell(5)),
                    description: None,
                },
            ))
        })
        .collect();

    // POSITION is zero-padded, so ordering it as text orders it numerically.
    rows.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(rows.into_iter().map(|(_, field)| field).collect())
}

/// The data element a field is typed with, or the built-in type when it has
/// none. Lower-cased to match the spelling a table's DDL source uses.
fn declared_type(rollname: &str, col_type: &str) -> String {
    if !rollname.is_empty() {
        return rollname.to_ascii_lowercase();
    }
    if col_type.is_empty() {
        return String::new();
    }
    format!("abap.{}", col_type.to_ascii_lowercase())
}

fn parse_length(value: &str) -> Option<u32> {
    value.trim_start_matches('0').parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::table::TableColumn;

    fn column(name: &str) -> TableColumn {
        TableColumn {
            name: name.to_owned(),
            sap_type: None,
            col_type: None,
            length: None,
            description: None,
        }
    }

    fn result(rows: Vec<Vec<&str>>) -> TableDataResult {
        TableDataResult {
            entity: None,
            executed_query: None,
            total_rows: None,
            columns: COLUMNS.iter().copied().map(column).collect(),
            rows: rows
                .into_iter()
                .map(|row| row.into_iter().map(str::to_owned).collect())
                .collect(),
        }
    }

    #[test]
    fn names_the_entity_and_quotes_it_once() {
        assert_eq!(
            field_query("ZSAMPLE_RECORD"),
            "SELECT POSITION, FIELDNAME, KEYFLAG, ROLLNAME, DATATYPE, LENG FROM DD03L WHERE TABNAME = 'ZSAMPLE_RECORD' AND AS4LOCAL = 'A'"
        );
    }

    #[test]
    fn orders_by_position_and_maps_every_column() {
        let fields = parse_fields(
            "ZSAMPLE_RECORD",
            &result(vec![
                vec!["0002", "STATUS", "", "ZSAMPLE_STATUS", "CHAR", "000012"],
                vec!["0001", "MANDT", "X", "MANDT", "CLNT", "000003"],
            ]),
        )
        .unwrap();

        assert_eq!(fields.len(), 2);
        assert_eq!(fields[0].name, "mandt");
        assert!(fields[0].is_key);
        assert_eq!(fields[0].declared_type, "mandt");
        assert_eq!(fields[0].col_type.as_deref(), Some("CLNT"));
        assert_eq!(fields[0].length, Some(3));
        assert_eq!(fields[1].name, "status");
        assert!(!fields[1].is_key);
        assert_eq!(fields[1].declared_type, "zsample_status");
    }

    /// The whole reason this reads DD03L: the spliced fields are present and
    /// only the marker rows have to be dropped.
    #[test]
    fn drops_include_and_append_markers_but_keeps_their_fields() {
        let fields = parse_fields(
            "ZSAMPLE_RECORD",
            &result(vec![
                vec!["0001", "MANDT", "X", "MANDT", "CLNT", "000003"],
                vec!["0002", ".INCLUDE", "", "", "", "000000"],
                vec!["0003", "FROM_INCLUDE", "", "ZSAMPLE_TEXT", "CHAR", "000020"],
                vec!["0004", ".INCLU--AP", "", "", "", "000000"],
                vec!["0005", "FROM_APPEND", "", "ZSAMPLE_FLAG", "CHAR", "000001"],
            ]),
        )
        .unwrap();

        let names: Vec<_> = fields.iter().map(|field| field.name.as_str()).collect();
        assert_eq!(names, ["mandt", "from_include", "from_append"]);
    }

    #[test]
    fn types_a_field_with_no_data_element_from_its_built_in_type() {
        let fields = parse_fields(
            "ZSAMPLE_RECORD",
            &result(vec![vec!["0001", "MANDT", "X", "", "CLNT", "000003"]]),
        )
        .unwrap();

        assert_eq!(fields[0].declared_type, "abap.clnt");
    }

    #[test]
    fn reports_a_response_missing_a_column_it_needs() {
        let mut response = result(vec![]);
        response.columns.retain(|column| column.name != "KEYFLAG");

        let error = parse_fields("ZSAMPLE_RECORD", &response).unwrap_err();
        assert!(matches!(error, TableError::FieldListUnreadable { .. }));
    }
}

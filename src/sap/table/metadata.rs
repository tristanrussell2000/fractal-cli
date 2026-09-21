use std::collections::HashMap;

use super::TableColumn;

/// One table field, as SAP records it, enriched from DDIC preview metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableFieldMetadata {
    pub name: String,
    pub declared_type: String,
    pub is_key: bool,
    pub sap_type: Option<String>,
    pub col_type: Option<String>,
    pub length: Option<u32>,
    pub description: Option<String>,
}

/// The combined metadata available for one DDIC table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMetadata {
    pub entity: String,
    pub total_rows: Option<u64>,
    pub fields: Vec<TableFieldMetadata>,
}

/// Adds what only a preview response carries to the recorded field list.
///
/// The field list decides which fields exist, their order, and which are key.
/// The preview contributes the runtime type and the description, and fills
/// `col_type`/`length` only where the field list left them empty — some
/// releases serve no `colType` at all, so the recorded value is preferred.
pub(super) fn merge_table_metadata(
    entity: String,
    fields: Vec<TableFieldMetadata>,
    columns: &[TableColumn],
) -> TableMetadata {
    let columns_by_name: HashMap<_, _> = columns
        .iter()
        .map(|column| (column.name.to_ascii_uppercase(), column))
        .collect();
    let fields = fields
        .into_iter()
        .map(|field| {
            let Some(column) = columns_by_name.get(&field.name.to_ascii_uppercase()) else {
                return field;
            };

            TableFieldMetadata {
                sap_type: column.sap_type.clone(),
                col_type: field.col_type.or_else(|| column.col_type.clone()),
                length: field.length.or(column.length),
                description: column.description.clone(),
                ..field
            }
        })
        .collect();

    TableMetadata {
        entity,
        total_rows: None,
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, declared_type: &str, is_key: bool) -> TableFieldMetadata {
        TableFieldMetadata {
            name: name.to_owned(),
            declared_type: declared_type.to_owned(),
            is_key,
            sap_type: None,
            col_type: None,
            length: None,
            description: None,
        }
    }

    #[test]
    fn enriches_case_insensitively_and_keeps_the_recorded_order() {
        let result = merge_table_metadata(
            "zsample_record".to_owned(),
            vec![
                field("client", "mandt", true),
                field("status", "zsample_status", false),
            ],
            &[
                TableColumn {
                    name: "STATUS".to_owned(),
                    sap_type: Some("C".to_owned()),
                    col_type: Some("CHAR".to_owned()),
                    length: Some(12),
                    description: Some("Status".to_owned()),
                },
                TableColumn {
                    name: "CLIENT".to_owned(),
                    sap_type: Some("C".to_owned()),
                    col_type: Some("CLNT".to_owned()),
                    length: Some(3),
                    description: Some("Client".to_owned()),
                },
            ],
        );

        assert_eq!(result.entity, "zsample_record");
        assert_eq!(result.total_rows, None);
        assert_eq!(result.fields[0].name, "client");
        assert!(result.fields[0].is_key);
        assert_eq!(result.fields[0].col_type.as_deref(), Some("CLNT"));
        assert_eq!(result.fields[0].sap_type.as_deref(), Some("C"));
        assert_eq!(result.fields[1].name, "status");
        assert_eq!(result.fields[1].description.as_deref(), Some("Status"));
    }

    /// A release that serves no `colType` must not blank the recorded one.
    #[test]
    fn keeps_the_recorded_col_type_when_the_preview_serves_none() {
        let mut recorded = field("client", "mandt", true);
        recorded.col_type = Some("CLNT".to_owned());
        recorded.length = Some(3);

        let result = merge_table_metadata(
            "zsample_record".to_owned(),
            vec![recorded],
            &[TableColumn {
                name: "CLIENT".to_owned(),
                sap_type: Some("C".to_owned()),
                col_type: None,
                length: None,
                description: Some("Client".to_owned()),
            }],
        );

        assert_eq!(result.fields[0].col_type.as_deref(), Some("CLNT"));
        assert_eq!(result.fields[0].length, Some(3));
        assert_eq!(result.fields[0].sap_type.as_deref(), Some("C"));
    }

    #[test]
    fn leaves_a_field_the_preview_does_not_mention_untouched() {
        let result = merge_table_metadata(
            "zsample_partial".to_owned(),
            vec![field("id", "abap.int4", true)],
            &[TableColumn {
                name: "PREVIEW_ONLY".to_owned(),
                sap_type: Some("C".to_owned()),
                col_type: Some("CHAR".to_owned()),
                length: Some(4),
                description: Some("Ignored".to_owned()),
            }],
        );

        assert_eq!(result.fields.len(), 1);
        assert_eq!(result.fields[0].name, "id");
        assert_eq!(result.fields[0].sap_type, None);
        assert_eq!(result.fields[0].description, None);
    }
}

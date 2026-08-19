use std::collections::HashMap;

use super::{TableColumn, TableDdl};

/// One table field enriched from both DDL and DDIC preview metadata.
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

pub(super) fn merge_table_metadata(ddl: TableDdl, columns: &[TableColumn]) -> TableMetadata {
    let columns_by_name: HashMap<_, _> = columns
        .iter()
        .map(|column| (column.name.to_ascii_uppercase(), column))
        .collect();
    let fields = ddl
        .fields
        .into_iter()
        .map(|field| {
            let column = columns_by_name
                .get(&field.name.to_ascii_uppercase())
                .copied();

            TableFieldMetadata {
                name: field.name,
                declared_type: field.declared_type,
                is_key: field.is_key,
                sap_type: column.and_then(|column| column.sap_type.clone()),
                col_type: column.and_then(|column| column.col_type.clone()),
                length: column.and_then(|column| column.length),
                description: column.and_then(|column| column.description.clone()),
            }
        })
        .collect();

    TableMetadata {
        entity: ddl.name,
        total_rows: None,
        fields,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::table::TableDdlField;

    #[test]
    fn merges_columns_case_insensitively_in_ddl_order() {
        let result = merge_table_metadata(
            TableDdl {
                name: "zsample_record".to_owned(),
                fields: vec![
                    TableDdlField {
                        name: "client".to_owned(),
                        declared_type: "abap.clnt".to_owned(),
                        is_key: true,
                    },
                    TableDdlField {
                        name: "status".to_owned(),
                        declared_type: "zsample_status".to_owned(),
                        is_key: false,
                    },
                ],
            },
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
        assert_eq!(result.fields[1].name, "status");
        assert_eq!(result.fields[1].declared_type, "zsample_status");
        assert_eq!(result.fields[1].description.as_deref(), Some("Status"));
    }

    #[test]
    fn leaves_missing_enrichment_empty_and_ignores_extra_preview_columns() {
        let result = merge_table_metadata(
            TableDdl {
                name: "zsample_partial".to_owned(),
                fields: vec![TableDdlField {
                    name: "id".to_owned(),
                    declared_type: "abap.int4".to_owned(),
                    is_key: true,
                }],
            },
            &[TableColumn {
                name: "PREVIEW_ONLY".to_owned(),
                sap_type: Some("C".to_owned()),
                col_type: Some("CHAR".to_owned()),
                length: Some(1),
                description: Some("Not in DDL".to_owned()),
            }],
        );

        assert_eq!(result.fields.len(), 1);
        assert_eq!(result.fields[0].name, "id");
        assert_eq!(result.fields[0].sap_type, None);
        assert_eq!(result.fields[0].col_type, None);
        assert_eq!(result.fields[0].length, None);
        assert_eq!(result.fields[0].description, None);
    }
}

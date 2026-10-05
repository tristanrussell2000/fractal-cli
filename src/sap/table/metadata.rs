use crate::sap::ddic_fields::DdicField;

/// The combined metadata available for one DDIC table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TableMetadata {
    pub entity: String,
    pub total_rows: Option<u64>,
    pub fields: Vec<TableFieldMetadata>,
}

/// One table field, projected from the recorded field list.
///
/// A narrower view of [`DdicField`] than `object show` reports: the table
/// commands need a type, a length and a key flag, and the write path needs a
/// declared type it can recognise a client column by.
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

impl From<DdicField> for TableFieldMetadata {
    fn from(field: DdicField) -> Self {
        Self {
            declared_type: declared_type(&field),
            name: field.name,
            is_key: field.is_key,
            sap_type: field.sap_type,
            col_type: field.col_type,
            length: field.length,
            description: field.description,
        }
    }
}

/// The data element a field is typed with, or the built-in type when it has
/// none. Spelled the way a DDL source spells it, because that is what the
/// client-column check matches against.
fn declared_type(field: &DdicField) -> String {
    if let Some(data_element) = &field.data_element {
        return data_element.clone();
    }
    match &field.col_type {
        Some(col_type) => format!("abap.{}", col_type.to_ascii_lowercase()),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ddic_field(name: &str, data_element: Option<&str>, col_type: Option<&str>) -> DdicField {
        DdicField {
            name: name.to_owned(),
            is_key: true,
            data_element: data_element.map(str::to_owned),
            domain: None,
            col_type: col_type.map(str::to_owned),
            sap_type: Some("C".to_owned()),
            length: Some(3),
            decimals: None,
            not_null: false,
            check_table: None,
            description: Some("Client".to_owned()),
        }
    }

    #[test]
    fn projects_the_fields_the_table_commands_use() {
        let field = TableFieldMetadata::from(ddic_field("mandt", Some("mandt"), Some("CLNT")));

        assert_eq!(field.name, "mandt");
        assert_eq!(field.declared_type, "mandt");
        assert!(field.is_key);
        assert_eq!(field.col_type.as_deref(), Some("CLNT"));
        assert_eq!(field.sap_type.as_deref(), Some("C"));
        assert_eq!(field.length, Some(3));
        assert_eq!(field.description.as_deref(), Some("Client"));
    }

    /// A field with no data element still has to present a declared type the
    /// client-column check can recognise, or a write to a client-dependent
    /// table typed that way would be refused for an incomplete key.
    #[test]
    fn names_a_built_in_type_the_way_a_ddl_source_would() {
        let field = TableFieldMetadata::from(ddic_field("mandt", None, Some("CLNT")));
        assert_eq!(field.declared_type, "abap.clnt");

        let field = TableFieldMetadata::from(ddic_field("mystery", None, None));
        assert_eq!(field.declared_type, "");
    }
}

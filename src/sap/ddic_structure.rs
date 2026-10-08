//! Inspection of DDIC objects that have a flat field list — structures,
//! tables and append structures.
//!
//! The DDL source is not the field list: an `include` names another structure
//! whose fields are spliced in but never shown, and an append does not appear
//! in the source at all. The recorded list in `DD03L` holds both kinds already
//! flattened, so that is what this reads.
//!
//! **What came back is not necessarily a structure.** SAP's `structures`
//! collection serves transparent tables and append structures through the same
//! route, and a transparent table is a table and nothing more — there is one
//! `DD02L` row for `MARA`, not a table plus a structure of the same name. It
//! is reachable here because ABAP treats a table name as a row type, so the
//! field list is a sensible answer to give; it is reported as a table because
//! that is what it is.
//!
//! **The document cannot be asked what it is.** ADT derives `adtcore:type`
//! from the collection in the URI, not from the object: the same `MARA` comes
//! back as `TABL/DS` through `ddic/structures` and `TABL/DT` through
//! `ddic/tables`. `DD02L-TABCLASS` is the only answer that does not depend on
//! how it was asked.
//!
//! The header — description, package, which layer arrived — still comes from
//! the object's own ADT document, so it reads the same way every other object
//! in Fractal does.

use thiserror::Error;

use super::{
    adt_response::{AdtResponseParseError, parse_adt_document},
    adt_version::AdtVersion,
    client::{SapClient, SapClientError},
    ddic_fields::{DdicField, DdicFieldsError, DdicTableClass, read_field_list},
    editable_source::validate_object_name,
    find_child, find_non_empty_attribute,
    metadata_document::declared_version,
    table::{QueryOptions, run_query},
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

const STRUCTURE_COLLECTION: &str = "/sap/bc/adt/ddic/structures";
/// The language a view's description is read in, matching the field texts.
const VIEW_TEXT_LANGUAGE: &str = "E";

/// One object, with every field an include or append contributed in place.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DdicStructureInfo {
    pub name: String,
    /// What `DD02L` says this is — a structure, a table, an append structure.
    /// Never taken from the document, whose `adtcore:type` reflects the URI it
    /// was fetched through rather than the object.
    pub kind: DdicTableClass,
    /// Absent for a view: ADT serves no usable document for one, so there is
    /// no address to report.
    pub uri: Option<String>,
    /// The layer that was asked for.
    pub requested_version: &'static str,
    /// The layer the document declared itself to be. Absent for a view, which
    /// has no document to declare it — the field list was read at the layer
    /// that was asked for, with no silent fallback for it to disagree with.
    pub version: Option<String>,
    pub description: Option<String>,
    pub package: Option<String>,
    pub field_count: usize,
    pub key_field_count: usize,
    pub fields: Vec<DdicField>,
}

#[derive(Debug, Error)]
pub enum DdicStructureError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error(transparent)]
    Parse(#[from] AdtResponseParseError),
    #[error(transparent)]
    Fields(#[from] DdicFieldsError),
    #[error("invalid DDIC object name '{0}'")]
    InvalidName(String),
}

impl DdicStructureError {
    #[must_use]
    pub fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            Self::Fields(DdicFieldsError::Query(error)) => error.sap_error(),
            _ => None,
        }
    }

    /// Whether this says the name is not an object of this kind, as opposed to
    /// something going wrong while reading one that is.
    #[must_use]
    pub const fn is_not_found(&self) -> bool {
        matches!(self, Self::Fields(DdicFieldsError::NoFields { .. }))
            || matches!(self, Self::Sap(_))
    }
}

impl ReportableError for DdicStructureError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(error) => error.code(),
            Self::Fields(DdicFieldsError::Query(error)) => error.code(),
            Self::Fields(DdicFieldsError::NoFields { .. }) => "ddic_structure_no_fields",
            Self::Fields(DdicFieldsError::TooManyFields { .. }) => "ddic_structure_too_many_fields",
            Self::Fields(DdicFieldsError::Unreadable { .. }) => {
                "ddic_structure_field_list_unreadable"
            }
            Self::InvalidName(_) => "invalid_object_name",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(error) => error.hint(),
            Self::Fields(DdicFieldsError::Query(error)) => error.hint(),
            Self::Fields(DdicFieldsError::NoFields { .. }) => {
                Some("Check the name exists and has been activated.".to_owned())
            }
            Self::Fields(DdicFieldsError::TooManyFields { .. }) => {
                Some("This object is too wide for Fractal to describe safely.".to_owned())
            }
            Self::Fields(DdicFieldsError::Unreadable { .. }) => {
                Some("The DD03L read did not return the expected columns.".to_owned())
            }
            Self::InvalidName(_) => {
                Some("Use a DDIC name: letters, digits, underscore and slash only.".to_owned())
            }
        }
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Fields(DdicFieldsError::NoFields { name }) => {
                Some(suggested_command::object_search("STRU", name))
            }
            _ => None,
        }
    }
}

/// Reads one object's ADT header and its complete field list.
///
/// # Errors
///
/// Returns [`DdicStructureError`] when the name is invalid, SAP cannot serve
/// the document or the field list, or either response cannot be parsed.
pub async fn get_ddic_structure(
    sap: &mut SapClient,
    name: &str,
    version: AdtVersion,
) -> Result<DdicStructureInfo, DdicStructureError> {
    let name =
        validate_object_name(name).map_err(|_| DdicStructureError::InvalidName(name.to_owned()))?;

    // The field list comes first because it says what the object is, and that
    // decides where its header can be read from.
    let list = read_field_list(sap, &name, version).await?;
    let header = match list.class {
        DdicTableClass::View => read_view_header(sap, &name).await?,
        _ => read_document_header(sap, &name, version).await?,
    };

    Ok(DdicStructureInfo {
        name: header.name.unwrap_or_else(|| name.clone()),
        kind: list.class,
        uri: header.uri,
        requested_version: version.as_str(),
        version: header.version,
        description: header.description,
        package: header.package,
        field_count: list.fields.len(),
        key_field_count: list.fields.iter().filter(|field| field.is_key).count(),
        fields: list.fields,
    })
}

/// What a header read contributes, whichever source answered.
#[derive(Debug, Default)]
struct Header {
    name: Option<String>,
    uri: Option<String>,
    version: Option<String>,
    description: Option<String>,
    package: Option<String>,
}

/// The header of anything ADT serves through its structures collection —
/// structures, tables and append structures.
async fn read_document_header(
    sap: &SapClient,
    name: &str,
    version: AdtVersion,
) -> Result<Header, DdicStructureError> {
    let uri = structure_uri(name);
    let xml = sap
        .get_text_with_query(&uri, &[("version", version.as_str())])
        .await?;

    let document = parse_adt_document(&xml)?;
    let root = document.root_element();
    Ok(Header {
        name: find_non_empty_attribute(root, "name"),
        uri: Some(uri),
        version: declared_version(root),
        description: find_non_empty_attribute(root, "description"),
        package: find_child(root, "packageRef")
            .and_then(|node| find_non_empty_attribute(node, "name")),
    })
}

/// The header of a view, which ADT will not serve: `ddic/structures/<view>` is
/// a 404 and `ddic/views/<view>` a 500. The DDIC knows it anyway.
///
/// `DD25T` rather than `DD02T`, because a view has a row in both and `DD02T`
/// describes the generated table behind it — "Generated Table for View" where
/// `DD25T` names the view itself. `TADIR` is joined outer and pinned to
/// `R3TR VIEW`, because a name can hold several `TADIR` rows under different
/// object types, each with its own package.
async fn read_view_header(sap: &mut SapClient, name: &str) -> Result<Header, DdicStructureError> {
    let query = format!(
        "SELECT t~ddtext, d~devclass FROM dd25t AS t \
         LEFT OUTER JOIN tadir AS d \
         ON d~obj_name = t~viewname AND d~pgmid = 'R3TR' AND d~object = 'VIEW' \
         WHERE t~viewname = '{name}' AND t~ddlanguage = '{VIEW_TEXT_LANGUAGE}' \
         AND t~as4local = 'A'"
    );
    let result = run_query(
        sap,
        &query,
        &QueryOptions {
            offset: 0,
            limit: 1,
        },
    )
    .await
    .map_err(DdicFieldsError::Query)?;

    let cell = |column: &str| {
        let index = result
            .columns
            .iter()
            .position(|candidate| candidate.name.eq_ignore_ascii_case(column))?;
        let value = result.rows.first()?.get(index)?.trim();
        (!value.is_empty()).then(|| value.to_owned())
    };

    Ok(Header {
        description: cell("DDTEXT"),
        package: cell("DEVCLASS"),
        ..Header::default()
    })
}

fn structure_uri(name: &str) -> String {
    let path_name = name.to_ascii_lowercase().replace('/', "%2f");
    format!("{STRUCTURE_COLLECTION}/{path_name}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builds_uris_for_plain_and_namespaced_names() {
        assert_eq!(
            structure_uri("ZSAMPLE_RECORD_S"),
            "/sap/bc/adt/ddic/structures/zsample_record_s"
        );
        assert_eq!(
            structure_uri("/ACME/RECORD_S"),
            "/sap/bc/adt/ddic/structures/%2facme%2frecord_s"
        );
    }
}

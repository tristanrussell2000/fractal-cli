//! Object short-description retrieval.

use thiserror::Error;

use super::{
    adt_object_uri::{AdtObjectUriError, validate_adt_object_uri},
    adt_response::{AdtResponseParseError, parse_adt_document},
    client::{SapClient, SapClientError},
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

/// A failure while reading an object's description.
#[derive(Debug, Error)]
pub enum ObjectInfoError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error(transparent)]
    Uri(#[from] AdtObjectUriError),
    #[error(transparent)]
    Parse(#[from] AdtResponseParseError),
    #[error("no description found for object URI: {0}")]
    NoDescription(String),
}

impl ObjectInfoError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            _ => None,
        }
    }
}

impl ReportableError for ObjectInfoError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Uri(error) => error.code(),
            Self::Parse(error) => error.code(),
            Self::NoDescription(_) => "no_description",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Sap(error) => error.hint()?,
            Self::Uri(error) => error.hint()?,
            Self::Parse(error) => error.hint()?,
            Self::NoDescription(_) => {
                "This URI doesn't expose a description (shadow or fragment URIs are common causes). Try `fractal object xml` for full metadata, or strip any #fragment from the URI and retry against the primary object."
                    .to_owned()
            }
        })
    }

    /// A read-only command that diagnoses this failure, if one exists.
    fn suggested_command(&self) -> Option<String> {
        match self {
            // The object exists; only this view of it lacks a description.
            Self::NoDescription(uri) => Some(suggested_command::object_xml(uri)),
            Self::Sap(error) => error.suggested_command(),
            Self::Uri(_) | Self::Parse(_) => None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectInfoResult {
    pub uri: String,
    pub description: String,
}

/// Fetches an ADT object's metadata XML and extracts its short description.
///
/// This requests the same URI as [`get_xml`]; unlike `get_xml`, the response is
/// parsed and reduced to the first `description` attribute found anywhere in
/// the document, matching `SapFractal`'s `get_object_info` behavior.
///
/// # Errors
///
/// Returns [`ObjectInfoError::InvalidUri`] for a non-ADT URI, the underlying SAP error
/// when the metadata request fails, [`ObjectInfoError::Parse`] when the response is
/// not valid XML, or [`ObjectInfoError::NoDescription`] when no `description`
/// attribute is present anywhere in the document.
pub async fn get_object_info(
    sap: &mut SapClient,
    uri: &str,
) -> Result<ObjectInfoResult, ObjectInfoError> {
    validate_adt_object_uri(uri)?;
    let xml = sap.get_text(uri).await?;
    parse_object_info(&xml, uri)
}

fn parse_object_info(xml: &str, uri: &str) -> Result<ObjectInfoResult, ObjectInfoError> {
    let document = parse_adt_document(xml)?;
    let description = find_description(document.root_element())
        .ok_or_else(|| ObjectInfoError::NoDescription(uri.to_owned()))?;
    Ok(ObjectInfoResult {
        uri: uri.to_owned(),
        description,
    })
}

/// Depth-first search for the first `description` attribute in the document,
/// checking each element's own attributes before descending into its children.
fn find_description(node: roxmltree::Node) -> Option<String> {
    node.attributes()
        .find(|attr| attr.name() == "description")
        .map(|attr| attr.value().to_owned())
        .or_else(|| {
            node.children()
                .filter(roxmltree::Node::is_element)
                .find_map(find_description)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_description_on_the_root_element() {
        let xml = r#"<class:abapClass xmlns:class="urn:test" description="Root class"/>"#;
        let info = parse_object_info(xml, "/sap/bc/adt/oo/classes/zcl_example").unwrap();
        assert_eq!(info.description, "Root class");
        assert_eq!(info.uri, "/sap/bc/adt/oo/classes/zcl_example");
    }

    #[test]
    fn finds_description_nested_under_a_child_element() {
        let xml = r#"<class:abapClass xmlns:class="urn:test">
            <class:include>
                <class:section description="Nested description"/>
            </class:include>
        </class:abapClass>"#;
        let info = parse_object_info(xml, "/uri").unwrap();
        assert_eq!(info.description, "Nested description");
    }

    #[test]
    fn matches_description_attributes_regardless_of_namespace_prefix() {
        let xml = r#"<class:abapClass xmlns:class="urn:test" xmlns:adtcore="urn:adt" adtcore:description="Namespaced description"/>"#;
        let info = parse_object_info(xml, "/uri").unwrap();
        assert_eq!(info.description, "Namespaced description");
    }

    #[test]
    fn returns_a_hinted_error_when_no_description_is_present() {
        let xml = r#"<class:abapClass xmlns:class="urn:test"><class:include/></class:abapClass>"#;
        let error = parse_object_info(xml, "/uri").unwrap_err();
        assert_eq!(error.code(), "no_description");
        assert!(error.hint().is_some());
    }

    #[test]
    fn returns_a_parse_error_for_malformed_object_info_xml() {
        let error = parse_object_info("<not-closed", "/uri").unwrap_err();
        assert_eq!(error.code(), "adt_response_parse_error");
    }
}

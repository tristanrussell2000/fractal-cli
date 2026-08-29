//! The ADT XML parse failure shared by search, object info, and usages.

use crate::reportable_error::ReportableError;
use roxmltree::Document;
use thiserror::Error;

/// SAP returned a response that is not the ADT XML the operation expected.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("could not parse ADT XML response: {0}")]
pub struct AdtResponseParseError(pub String);

impl ReportableError for AdtResponseParseError {
    fn code(&self) -> &'static str {
        "adt_response_parse_error"
    }

    fn hint(&self) -> Option<String> {
        Some("The SAP ADT response did not match the expected search format.".to_owned())
    }
}

/// Parses an ADT XML response body.
///
/// # Errors
///
/// Returns [`AdtResponseParseError`] when the body is not well-formed XML.
pub(super) fn parse_adt_document(xml: &str) -> Result<Document<'_>, AdtResponseParseError> {
    Document::parse(xml).map_err(|error| AdtResponseParseError(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_malformed_xml_with_a_stable_code() {
        let error = parse_adt_document("<not-closed>").unwrap_err();

        assert_eq!(error.code(), "adt_response_parse_error");
        assert!(error.hint().is_some());
    }
}

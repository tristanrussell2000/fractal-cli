//! Validation shared by every operation that takes an ADT object URI.
//!
//! Four of the five object operations reject the same two malformed URIs, so
//! the check and its stable codes are defined once and wrapped by each
//! operation's error rather than restated per module.

use crate::reportable_error::ReportableError;
use thiserror::Error;

pub(super) const ADT_ROOT: &str = "/sap/bc/adt/";
pub(super) const SOURCE_SUFFIX: &str = "/source/main";

/// A caller-supplied ADT object URI that cannot be requested as given.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdtObjectUriError {
    #[error("invalid ADT object URI: {0}")]
    NotAnAdtUri(String),
    #[error("the URI already includes a source suffix: {0}")]
    DoubledSourceSuffix(String),
}

impl ReportableError for AdtObjectUriError {
    fn code(&self) -> &'static str {
        match self {
            Self::NotAnAdtUri(_) => "invalid_adt_uri",
            Self::DoubledSourceSuffix(_) => "doubled_source_suffix",
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::NotAnAdtUri(_) => {
                "Use an object URI under /sap/bc/adt/. Discover it with `fractal object search`."
                    .to_owned()
            }
            Self::DoubledSourceSuffix(_) => {
                "Pass the object URI without /source/main; Fractal appends it.".to_owned()
            }
        })
    }
}

/// Verifies that a URI addresses the ADT namespace.
///
/// # Errors
///
/// Returns [`AdtObjectUriError::NotAnAdtUri`] when the URI is outside
/// `/sap/bc/adt/`.
pub fn validate_adt_object_uri(uri: &str) -> Result<(), AdtObjectUriError> {
    if uri.starts_with(ADT_ROOT) {
        Ok(())
    } else {
        Err(AdtObjectUriError::NotAnAdtUri(uri.to_owned()))
    }
}

/// Verifies a URI that Fractal will extend with the source suffix itself.
///
/// # Errors
///
/// Returns [`AdtObjectUriError`] for a non-ADT URI, or when the caller already
/// appended `/source/main`.
pub fn validate_source_object_uri(uri: &str) -> Result<(), AdtObjectUriError> {
    validate_adt_object_uri(uri)?;
    if uri.ends_with(SOURCE_SUFFIX) {
        return Err(AdtObjectUriError::DoubledSourceSuffix(uri.to_owned()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_uris_outside_the_adt_namespace() {
        let error = validate_adt_object_uri("/sap/bc/rest/thing").unwrap_err();

        assert_eq!(error.code(), "invalid_adt_uri");
        assert!(error.hint().unwrap().contains("/sap/bc/adt/"));
    }

    #[test]
    fn rejects_a_uri_that_already_carries_the_source_suffix() {
        let error =
            validate_source_object_uri("/sap/bc/adt/oo/classes/zcl_x/source/main").unwrap_err();

        assert_eq!(error.code(), "doubled_source_suffix");
    }

    #[test]
    fn accepts_a_plain_object_uri_for_both_checks() {
        assert!(validate_adt_object_uri("/sap/bc/adt/oo/classes/zcl_x").is_ok());
        assert!(validate_source_object_uri("/sap/bc/adt/oo/classes/zcl_x").is_ok());
    }
}

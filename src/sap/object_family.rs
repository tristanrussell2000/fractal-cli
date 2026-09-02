//! Which family an object type belongs to.
//!
//! Two families reach the same create and delete operations by different
//! validation: source-based objects, which have a `source/main` and can be
//! filled with `edit set`, and DDIC metadata objects, which have no source at
//! all and are filled by writing their XML back.
//!
//! The distinction is not cosmetic. Routing a data element through the
//! source-based path builds a `source/main` URI that 404s, and the failure
//! surfaces late — after the object has already been created.

use thiserror::Error;

use super::{editable_source::EditableAdtObjectType, metadata_object::MetadataAdtObjectType};
use crate::{reportable_error::ReportableError, suggested_command};

/// Which family an object type belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdtObjectFamily {
    /// Has source; `edit set` fills it.
    Source(EditableAdtObjectType),
    /// Has no source; its XML document is the object.
    Metadata(MetadataAdtObjectType),
}

impl AdtObjectFamily {
    /// Resolves a logical type such as `CLAS` or `DTEL` to its family.
    ///
    /// # Errors
    ///
    /// Returns [`UnsupportedObjectTypeError`] when the type belongs to neither
    /// family, naming both sets rather than only the one tried first.
    pub fn parse(value: &str) -> Result<Self, UnsupportedObjectTypeError> {
        EditableAdtObjectType::parse(value).map_or_else(
            |_| {
                MetadataAdtObjectType::parse(value)
                    .map(Self::Metadata)
                    .map_err(|_| UnsupportedObjectTypeError(value.to_owned()))
            },
            |source| Ok(Self::Source(source)),
        )
    }
}

#[derive(Debug, Error)]
#[error("unsupported object type '{0}'")]
pub struct UnsupportedObjectTypeError(pub String);

impl ReportableError for UnsupportedObjectTypeError {
    fn code(&self) -> &'static str {
        "unsupported_object_type"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Source-based types: CLAS, INTF, PROG, DDLS, TABL. Metadata types, which have no source: DTEL, DOMA."
                .to_owned(),
        )
    }

    fn suggested_command(&self) -> Option<String> {
        Some(suggested_command::object_kinds())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_each_type_to_its_own_family() {
        assert_eq!(
            AdtObjectFamily::parse("PROG").unwrap(),
            AdtObjectFamily::Source(EditableAdtObjectType::Program)
        );
        assert_eq!(
            AdtObjectFamily::parse("dtel").unwrap(),
            AdtObjectFamily::Metadata(MetadataAdtObjectType::DataElement)
        );
        assert_eq!(
            AdtObjectFamily::parse("DOMA").unwrap(),
            AdtObjectFamily::Metadata(MetadataAdtObjectType::Domain)
        );
    }

    #[test]
    fn an_unknown_type_names_both_families() {
        // Trying one family and reporting its error would advertise only half
        // of what the command accepts.
        let error = AdtObjectFamily::parse("FUGR").unwrap_err();

        assert_eq!(error.code(), "unsupported_object_type");
        let hint = error.hint().unwrap();
        assert!(hint.contains("CLAS"));
        assert!(hint.contains("DTEL"));
    }
}

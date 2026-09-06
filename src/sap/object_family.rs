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

use serde::{Deserialize, Deserializer, Serialize, Serializer};
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

impl AdtObjectFamily {
    /// The logical type name, spelled once in [`RepositoryKind`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Source(object_type) => object_type.as_str(),
            Self::Metadata(object_type) => object_type.as_str(),
        }
    }
}

impl Serialize for AdtObjectFamily {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> Deserialize<'de> for AdtObjectFamily {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value).map_err(serde::de::Error::custom)
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
            "Source-based types: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, DCLS. Metadata types, which have no source: DTEL, DOMA, TTYP, MSAG."
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
    fn round_trips_through_its_type_name() {
        for name in ["PROG", "CLAS", "DTEL", "SRVB"] {
            let family = AdtObjectFamily::parse(name).unwrap();
            assert_eq!(family.as_str(), name);
            let json = serde_json::to_string(&family).unwrap();
            assert_eq!(json, format!("\"{name}\""));
            assert_eq!(
                serde_json::from_str::<AdtObjectFamily>(&json).unwrap(),
                family
            );
        }
    }

    #[test]
    fn an_unknown_stored_type_fails_to_deserialize_rather_than_defaulting() {
        assert!(serde_json::from_str::<AdtObjectFamily>("\"FUGR\"").is_err());
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

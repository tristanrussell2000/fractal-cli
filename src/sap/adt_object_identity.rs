//! The identity every ADT object has, whatever family it belongs to.
//!
//! This is a model, not operation code. It exists because creating and
//! deleting an object need almost nothing family-specific: a URI to act on and
//! a name to report. Families differ in how that URI is *derived* — namespace
//! rules, path layout, and whether there is any source at all — so validation
//! stays with each family and the shared operations take this.

use super::editable_source::EditableAdtSourceIdentity;

/// One ADT object, reduced to what the shared operations need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtObjectIdentity {
    /// Logical type label for messages, such as `PROG` or `DTEL`.
    pub object_type: String,
    pub name: String,
    pub object_uri: String,
    /// Where the object's source lives, when it has any.
    ///
    /// `None` for the DDIC families that ADT edits as a form rather than as
    /// code — a data element has no source. Carried for reporting only: no
    /// shared operation reads it, because an object with no source is created
    /// and deleted identically.
    pub source_uri: Option<String>,
}

impl From<EditableAdtSourceIdentity> for AdtObjectIdentity {
    fn from(identity: EditableAdtSourceIdentity) -> Self {
        Self {
            object_type: identity.object_type.as_str().to_owned(),
            name: identity.name,
            object_uri: identity.object_uri,
            source_uri: Some(identity.source_uri),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::editable_source::EditableAdtObjectType;

    #[test]
    fn a_source_based_object_keeps_its_source_uri() {
        let identity: AdtObjectIdentity = EditableAdtSourceIdentity {
            object_type: EditableAdtObjectType::Program,
            name: "ZSAMPLE".to_owned(),
            object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
            source_uri: "/sap/bc/adt/programs/programs/zsample/source/main".to_owned(),
        }
        .into();

        assert_eq!(identity.object_type, "PROG");
        assert_eq!(identity.name, "ZSAMPLE");
        assert_eq!(
            identity.source_uri.as_deref(),
            Some("/sap/bc/adt/programs/programs/zsample/source/main")
        );
    }
}

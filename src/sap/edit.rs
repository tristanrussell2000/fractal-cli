use std::string::FromUtf8Error;

use thiserror::Error;

use super::{
    adt::RepositoryKind,
    client::{SapClient, SapError},
};
use crate::edit::source_sha256;

const SOURCE_SUFFIX: &str = "/source/main";

/// A source-based ADT object family supported by the safe-edit workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditObjectType {
    Class,
    Interface,
    Program,
    DdlSource,
    Table,
}

impl EditObjectType {
    /// Parses a logical repository type such as `CLAS` or `DDLS`.
    ///
    /// # Errors
    ///
    /// Returns [`EditSourceError::UnsupportedObjectType`] when the type does
    /// not have a source mapping in the initial safe-edit implementation.
    pub fn parse(value: &str) -> Result<Self, EditSourceError> {
        let kind = RepositoryKind::parse(value.trim())
            .map_err(|_| EditSourceError::UnsupportedObjectType(value.to_owned()))?;
        Self::try_from(kind)
    }

    #[must_use]
    pub const fn repository_kind(self) -> RepositoryKind {
        match self {
            Self::Class => RepositoryKind::Clas,
            Self::Interface => RepositoryKind::Intf,
            Self::Program => RepositoryKind::Prog,
            Self::DdlSource => RepositoryKind::Ddls,
            Self::Table => RepositoryKind::Tabl,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.repository_kind().as_str()
    }

    const fn base_path(self) -> &'static str {
        match self {
            Self::Class => "/sap/bc/adt/oo/classes",
            Self::Interface => "/sap/bc/adt/oo/interfaces",
            Self::Program => "/sap/bc/adt/programs/programs",
            Self::DdlSource => "/sap/bc/adt/ddic/ddl/sources",
            Self::Table => "/sap/bc/adt/ddic/tables",
        }
    }
}

impl TryFrom<RepositoryKind> for EditObjectType {
    type Error = EditSourceError;

    fn try_from(kind: RepositoryKind) -> Result<Self, Self::Error> {
        match kind {
            RepositoryKind::Clas => Ok(Self::Class),
            RepositoryKind::Intf => Ok(Self::Interface),
            RepositoryKind::Prog => Ok(Self::Program),
            RepositoryKind::Ddls => Ok(Self::DdlSource),
            RepositoryKind::Tabl => Ok(Self::Table),
            unsupported => Err(EditSourceError::UnsupportedObjectType(
                unsupported.as_str().to_owned(),
            )),
        }
    }
}

/// The stored source version requested from SAP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditSourceVersion {
    Active,
    Inactive,
}

impl EditSourceVersion {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
        }
    }
}

/// Complete source and concurrency metadata returned by the edit-read boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditSource {
    pub object_type: EditObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
    pub version: EditSourceVersion,
    pub source: String,
    pub sha256: String,
    pub bytes: usize,
}

/// A deterministic failure while identifying or reading editable ADT source.
#[derive(Debug, Error)]
pub enum EditSourceError {
    #[error(transparent)]
    Sap(#[from] SapError),
    #[error("unsupported edit source object type '{0}'")]
    UnsupportedObjectType(String),
    #[error("invalid edit source object name '{0}'")]
    InvalidObjectName(String),
    #[error("SAP returned non-UTF-8 source for {object_type} object '{name}': {source}")]
    InvalidSourceEncoding {
        object_type: &'static str,
        name: String,
        #[source]
        source: FromUtf8Error,
    },
}

impl EditSourceError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::UnsupportedObjectType(_) => "unsupported_edit_object_type",
            Self::InvalidObjectName(_) => "invalid_edit_object_name",
            Self::InvalidSourceEncoding { .. } => "edit_source_encoding_error",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Sap(error) => error.hint().to_owned(),
            Self::UnsupportedObjectType(_) => {
                "Use one of the initially supported source types: CLAS, INTF, PROG, DDLS, or TABL."
                    .to_owned()
            }
            Self::InvalidObjectName(_) => {
                "Use an ABAP object name containing letters, digits, or underscores, optionally in the form /NAMESPACE/NAME."
                    .to_owned()
            }
            Self::InvalidSourceEncoding { .. } => {
                "The native ADT source response must be valid UTF-8 before it can be patched safely."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::Sap(error) => Some(error),
            _ => None,
        }
    }
}

/// Fetches the complete active or inactive source for a constrained ADT object.
///
/// The SHA-256 is calculated over the exact valid UTF-8 bytes returned by SAP.
/// Object names are mapped to known ADT roots rather than accepted as arbitrary
/// request paths.
///
/// # Errors
///
/// Returns [`EditSourceError`] when the object name is invalid, SAP rejects the
/// request, or the source response is not valid UTF-8.
pub async fn get_edit_source(
    sap: &SapClient,
    object_type: EditObjectType,
    name: &str,
    version: EditSourceVersion,
) -> Result<EditSource, EditSourceError> {
    let name = validate_object_name(name)?;
    let path_name = name.to_ascii_lowercase().replace('/', "%2f");
    let object_uri = format!("{}/{path_name}", object_type.base_path());
    let source_uri = format!("{object_uri}{SOURCE_SUFFIX}");
    let response_bytes = sap
        .get_bytes_with_query(&source_uri, &[("version", version.as_str())])
        .await?;
    let bytes = response_bytes.len();
    let source = String::from_utf8(response_bytes).map_err(|source| {
        EditSourceError::InvalidSourceEncoding {
            object_type: object_type.as_str(),
            name: name.clone(),
            source,
        }
    })?;

    Ok(EditSource {
        object_type,
        name,
        object_uri,
        source_uri,
        version,
        sha256: source_sha256(&source),
        source,
        bytes,
    })
}

fn validate_object_name(name: &str) -> Result<String, EditSourceError> {
    let trimmed = name.trim();
    let characters_are_valid = trimmed
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '_' || character == '/');
    let namespace_shape_is_valid = trimmed.strip_prefix('/').map_or_else(
        || !trimmed.contains('/'),
        |namespaced| {
            let mut parts = namespaced.split('/');
            matches!((parts.next(), parts.next(), parts.next()), (Some(namespace), Some(object), None) if !namespace.is_empty() && !object.is_empty())
        },
    );

    if !trimmed.is_empty() && characters_are_valid && namespace_shape_is_valid {
        Ok(trimmed.to_ascii_uppercase())
    } else {
        Err(EditSourceError::InvalidObjectName(name.to_owned()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_initial_object_type_to_a_fixed_adt_root() {
        let cases = [
            (EditObjectType::Class, "/sap/bc/adt/oo/classes"),
            (EditObjectType::Interface, "/sap/bc/adt/oo/interfaces"),
            (EditObjectType::Program, "/sap/bc/adt/programs/programs"),
            (EditObjectType::DdlSource, "/sap/bc/adt/ddic/ddl/sources"),
            (EditObjectType::Table, "/sap/bc/adt/ddic/tables"),
        ];

        for (object_type, expected) in cases {
            assert_eq!(object_type.base_path(), expected);
        }
    }

    #[test]
    fn parses_supported_types_case_insensitively() {
        assert_eq!(
            EditObjectType::parse(" clas ").unwrap(),
            EditObjectType::Class
        );
        assert_eq!(
            EditObjectType::parse("ddls").unwrap(),
            EditObjectType::DdlSource
        );
    }

    #[test]
    fn converts_only_the_supported_repository_kind_subset() {
        assert_eq!(
            EditObjectType::try_from(RepositoryKind::Prog).unwrap(),
            EditObjectType::Program
        );

        let error = EditObjectType::try_from(RepositoryKind::Doma).unwrap_err();
        assert!(matches!(
            error,
            EditSourceError::UnsupportedObjectType(kind) if kind == "DOMA"
        ));
    }

    #[test]
    fn rejects_unsupported_types() {
        let error = EditObjectType::parse("DOMA").unwrap_err();

        assert_eq!(error.code(), "unsupported_edit_object_type");
        assert!(error.hint().contains("CLAS"));

        assert!(matches!(
            EditObjectType::parse("NOT_A_KIND"),
            Err(EditSourceError::UnsupportedObjectType(kind)) if kind == "NOT_A_KIND"
        ));
    }

    #[test]
    fn validates_and_canonicalizes_plain_and_namespaced_names() {
        assert_eq!(
            validate_object_name(" zcl_example ").unwrap(),
            "ZCL_EXAMPLE"
        );
        assert_eq!(
            validate_object_name("/acme/example").unwrap(),
            "/ACME/EXAMPLE"
        );
    }

    #[test]
    fn rejects_invalid_names_and_malformed_namespaces() {
        for name in ["", "ZCL-EXAMPLE", "ACME/EXAMPLE", "/ACME/", "/A/B/C"] {
            assert!(matches!(
                validate_object_name(name),
                Err(EditSourceError::InvalidObjectName(_))
            ));
        }
    }
}

use std::string::FromUtf8Error;

use reqwest::header::HeaderMap;
use thiserror::Error;

use super::{
    adt::RepositoryKind,
    client::{SapClient, SapError},
};
use crate::{pattern::glob_matches, source_change::source_sha256, suggested_command};

const SOURCE_SUFFIX: &str = "/source/main";

/// A source-based ADT object family supported by the safe-edit workflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditableAdtObjectType {
    Class,
    Interface,
    Program,
    DdlSource,
    Table,
}

impl EditableAdtObjectType {
    /// Parses a logical repository type such as `CLAS` or `DDLS`.
    ///
    /// # Errors
    ///
    /// Returns [`EditableAdtSourceTargetError::UnsupportedObjectType`] when the type does
    /// not have a source mapping in the initial safe-edit implementation.
    pub fn parse(value: &str) -> Result<Self, EditableAdtSourceTargetError> {
        let kind = RepositoryKind::parse(value.trim())
            .map_err(|_| EditableAdtSourceTargetError::UnsupportedObjectType(value.to_owned()))?;
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

impl TryFrom<RepositoryKind> for EditableAdtObjectType {
    type Error = EditableAdtSourceTargetError;

    fn try_from(kind: RepositoryKind) -> Result<Self, Self::Error> {
        match kind {
            RepositoryKind::Clas => Ok(Self::Class),
            RepositoryKind::Intf => Ok(Self::Interface),
            RepositoryKind::Prog => Ok(Self::Program),
            RepositoryKind::Ddls => Ok(Self::DdlSource),
            RepositoryKind::Tabl => Ok(Self::Table),
            unsupported => Err(EditableAdtSourceTargetError::UnsupportedObjectType(
                unsupported.as_str().to_owned(),
            )),
        }
    }
}

/// The stored source version requested from SAP.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdtSourceVersion {
    Active,
    Inactive,
}

impl AdtSourceVersion {
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
pub struct AdtSourceReadResult {
    pub identity: EditableAdtSourceIdentity,
    pub requested_version: AdtSourceVersion,
    pub snapshot: AdtSourceSnapshot,
}

/// Exact source bytes and revision metadata observed at one workflow stage.
///
/// The three values always travel together — a hash is meaningless without the
/// bytes it was taken over — so mutation results report a snapshot per stage
/// (original, proposed, stored) rather than parallel `*_sha256`/`*_bytes`
/// fields. Command output still flattens them into its established field names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceSnapshot {
    pub source: String,
    pub sha256: String,
    pub bytes: usize,
}

impl AdtSourceSnapshot {
    #[must_use]
    pub fn from_parts(source: String, sha256: String, bytes: usize) -> Self {
        Self {
            source,
            sha256,
            bytes,
        }
    }
}

/// The canonical identity of one editable ADT source object.
///
/// Every edit workflow resolves this before contacting SAP and every result
/// reports it, so the four fields are declared here once instead of being
/// re-declared on each result type. Command output flattens it back into the
/// same top-level JSON fields callers already receive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditableAdtSourceIdentity {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
}

/// A syntactically valid source object type and name.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EditableAdtSourceTargetError {
    #[error("unsupported edit source object type '{0}'")]
    UnsupportedObjectType(String),
    #[error("invalid edit source object name '{0}'")]
    InvalidObjectName(String),
}

impl EditableAdtSourceTargetError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedObjectType(_) => "unsupported_edit_object_type",
            Self::InvalidObjectName(_) => "invalid_edit_object_name",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::UnsupportedObjectType(_) => {
                "Use one of the initially supported source types: CLAS, INTF, PROG, DDLS, or TABL."
                    .to_owned()
            }
            Self::InvalidObjectName(_) => {
                "Use an ABAP object name containing letters, digits, or underscores, optionally in the form /NAMESPACE/NAME."
                    .to_owned()
            }
        }
    }
}

/// A failure to authorize an edit against the configured customer namespaces.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("object '{name}' is outside the configured customer namespaces")]
pub struct CustomerNamespaceError {
    pub name: String,
    pub namespaces: Vec<String>,
}

impl CustomerNamespaceError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "object_outside_customer_namespaces"
    }

    #[must_use]
    pub fn hint(&self) -> String {
        if self.namespaces.is_empty() {
            return "Configure at least one customer namespace on the selected profile before editing."
                .to_owned();
        }

        format!(
            "Only objects matching these configured patterns may be edited: {}.",
            self.namespaces.join(", ")
        )
    }
}

/// A deterministic failure while validating a transport request identifier.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
#[error("invalid transport request '{value}'")]
pub struct TransportRequestError {
    pub value: String,
}

impl TransportRequestError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        "invalid_transport_request"
    }

    #[must_use]
    pub fn hint(&self) -> String {
        "Use a transport request identifier containing 1-20 ASCII letters or digits, for example DE3K900575."
            .to_owned()
    }
}

/// A failure before an ADT edit workflow is allowed to contact SAP.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum AdtEditTargetValidationError {
    #[error("invalid editable ADT object: {0}")]
    InvalidObject(#[source] EditableAdtSourceTargetError),
    #[error(transparent)]
    Namespace(#[from] CustomerNamespaceError),
    #[error(transparent)]
    InvalidTransport(#[from] TransportRequestError),
}

impl AdtEditTargetValidationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidObject(error) => error.code(),
            Self::Namespace(error) => error.code(),
            Self::InvalidTransport(error) => error.code(),
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::InvalidObject(error) => error.hint(),
            Self::Namespace(error) => error.hint(),
            Self::InvalidTransport(error) => error.hint(),
        }
    }
}

/// Canonical object identity and optional transport shared by mutating workflows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ValidatedAdtEditTarget {
    pub(super) identity: EditableAdtSourceIdentity,
    pub(super) transport: Option<String>,
}

/// A deterministic or remote failure while identifying or reading editable ADT source.
#[derive(Debug, Error)]
pub enum AdtSourceReadError {
    #[error(transparent)]
    InvalidTarget(#[from] EditableAdtSourceTargetError),
    #[error("{source}")]
    Sap {
        object_type: &'static str,
        name: String,
        #[source]
        source: SapError,
    },
    #[error("SAP returned non-UTF-8 source for {object_type} object '{name}': {source}")]
    InvalidSourceEncoding {
        object_type: &'static str,
        name: String,
        #[source]
        source: FromUtf8Error,
    },
}

impl AdtSourceReadError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidTarget(error) => error.code(),
            Self::Sap { source, .. } => source.code(),
            Self::InvalidSourceEncoding { .. } => "edit_source_encoding_error",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::InvalidTarget(error) => error.hint(),
            Self::Sap { source, .. } => source.hint(),
            Self::InvalidSourceEncoding { .. } => {
                "The native ADT source response must be valid UTF-8 before it can be patched safely."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::Sap { source, .. } => Some(source),
            Self::InvalidTarget(_) | Self::InvalidSourceEncoding { .. } => None,
        }
    }

    /// A read-only command that diagnoses this failure, if one exists.
    #[must_use]
    pub fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Sap {
                object_type,
                name,
                source,
            } if source.is_not_found() => Some(suggested_command::object_search(object_type, name)),
            Self::Sap { source, .. } => source.suggested_command(),
            Self::InvalidTarget(_) | Self::InvalidSourceEncoding { .. } => None,
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
/// Returns [`AdtSourceReadError`] when the object name is invalid, SAP rejects the
/// request, or the source response is not valid UTF-8.
pub async fn read_adt_source_for_edit(
    sap: &SapClient,
    object_type: EditableAdtObjectType,
    name: &str,
    version: AdtSourceVersion,
) -> Result<AdtSourceReadResult, AdtSourceReadError> {
    let identity = editable_source_identity(object_type, name)?;
    read_adt_source_by_identity(sap, &identity, version).await
}

pub(super) async fn read_adt_source_by_identity(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
    version: AdtSourceVersion,
) -> Result<AdtSourceReadResult, AdtSourceReadError> {
    read_adt_source(sap, identity, version, HeaderMap::new()).await
}

pub(super) async fn read_adt_source(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
    version: AdtSourceVersion,
    headers: HeaderMap,
) -> Result<AdtSourceReadResult, AdtSourceReadError> {
    let response_bytes = sap
        .get_bytes_with_query_and_headers(
            &identity.source_uri,
            &[("version", version.as_str())],
            headers,
        )
        .await
        .map_err(|source| AdtSourceReadError::Sap {
            object_type: identity.object_type.as_str(),
            name: identity.name.clone(),
            source,
        })?;
    let bytes = response_bytes.len();
    let source = String::from_utf8(response_bytes).map_err(|source| {
        AdtSourceReadError::InvalidSourceEncoding {
            object_type: identity.object_type.as_str(),
            name: identity.name.clone(),
            source,
        }
    })?;

    Ok(AdtSourceReadResult {
        identity: identity.clone(),
        requested_version: version,
        snapshot: AdtSourceSnapshot {
            sha256: source_sha256(&source),
            source,
            bytes,
        },
    })
}

pub(super) fn validate_adt_edit_target(
    object_type: EditableAdtObjectType,
    name: &str,
    customer_namespaces: &[String],
    transport: Option<&str>,
) -> Result<ValidatedAdtEditTarget, AdtEditTargetValidationError> {
    let identity = editable_source_identity(object_type, name)
        .map_err(AdtEditTargetValidationError::InvalidObject)?;
    validate_customer_namespace(&identity.name, customer_namespaces)?;
    let transport = canonicalize_transport_request(transport)?;
    Ok(ValidatedAdtEditTarget {
        identity,
        transport,
    })
}

pub(super) fn editable_source_identity(
    object_type: EditableAdtObjectType,
    name: &str,
) -> Result<EditableAdtSourceIdentity, EditableAdtSourceTargetError> {
    let name = validate_object_name(name)?;
    let path_name = name.to_ascii_lowercase().replace('/', "%2f");
    let object_uri = format!("{}/{path_name}", object_type.base_path());
    let source_uri = format!("{object_uri}{SOURCE_SUFFIX}");
    Ok(EditableAdtSourceIdentity {
        object_type,
        name,
        object_uri,
        source_uri,
    })
}

fn canonicalize_transport_request(
    transport: Option<&str>,
) -> Result<Option<String>, TransportRequestError> {
    let Some(transport) = transport else {
        return Ok(None);
    };
    let trimmed = transport.trim();
    if (1..=20).contains(&trimmed.len()) && trimmed.bytes().all(|byte| byte.is_ascii_alphanumeric())
    {
        Ok(Some(trimmed.to_ascii_uppercase()))
    } else {
        Err(TransportRequestError {
            value: transport.to_owned(),
        })
    }
}

/// Verifies that an object belongs to one configured customer namespace.
///
/// Matching is case-insensitive and uses the same `*` glob behavior as object
/// search. An empty pattern list denies every object.
fn validate_customer_namespace(
    name: &str,
    namespaces: &[String],
) -> Result<(), CustomerNamespaceError> {
    if namespaces.iter().any(|pattern| glob_matches(pattern, name)) {
        return Ok(());
    }

    Err(CustomerNamespaceError {
        name: name.to_owned(),
        namespaces: namespaces.to_vec(),
    })
}

fn validate_object_name(name: &str) -> Result<String, EditableAdtSourceTargetError> {
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
        Err(EditableAdtSourceTargetError::InvalidObjectName(
            name.to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_every_initial_object_type_to_a_fixed_adt_root() {
        let cases = [
            (EditableAdtObjectType::Class, "/sap/bc/adt/oo/classes"),
            (
                EditableAdtObjectType::Interface,
                "/sap/bc/adt/oo/interfaces",
            ),
            (
                EditableAdtObjectType::Program,
                "/sap/bc/adt/programs/programs",
            ),
            (
                EditableAdtObjectType::DdlSource,
                "/sap/bc/adt/ddic/ddl/sources",
            ),
            (EditableAdtObjectType::Table, "/sap/bc/adt/ddic/tables"),
        ];

        for (object_type, expected) in cases {
            assert_eq!(object_type.base_path(), expected);
        }
    }

    #[test]
    fn parses_supported_types_case_insensitively() {
        assert_eq!(
            EditableAdtObjectType::parse(" clas ").unwrap(),
            EditableAdtObjectType::Class
        );
        assert_eq!(
            EditableAdtObjectType::parse("ddls").unwrap(),
            EditableAdtObjectType::DdlSource
        );
    }

    #[test]
    fn converts_only_the_supported_repository_kind_subset() {
        assert_eq!(
            EditableAdtObjectType::try_from(RepositoryKind::Prog).unwrap(),
            EditableAdtObjectType::Program
        );

        let error = EditableAdtObjectType::try_from(RepositoryKind::Doma).unwrap_err();
        assert!(matches!(
            error,
            EditableAdtSourceTargetError::UnsupportedObjectType(kind) if kind == "DOMA"
        ));
    }

    #[test]
    fn rejects_unsupported_types() {
        let error = EditableAdtObjectType::parse("DOMA").unwrap_err();

        assert_eq!(error.code(), "unsupported_edit_object_type");
        assert!(error.hint().contains("CLAS"));

        assert!(matches!(
            EditableAdtObjectType::parse("NOT_A_KIND"),
            Err(EditableAdtSourceTargetError::UnsupportedObjectType(kind)) if kind == "NOT_A_KIND"
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
                Err(EditableAdtSourceTargetError::InvalidObjectName(_))
            ));
        }
    }

    #[test]
    fn validates_and_canonicalizes_a_complete_edit_target() {
        let target = validate_adt_edit_target(
            EditableAdtObjectType::Class,
            " zcl_example ",
            &["Z*".to_owned()],
            Some(" de3k900575 "),
        )
        .unwrap();

        assert_eq!(target.identity.name, "ZCL_EXAMPLE");
        assert_eq!(target.transport.as_deref(), Some("DE3K900575"));
    }

    #[test]
    fn target_validation_errors_are_narrow_and_structured() {
        let namespace = validate_adt_edit_target(
            EditableAdtObjectType::Class,
            "SAP_STANDARD",
            &["Z*".to_owned()],
            None,
        )
        .unwrap_err();
        assert_eq!(namespace.code(), "object_outside_customer_namespaces");

        let transport = validate_adt_edit_target(
            EditableAdtObjectType::Class,
            "ZCL_EXAMPLE",
            &["Z*".to_owned()],
            Some("invalid request"),
        )
        .unwrap_err();
        assert_eq!(transport.code(), "invalid_transport_request");
    }

    #[test]
    fn accepts_plain_and_registered_customer_namespaces() {
        let namespaces = vec!["Z*".to_owned(), "/ACME/*".to_owned()];

        assert!(validate_customer_namespace("zsample", &namespaces).is_ok());
        assert!(validate_customer_namespace("/acme/example", &namespaces).is_ok());
    }

    #[test]
    fn rejects_objects_outside_customer_namespaces() {
        let namespaces = vec!["Z*".to_owned(), "Y*".to_owned()];
        let error = validate_customer_namespace("SAP_STANDARD", &namespaces).unwrap_err();

        assert_eq!(error.name, "SAP_STANDARD");
        assert_eq!(error.code(), "object_outside_customer_namespaces");
        assert!(error.hint().contains("Z*"));
    }

    #[test]
    fn empty_namespace_configuration_fails_closed() {
        let error = validate_customer_namespace("Z_SAMPLE", &[]).unwrap_err();

        assert!(error.hint().contains("Configure at least one"));
    }
}

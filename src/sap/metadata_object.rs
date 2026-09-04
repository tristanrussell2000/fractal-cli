//! DDIC objects that ADT edits as a form rather than as code.
//!
//! A data element has no `source/main` at all — asking for one is a 404 — so
//! the source-based edit workflow does not apply to this family. What it has
//! instead is a complete XML document, and SAP hands back the *whole* skeleton
//! on creation: every field present, blank where a decision is needed and
//! defaulted where SAP has an opinion. That is what makes a create-then-fill
//! workflow possible here without modelling DDIC semantics field by field.
//!
//! Creating and deleting are shared with every other family (see
//! [`super::object_creation`] and [`super::object_deletion`]); only the
//! identity and the creation payload live here.

use super::package_authorization::{
    PackageAuthorizationError, authorize_known_package, package_of_object_xml,
};
use crate::config::EditPolicy;
use crate::source_change::{SourceChangePlanError, verify_expected_sha256};
use thiserror::Error;

use reqwest::header::HeaderValue;

use super::{
    adt_object_identity::AdtObjectIdentity,
    client::{SapClient, SapClientError},
    edit_session::{
        AdtEditSessionError, AdtObjectLock, acquire_adt_object_lock, release_adt_object_lock,
        stateful_session_headers,
    },
    editable_source::{
        AdtEditTargetValidationError, EditableAdtSourceTargetError, canonicalize_transport_request,
        validate_customer_namespace, validate_object_name,
    },
    object_creation::{
        AdtObjectCreatePayload, AdtObjectCreationError, AdtObjectCreationResult,
        create_validated_adt_object, validate_package_name,
    },
    object_deletion::{
        AdtObjectDeletionError, AdtObjectDeletionPreview, AdtObjectDeletionResult,
        delete_validated_adt_object, preview_validated_deletion,
    },
    repository_kind::{AdtObjectType, RepositoryKind},
};
use crate::{
    reportable_error::{ReportableError, sap_http_status},
    suggested_command,
};

/// A DDIC family whose objects are XML documents rather than source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MetadataAdtObjectType {
    DataElement,
    Domain,
    TableType,
    MessageClass,
    ServiceBinding,
}

impl MetadataAdtObjectType {
    /// Parses a logical type such as `DTEL`.
    ///
    /// # Errors
    ///
    /// Returns [`MetadataObjectTypeError`] for any type outside this family.
    pub fn parse(value: &str) -> Result<Self, MetadataObjectTypeError> {
        let kind = RepositoryKind::parse(value.trim())
            .map_err(|_| MetadataObjectTypeError(value.to_owned()))?;
        Self::try_from(kind).map_err(|_| MetadataObjectTypeError(value.to_owned()))
    }

    #[must_use]
    pub const fn repository_kind(self) -> RepositoryKind {
        match self {
            Self::DataElement => RepositoryKind::Dtel,
            Self::Domain => RepositoryKind::Doma,
            Self::TableType => RepositoryKind::Ttyp,
            Self::MessageClass => RepositoryKind::Msag,
            Self::ServiceBinding => RepositoryKind::Srvb,
        }
    }

    /// The logical type name, spelled once in [`RepositoryKind`].
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        self.repository_kind().as_str()
    }

    #[must_use]
    pub const fn collection_path(self) -> &'static str {
        match self {
            Self::DataElement => "/sap/bc/adt/ddic/dataelements",
            Self::Domain => "/sap/bc/adt/ddic/domains",
            Self::TableType => "/sap/bc/adt/ddic/tabletypes",
            // Not under `ddic/`, unlike every other member of this family.
            Self::MessageClass => "/sap/bc/adt/messageclass",
            Self::ServiceBinding => "/sap/bc/adt/businessservices/bindings",
        }
    }

    /// SAP's own subtype code, which the creation payload has to carry.
    ///
    /// Spelled in [`AdtObjectType`] rather than here, for the same reason the
    /// logical name is spelled in [`RepositoryKind`]: search results are
    /// classified by these codes, and a second table of them drifts.
    #[must_use]
    pub const fn adt_object_type(self) -> AdtObjectType {
        match self {
            Self::DataElement => AdtObjectType::DtelDe,
            Self::Domain => AdtObjectType::DomaDd,
            Self::TableType => AdtObjectType::TtypDa,
            Self::MessageClass => AdtObjectType::MsagN,
            Self::ServiceBinding => AdtObjectType::SrvbSvb,
        }
    }

    /// The media type from the backend's discovery document.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::DataElement => "application/vnd.sap.adt.dataelements.v2+xml",
            Self::Domain => "application/vnd.sap.adt.domains.v2+xml",
            Self::TableType => "application/vnd.sap.adt.tabletype.v1+xml",
            Self::MessageClass => "application/vnd.sap.adt.messageclass.v1+xml",
            Self::ServiceBinding => {
                "application/vnd.sap.adt.businessservices.servicebinding.v2+xml"
            }
        }
    }

    /// The root element and its namespace, read off a real object of this type.
    ///
    /// No two of these share an envelope: a data element is a generic
    /// `blue:wbobj` wrapper, while a domain and a table type each have their
    /// own root, with unrelated namespace URIs. None of it is guessable from
    /// the type code, so each was read off a live object.
    const fn root_element(self) -> (&'static str, &'static str) {
        match self {
            Self::DataElement => ("blue:wbobj", "http://www.sap.com/wbobj/dictionary/dtel"),
            Self::Domain => ("doma:domain", "http://www.sap.com/dictionary/domain"),
            Self::TableType => ("ttyp:tableType", "http://www.sap.com/dictionary/tabletype"),
            // Note the capitalisation: SAP spells this namespace differently
            // from every other one in this family.
            Self::MessageClass => ("mc:messageClass", "http://www.sap.com/adt/MessageClass"),
            Self::ServiceBinding => (
                "srvb:serviceBinding",
                "http://www.sap.com/adt/ddic/ServiceBindings",
            ),
        }
    }

    /// The namespace prefix declared on the root element.
    const fn root_prefix(self) -> &'static str {
        match self {
            Self::DataElement => "blue",
            Self::Domain => "doma",
            Self::TableType => "ttyp",
            Self::MessageClass => "mc",
            Self::ServiceBinding => "srvb",
        }
    }
}

impl TryFrom<RepositoryKind> for MetadataAdtObjectType {
    type Error = MetadataObjectTypeError;

    fn try_from(kind: RepositoryKind) -> Result<Self, Self::Error> {
        match kind {
            RepositoryKind::Dtel => Ok(Self::DataElement),
            RepositoryKind::Doma => Ok(Self::Domain),
            RepositoryKind::Ttyp => Ok(Self::TableType),
            RepositoryKind::Msag => Ok(Self::MessageClass),
            RepositoryKind::Srvb => Ok(Self::ServiceBinding),
            unsupported => Err(MetadataObjectTypeError(unsupported.as_str().to_owned())),
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported metadata object type '{0}'")]
pub struct MetadataObjectTypeError(pub String);

impl ReportableError for MetadataObjectTypeError {
    fn code(&self) -> &'static str {
        "unsupported_metadata_object_type"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Metadata objects are DTEL, DOMA, TTYP and MSAG. Source-based types use the same commands."
                .to_owned(),
        )
    }
}

/// A failure while writing a metadata object's XML.
#[derive(Debug, Error)]
pub enum MetadataObjectWriteError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
    /// The document changed between the caller reading it and this write.
    #[error(transparent)]
    Stale(#[from] SourceChangePlanError),
    #[error("the replacement document is empty")]
    BlankDocument,
    #[error("ADT edit session failed while writing: {0}")]
    Session(#[source] AdtEditSessionError),
    #[error("the ADT write request failed: {0}")]
    Write(#[source] SapClientError),
    /// SAP refused the write for want of a description.
    ///
    /// Its own read-back does not include one on a freshly created shell, so
    /// writing back exactly what was read fails until the caller adds it.
    #[error("SAP requires a description on this object: {0}")]
    DescriptionMissing(#[source] SapClientError),
    #[error("could not read {name} back: {source}")]
    Read {
        name: String,
        #[source]
        source: SapClientError,
    },
    /// The write failed and its lock could not be released. Wraps the cause, so
    /// the reported code, status, and message are unchanged; only the hint
    /// gains the stuck lock, because that changes the caller's next move.
    #[error(transparent)]
    AbandonedLock(Box<Self>),
}

impl ReportableError for MetadataObjectWriteError {
    fn code(&self) -> &'static str {
        match self {
            Self::AbandonedLock(primary) => primary.code(),
            Self::Validation(error) => error.code(),
            Self::PackageNotAllowed(error) => error.code(),
            Self::Stale(error) => error.code(),
            Self::BlankDocument => "blank_xml_document",
            Self::Session(_) => "edit_xml_lock_failed",
            Self::Write(_) => "edit_xml_write_failed",
            Self::DescriptionMissing(_) => "edit_xml_description_missing",
            Self::Read { .. } => "edit_xml_verification_failed",
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::AbandonedLock(primary) => primary.status(),
            Self::Session(error) => error.status(),
            Self::Write(error) | Self::DescriptionMissing(error) => sap_http_status(Some(error)),
            Self::Read { source, .. } => sap_http_status(Some(source)),
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::PackageNotAllowed(error) => return error.hint(),
            Self::Stale(error) => return error.hint(),
            Self::AbandonedLock(primary) => format!(
                "{} Releasing its lock also failed, so the object is still locked: clear the lock before retrying, or the next attempt will fail on the lock rather than the original cause.",
                primary.hint().unwrap_or_default()
            ),
            Self::Validation(error) => return error.hint(),
            Self::BlankDocument => {
                "Pass the object's complete XML document; this replaces it rather than patching it."
                    .to_owned()
            }
            Self::Session(error) => return error.hint(),
            Self::Write(error) => format!(
                "The object was not changed. {}",
                error.hint().unwrap_or_default()
            ),
            Self::DescriptionMissing(_) => {
                "Add adtcore:description=\"...\" to the root element and write again. A newly created shell stores no description, so the document `object xml` returns has none, and SAP refuses to save without one."
                    .to_owned()
            }
            Self::Read { .. } => {
                "The write may have been applied. Read the object before writing again, so a retry does not overwrite a change that landed."
                    .to_owned()
            }
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::AbandonedLock(primary) => primary.suggested_command(),
            Self::Read { .. } => Some(suggested_command::object_xml("<the object uri>")),
            _ => None,
        }
    }
}

/// One metadata object to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataObjectCreationRequest {
    pub object_type: MetadataAdtObjectType,
    pub name: String,
    pub package: String,
    pub description: String,
    pub transport: Option<String>,
    /// Required for a service binding, meaningless for every other type.
    ///
    /// A binding is not a shell: it exists to expose one service definition
    /// over one protocol, and SAP refuses to create it without both. Asking
    /// for them up front is the only option, because the refusal is an
    /// HTTP 500 with no message at all.
    pub binding: Option<ServiceBindingSpec>,
}

/// What a service binding needs beyond a name and a package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceBindingSpec {
    /// The service definition this binding exposes.
    pub service_definition: String,
    pub binding_type: ServiceBindingType,
}

/// The protocol a binding exposes its service over.
///
/// SAP lists six at `/businessservices/bindings/bindingtypes` as a
/// (name, category) pair: `INA`, `ODATA` V2 and V4, and `SQL`, each in a UI or
/// Web API category. Only the OData four are offered here — they are the RAP
/// cases — and the other two are refused by name rather than guessed at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceBindingType {
    ODataV2Ui,
    ODataV2WebApi,
    ODataV4Ui,
    ODataV4WebApi,
}

impl ServiceBindingType {
    /// Parses the CLI spelling, such as `odata-v4-ui`.
    ///
    /// # Errors
    ///
    /// Returns [`ServiceBindingTypeError`] for anything else, including the
    /// `INA` and `SQL` bindings SAP supports but this does not.
    pub fn parse(value: &str) -> Result<Self, ServiceBindingTypeError> {
        match value.trim().to_ascii_lowercase().replace('_', "-").as_str() {
            "odata-v2-ui" => Ok(Self::ODataV2Ui),
            "odata-v2-web-api" => Ok(Self::ODataV2WebApi),
            "odata-v4-ui" => Ok(Self::ODataV4Ui),
            "odata-v4-web-api" => Ok(Self::ODataV4WebApi),
            _ => Err(ServiceBindingTypeError(value.to_owned())),
        }
    }

    /// SAP's own triple: protocol, version, and category.
    ///
    /// Category `0` is a UI service and `1` a Web API; both are real values
    /// from the backend's own list, not a convention invented here.
    const fn as_sap_parts(self) -> (&'static str, &'static str, &'static str) {
        match self {
            Self::ODataV2Ui => ("ODATA", "V2", "0"),
            Self::ODataV2WebApi => ("ODATA", "V2", "1"),
            Self::ODataV4Ui => ("ODATA", "V4", "0"),
            Self::ODataV4WebApi => ("ODATA", "V4", "1"),
        }
    }
}

#[derive(Debug, Error)]
#[error("unsupported service binding type '{0}'")]
pub struct ServiceBindingTypeError(pub String);

impl ReportableError for ServiceBindingTypeError {
    fn code(&self) -> &'static str {
        "unsupported_binding_type"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Use odata-v2-ui, odata-v2-web-api, odata-v4-ui, or odata-v4-web-api. SAP also has INA and SQL bindings, which this command does not build."
                .to_owned(),
        )
    }
}

/// Builds the identity of a metadata object, validating name and namespace.
///
/// # Errors
///
/// Returns [`AdtEditTargetValidationError`] when the name is malformed or falls
/// outside the configured customer namespaces.
pub fn metadata_object_identity(
    object_type: MetadataAdtObjectType,
    name: &str,
    policy: &EditPolicy,
) -> Result<AdtObjectIdentity, AdtEditTargetValidationError> {
    let name = validate_object_name(name).map_err(|error: EditableAdtSourceTargetError| {
        AdtEditTargetValidationError::InvalidObject(error)
    })?;
    validate_customer_namespace(&name, &policy.customer_namespaces)?;
    let path_name = name.to_ascii_lowercase().replace('/', "%2f");

    Ok(AdtObjectIdentity {
        object_type: object_type.as_str().to_owned(),
        object_uri: format!("{}/{path_name}", object_type.collection_path()),
        name,
        // The whole point of this family: there is no source to point at.
        source_uri: None,
    })
}

/// Releases a lock after a failure, wrapping the cause when the lock sticks.
///
/// The failure that stopped the operation stays the reported cause; only the
/// hint gains the stuck lock, because that changes what the caller has to do
/// next.
async fn abandon_lock_if_stuck(
    sap: &mut SapClient,
    identity: &AdtObjectIdentity,
    lock: &AdtObjectLock,
    primary: MetadataObjectWriteError,
) -> MetadataObjectWriteError {
    if release_adt_object_lock(sap, &identity.object_uri, lock)
        .await
        .is_err()
    {
        MetadataObjectWriteError::AbandonedLock(Box::new(primary))
    } else {
        primary
    }
}

/// Creates an empty metadata object and confirms that it exists.
///
/// The object is a shell: SAP accepts a data element with no type information
/// at all. Fill it by reading its XML, editing the blanks, and writing it back.
///
/// # Errors
///
/// Returns [`AdtObjectCreationError`] for validation, a refused creation, or a
/// new object that could not be read back.
pub async fn create_metadata_object(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &MetadataObjectCreationRequest,
) -> Result<AdtObjectCreationResult, AdtObjectCreationError> {
    let identity = metadata_object_identity(request.object_type, &request.name, policy)?;
    let transport = canonicalize_transport_request(request.transport.as_deref())
        .map_err(AdtEditTargetValidationError::from)?;
    let package = validate_package_name(&request.package)?;
    let description = request.description.trim();
    if description.is_empty() {
        return Err(AdtObjectCreationError::BlankDescription);
    }

    let body = match (request.object_type, request.binding.as_ref()) {
        (MetadataAdtObjectType::ServiceBinding, Some(binding)) => {
            service_binding_payload(&identity.name, &package, description, binding)
        }
        (MetadataAdtObjectType::ServiceBinding, None) => {
            return Err(AdtObjectCreationError::MissingBindingDetails);
        }
        (_, Some(_)) => return Err(AdtObjectCreationError::UnexpectedBindingDetails),
        (object_type, None) => creation_payload(object_type, &identity.name, &package, description),
    };
    create_validated_adt_object(
        sap,
        policy,
        identity,
        AdtObjectCreatePayload {
            collection_path: request.object_type.collection_path(),
            media_type: request.object_type.media_type(),
            body: &body,
        },
        &package,
        description,
        transport,
    )
    .await
}

/// Deletes a metadata object, with the same guards as any other object.
///
/// The where-used guard matters more here rather than less: a data element is
/// typically referenced by every table field and structure component built on
/// it, so deleting one unguarded breaks things far from the object itself.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation, a failed where-used
/// lookup, remaining references, lock failures, a rejected delete, or an object
/// that is still readable afterwards.
pub async fn delete_metadata_object(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object_type: MetadataAdtObjectType,
    name: &str,
    transport: Option<&str>,
    force: bool,
) -> Result<AdtObjectDeletionResult, AdtObjectDeletionError> {
    let identity = metadata_object_identity(object_type, name, policy)?;
    let transport =
        canonicalize_transport_request(transport).map_err(AdtEditTargetValidationError::from)?;

    delete_validated_adt_object(sap, policy, identity, transport, force).await
}

/// Reports what deleting a metadata object would do, without doing any of it.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation failures or a where-used
/// lookup that could not be completed.
pub async fn preview_metadata_object_deletion(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object_type: MetadataAdtObjectType,
    name: &str,
    transport: Option<&str>,
    force: bool,
) -> Result<AdtObjectDeletionPreview, AdtObjectDeletionError> {
    let identity = metadata_object_identity(object_type, name, policy)?;
    let transport =
        canonicalize_transport_request(transport).map_err(AdtEditTargetValidationError::from)?;

    preview_validated_deletion(sap, policy, identity, transport, force).await
}

/// The result of writing a metadata object's XML back to SAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataObjectWriteResult {
    pub identity: AdtObjectIdentity,
    pub transport: Option<String>,
    /// What SAP holds after the write, read back rather than assumed.
    pub stored_xml: String,
    /// The write landed, but its lock could not be released.
    ///
    /// Reported as success with a caveat rather than as a failure: the document
    /// is written, and saying otherwise would invite a retry that then fails on
    /// the stuck lock. The caller still has to clear it, because it blocks the
    /// next edit of this object.
    pub still_locked: bool,
    /// Whether the stored document differs from the one before the write.
    ///
    /// A write SAP accepts but does not apply is a shape this backend has
    /// produced before (`activation?method=discard` answered 200 and did
    /// nothing), so "it changed" is checked rather than trusted.
    pub changed: bool,
}

/// Writes a metadata object's XML document, under a lock, and reads it back.
///
/// The whole document is replaced: for this family the XML *is* the object, so
/// there is no partial edit to make. Read it with `object xml`, change the
/// blanks, and pass the result here.
///
/// # Errors
///
/// Returns [`MetadataObjectWriteError`] for validation, a blank document, a
/// failed lock, a rejected write, or a document that cannot be read back
/// afterwards.
pub async fn write_metadata_object(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object_type: MetadataAdtObjectType,
    name: &str,
    xml: &str,
    transport: Option<&str>,
    expected_sha256: Option<&str>,
) -> Result<MetadataObjectWriteResult, MetadataObjectWriteError> {
    let identity = metadata_object_identity(object_type, name, policy)
        .map_err(MetadataObjectWriteError::Validation)?;
    let transport =
        canonicalize_transport_request(transport).map_err(AdtEditTargetValidationError::from)?;
    if xml.trim().is_empty() {
        return Err(MetadataObjectWriteError::BlankDocument);
    }

    // The package guard has to answer before the lock, so a refusal never
    // leaves one behind. That costs its own read, and only when a profile
    // actually restricts packages.
    if policy.restricts_packages() {
        let current = read_metadata_object(sap, &identity).await?;
        let package = package_of_object_xml(&current)
            .map_err(|source| PackageAuthorizationError::Parse {
                name: identity.name.clone(),
                source,
            })?
            .ok_or_else(|| PackageAuthorizationError::PackageUnknown {
                name: identity.name.clone(),
                object_uri: identity.object_uri.clone(),
            })?;
        authorize_known_package(policy, &identity.name, &package)?;
    }
    let lock = acquire_adt_object_lock(sap, &identity.object_uri, transport.as_deref())
        .await
        .map_err(MetadataObjectWriteError::Session)?;

    // Read the document under the lock, not before it. A hash checked against
    // an unlocked read proves nothing: the document could change between that
    // read and the lock, which is the exact race this guard exists to close.
    let before = match read_metadata_object(sap, &identity).await {
        Ok(before) => before,
        Err(primary) => return Err(abandon_lock_if_stuck(sap, &identity, &lock, primary).await),
    };
    if let Err(stale) = verify_expected_sha256(expected_sha256, &before) {
        // Nothing has been written, so the only cleanup is the lock.
        return Err(abandon_lock_if_stuck(sap, &identity, &lock, stale.into()).await);
    }

    let written = write_locked_xml(sap, &identity, object_type, &lock, xml).await;
    // The object still exists either way, so its lock always has to come off.
    let released = release_adt_object_lock(sap, &identity.object_uri, &lock).await;
    if let Err(primary) = written {
        // A cleanup failure must not replace the write failure that caused it,
        // but whether the lock survived is state the caller needs: the next
        // attempt would otherwise fail on the lock rather than on the original
        // cause, with nothing having said so.
        return Err(if released.is_err() {
            MetadataObjectWriteError::AbandonedLock(Box::new(primary))
        } else {
            primary
        });
    }
    let still_locked = released.is_err();

    let stored_xml = read_metadata_object(sap, &identity).await?;
    Ok(MetadataObjectWriteResult {
        changed: stored_xml != before,
        identity,
        transport,
        stored_xml,
        still_locked,
    })
}

async fn write_locked_xml(
    sap: &mut SapClient,
    identity: &AdtObjectIdentity,
    object_type: MetadataAdtObjectType,
    lock: &AdtObjectLock,
    xml: &str,
) -> Result<(), MetadataObjectWriteError> {
    let mut headers = stateful_session_headers();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static(object_type.media_type()),
    );
    let mut query = vec![("lockHandle", lock.handle())];
    if let Some(transport) = lock.transport() {
        query.push(("corrNr", transport));
    }

    sap.put_text(&identity.object_uri, &query, xml, headers)
        .await
        .map(|_| ())
        .map_err(|error| {
            if reports_a_missing_description(&error) {
                MetadataObjectWriteError::DescriptionMissing(error)
            } else {
                MetadataObjectWriteError::Write(error)
            }
        })
}

/// Whether SAP refused the write only because it carried no description.
///
/// Matched on the message, as there is no machine-readable marker. Observed
/// live as HTTP 400 `The description is missing` when writing back the exact
/// document `object xml` returned for a newly created data element.
fn reports_a_missing_description(error: &SapClientError) -> bool {
    matches!(
        error,
        SapClientError::Http { message, .. }
            if message.to_ascii_lowercase().contains("description is missing")
    )
}

async fn read_metadata_object(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
) -> Result<String, MetadataObjectWriteError> {
    sap.get_text(&identity.object_uri)
        .await
        .map_err(|source| MetadataObjectWriteError::Read {
            name: identity.name.clone(),
            source,
        })
}

/// The creation body for a service binding, which is not a shell.
///
/// It carries the service definition being exposed and the protocol to expose
/// it over, both of which SAP requires: without them the create fails with an
/// HTTP 500 carrying no message.
///
/// `srvb:contract` is sent because a live binding shows it, but SAP does not
/// echo it back — it is evidently derived rather than stored.
fn service_binding_payload(
    name: &str,
    package: &str,
    description: &str,
    binding: &ServiceBindingSpec,
) -> String {
    let (protocol, version, category) = binding.binding_type.as_sap_parts();
    let definition = binding.service_definition.to_ascii_uppercase();
    let definition_uri = format!(
        "/sap/bc/adt/ddic/srvd/sources/{}",
        definition.to_ascii_lowercase()
    );
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<srvb:serviceBinding xmlns:srvb=\"http://www.sap.com/adt/ddic/ServiceBindings\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
srvb:contract=\"C1\" adtcore:name=\"{name}\" adtcore:type=\"SRVB/SVB\" adtcore:description=\"{description}\">\
<adtcore:packageRef adtcore:name=\"{package}\"/>\
<srvb:services srvb:name=\"{name}\">\
<srvb:content srvb:version=\"0001\">\
<srvb:serviceDefinition adtcore:name=\"{definition}\" adtcore:type=\"SRVD/SRV\" adtcore:uri=\"{definition_uri}\"/>\
</srvb:content>\
</srvb:services>\
<srvb:binding srvb:type=\"{protocol}\" srvb:version=\"{version}\" srvb:category=\"{category}\"/>\
</srvb:serviceBinding>",
        name = xml_escape(name),
        description = xml_escape(description),
        package = xml_escape(package),
        definition = xml_escape(&definition),
    )
}

/// The creation body: a bare root element with the object's identity on it.
///
/// No type information is sent. SAP accepts that and answers with the full
/// skeleton, which is a better starting point than anything guessed here would
/// be — a data element created with a domain would still need every label
/// filling in afterwards.
fn creation_payload(
    object_type: MetadataAdtObjectType,
    name: &str,
    package: &str,
    description: &str,
) -> String {
    let (element, namespace) = object_type.root_element();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<{element} xmlns:{prefix}=\"{namespace}\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
adtcore:name=\"{name}\" adtcore:type=\"{object_code}\" adtcore:description=\"{description}\">\
<adtcore:packageRef adtcore:name=\"{package}\"/>\
</{element}>",
        prefix = object_type.root_prefix(),
        object_code = object_type.adt_object_type().as_str(),
        description = xml_escape(description),
        package = xml_escape(package),
        name = xml_escape(name),
    )
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_only_the_types_that_have_no_source() {
        assert_eq!(
            MetadataAdtObjectType::parse("dtel").unwrap(),
            MetadataAdtObjectType::DataElement
        );
        assert_eq!(
            MetadataAdtObjectType::parse(" DOMA ").unwrap(),
            MetadataAdtObjectType::Domain
        );
        // A source-based type must not be routed through this family.
        assert_eq!(
            MetadataAdtObjectType::parse("TTYP").unwrap(),
            MetadataAdtObjectType::TableType
        );
        assert_eq!(
            MetadataAdtObjectType::parse("MSAG").unwrap(),
            MetadataAdtObjectType::MessageClass
        );
        assert_eq!(
            MetadataAdtObjectType::parse("SRVB").unwrap(),
            MetadataAdtObjectType::ServiceBinding
        );
        assert!(MetadataAdtObjectType::parse("CLAS").is_err());
        assert!(MetadataAdtObjectType::parse("TABL").is_err());
    }

    #[test]
    fn the_type_name_is_the_one_the_shared_kind_table_spells() {
        // Spelling these here as well is what lets two tables drift. The name
        // reported back to the caller has to be the name the CLI accepts.
        assert_eq!(
            MetadataAdtObjectType::DataElement.as_str(),
            RepositoryKind::Dtel.as_str()
        );
        assert_eq!(
            MetadataAdtObjectType::Domain.as_str(),
            RepositoryKind::Doma.as_str()
        );
    }

    #[test]
    fn every_metadata_type_agrees_with_the_shared_code_table() {
        for object_type in [
            MetadataAdtObjectType::DataElement,
            MetadataAdtObjectType::Domain,
            MetadataAdtObjectType::TableType,
            MetadataAdtObjectType::MessageClass,
            MetadataAdtObjectType::ServiceBinding,
        ] {
            assert_eq!(
                object_type.adt_object_type().kind(),
                object_type.repository_kind(),
                "{} maps to a subtype code of a different kind",
                object_type.as_str()
            );
        }
    }

    #[test]
    fn every_type_round_trips_through_its_own_name() {
        for object_type in [
            MetadataAdtObjectType::DataElement,
            MetadataAdtObjectType::Domain,
            MetadataAdtObjectType::TableType,
            MetadataAdtObjectType::MessageClass,
            MetadataAdtObjectType::ServiceBinding,
        ] {
            assert_eq!(
                MetadataAdtObjectType::parse(object_type.as_str()).unwrap(),
                object_type
            );
        }
    }

    #[test]
    fn an_identity_has_no_source_uri() {
        let identity = metadata_object_identity(
            MetadataAdtObjectType::DataElement,
            "ZSAMPLE_DE",
            &EditPolicy::namespaces_only(&["Z*"]),
        )
        .unwrap();

        assert_eq!(identity.object_type, "DTEL");
        assert_eq!(identity.name, "ZSAMPLE_DE");
        assert_eq!(
            identity.object_uri,
            "/sap/bc/adt/ddic/dataelements/zsample_de"
        );
        assert_eq!(identity.source_uri, None);
    }

    #[test]
    fn refuses_a_name_outside_the_customer_namespaces() {
        let error = metadata_object_identity(
            MetadataAdtObjectType::Domain,
            "SFLIGHT",
            &EditPolicy::namespaces_only(&["Z*"]),
        )
        .unwrap_err();

        assert_eq!(error.code(), "object_outside_customer_namespaces");
    }

    #[test]
    fn builds_the_data_element_payload_sap_accepts() {
        let body = creation_payload(
            MetadataAdtObjectType::DataElement,
            "ZSAMPLE_DE",
            "$TMP",
            "Sample element",
        );

        assert!(body.contains("<blue:wbobj"));
        assert!(body.contains("xmlns:blue=\"http://www.sap.com/wbobj/dictionary/dtel\""));
        assert!(body.contains("adtcore:type=\"DTEL/DE\""));
        assert!(body.contains("adtcore:name=\"ZSAMPLE_DE\""));
        assert!(body.contains("<adtcore:packageRef adtcore:name=\"$TMP\"/>"));
    }

    fn binding(binding_type: ServiceBindingType) -> ServiceBindingSpec {
        ServiceBindingSpec {
            service_definition: "zsample_sd".to_owned(),
            binding_type,
        }
    }

    #[test]
    fn a_service_binding_carries_its_definition_and_protocol() {
        // Not a shell: SAP refuses a bare binding with an HTTP 500 carrying no
        // message at all, so both have to be in the creation payload.
        let body = service_binding_payload(
            "ZSAMPLE_SB",
            "$TMP",
            "Sample",
            &binding(ServiceBindingType::ODataV4Ui),
        );

        assert!(body.contains("<srvb:serviceBinding"));
        assert!(body.contains("xmlns:srvb=\"http://www.sap.com/adt/ddic/ServiceBindings\""));
        assert!(body.contains("adtcore:type=\"SRVB/SVB\""));
        // The reference is by name *and* URI, and the URI is lower-cased.
        assert!(body.contains("adtcore:name=\"ZSAMPLE_SD\" adtcore:type=\"SRVD/SRV\""));
        assert!(body.contains("/sap/bc/adt/ddic/srvd/sources/zsample_sd"));
        assert!(
            body.contains(
                "<srvb:binding srvb:type=\"ODATA\" srvb:version=\"V4\" srvb:category=\"0\""
            )
        );
    }

    #[test]
    fn each_binding_type_maps_to_saps_own_triple() {
        // Category 0 is a UI service and 1 a Web API; both come from SAP's own
        // bindingtypes list, not from a convention invented here.
        for (parsed, expected) in [
            ("odata-v2-ui", ("ODATA", "V2", "0")),
            ("odata-v2-web-api", ("ODATA", "V2", "1")),
            ("odata-v4-ui", ("ODATA", "V4", "0")),
            ("odata-v4-web-api", ("ODATA", "V4", "1")),
        ] {
            assert_eq!(
                ServiceBindingType::parse(parsed).unwrap().as_sap_parts(),
                expected,
                "{parsed}"
            );
        }
        // Underscores and case are accepted; the two SAP types this does not
        // build are refused by name rather than guessed at.
        assert!(ServiceBindingType::parse("ODATA_V4_UI").is_ok());
        let error = ServiceBindingType::parse("sql").unwrap_err();
        assert_eq!(error.code(), "unsupported_binding_type");
        assert!(error.hint().unwrap().contains("INA and SQL"));
    }

    #[test]
    fn a_message_class_uses_its_own_root_and_namespace() {
        // The namespace is capitalised unlike every other member of this
        // family, and the collection is not under `ddic/`; both were read off
        // a live object.
        let body = creation_payload(
            MetadataAdtObjectType::MessageClass,
            "ZSAMPLE_MSG",
            "$TMP",
            "Sample",
        );

        assert!(body.contains("<mc:messageClass"));
        assert!(body.contains("xmlns:mc=\"http://www.sap.com/adt/MessageClass\""));
        assert!(body.contains("adtcore:type=\"MSAG/N\""));
        assert_eq!(
            MetadataAdtObjectType::MessageClass.collection_path(),
            "/sap/bc/adt/messageclass"
        );
    }

    #[test]
    fn a_table_type_uses_its_own_root_and_namespace() {
        let body = creation_payload(
            MetadataAdtObjectType::TableType,
            "ZSAMPLE_TT",
            "$TMP",
            "Sample",
        );

        assert!(body.contains("<ttyp:tableType"));
        assert!(body.contains("xmlns:ttyp=\"http://www.sap.com/dictionary/tabletype\""));
        assert!(body.contains("adtcore:type=\"TTYP/DA\""));
    }

    #[test]
    fn a_domain_uses_its_own_root_and_namespace() {
        // The two families share no envelope; using the data element's wrapper
        // for a domain is a 400 rather than anything descriptive.
        let body = creation_payload(
            MetadataAdtObjectType::Domain,
            "ZSAMPLE_DO",
            "$TMP",
            "Sample",
        );

        assert!(body.contains("<doma:domain"));
        assert!(body.contains("xmlns:doma=\"http://www.sap.com/dictionary/domain\""));
        assert!(body.contains("adtcore:type=\"DOMA/DD\""));
    }

    #[test]
    fn escapes_a_description_that_would_break_the_document() {
        let body = creation_payload(
            MetadataAdtObjectType::DataElement,
            "ZSAMPLE_DE",
            "$TMP",
            r#"Width & "height" <check>"#,
        );

        assert!(body.contains("Width &amp; &quot;height&quot; &lt;check&gt;"));
    }
}

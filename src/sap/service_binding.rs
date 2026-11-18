//! Publishing a service binding, reading whether it is published, and reading
//! from the published service.
//!
//! An activated binding is not yet a service: nothing answers at its URL until
//! it is published, which registers it with the gateway. That registration is
//! outside the object, so it has its own endpoints and its own state.
//!
//! - **The protocol is the binding's, not the caller's.** A binding is created
//!   as OData V2 or V4 and publishes as what it is; each protocol has its own
//!   `odatav2` / `odatav4` endpoints and its own details document.
//! - **The details document is the published state.** The binding document
//!   carries a `srvb:published` flag too, and it has been seen saying `true`
//!   for a binding whose service no longer exists. Both are reported; only the
//!   details document is trusted.
//! - **The publish response is not trusted either.** A refusal arrives as
//!   HTTP 200 with `SEVERITY` `ERROR`, so the body is parsed, and success is
//!   proved by reading the details back.
//! - Publishing a published binding, or unpublishing an unpublished one, is
//!   answered `OK` by SAP. Fractal checks first and sends nothing.
//! - **Reading is open, writing is not.** `show` and `get` work on any
//!   binding; the customer-namespace rule guards `publish` and `unpublish`.

use thiserror::Error;

use super::{
    adt_message_severity::AdtMessageSeverity,
    adt_object_identity::AdtObjectIdentity,
    adt_response::{AdtResponseParseError, parse_adt_document},
    adt_version::AdtVersion,
    client::{SapClient, SapClientError, SapHttpErrorKind},
    editable_source::{AdtEditTargetValidationError, validate_object_name},
    find_child, find_non_empty_attribute,
    metadata_activation::ACTIVE_VERSION,
    metadata_document::declared_version,
    metadata_object::{MetadataAdtObjectType, metadata_object_identity},
    object_family::AdtObjectFamily,
    package_authorization::{PackageAuthorizationError, authorize_object_package},
};
use crate::config::EditPolicy;
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

const BUSINESS_SERVICES: &str = "/sap/bc/adt/businessservices";

/// The OData version a binding exposes its service over.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ODataVersion {
    V2,
    V4,
}

impl ODataVersion {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::V2 => "V2",
            Self::V4 => "V4",
        }
    }

    const fn path_segment(self) -> &'static str {
        match self {
            Self::V2 => "odatav2",
            Self::V4 => "odatav4",
        }
    }
}

/// Publishing or unpublishing: one flow, two directions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOperation {
    Publish,
    Unpublish,
}

impl PublishOperation {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Publish => "publish",
            Self::Unpublish => "unpublish",
        }
    }

    const fn jobs(self) -> &'static str {
        match self {
            Self::Publish => "publishjobs",
            Self::Unpublish => "unpublishjobs",
        }
    }

    /// The published state this operation leaves behind.
    const fn target(self) -> bool {
        matches!(self, Self::Publish)
    }
}

/// One service version a binding carries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingService {
    pub service_name: String,
    pub service_version: String,
    pub service_definition: String,
}

/// What the binding document says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceBindingDocument {
    pub protocol: ODataVersion,
    pub services: Vec<BindingService>,
    /// The binding's own flag, which can disagree with the registration.
    pub published_flag: Option<bool>,
}

/// One entity set the published service exposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntitySet {
    pub name: String,
    pub is_root: bool,
}

/// The registration state of one service version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceDetails {
    pub published: bool,
    pub service_url: Option<String>,
    pub annotation_url: Option<String>,
    pub entity_sets: Vec<EntitySet>,
}

/// One service version and its registration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceState {
    pub service: BindingService,
    pub details: ServiceDetails,
}

/// What `binding show` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceBindingShowResult {
    pub name: String,
    pub object_uri: String,
    pub protocol: ODataVersion,
    pub binding_published_flag: Option<bool>,
    pub services: Vec<ServiceState>,
}

/// How a publish or unpublish ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishStatus {
    /// SAP did it and the details document confirms it.
    Verified,
    /// The service was already in the requested state; nothing was sent.
    AlreadyDone,
    /// `--dry-run`: nothing was sent.
    WouldSend,
}

/// What `binding publish` / `unpublish` reports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishResult {
    pub operation: PublishOperation,
    pub status: PublishStatus,
    pub name: String,
    pub object_uri: String,
    pub protocol: ODataVersion,
    pub service: BindingService,
    /// The request that was, or under `--dry-run` would have been, sent.
    pub request_path: String,
    /// SAP's own words, when a request was sent.
    pub sap_message: Option<SapPublishMessage>,
    pub details_after: ServiceDetails,
}

/// The `SEVERITY` / `SHORT_TEXT` / `LONG_TEXT` triple a publish job answers with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SapPublishMessage {
    pub severity: String,
    pub short_text: String,
    pub long_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishRequest {
    pub operation: PublishOperation,
    pub name: String,
    pub service_version: Option<String>,
    pub dry_run: bool,
}

#[derive(Debug, Error)]
pub enum ServiceBindingError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
    #[error("service binding {name} does not exist")]
    NotFound { name: String },
    #[error("could not read the {what} of service binding {name}: {source}")]
    Read {
        name: String,
        what: &'static str,
        #[source]
        source: SapClientError,
    },
    #[error("could not parse the {what} of service binding {name}: {reason}")]
    Parse {
        name: String,
        what: &'static str,
        reason: String,
    },
    #[error("service binding {name} has no active version")]
    NotActive { name: String },
    #[error("service binding {name} is a {protocol} binding; only OData bindings publish")]
    TypeUnsupported { name: String, protocol: String },
    #[error("service binding {name} has several service versions ({}); name one", versions.join(", "))]
    ServiceVersionAmbiguous { name: String, versions: Vec<String> },
    #[error("service binding {name} has no service version {requested} (it has {})", versions.join(", "))]
    ServiceVersionUnknown {
        name: String,
        requested: String,
        versions: Vec<String>,
    },
    #[error("the {} request for service binding {name} failed: {source}", operation.as_str())]
    Request {
        operation: PublishOperation,
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP refused to {} service binding {name}: {}", operation.as_str(), message.short_text)]
    Refused {
        operation: PublishOperation,
        name: String,
        message: SapPublishMessage,
    },
    #[error("the service of binding {name} is not published")]
    NotPublished { name: String },
    #[error("path '{path}' {reason}")]
    PathInvalid { path: String, reason: &'static str },
    #[error("reading {url} from service binding {name} failed: {source}")]
    GetFailed {
        name: String,
        url: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP answered the {} of service binding {name} without error, but the service is still {}", operation.as_str(), if operation.target() { "unpublished" } else { "published" })]
    Unverified {
        operation: PublishOperation,
        name: String,
        message: Option<SapPublishMessage>,
    },
}

impl ReportableError for ServiceBindingError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::PackageNotAllowed(error) => error.code(),
            Self::NotFound { .. } => "binding_not_found",
            Self::Read { .. } => "binding_read_failed",
            Self::Parse { .. } => "binding_response_parse_error",
            Self::NotActive { .. } => "binding_not_active",
            Self::TypeUnsupported { .. } => "binding_type_unsupported",
            Self::ServiceVersionAmbiguous { .. } => "binding_service_version_ambiguous",
            Self::ServiceVersionUnknown { .. } => "binding_service_version_unknown",
            Self::NotPublished { .. } => "binding_not_published",
            Self::PathInvalid { .. } => "binding_path_invalid",
            Self::GetFailed { .. } => "binding_get_failed",
            Self::Request { operation, .. } => match operation {
                PublishOperation::Publish => "binding_publish_request_failed",
                PublishOperation::Unpublish => "binding_unpublish_request_failed",
            },
            Self::Refused { operation, .. } => match operation {
                PublishOperation::Publish => "binding_publish_failed",
                PublishOperation::Unpublish => "binding_unpublish_failed",
            },
            Self::Unverified { operation, .. } => match operation {
                PublishOperation::Publish => "binding_publish_unverified",
                PublishOperation::Unpublish => "binding_unpublish_unverified",
            },
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Validation(error) => error.status(),
            Self::PackageNotAllowed(error) => error.status(),
            Self::Read { source, .. }
            | Self::Request { source, .. }
            | Self::GetFailed { source, .. } => sap_http_status(Some(source)),
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Validation(error) => return error.hint(),
            Self::PackageNotAllowed(error) => return error.hint(),
            Self::NotFound { .. } => "Check the name, or search for the binding.".to_owned(),
            Self::Read { .. } | Self::Request { .. } => {
                "Nothing was changed by this command. Check the connection and retry.".to_owned()
            }
            Self::Parse { .. } => {
                "SAP's answer did not have the expected shape; nothing further was sent."
                    .to_owned()
            }
            Self::NotActive { name } => format!(
                "Only an active binding can be published. Activate it first with `fractal edit activate --type SRVB --name {name}`."
            ),
            Self::TypeUnsupported { .. } => {
                "INA and SQL bindings have no publish step that Fractal drives.".to_owned()
            }
            Self::NotPublished { name } => format!(
                "Nothing answers at the service URL until it is published. Publish it with `fractal binding publish --name {name}`."
            ),
            Self::PathInvalid { .. } => {
                "Give the part of the URL after the service root, such as `Items?$top=5` or `$metadata`.".to_owned()
            }
            Self::GetFailed { source, .. } => match source {
                SapClientError::Http { status, .. } if status.as_u16() == 404 => {
                    "Check the entity set and key against the service's `$metadata`.".to_owned()
                }
                SapClientError::Http { status, .. } if status.is_server_error() => {
                    "The service failed inside SAP; nothing was changed. If it left a short dump, the suggested command shows it.".to_owned()
                }
                _ => "The message is the service's own; nothing was changed.".to_owned(),
            },
            Self::ServiceVersionAmbiguous { .. } | Self::ServiceVersionUnknown { .. } => {
                "Pass --service-version with one of the versions listed.".to_owned()
            }
            Self::Refused { message, .. } => message.long_text.clone().map_or_else(
                || "SAP gave no further detail.".to_owned(),
                |long| format!("SAP: {long}"),
            ),
            Self::Unverified { .. } => {
                "Read the binding's state again before retrying; the registration may still be catching up, or SAP may have dropped the request.".to_owned()
            }
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Validation(error) => error.suggested_command(),
            Self::PackageNotAllowed(error) => error.suggested_command(),
            Self::NotFound { name } => Some(suggested_command::object_search("SRVB", name)),
            Self::NotActive { .. }
            | Self::TypeUnsupported { .. }
            | Self::Parse { .. }
            | Self::PathInvalid { .. } => None,
            Self::GetFailed {
                source: SapClientError::Http { status, .. },
                ..
            } if status.is_server_error() => Some(suggested_command::dump_show_latest()),
            Self::Read { name, .. }
            | Self::ServiceVersionAmbiguous { name, .. }
            | Self::ServiceVersionUnknown { name, .. }
            | Self::Request { name, .. }
            | Self::Refused { name, .. }
            | Self::Unverified { name, .. }
            | Self::NotPublished { name }
            | Self::GetFailed { name, .. } => Some(suggested_command::binding_show(name)),
        }
    }
}

/// Reads a binding and the registration state of every service version it has.
///
/// Any binding, not only one in the customer namespaces: that rule guards
/// writes, and this reads.
///
/// # Errors
///
/// Returns [`ServiceBindingError`] for a malformed name, a binding that does
/// not exist or is not active, or a read SAP refused.
pub async fn show_service_binding(
    sap: &SapClient,
    name: &str,
) -> Result<ServiceBindingShowResult, ServiceBindingError> {
    let identity = read_identity(name)?;
    let binding = read_active_binding(sap, &identity).await?;
    let mut services = Vec::with_capacity(binding.services.len());
    for service in binding.services {
        let details = read_details(sap, &identity.name, binding.protocol, &service).await?;
        services.push(ServiceState { service, details });
    }

    Ok(ServiceBindingShowResult {
        name: identity.name,
        object_uri: identity.object_uri,
        protocol: binding.protocol,
        binding_published_flag: binding.published_flag,
        services,
    })
}

/// What `binding get` reports: the response, as the service sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceGetResult {
    pub name: String,
    pub protocol: ODataVersion,
    pub service: BindingService,
    /// The path that was read: the registered service root plus the caller's.
    pub url: String,
    pub status: u16,
    pub content_type: Option<String>,
    pub body: String,
}

/// Reads `path` from a binding's published service.
///
/// `path` is the part after the service root, as written in
/// `/IWFND/GW_CLIENT`: an entity set, key, navigation, `$metadata`, and query
/// options. The root comes from the service's registration, so the caller
/// never assembles V4's `srvd/…/0001/` form. Only a GET is sent, with no CSRF
/// token, so actions and `$batch` (both POST) are unreachable.
///
/// # Errors
///
/// Returns [`ServiceBindingError`] for a malformed name or path, a binding
/// that does not exist, is not active or is not published, or a response
/// other than success, carrying the service's own message.
pub async fn get_from_service(
    sap: &SapClient,
    name: &str,
    service_version: Option<&str>,
    path: &str,
) -> Result<ServiceGetResult, ServiceBindingError> {
    let identity = read_identity(name)?;
    validate_service_path(path)?;
    let binding = read_active_binding(sap, &identity).await?;
    let service = choose_service(&identity.name, binding.services, service_version)?;
    let details = read_details(sap, &identity.name, binding.protocol, &service).await?;
    let (true, Some(root)) = (details.published, details.service_url) else {
        return Err(ServiceBindingError::NotPublished {
            name: identity.name,
        });
    };
    let url = join_service_path(&root, path)?;

    let response = sap.get_text_response(&url, &[]).await.map_err(|source| {
        ServiceBindingError::GetFailed {
            name: identity.name.clone(),
            url: url.clone(),
            source,
        }
    })?;
    Ok(ServiceGetResult {
        name: identity.name,
        protocol: binding.protocol,
        service,
        url,
        status: response.status,
        content_type: response.content_type,
        body: response.body,
    })
}

/// A binding's identity for reading: the name's shape is checked, the
/// customer namespace is not.
fn read_identity(name: &str) -> Result<AdtObjectIdentity, ServiceBindingError> {
    let name = validate_object_name(name).map_err(AdtEditTargetValidationError::InvalidObject)?;
    let object_type = MetadataAdtObjectType::ServiceBinding;
    Ok(AdtObjectIdentity {
        object_type: AdtObjectFamily::Metadata(object_type),
        object_uri: format!(
            "{}/{}",
            object_type.collection_path(),
            name.to_ascii_lowercase().replace('/', "%2f")
        ),
        name,
        source_uri: None,
    })
}

/// Refuses a path that could address anything outside the service root.
fn validate_service_path(path: &str) -> Result<(), ServiceBindingError> {
    let lower = path.to_ascii_lowercase();
    let reason = if path.starts_with('/') || path.starts_with('\\') {
        Some("is absolute; give only the part after the service root")
    } else if lower.contains("://") {
        Some("names a host; give only the part after the service root")
    } else if path.contains("..")
        || lower.contains("%2e%2e")
        || lower.contains(".%2e")
        || lower.contains("%2e.")
    {
        Some("contains `..`, which could leave the service root")
    } else if path.contains('#') {
        Some("contains a fragment (`#`), which is never sent")
    } else {
        None
    };
    reason.map_or(Ok(()), |reason| {
        Err(ServiceBindingError::PathInvalid {
            path: path.to_owned(),
            reason,
        })
    })
}

/// Joins the caller's path onto the service root, and proves the result is
/// still under it after URL normalisation.
fn join_service_path(root: &str, path: &str) -> Result<String, ServiceBindingError> {
    let root = if root.ends_with('/') {
        root.to_owned()
    } else {
        format!("{root}/")
    };
    let joined = format!("{root}{path}");
    let normalised = url::Url::parse("http://sap.invalid")
        .and_then(|base| base.join(&joined))
        .ok()
        .filter(|url| url.path().starts_with(root.trim_end_matches('/')));
    if normalised.is_none() {
        return Err(ServiceBindingError::PathInvalid {
            path: path.to_owned(),
            reason: "does not stay under the service root",
        });
    }
    Ok(joined)
}

/// Publishes or unpublishes one service version of a binding, and proves it.
///
/// Sends nothing when the service is already in the requested state, or under
/// `dry_run`.
///
/// # Errors
///
/// Returns [`ServiceBindingError`] for validation, a package outside the
/// allowlist, an inactive or non-OData binding, an unclear service version,
/// a refusal, or a result the details document does not confirm.
pub async fn publish_service_binding(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &PublishRequest,
) -> Result<PublishResult, ServiceBindingError> {
    let identity =
        metadata_object_identity(MetadataAdtObjectType::ServiceBinding, &request.name, policy)?;
    authorize_object_package(sap, policy, &identity.name, &identity.object_uri).await?;
    let binding = read_active_binding(sap, &identity).await?;
    let service = choose_service(
        &identity.name,
        binding.services,
        request.service_version.as_deref(),
    )?;
    let operation = request.operation;
    let request_path = format!(
        "{BUSINESS_SERVICES}/{}/{}",
        binding.protocol.path_segment(),
        operation.jobs()
    );

    let before = read_details(sap, &identity.name, binding.protocol, &service).await?;
    let finished = |status, sap_message, details_after| PublishResult {
        operation,
        status,
        name: identity.name.clone(),
        object_uri: identity.object_uri.clone(),
        protocol: binding.protocol,
        service: service.clone(),
        request_path: request_path.clone(),
        sap_message,
        details_after,
    };
    if before.published == operation.target() {
        return Ok(finished(PublishStatus::AlreadyDone, None, before));
    }
    if request.dry_run {
        return Ok(finished(PublishStatus::WouldSend, None, before));
    }

    let response = sap
        .post_text(
            &request_path,
            &[
                ("servicename", service.service_name.as_str()),
                ("serviceversion", service.service_version.as_str()),
            ],
            Some(&publish_body(&identity.name)),
            publish_headers(),
        )
        .await
        .map_err(|source| ServiceBindingError::Request {
            operation,
            name: identity.name.clone(),
            source,
        })?;
    let message = parse_publish_response(&response);
    if let Some(message) = &message
        && AdtMessageSeverity::from_sap_code(&message.severity) == AdtMessageSeverity::Error
    {
        return Err(ServiceBindingError::Refused {
            operation,
            name: identity.name.clone(),
            message: message.clone(),
        });
    }

    let after = read_details(sap, &identity.name, binding.protocol, &service).await?;
    if after.published != operation.target() {
        return Err(ServiceBindingError::Unverified {
            operation,
            name: identity.name.clone(),
            message,
        });
    }
    Ok(finished(PublishStatus::Verified, message, after))
}

async fn read_active_binding(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
) -> Result<ServiceBindingDocument, ServiceBindingError> {
    let xml = sap
        .get_text_with_query(
            &identity.object_uri,
            &[("version", AdtVersion::Active.as_str())],
        )
        .await
        .map_err(|source| match source {
            SapClientError::Http {
                kind: SapHttpErrorKind::NotFound,
                ..
            } => ServiceBindingError::NotFound {
                name: identity.name.clone(),
            },
            source => ServiceBindingError::Read {
                name: identity.name.clone(),
                what: "binding",
                source,
            },
        })?;
    parse_binding_document(&identity.name, &xml)
}

/// Parses the binding document, refusing one that is not active.
///
/// SAP serves the inactive document when asked for an active one that does not
/// exist, so the declared version is what decides, not the request.
fn parse_binding_document(
    name: &str,
    xml: &str,
) -> Result<ServiceBindingDocument, ServiceBindingError> {
    let parse_error = |reason: String| ServiceBindingError::Parse {
        name: name.to_owned(),
        what: "binding",
        reason,
    };
    let document =
        parse_adt_document(xml).map_err(|AdtResponseParseError(reason)| parse_error(reason))?;
    let root = document.root_element();
    if declared_version(root).as_deref() != Some(ACTIVE_VERSION) {
        return Err(ServiceBindingError::NotActive {
            name: name.to_owned(),
        });
    }

    let binding = find_child(root, "binding")
        .ok_or_else(|| parse_error("no srvb:binding element".to_owned()))?;
    let kind = find_non_empty_attribute(binding, "type").unwrap_or_default();
    let version = find_non_empty_attribute(binding, "version").unwrap_or_default();
    let protocol = match (kind.as_str(), version.as_str()) {
        ("ODATA", "V2") => ODataVersion::V2,
        ("ODATA", "V4") => ODataVersion::V4,
        _ => {
            return Err(ServiceBindingError::TypeUnsupported {
                name: name.to_owned(),
                protocol: format!("{kind} {version}").trim().to_owned(),
            });
        }
    };

    let mut services = Vec::new();
    for group in root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "services")
    {
        let service_name = find_non_empty_attribute(group, "name")
            .ok_or_else(|| parse_error("a srvb:services element has no name".to_owned()))?;
        for content in group
            .children()
            .filter(|node| node.is_element() && node.tag_name().name() == "content")
        {
            let service_version = find_non_empty_attribute(content, "version")
                .ok_or_else(|| parse_error("a srvb:content element has no version".to_owned()))?;
            let service_definition = find_child(content, "serviceDefinition")
                .and_then(|node| find_non_empty_attribute(node, "name"))
                .ok_or_else(|| {
                    parse_error("a srvb:content element names no service definition".to_owned())
                })?;
            services.push(BindingService {
                service_name: service_name.clone(),
                service_version,
                service_definition,
            });
        }
    }
    if services.is_empty() {
        return Err(parse_error("the binding carries no service".to_owned()));
    }

    Ok(ServiceBindingDocument {
        protocol,
        services,
        published_flag: find_non_empty_attribute(root, "published").map(|flag| flag == "true"),
    })
}

fn choose_service(
    name: &str,
    services: Vec<BindingService>,
    requested: Option<&str>,
) -> Result<BindingService, ServiceBindingError> {
    let versions: Vec<String> = services
        .iter()
        .map(|service| service.service_version.clone())
        .collect();
    let mut matching = services
        .into_iter()
        .filter(|service| requested.is_none_or(|requested| service.service_version == requested));
    match (matching.next(), matching.next(), requested) {
        (Some(service), None, _) => Ok(service),
        (None, _, Some(requested)) => Err(ServiceBindingError::ServiceVersionUnknown {
            name: name.to_owned(),
            requested: requested.to_owned(),
            versions,
        }),
        _ => Err(ServiceBindingError::ServiceVersionAmbiguous {
            name: name.to_owned(),
            versions,
        }),
    }
}

async fn read_details(
    sap: &SapClient,
    name: &str,
    protocol: ODataVersion,
    service: &BindingService,
) -> Result<ServiceDetails, ServiceBindingError> {
    let path = format!("{BUSINESS_SERVICES}/{}/{name}", protocol.path_segment());
    let xml = sap
        .get_text_with_query(
            &path,
            &[
                ("servicename", service.service_name.as_str()),
                ("serviceversion", service.service_version.as_str()),
                ("srvdname", service.service_definition.as_str()),
            ],
        )
        .await
        .map_err(|source| ServiceBindingError::Read {
            name: name.to_owned(),
            what: "service details",
            source,
        })?;
    parse_details(protocol, service, &xml).map_err(|reason| ServiceBindingError::Parse {
        name: name.to_owned(),
        what: "service details",
        reason,
    })
}

/// Parses the details document of either protocol.
///
/// V4 wraps its services in a `serviceGroup` that carries the published flag;
/// V2 lists them in a `serviceList` and flags each service. Both name the
/// service URL and its entity sets on the `services` element.
fn parse_details(
    protocol: ODataVersion,
    service: &BindingService,
    xml: &str,
) -> Result<ServiceDetails, String> {
    let document = parse_adt_document(xml).map_err(|AdtResponseParseError(reason)| reason)?;
    let root = document.root_element();
    let entry = root
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "services")
        .find(|node| {
            find_non_empty_attribute(*node, "serviceVersion").as_deref()
                == Some(service.service_version.as_str())
        })
        .ok_or_else(|| format!("no entry for service version {}", service.service_version))?;
    let flag_holder = match protocol {
        ODataVersion::V4 => root,
        ODataVersion::V2 => entry,
    };
    let published = find_non_empty_attribute(flag_holder, "published")
        .ok_or_else(|| "no published flag".to_owned())?
        == "true";
    let entity_sets = find_child(entry, "serviceInformation")
        .map(|information| {
            information
                .children()
                .filter(|node| node.is_element() && node.tag_name().name() == "collection")
                .filter_map(|node| {
                    Some(EntitySet {
                        name: find_non_empty_attribute(node, "name")?,
                        is_root: find_non_empty_attribute(node, "isRoot").as_deref()
                            == Some("true"),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(ServiceDetails {
        published,
        service_url: find_non_empty_attribute(entry, "serviceUrl"),
        annotation_url: find_non_empty_attribute(entry, "annotationUrl"),
        entity_sets,
    })
}

fn publish_body(name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\">\
<adtcore:objectReference adtcore:type=\"SRVB/SVB\" adtcore:name=\"{name}\"/>\
</adtcore:objectReferences>"
    )
}

fn publish_headers() -> reqwest::header::HeaderMap {
    use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/xml"));
    headers.insert(
        ACCEPT,
        HeaderValue::from_static("application/xml, application/vnd.sap.as+xml"),
    );
    headers
}

/// Reads the `asx:abap` answer of a publish job.
///
/// `None` when the body has no recognisable message: success is decided by
/// reading the details back, so an unreadable answer is not itself a failure.
fn parse_publish_response(xml: &str) -> Option<SapPublishMessage> {
    let document = parse_adt_document(xml).ok()?;
    let data = document
        .descendants()
        .find(|node| node.is_element() && node.tag_name().name() == "DATA")?;
    let text = |tag: &str| {
        find_child(data, tag)
            .and_then(|node| node.text())
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Some(SapPublishMessage {
        severity: text("SEVERITY")?,
        short_text: text("SHORT_TEXT").unwrap_or_default(),
        long_text: text("LONG_TEXT"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_service_that_fails_inside_sap_points_at_the_dump() {
        let error = ServiceBindingError::GetFailed {
            name: "ZSAMPLE_UI_O4".to_owned(),
            url: "https://sap.example/x".to_owned(),
            source: SapClientError::Http {
                kind: crate::sap::client::SapHttpErrorKind::ServerError,
                status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                url: "https://sap.example/x".to_owned(),
                message: "Internal Server Error".to_owned(),
            },
        };
        assert_eq!(
            error.suggested_command().as_deref(),
            Some("fractal dump show --latest")
        );
        assert!(error.hint().unwrap().contains("short dump"));
    }

    const V4_BINDING: &str = r#"<?xml version="1.0" encoding="utf-8"?><srvb:serviceBinding srvb:published="false" adtcore:name="ZSAMPLE_UI_O4" adtcore:type="SRVB/SVB" adtcore:version="active" xmlns:srvb="http://www.sap.com/adt/ddic/ServiceBindings" xmlns:adtcore="http://www.sap.com/adt/core"><srvb:services srvb:name="ZSAMPLE_UI_O4"><srvb:content srvb:version="0001"><srvb:serviceDefinition adtcore:name="ZSAMPLE_SD"/></srvb:content></srvb:services><srvb:binding srvb:type="ODATA" srvb:version="V4" srvb:category="0"/></srvb:serviceBinding>"#;

    fn service() -> BindingService {
        BindingService {
            service_name: "ZSAMPLE_UI_O4".to_owned(),
            service_version: "0001".to_owned(),
            service_definition: "ZSAMPLE_SD".to_owned(),
        }
    }

    #[test]
    fn a_path_cannot_leave_the_service_root() {
        for path in [
            "/sap/bc/adt/discovery",
            "https://elsewhere/x",
            "../../bc/adt/x",
            "Items/%2e%2e/%2E%2E/x",
            "Items#frag",
        ] {
            assert_eq!(
                validate_service_path(path).unwrap_err().code(),
                "binding_path_invalid",
                "{path}"
            );
        }
        for path in [
            "",
            "$metadata",
            "Items?$top=5&$filter=Name eq 'A.B'",
            "Items('K1')/_Child?$format=json",
            "Items?$filter=CreatedAt gt 2024-01-01T00:00:00Z",
        ] {
            assert!(validate_service_path(path).is_ok(), "{path}");
        }
    }

    #[test]
    fn joins_onto_either_protocol_root() {
        assert_eq!(
            join_service_path("/sap/opu/odata/sap/ZSAMPLE_O2", "Items?$format=json").unwrap(),
            "/sap/opu/odata/sap/ZSAMPLE_O2/Items?$format=json"
        );
        assert_eq!(
            join_service_path(
                "/sap/opu/odata4/sap/zsample_o4/srvd/sap/zsample_o4/0001/",
                "$metadata"
            )
            .unwrap(),
            "/sap/opu/odata4/sap/zsample_o4/srvd/sap/zsample_o4/0001/$metadata"
        );
    }

    #[test]
    fn reads_protocol_and_services_from_the_binding() {
        let binding = parse_binding_document("ZSAMPLE_UI_O4", V4_BINDING).unwrap();
        assert_eq!(binding.protocol, ODataVersion::V4);
        assert_eq!(binding.services, vec![service()]);
        assert_eq!(binding.published_flag, Some(false));
    }

    #[test]
    fn an_inactive_document_is_refused_even_when_active_was_requested() {
        let inactive =
            V4_BINDING.replace("adtcore:version=\"active\"", "adtcore:version=\"inactive\"");
        assert!(matches!(
            parse_binding_document("ZSAMPLE_UI_O4", &inactive),
            Err(ServiceBindingError::NotActive { .. })
        ));
    }

    #[test]
    fn a_non_odata_binding_is_refused_by_name() {
        let ina = V4_BINDING
            .replace("srvb:type=\"ODATA\"", "srvb:type=\"INA\"")
            .replace(
                "srvb:version=\"V4\" srvb:category",
                "srvb:version=\"V1\" srvb:category",
            );
        let error = parse_binding_document("ZSAMPLE_UI_O4", &ina).unwrap_err();
        assert_eq!(error.code(), "binding_type_unsupported");
        assert!(error.to_string().contains("INA V1"));
    }

    #[test]
    fn a_version_must_be_named_when_there_are_several() {
        let two = vec![
            service(),
            BindingService {
                service_version: "0002".to_owned(),
                ..service()
            },
        ];
        assert_eq!(
            choose_service("ZSAMPLE_UI_O4", two.clone(), None)
                .unwrap_err()
                .code(),
            "binding_service_version_ambiguous"
        );
        assert_eq!(
            choose_service("ZSAMPLE_UI_O4", two.clone(), Some("0003"))
                .unwrap_err()
                .code(),
            "binding_service_version_unknown"
        );
        assert_eq!(
            choose_service("ZSAMPLE_UI_O4", two, Some("0002"))
                .unwrap()
                .service_version,
            "0002"
        );
    }

    #[test]
    fn v4_details_take_the_flag_from_the_service_group() {
        let xml = r#"<odatav4:serviceGroup odatav4:published="true" xmlns:odatav4="http://www.sap.com/categories/odatav4"><odatav4:services odatav4:serviceId="ZSAMPLE_UI_O4" odatav4:serviceVersion="0001" odatav4:serviceUrl="/sap/opu/odata4/sap/zsample_ui_o4/srvd/sap/zsample_ui_o4/0001/" odatav4:annotationUrl=""><serviceInfo:serviceInformation xmlns:serviceInfo="http://www.sap.com/categories/serviceinformation"><serviceInfo:collection serviceInfo:name="Item" serviceInfo:isRoot="true"/></serviceInfo:serviceInformation></odatav4:services></odatav4:serviceGroup>"#;
        let details = parse_details(ODataVersion::V4, &service(), xml).unwrap();
        assert!(details.published);
        assert_eq!(
            details.service_url.as_deref(),
            Some("/sap/opu/odata4/sap/zsample_ui_o4/srvd/sap/zsample_ui_o4/0001/")
        );
        assert_eq!(details.annotation_url, None);
        assert_eq!(
            details.entity_sets,
            vec![EntitySet {
                name: "Item".to_owned(),
                is_root: true
            }]
        );
    }

    #[test]
    fn v2_details_take_the_flag_from_the_service() {
        let xml = r#"<odatav2:serviceList xmlns:odatav2="http://www.sap.com/categories/odatav2"><odatav2:services odatav2:serviceVersion="0001" odatav2:serviceUrl="" odatav2:published="false"><serviceInfo:serviceInformation xmlns:serviceInfo="http://www.sap.com/categories/serviceinformation"/></odatav2:services></odatav2:serviceList>"#;
        let details = parse_details(ODataVersion::V2, &service(), xml).unwrap();
        assert!(!details.published);
        assert_eq!(details.service_url, None);
        assert!(details.entity_sets.is_empty());
    }

    #[test]
    fn reads_a_refusal_from_a_successful_http_answer() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?><asx:abap version="1.0" xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA><SEVERITY>ERROR</SEVERITY><SHORT_TEXT>Local Publish of ZSAMPLE_UI_O4 failed</SHORT_TEXT><LONG_TEXT>Service Binding ZSAMPLE_UI_O4 does not exist.</LONG_TEXT></DATA></asx:values></asx:abap>"#;
        let message = parse_publish_response(xml).unwrap();
        assert_eq!(
            AdtMessageSeverity::from_sap_code(&message.severity),
            AdtMessageSeverity::Error
        );
        assert_eq!(
            message.long_text.as_deref(),
            Some("Service Binding ZSAMPLE_UI_O4 does not exist.")
        );

        let ok = xml.replace("ERROR", "OK").replace(
            "<LONG_TEXT>Service Binding ZSAMPLE_UI_O4 does not exist.</LONG_TEXT>",
            "<LONG_TEXT/>",
        );
        let message = parse_publish_response(&ok).unwrap();
        assert_ne!(
            AdtMessageSeverity::from_sap_code(&message.severity),
            AdtMessageSeverity::Error
        );
        assert_eq!(message.long_text, None);
    }
}

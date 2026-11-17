//! Creating and deleting a package.
//!
//! A package is a repository object (`DEVC/K`) but not one of the edit
//! families: it has no source and no XML to fill, and where an object names
//! the package it lives in, a package names its *parent* and carries the
//! software component and transport layer that decide whether, and where, its
//! contents travel. So it has its own create and delete.
//!
//! - **A local package** (`$…`) is never transported: software component
//!   `LOCAL`, no transport layer, no request.
//! - **A transportable one** needs a request, and a software component and
//!   transport layer. With a parent they default to the parent's, which is what
//!   the Eclipse wizard does.
//! - **Delete only an empty package.** Its contents are read first and any
//!   object or subpackage refuses the delete. SAP refuses too, and for a
//!   transportable package keeps refusing after its objects are gone: their
//!   deletions sit in the request until it is released.
//! - Not journaled. An empty package holds nothing but its own attributes,
//!   which the creation command restates in full.

use thiserror::Error;

use super::{
    adt_response::parse_adt_document,
    adt_version::AdtVersion,
    client::{SapClient, SapClientError, SapHttpErrorKind},
    edit_session::{
        AdtEditSessionError, acquire_adt_object_lock, release_adt_object_lock,
        stateful_session_headers,
    },
    editable_source::canonicalize_transport_request,
    find_child, find_non_empty_attribute,
    package::{PackageError, get_package_contents},
    package_authorization::{PackageAuthorizationError, authorize_known_package},
};
use crate::config::EditPolicy;
use crate::pattern::glob_matches;
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

const PACKAGES_PATH: &str = "/sap/bc/adt/packages";
const PACKAGES_MEDIA_TYPE: &str = "application/vnd.sap.adt.packages.v2+xml";
const LOCAL_SOFTWARE_COMPONENT: &str = "LOCAL";
/// SAP's own limit on a package name.
const MAX_PACKAGE_NAME: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCreationRequest {
    pub name: String,
    pub description: String,
    pub parent: Option<String>,
    pub software_component: Option<String>,
    pub transport_layer: Option<String>,
    pub transport: Option<String>,
    /// Recorded as the package's person responsible.
    pub responsible: String,
}

/// A package as SAP reads it back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageAttributes {
    pub name: String,
    pub uri: String,
    pub description: Option<String>,
    pub parent: Option<String>,
    pub software_component: Option<String>,
    pub transport_layer: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageCreationResult {
    pub package: PackageAttributes,
    pub transport: Option<String>,
    pub local: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageDeletionResult {
    /// The package is gone but its lock could not be released, so the name
    /// stays blocked until SAP's session times out.
    pub name_still_locked: bool,
    pub name: String,
    pub uri: String,
    pub transport: Option<String>,
    /// `false` for a dry run: the package was found empty and left alone.
    pub deleted: bool,
}

#[derive(Debug, Error)]
pub enum PackageLifecycleError {
    #[error("invalid package name '{0}'")]
    InvalidName(String),
    #[error("package {name} is outside the customer namespaces ({})", patterns.join(", "))]
    OutsideNamespace { name: String, patterns: Vec<String> },
    #[error("local package {name} is refused because this profile turns off local packages")]
    LocalPackagesOff { name: String },
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
    #[error("invalid transport request '{0}'")]
    InvalidTransport(String),
    #[error("{name} is a local package and is never transported, so it takes no transport")]
    TransportOnLocal { name: String },
    #[error("{name} is transportable and needs a transport request")]
    TransportRequired { name: String },
    #[error(
        "{name} needs a software component and a transport layer: give --parent to inherit them, or both flags"
    )]
    AttributesRequired { name: String },
    #[error("parent package {parent} does not exist")]
    ParentNotFound { parent: String },
    #[error("package {name} already exists")]
    AlreadyExists { name: String },
    #[error("package {name} does not exist")]
    NotFound { name: String },
    #[error(
        "package {name} is not empty: {}",
        describe_contents(items, subpackages)
    )]
    NotEmpty {
        name: String,
        items: Vec<String>,
        subpackages: Vec<String>,
    },
    /// Empty to look at, refused by SAP: deleted objects stay registered to the
    /// package until the request recording their deletion is released.
    #[error("SAP still counts deleted objects in package {name}: {source}")]
    PendingDeletions {
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("could not read package {name}: {source}")]
    Read {
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("could not list package {name}: {source}")]
    Contents {
        name: String,
        #[source]
        source: PackageError,
    },
    #[error("SAP refused to create package {name}: {source}")]
    CreateRefused {
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP accepted package {name} but it cannot be read back")]
    NotCreated { name: String },
    #[error("could not lock package {name}: {source}")]
    Lock {
        name: String,
        #[source]
        source: AdtEditSessionError,
    },
    #[error("SAP refused to delete package {name}: {source}")]
    DeleteRefused {
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP accepted the delete but package {name} can still be read")]
    NotDeleted { name: String },
}

fn describe_contents(items: &[String], subpackages: &[String]) -> String {
    let mut parts = Vec::new();
    if !items.is_empty() {
        parts.push(format!("{} object(s), such as {}", items.len(), items[0]));
    }
    if !subpackages.is_empty() {
        parts.push(format!(
            "{} subpackage(s), such as {}",
            subpackages.len(),
            subpackages[0]
        ));
    }
    parts.join(" and ")
}

impl ReportableError for PackageLifecycleError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidName(_) => "package_name_invalid",
            Self::OutsideNamespace { .. } => "object_outside_customer_namespaces",
            Self::LocalPackagesOff { .. } => "package_local_not_allowed",
            Self::PackageNotAllowed(error) => error.code(),
            Self::InvalidTransport(_) => "invalid_transport_request",
            Self::TransportOnLocal { .. } => "package_transport_not_applicable",
            Self::TransportRequired { .. } => "package_transport_required",
            Self::AttributesRequired { .. } => "package_attributes_required",
            Self::ParentNotFound { .. } => "package_parent_not_found",
            Self::AlreadyExists { .. } => "package_already_exists",
            Self::NotFound { .. } => "package_not_found",
            Self::NotEmpty { .. } => "package_not_empty",
            Self::PendingDeletions { .. } => "package_pending_deletions",
            Self::Read { .. } => "package_read_failed",
            Self::Contents { source, .. } => source.code(),
            Self::CreateRefused { .. } => "package_create_failed",
            Self::NotCreated { .. } => "package_create_not_verified",
            Self::Lock { .. } => "package_lock_failed",
            Self::DeleteRefused { .. } => "package_delete_failed",
            Self::NotDeleted { .. } => "package_delete_not_verified",
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::PackageNotAllowed(error) => error.status(),
            Self::PendingDeletions { source, .. }
            | Self::Read { source, .. }
            | Self::CreateRefused { source, .. }
            | Self::DeleteRefused { source, .. } => sap_http_status(Some(source)),
            Self::Contents { source, .. } => source.status(),
            Self::Lock { source, .. } => source.status(),
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::InvalidName(_) => format!(
                "Use letters, digits and underscores, up to {MAX_PACKAGE_NAME} characters, starting with a customer prefix such as Z or Y, or with $ for a local package."
            ),
            Self::OutsideNamespace { .. } => {
                "Name the package inside a customer namespace, or with $ for a local package."
                    .to_owned()
            }
            Self::LocalPackagesOff { .. } => {
                "Turn local packages back on with `fractal auth set <profile> --allow-temporary-package true`, or use a transportable name.".to_owned()
            }
            Self::PackageNotAllowed(error) => return error.hint(),
            Self::InvalidTransport(_) => {
                "Use a request number such as A4HK900123.".to_owned()
            }
            Self::TransportOnLocal { .. } => "Drop --transport.".to_owned(),
            Self::TransportRequired { .. } => {
                "Pass --transport with a modifiable request, or name the package with $ to keep it local."
                    .to_owned()
            }
            Self::AttributesRequired { .. } => {
                "Read a sibling package's with `fractal object xml /sap/bc/adt/packages/<name>`: they are `pak:softwareComponent` and `pak:transportLayer`."
                    .to_owned()
            }
            Self::ParentNotFound { .. } => "Check the parent's name.".to_owned(),
            Self::AlreadyExists { .. } => "Pick another name, or use the existing package.".to_owned(),
            Self::NotFound { .. } => "Check the name.".to_owned(),
            Self::NotEmpty { .. } => {
                "Only an empty package is deleted. Delete or move what is in it first.".to_owned()
            }
            Self::PendingDeletions { .. } => {
                "The package looks empty, but its objects were deleted under a transport request that has not been released, and SAP keeps them registered to the package until it is. Release that request, then delete the package.".to_owned()
            }
            Self::Read { .. } | Self::Contents { .. } => {
                "Nothing was changed. Check the connection and retry.".to_owned()
            }
            Self::CreateRefused { source, .. } | Self::DeleteRefused { source, .. } => {
                return source.hint().or_else(|| Some("Nothing was changed.".to_owned()));
            }
            Self::NotCreated { .. } => {
                "Check whether it exists before retrying.".to_owned()
            }
            Self::Lock { .. } => {
                "Nothing was deleted. Someone may have the package open for editing.".to_owned()
            }
            Self::NotDeleted { .. } => {
                "Check whether it still exists before retrying.".to_owned()
            }
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::PackageNotAllowed(error) => error.suggested_command(),
            Self::TransportRequired { .. } => Some(suggested_command::transport_list()),
            Self::NotEmpty { name, .. } | Self::PendingDeletions { name, .. } => {
                Some(suggested_command::package_items(name))
            }
            Self::NotCreated { name } | Self::NotDeleted { name } => {
                Some(suggested_command::object_xml(&package_uri(name)))
            }
            _ => None,
        }
    }
}

/// Creates a package and reads it back.
///
/// # Errors
///
/// Returns [`PackageLifecycleError`] for an invalid name, a refused policy, a
/// missing transport or attribute, a parent that does not exist, or SAP
/// refusing or not completing the creation.
pub async fn create_package(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &PackageCreationRequest,
) -> Result<PackageCreationResult, PackageLifecycleError> {
    let name = validate_package_name(&request.name)?;
    let local = is_local(&name);
    authorize_package_name(policy, &name)?;

    let transport = canonicalize_transport_request(request.transport.as_deref()).map_err(|_| {
        PackageLifecycleError::InvalidTransport(request.transport.clone().unwrap_or_default())
    })?;
    match (local, &transport) {
        (true, Some(_)) => return Err(PackageLifecycleError::TransportOnLocal { name }),
        (false, None) => return Err(PackageLifecycleError::TransportRequired { name }),
        _ => {}
    }

    let parent = match &request.parent {
        Some(parent) => {
            let parent = validate_package_name(parent)?;
            match read_package(sap, &parent).await? {
                Some(attributes) => Some(attributes),
                None => return Err(PackageLifecycleError::ParentNotFound { parent }),
            }
        }
        None => None,
    };
    if read_package(sap, &name).await?.is_some() {
        return Err(PackageLifecycleError::AlreadyExists { name });
    }

    let (software_component, transport_layer) = if local {
        (LOCAL_SOFTWARE_COMPONENT.to_owned(), String::new())
    } else {
        let inherited =
            |pick: fn(&PackageAttributes) -> Option<String>| parent.as_ref().and_then(pick);
        match (
            request
                .software_component
                .clone()
                .or_else(|| inherited(|p| p.software_component.clone())),
            request
                .transport_layer
                .clone()
                .or_else(|| inherited(|p| p.transport_layer.clone())),
        ) {
            (Some(component), Some(layer)) => {
                (component.to_ascii_uppercase(), layer.to_ascii_uppercase())
            }
            _ => return Err(PackageLifecycleError::AttributesRequired { name }),
        }
    };

    let body = creation_body(
        &name,
        &request.description,
        &request.responsible,
        parent.as_ref().map(|p| p.name.as_str()),
        &software_component,
        &transport_layer,
    );
    let mut query = Vec::new();
    if let Some(transport) = &transport {
        query.push(("corrNr", transport.as_str()));
    }
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CONTENT_TYPE,
        reqwest::header::HeaderValue::from_static(PACKAGES_MEDIA_TYPE),
    );
    headers.insert(
        reqwest::header::ACCEPT,
        reqwest::header::HeaderValue::from_static(PACKAGES_MEDIA_TYPE),
    );
    sap.post_text(PACKAGES_PATH, &query, Some(&body), headers)
        .await
        .map_err(|source| PackageLifecycleError::CreateRefused {
            name: name.clone(),
            source,
        })?;

    let package = read_package(sap, &name)
        .await?
        .ok_or_else(|| PackageLifecycleError::NotCreated { name: name.clone() })?;
    Ok(PackageCreationResult {
        package,
        transport,
        local,
    })
}

/// Deletes an empty package, or under `dry_run` only proves that it is empty.
///
/// # Errors
///
/// Returns [`PackageLifecycleError`] for an invalid name, a refused policy, a
/// package that does not exist or is not empty, or SAP refusing or not
/// completing the delete.
pub async fn delete_package(
    sap: &mut SapClient,
    policy: &EditPolicy,
    name: &str,
    transport: Option<&str>,
    dry_run: bool,
) -> Result<PackageDeletionResult, PackageLifecycleError> {
    let name = validate_package_name(name)?;
    authorize_package_name(policy, &name)?;
    let transport = canonicalize_transport_request(transport).map_err(|_| {
        PackageLifecycleError::InvalidTransport(transport.unwrap_or_default().to_owned())
    })?;
    if is_local(&name) && transport.is_some() {
        return Err(PackageLifecycleError::TransportOnLocal { name });
    }
    let uri = package_uri(&name);
    if read_package(sap, &name).await?.is_none() {
        return Err(PackageLifecycleError::NotFound { name });
    }

    let contents = get_package_contents(sap, &name).await.map_err(|source| {
        PackageLifecycleError::Contents {
            name: name.clone(),
            source,
        }
    })?;
    if !contents.items.is_empty() || !contents.subpackages.is_empty() {
        return Err(PackageLifecycleError::NotEmpty {
            name,
            items: contents
                .items
                .iter()
                .map(|item| format!("{} {}", item.object_type.as_str(), item.name))
                .collect(),
            subpackages: contents
                .subpackages
                .iter()
                .map(|subpackage| subpackage.name.clone())
                .collect(),
        });
    }
    if dry_run {
        return Ok(PackageDeletionResult {
            name_still_locked: false,
            name,
            uri,
            transport,
            deleted: false,
        });
    }

    let lock = acquire_adt_object_lock(sap, &uri, transport.as_deref())
        .await
        .map_err(|source| PackageLifecycleError::Lock {
            name: name.clone(),
            source,
        })?;
    let mut query = vec![("lockHandle", lock.handle())];
    if let Some(transport) = &transport {
        query.push(("corrNr", transport.as_str()));
    }
    if let Err(source) = sap.delete(&uri, &query, stateful_session_headers()).await {
        // The package is still there, so its lock still means something.
        let _ = release_adt_object_lock(sap, &uri, &lock).await;
        return Err(if still_contains_objects(&source) {
            PackageLifecycleError::PendingDeletions { name, source }
        } else {
            PackageLifecycleError::DeleteRefused { name, source }
        });
    }

    // The lock outlives the package it was taken on, and holds the name.
    let name_still_locked = release_adt_object_lock(sap, &uri, &lock).await.is_err();
    if read_package(sap, &name).await?.is_some() {
        return Err(PackageLifecycleError::NotDeleted { name });
    }
    Ok(PackageDeletionResult {
        name_still_locked,
        name,
        uri,
        transport,
        deleted: true,
    })
}

/// `400 Package … still contains development objects or other packages`.
fn still_contains_objects(source: &SapClientError) -> bool {
    matches!(
        source,
        SapClientError::Http { message, .. } if message.to_ascii_lowercase().contains("still contains")
    )
}

/// Reads a package's attributes, or `None` when it does not exist.
async fn read_package(
    sap: &SapClient,
    name: &str,
) -> Result<Option<PackageAttributes>, PackageLifecycleError> {
    match sap
        .get_text_with_query(
            &package_uri(name),
            &[("version", AdtVersion::Active.as_str())],
        )
        .await
    {
        Ok(xml) => Ok(Some(parse_package_attributes(name, &xml))),
        Err(SapClientError::Http {
            kind: SapHttpErrorKind::NotFound,
            ..
        }) => Ok(None),
        Err(source) => Err(PackageLifecycleError::Read {
            name: name.to_owned(),
            source,
        }),
    }
}

fn parse_package_attributes(name: &str, xml: &str) -> PackageAttributes {
    let mut attributes = PackageAttributes {
        name: name.to_owned(),
        uri: package_uri(name),
        description: None,
        parent: None,
        software_component: None,
        transport_layer: None,
    };
    let Ok(document) = parse_adt_document(xml) else {
        return attributes;
    };
    let root = document.root_element();
    attributes.description = find_non_empty_attribute(root, "description");
    attributes.parent =
        find_child(root, "superPackage").and_then(|node| find_non_empty_attribute(node, "name"));
    if let Some(transport) = find_child(root, "transport") {
        attributes.software_component = find_child(transport, "softwareComponent")
            .and_then(|node| find_non_empty_attribute(node, "name"));
        attributes.transport_layer = find_child(transport, "transportLayer")
            .and_then(|node| find_non_empty_attribute(node, "name"));
    }
    attributes
}

fn creation_body(
    name: &str,
    description: &str,
    responsible: &str,
    parent: Option<&str>,
    software_component: &str,
    transport_layer: &str,
) -> String {
    let super_package = parent.map_or_else(
        || "<pak:superPackage/>".to_owned(),
        |parent| format!("<pak:superPackage adtcore:name=\"{}\"/>", escape(parent)),
    );
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<pak:package xmlns:pak=\"http://www.sap.com/adt/packages\" xmlns:adtcore=\"http://www.sap.com/adt/core\" \
adtcore:description=\"{description}\" adtcore:name=\"{name}\" adtcore:type=\"DEVC/K\" adtcore:version=\"active\" adtcore:responsible=\"{responsible}\">\
<adtcore:packageRef adtcore:name=\"{name}\"/>\
<pak:attributes pak:packageType=\"development\"/>\
{super_package}\
<pak:applicationComponent/>\
<pak:transport><pak:softwareComponent pak:name=\"{software_component}\"/><pak:transportLayer pak:name=\"{transport_layer}\"/></pak:transport>\
<pak:translation/><pak:useAccesses/><pak:packageInterfaces/><pak:subPackages/>\
</pak:package>",
        description = escape(description),
        name = escape(name),
        responsible = escape(&responsible.to_ascii_uppercase()),
        software_component = escape(software_component),
        transport_layer = escape(transport_layer),
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn package_uri(name: &str) -> String {
    format!(
        "{PACKAGES_PATH}/{}",
        name.to_ascii_lowercase()
            .replace('/', "%2f")
            .replace('$', "%24")
    )
}

fn is_local(name: &str) -> bool {
    name.starts_with('$')
}

fn validate_package_name(name: &str) -> Result<String, PackageLifecycleError> {
    let trimmed = name.trim().to_ascii_uppercase();
    let body = trimmed.strip_prefix('$').unwrap_or(&trimmed);
    let valid = !body.is_empty()
        && trimmed.len() <= MAX_PACKAGE_NAME
        && body
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '/');
    if valid {
        Ok(trimmed)
    } else {
        Err(PackageLifecycleError::InvalidName(name.to_owned()))
    }
}

/// The same two rules every write passes: the name is the customer's (or
/// local, when the profile allows local objects), and the package is one the
/// profile may edit — here the package being created or deleted is itself the
/// package, so it is checked directly.
fn authorize_package_name(policy: &EditPolicy, name: &str) -> Result<(), PackageLifecycleError> {
    if is_local(name) {
        if !policy.allow_temporary_package {
            return Err(PackageLifecycleError::LocalPackagesOff {
                name: name.to_owned(),
            });
        }
        return Ok(());
    }
    if !policy
        .customer_namespaces
        .iter()
        .any(|pattern| glob_matches(pattern, name))
    {
        return Err(PackageLifecycleError::OutsideNamespace {
            name: name.to_owned(),
            patterns: policy.customer_namespaces.clone(),
        });
    }
    authorize_known_package(policy, name, name)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_package_names() {
        assert_eq!(validate_package_name(" zfrt_test ").unwrap(), "ZFRT_TEST");
        assert_eq!(validate_package_name("$frt_local").unwrap(), "$FRT_LOCAL");
        for bad in ["", "$", "Z-BAD", &"Z".repeat(31)] {
            assert!(validate_package_name(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn a_local_package_uri_escapes_the_dollar() {
        assert_eq!(
            package_uri("$FRT_LOCAL"),
            "/sap/bc/adt/packages/%24frt_local"
        );
    }

    #[test]
    fn local_packages_follow_the_profile_and_others_the_namespace() {
        let mut policy = EditPolicy::namespaces_only(&["Z*"]);
        assert!(authorize_package_name(&policy, "$FRT_LOCAL").is_ok());
        assert!(authorize_package_name(&policy, "ZFRT").is_ok());
        assert_eq!(
            authorize_package_name(&policy, "SAPFOO")
                .unwrap_err()
                .code(),
            "object_outside_customer_namespaces"
        );
        policy.allow_temporary_package = false;
        assert_eq!(
            authorize_package_name(&policy, "$FRT_LOCAL")
                .unwrap_err()
                .code(),
            "package_local_not_allowed"
        );
        policy.edit_packages = Some(vec!["ZPROJ*".to_owned()]);
        assert_eq!(
            authorize_package_name(&policy, "ZOTHER")
                .unwrap_err()
                .code(),
            "object_outside_edit_packages"
        );
    }

    #[test]
    fn the_body_names_the_parent_and_the_transport_attributes() {
        let body = creation_body("ZFRT_SUB", "A & B", "trussell", Some("ZFRT"), "HOME", "SAP");
        assert!(
            body.contains(r#"<pak:superPackage adtcore:name="ZFRT"/>"#),
            "{body}"
        );
        assert!(body.contains(r#"adtcore:description="A &amp; B""#));
        assert!(body.contains(r#"adtcore:responsible="TRUSSELL""#));
        assert!(body.contains(r#"<pak:softwareComponent pak:name="HOME"/>"#));
        assert!(body.contains(r#"<pak:transportLayer pak:name="SAP"/>"#));
        assert!(creation_body("$X", "d", "u", None, "LOCAL", "").contains("<pak:superPackage/>"));
    }

    #[test]
    fn reads_attributes_from_a_package_document() {
        let xml = r#"<pak:package adtcore:name="ZFRT_SUB" adtcore:description="Sub" xmlns:pak="http://www.sap.com/adt/packages" xmlns:adtcore="http://www.sap.com/adt/core"><pak:superPackage adtcore:name="ZFRT"/><pak:transport><pak:softwareComponent pak:name="HOME"/><pak:transportLayer pak:name="SAP"/></pak:transport></pak:package>"#;
        let attributes = parse_package_attributes("ZFRT_SUB", xml);
        assert_eq!(attributes.parent.as_deref(), Some("ZFRT"));
        assert_eq!(attributes.software_component.as_deref(), Some("HOME"));
        assert_eq!(attributes.transport_layer.as_deref(), Some("SAP"));
        assert_eq!(attributes.description.as_deref(), Some("Sub"));
    }
}

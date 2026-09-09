//! Activating a metadata object.
//!
//! Without this, everything Fractal does to a `DTEL`, `DOMA`, `TTYP`, `MSAG` or
//! `SRVB` stays inactive and never takes effect: the family can be created and
//! written but never published.
//!
//! The activation request itself is shared with the source family and lives in
//! [`super::activation_request`] — the endpoint, the body and the response are
//! identical. Two things differ:
//!
//! - **There is no pre-check.** A source activation syntax-checks the inactive
//!   version first; a metadata object has no source to check, and SAP runs the
//!   dictionary checks during activation anyway.
//! - **Success is proved from two signals, not one.** The object must read back
//!   as `adtcore:version="active"` *and* have left the caller's inactive list.
//!   The version attribute alone is not enough: an object that already had an
//!   active version still reports `active` after a failed activation, because
//!   the read returns the older document. Only the object leaving the inactive
//!   list distinguishes "I just activated this" from "it was already active".
//!   The source path infers the same fact from the probe plus a content
//!   comparison, because ABAP text carries no layer marker at all.
//!
//! That second point is load-bearing rather than decorative. SAP has been
//! observed answering `activationExecuted="true"` for an activation that
//! plainly failed, so the response cannot be trusted and only the object's own
//! post-state settles it.

use thiserror::Error;

use super::{
    activation_request::{
        AdtActivationMessage, first_message_hint, parse_activation_response, post_activation,
    },
    adt_message_severity::AdtMessageSeverity,
    adt_object_identity::AdtObjectIdentity,
    adt_response::{AdtResponseParseError, parse_adt_document},
    client::{SapClient, SapClientError},
    edit_session::{AdtEditSessionError, attach_adt_object_to_transport},
    editable_source::{AdtEditTargetValidationError, canonicalize_transport_request},
    find_non_empty_attribute,
    metadata_document::strip_navigation_links,
    metadata_object::{MetadataAdtObjectType, metadata_object_identity},
    package_authorization::{PackageAuthorizationError, authorize_object_package},
    source_check::{AdtInactiveSourceProbeError, probe_inactive_adt_source},
};
use crate::config::EditPolicy;
use crate::journal::JournalError;
use crate::journal::entry::{EntryObject, JournalEntry, JournalOperation};
use crate::journal::recorder::Journal;
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::sap::object_family::AdtObjectFamily;
use crate::suggested_command;

/// The `adtcore:version` a document reports once it has been activated.
pub(super) const ACTIVE_VERSION: &str = "active";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataObjectActivationRequest {
    pub object_type: MetadataAdtObjectType,
    pub name: String,
    pub transport: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetadataObjectActivationResult {
    pub identity: AdtObjectIdentity,
    pub transport: Option<String>,
    /// The document SAP holds now, read back rather than assumed, and
    /// stripped of its `atom:link` decoration — see
    /// [`super::metadata_document`]. That is the form the journal stores and
    /// the form an undo compares against, so a caller hashing this reads the
    /// same number `journal show` prints for the same operation.
    pub active_xml: String,
    /// Reported, never trusted. See the module docs.
    pub sap_reported_activation_executed: Option<bool>,
    pub activation_response_parsed: bool,
    pub activation_messages: Vec<AdtActivationMessage>,
}

#[derive(Debug, Error)]
pub enum MetadataObjectActivationError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    /// The journal could not be written. Fatal before the activation, because
    /// an operation with no before-image has no recovery at all.
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
    #[error("could not establish whether {name} has an inactive version: {source}")]
    InactiveVersionProbe {
        name: String,
        #[source]
        source: AdtInactiveSourceProbeError,
    },
    #[error("{name} has no inactive version to activate")]
    NoInactiveVersion {
        object_type: &'static str,
        name: String,
    },
    #[error(transparent)]
    TransportAttachment(AdtEditSessionError),
    #[error("the ADT activation request failed: {source}")]
    ActivationRequest {
        name: String,
        #[source]
        source: SapClientError,
    },
    /// SAP answered, and the object is still not active.
    #[error("{name} was not activated")]
    ActivationRefused {
        object_type: &'static str,
        name: String,
        sap_reported_activation_executed: Option<bool>,
        messages: Vec<AdtActivationMessage>,
    },
    #[error("could not establish whether {name} is still pending after activating it: {source}")]
    PostActivationProbe {
        name: String,
        #[source]
        source: AdtInactiveSourceProbeError,
    },
    #[error("could not read {name} back after activating it: {source}")]
    Verification {
        object_type: &'static str,
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP returned a document that could not be parsed: {0}")]
    ResponseInvalid(#[from] AdtResponseParseError),
}

impl MetadataObjectActivationError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::ActivationRequest { source, .. } | Self::Verification { source, .. } => {
                Some(source)
            }
            _ => None,
        }
    }
}

impl ReportableError for MetadataObjectActivationError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Journal(error) => error.code(),
            Self::PackageNotAllowed(error) => error.code(),
            Self::InactiveVersionProbe { .. } => "edit_activate_inactive_probe_failed",
            Self::PostActivationProbe { .. } => "edit_activate_post_probe_failed",
            Self::NoInactiveVersion { .. } => "edit_activate_no_inactive_version",
            Self::TransportAttachment(error) => error.code(),
            Self::ActivationRequest { .. } => "edit_activate_request_failed",
            Self::ActivationRefused { .. } => "edit_activate_refused",
            Self::Verification { .. } => "edit_activate_verification_failed",
            Self::ResponseInvalid(error) => error.code(),
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Validation(error) => return error.hint(),
            Self::Journal(error) => return error.hint(),
            Self::PackageNotAllowed(error) => return error.hint(),
            Self::TransportAttachment(error) => return error.hint(),
            Self::InactiveVersionProbe { .. } => {
                "SAP could not say whether this object has pending changes, so activating it was not attempted."
                    .to_owned()
            }
            Self::PostActivationProbe { .. } => {
                "The activation was sent, but SAP could not say whether the object is still pending, so whether it activated is unknown. Read it before trying again."
                    .to_owned()
            }
            // The probe answers for the calling user only, so say whose pending
            // changes are missing rather than implying the object has none.
            Self::NoInactiveVersion { .. } => {
                "You have no pending changes on this object, so there is nothing to activate. Write one with `fractal edit set-xml` first."
                    .to_owned()
            }
            Self::ActivationRequest { source, .. } => format!(
                "The object was not activated. {}",
                source.hint().unwrap_or_default()
            ),
            Self::ActivationRefused { messages, .. } => first_message_hint(
                messages
                    .iter()
                    .filter(|message| message.severity == AdtMessageSeverity::Error)
                    .map(|message| message.text.as_str()),
                "The object is still inactive. Fix what SAP reported and activate again.",
            ),
            Self::Verification { .. } => {
                "SAP answered the activation, but the object could not be read back, so whether it activated is unknown. Read it before trying again."
                    .to_owned()
            }
            Self::ResponseInvalid(error) => return error.hint(),
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            // The type has to come from the error: a hardcoded one would send
            // the caller looking for a data element when they activated a
            // domain.
            Self::NoInactiveVersion { object_type, name }
            | Self::ActivationRefused {
                object_type, name, ..
            }
            | Self::Verification {
                object_type, name, ..
            } => Some(suggested_command::object_search(object_type, name)),
            Self::PackageNotAllowed(error) => error.suggested_command(),
            Self::ActivationRequest { source, .. } => source.suggested_command(),
            _ => None,
        }
    }
}

/// Activates one metadata object and proves it is active.
///
/// # Errors
///
/// Returns [`MetadataObjectActivationError`] for validation, a package outside
/// the profile's allowlist, an object with no pending changes, a refused
/// activation, or an object that is still not active afterwards.
pub async fn activate_metadata_object(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &MetadataObjectActivationRequest,
    journal: Option<&Journal>,
) -> Result<MetadataObjectActivationResult, MetadataObjectActivationError> {
    let identity = metadata_object_identity(request.object_type, &request.name, policy)?;
    let transport = canonicalize_transport_request(request.transport.as_deref())
        .map_err(AdtEditTargetValidationError::from)?;

    // Before anything is published, and before the CSRF session: a refusal
    // should cost nothing.
    authorize_object_package(sap, policy, &identity.name, &identity.object_uri).await?;

    let inactive_exists = probe_inactive_adt_source(sap, &identity.object_uri)
        .await
        .map_err(
            |source| MetadataObjectActivationError::InactiveVersionProbe {
                name: identity.name.clone(),
                source,
            },
        )?;
    if !inactive_exists {
        return Err(MetadataObjectActivationError::NoInactiveVersion {
            object_type: request.object_type.as_str(),
            name: identity.name.clone(),
        });
    }

    sap.establish_csrf_session().await.map_err(|source| {
        MetadataObjectActivationError::ActivationRequest {
            name: identity.name.clone(),
            source,
        }
    })?;

    if let Some(transport) = &transport {
        attach_adt_object_to_transport(sap, &identity.object_uri, transport)
            .await
            .map_err(MetadataObjectActivationError::TransportAttachment)?;
    }

    let entry = match journal {
        Some(journal) => Some(
            journal
                .begin(
                    journal_object(request.object_type, &identity),
                    JournalOperation::Activate,
                    transport.clone(),
                    // Only read these when they will be recorded.
                    read_active_document_if_any(sap, &identity).await,
                    read_document(sap, &identity, "inactive").await,
                )
                .map_err(MetadataObjectActivationError::Journal)?,
        ),
        None => None,
    };

    let response = match post_activation(sap, &identity.object_uri, &identity.name).await {
        Ok(response) => response,
        Err(source) => {
            resolve_entry(journal, entry, Outcome::Failed)?;
            return Err(MetadataObjectActivationError::ActivationRequest {
                name: identity.name,
                source,
            });
        }
    };

    // Advisory only. Kept until the post-state says whether it matters.
    let parsed_response = parse_activation_response(&response);

    // Two signals, both required. Reading `version="active"` proves only that
    // *an* active version exists, which is also true when the activation failed
    // and an older one is still in place; the object leaving the caller's
    // inactive list is what proves this activation published something.
    let (active_xml, inactive_after) = tokio::join!(
        read_active_metadata_object(sap, request.object_type, &identity),
        probe_inactive_adt_source(sap, &identity.object_uri),
    );
    let active_xml = match active_xml {
        Ok(active_xml) => active_xml,
        Err(error) => {
            // SAP accepted it and the post-state could not be established.
            resolve_entry(journal, entry, Outcome::Unverified)?;
            return Err(error);
        }
    };
    let still_pending = match inactive_after {
        Ok(still_pending) => still_pending,
        Err(source) => {
            resolve_entry(journal, entry, Outcome::Unverified)?;
            return Err(MetadataObjectActivationError::PostActivationProbe {
                name: identity.name,
                source,
            });
        }
    };
    if still_pending || document_version(&active_xml)?.as_deref() != Some(ACTIVE_VERSION) {
        resolve_entry(journal, entry, Outcome::Failed)?;
        let (executed, messages) = parsed_response.map_or((None, Vec::new()), |parsed| {
            (parsed.activation_executed, parsed.messages)
        });
        return Err(MetadataObjectActivationError::ActivationRefused {
            object_type: request.object_type.as_str(),
            name: identity.name,
            sap_reported_activation_executed: executed,
            messages,
        });
    }

    resolve_entry(journal, entry, Outcome::Succeeded(active_xml.clone()))?;

    // The post-state proves success, so a response we could not parse is
    // metadata rather than a failure.
    let (activation_response_parsed, parsed) = parsed_response.map_or_else(
        |_| {
            (
                false,
                super::activation_request::ParsedActivationResponse::default(),
            )
        },
        |parsed| (true, parsed),
    );
    Ok(MetadataObjectActivationResult {
        identity,
        transport,
        active_xml,
        sap_reported_activation_executed: parsed.activation_executed,
        activation_response_parsed,
        activation_messages: parsed.messages,
    })
}

/// How an activation ended, from the journal's point of view.
enum Outcome {
    Succeeded(String),
    /// SAP refused it; nothing was published.
    Failed,
    /// SAP accepted it and the result could not be confirmed.
    Unverified,
}

fn resolve_entry(
    journal: Option<&Journal>,
    entry: Option<JournalEntry>,
    outcome: Outcome,
) -> Result<(), MetadataObjectActivationError> {
    let (Some(journal), Some(entry)) = (journal, entry) else {
        return Ok(());
    };
    match outcome {
        Outcome::Succeeded(active) => journal.succeeded(entry, Some(&active), None),
        Outcome::Failed => journal.failed(entry),
        Outcome::Unverified => journal.unverified(entry, None),
    }
    .map(|_| ())
    .map_err(MetadataObjectActivationError::Journal)
}

fn journal_object(object_type: MetadataAdtObjectType, identity: &AdtObjectIdentity) -> EntryObject {
    EntryObject {
        object_type: AdtObjectFamily::Metadata(object_type),
        name: identity.name.clone(),
        uri: identity.object_uri.clone(),
        source_part: None,
    }
}

/// The active document, or `None` when the object has never been activated.
///
/// `?version=active` serves the pending document when there is no active
/// version, so the response has to be parsed to figure out which it is, a document that
/// declares itself `new` is not a previous active version, and journal it as
/// one would have an undo restore something that was never active.
async fn read_active_document_if_any(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
) -> Option<String> {
    let document = read_document(sap, identity, ACTIVE_VERSION).await?;
    (document_version(&document).ok().flatten().as_deref() == Some(ACTIVE_VERSION))
        .then_some(document)
}

/// One version of the document, canonical, or `None` when there is none to
/// read.
async fn read_document(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
    version: &str,
) -> Option<String> {
    sap.get_text_with_query(&identity.object_uri, &[("version", version)])
        .await
        .ok()
        .map(|xml| strip_navigation_links(&xml))
}

/// Reads the **active** document, canonical.
///
/// The explicit selector matters: a plain GET serves the *inactive* document
/// whenever one exists, so verifying an activation without it would read the
/// pending edit and could never tell the two apart.
async fn read_active_metadata_object(
    sap: &SapClient,
    object_type: MetadataAdtObjectType,
    identity: &AdtObjectIdentity,
) -> Result<String, MetadataObjectActivationError> {
    sap.get_text_with_query(&identity.object_uri, &[("version", ACTIVE_VERSION)])
        .await
        .map(|xml| strip_navigation_links(&xml))
        .map_err(|source| MetadataObjectActivationError::Verification {
            object_type: object_type.as_str(),
            name: identity.name.clone(),
            source,
        })
}

/// The layer a document says it belongs to: `new`, `inactive` or `active`.
pub(super) fn document_version(xml: &str) -> Result<Option<String>, AdtResponseParseError> {
    let document = parse_adt_document(xml)?;
    Ok(find_non_empty_attribute(document.root_element(), "version"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(version: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core"
    adtcore:name="ZSAMPLE_DE" adtcore:type="DTEL/DE" adtcore:version="{version}"/>"#
        )
    }

    #[test]
    fn reads_the_layer_a_document_declares() {
        for version in ["active", "inactive", "new"] {
            assert_eq!(
                document_version(&document(version)).unwrap().as_deref(),
                Some(version)
            );
        }
    }

    #[test]
    fn a_document_without_a_version_is_not_treated_as_active() {
        let xml =
            r#"<blue:wbobj xmlns:blue="urn:b" xmlns:adtcore="urn:a" adtcore:name="ZSAMPLE_DE"/>"#;
        assert_eq!(document_version(xml).unwrap(), None);
    }

    #[test]
    fn malformed_metadata_is_a_parse_error() {
        assert!(document_version("<not-closed").is_err());
    }

    #[test]
    fn a_refusal_summarizes_only_the_errors() {
        let error = MetadataObjectActivationError::ActivationRefused {
            object_type: "DTEL",
            name: "ZSAMPLE_DE".to_owned(),
            // SAP said it executed. It did not.
            sap_reported_activation_executed: Some(true),
            messages: vec![
                AdtActivationMessage {
                    severity: AdtMessageSeverity::Warning,
                    text: "Something cosmetic".to_owned(),
                    line: None,
                    object_description: None,
                },
                AdtActivationMessage {
                    severity: AdtMessageSeverity::Error,
                    text: "No active domain ZSAMPLE_DOM available".to_owned(),
                    line: None,
                    object_description: None,
                },
            ],
        };

        assert_eq!(error.code(), "edit_activate_refused");
        let hint = error.hint().expect("has a hint");
        assert!(hint.contains("No active domain"), "{hint}");
        assert!(!hint.contains("cosmetic"), "{hint}");
    }

    #[test]
    fn having_no_pending_changes_says_whose() {
        let error = MetadataObjectActivationError::NoInactiveVersion {
            object_type: "DTEL",
            name: "ZSAMPLE_DE".to_owned(),
        };
        // The probe answers for the calling user only.
        assert!(
            error
                .hint()
                .unwrap()
                .starts_with("You have no pending changes")
        );
    }
}

use super::package_authorization::{PackageAuthorizationError, authorize_object_package};
use crate::config::EditPolicy;
use crate::journal::JournalError;
use crate::journal::entry::{EntryObject, JournalOperation};
use crate::journal::recorder::{Journal, Resolution, resolve};
use crate::sap::object_family::AdtObjectFamily;
use thiserror::Error;

use crate::suggested_command;

use super::{
    activation_request::{
        AdtActivationMessage, ParsedActivationResponse, first_message_hint,
        parse_activation_response, post_activation,
    },
    client::{SapClient, SapClientError},
    edit_session::{AdtEditSessionError, attach_adt_object_to_transport},
    editable_source::{
        AdtEditTargetValidationError, AdtSourceReadError, AdtSourceReadResult, AdtSourceSnapshot,
        AdtSourceVersion, EditableAdtObjectType, EditableAdtSourceIdentity, ValidatedAdtEditTarget,
        read_adt_source_for_edit, validate_adt_edit_target,
    },
    source_check::{
        AdtInactiveSourceProbeError, AdtSourceCheckError, AdtSourceCheckMessage,
        AdtSourceCheckResult, check_adt_source_by_identity, probe_inactive_adt_source,
    },
};
use crate::reportable_error::{ReportableError, sap_http_status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationRequest {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub transport: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationResult {
    /// The activation landed and its journal entry could not be completed, so
    /// this names the entry left at `pending`. Reported rather than failed: the
    /// same shape, and the same reason, as `still_locked`.
    pub journal_entry_incomplete: Option<String>,
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub precheck: AdtSourceCheckResult,
    pub inactive: AdtSourceSnapshot,
    pub active: AdtSourceSnapshot,
    pub sap_reported_activation_executed: Option<bool>,
    pub activation_response_parsed: bool,
    pub activation_messages: Vec<AdtActivationMessage>,
}

#[derive(Debug, Error)]
pub enum AdtSourceActivationError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    /// The journal could not be written. Fatal before the activation, because
    /// an operation with no before-image has no recovery at all.
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
    #[error("could not determine whether the object has inactive source: {0}")]
    InactiveVersionProbe(#[source] AdtInactiveSourceProbeError),
    #[error("{} object '{}' has no inactive source to activate", identity.object_type.as_str(), identity.name)]
    NoInactiveVersion {
        identity: Box<EditableAdtSourceIdentity>,
    },
    #[error("could not read the inactive source before activation: {0}")]
    InactiveSourceRead(#[source] AdtSourceReadError),
    #[error("the inactive-source precheck could not run: {0}")]
    Precheck(#[source] AdtSourceCheckError),
    #[error("the inactive source has {errors} syntax error(s), so activation was not attempted")]
    PrecheckRejected {
        identity: Box<EditableAdtSourceIdentity>,
        errors: usize,
        warnings: usize,
        messages: Vec<AdtSourceCheckMessage>,
    },
    #[error("could not attach the object to the requested transport before activation: {0}")]
    TransportAttachment(#[source] AdtEditSessionError),
    #[error("the ADT activation request failed: {source}")]
    ActivationRequest {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: SapClientError,
    },
    #[error("SAP returned malformed activation XML and the inactive version still exists: {0}")]
    ActivationResponseInvalid(#[source] roxmltree::Error),
    #[error("SAP did not remove the inactive version, so activation was not completed")]
    ActivationRefused {
        sap_reported_activation_executed: Option<bool>,
        messages: Vec<AdtActivationMessage>,
    },
    #[error(
        "SAP accepted the activation request, but the active source could not be read: {source}"
    )]
    ActiveSourceRead {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: AdtSourceReadError,
    },
    #[error(
        "SAP accepted the activation request, but its inactive-source state could not be verified: {source}"
    )]
    PostActivationProbe {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: AdtInactiveSourceProbeError,
    },
    #[error(
        "activation removed the inactive version, but active source SHA-256 {active_sha256} does not match the pre-activation inactive source SHA-256 {inactive_sha256}"
    )]
    VerificationMismatch {
        identity: Box<EditableAdtSourceIdentity>,
        inactive_sha256: String,
        active_sha256: String,
    },
}

impl AdtSourceActivationError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Journal(_) => None,
            Self::PackageNotAllowed(error) => error.sap_error(),
            Self::InactiveSourceRead(error) | Self::ActiveSourceRead { source: error, .. } => {
                error.sap_error()
            }
            Self::InactiveVersionProbe(error) | Self::PostActivationProbe { source: error, .. } => {
                error.sap_error()
            }
            Self::ActivationRequest { source, .. } => Some(source),
            Self::Precheck(error) => error.sap_error(),
            Self::TransportAttachment(error) => error.sap_error(),
            Self::Validation(_)
            | Self::NoInactiveVersion { .. }
            | Self::PrecheckRejected { .. }
            | Self::ActivationResponseInvalid(_)
            | Self::ActivationRefused { .. }
            | Self::VerificationMismatch { .. } => None,
        }
    }
}

impl ReportableError for AdtSourceActivationError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Journal(error) => error.code(),
            Self::PackageNotAllowed(error) => error.code(),
            Self::InactiveVersionProbe(_) => "edit_activation_inactive_probe_failed",
            Self::NoInactiveVersion { .. } => "edit_activation_no_inactive_source",
            Self::InactiveSourceRead(_) => "edit_activation_inactive_read_failed",
            Self::Precheck(_) => "edit_activation_precheck_failed",
            Self::PrecheckRejected { .. } => "edit_activation_precheck_rejected",
            Self::TransportAttachment(_) => "edit_activation_transport_failed",
            Self::ActivationRequest { .. } => "edit_activation_request_failed",
            Self::ActivationResponseInvalid(_) => "edit_activation_response_invalid",
            Self::ActivationRefused { .. } => "edit_activation_refused",
            Self::ActiveSourceRead { .. } => "edit_activation_active_read_failed",
            Self::PostActivationProbe { .. } => "edit_activation_verification_probe_failed",
            Self::VerificationMismatch { .. } => "edit_activation_verification_mismatch",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Validation(AdtEditTargetValidationError::InvalidTransport(_)) => {
                "Use a parent transport request containing 1-20 ASCII letters or digits, for example AB1K900575."
                    .to_owned()
            }
            Self::Validation(error) => error.hint()?,
            Self::Journal(error) => error.hint()?,
            Self::PackageNotAllowed(error) => error.hint()?,
            Self::InactiveVersionProbe(error) => error.hint()?,
            Self::NoInactiveVersion { identity } => format!(
                "Create or save an inactive change first. Run `{}` to inspect what is already live.",
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Active.as_str())
            ),
            Self::InactiveSourceRead(error) => error.hint()?,
            Self::Precheck(error) => error.hint()?,
            Self::PrecheckRejected {
                identity, messages, ..
            } => first_message_hint(
                messages.iter().map(|message| message.text.as_str()),
                &format!(
                    "Run `{}` to inspect every syntax finding.",
                    suggested_command::edit_check(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Inactive.as_str())
                ),
            ),
            Self::TransportAttachment(error) => error.hint()?,
            Self::ActivationRequest { identity, .. } => format!(
                "The request may have reached SAP. Re-run `{}` before retrying activation.",
                suggested_command::edit_check(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Inactive.as_str())
            ),
            Self::ActivationResponseInvalid(_) => {
                "SAP did not clear the inactive version, and its response could not be interpreted; inspect the object in ADT before retrying."
                    .to_owned()
            }
            Self::ActivationRefused { messages, .. } => first_message_hint(
                messages.iter().map(|message| message.text.as_str()),
                "Review SAP's activation messages and the object's transport before retrying.",
            ),
            Self::ActiveSourceRead { identity, .. } | Self::PostActivationProbe { identity, .. } => {
                format!(
                    "Activation may have succeeded. Read both active source and the inactive-object state before retrying. Run `{}`.",
                    suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Active.as_str())
                )
            }
            Self::VerificationMismatch { identity, .. } => format!(
                "Do not retry blindly: SAP activated different source than the version Fractal prechecked. Review the object history, starting with `{}`.",
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Active.as_str())
            ),
        })
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// Transport attachment failures return `None`: their remedy is to retry
    /// the activation with a different request, which is a mutation.
    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Journal(_) => None,
            Self::PackageNotAllowed(error) => error.suggested_command(),
            Self::NoInactiveVersion { identity }
            | Self::ActiveSourceRead { identity, .. }
            | Self::PostActivationProbe { identity, .. }
            | Self::VerificationMismatch { identity, .. } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                AdtSourceVersion::Active.as_str(),
            )),
            Self::PrecheckRejected { identity, .. } | Self::ActivationRequest { identity, .. } => {
                Some(suggested_command::edit_check(
                    identity.object_type.as_str(),
                    &identity.name,
                    AdtSourceVersion::Inactive.as_str(),
                ))
            }
            Self::InactiveSourceRead(error) => error.suggested_command(),
            Self::Validation(_)
            | Self::InactiveVersionProbe(_)
            | Self::Precheck(_)
            | Self::TransportAttachment(_)
            | Self::ActivationResponseInvalid(_)
            | Self::ActivationRefused { .. } => None,
        }
    }
}

/// Activates one confirmed inactive source version through native ADT.
///
/// The workflow refuses to activate when no inactive version exists or when
/// the standalone syntax check finds errors. It snapshots the inactive source,
/// requests ADT activation with preaudit enabled, and then verifies both that
/// the inactive version disappeared and that the active source has the same
/// SHA-256 as the source inspected before activation.
///
/// When a transport is supplied, a lock/unlock cycle first attaches the object
/// to that parent request using the same transport behavior as source patching.
/// The activation request itself does not accept a transport parameter.
///
/// # Errors
///
/// Returns [`AdtSourceActivationError`] when validation, precheck, transport
/// attachment, activation, or post-activation verification fails.
pub async fn activate_adt_source(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &AdtSourceActivationRequest,
    journal: Option<&Journal>,
) -> Result<AdtSourceActivationResult, AdtSourceActivationError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        policy,
        request.transport.as_deref(),
    )?;
    authorize_object_package(
        sap,
        policy,
        &target.identity.name,
        &target.identity.object_uri,
    )
    .await?;
    activate_validated_adt_source(sap, target, journal).await
}

pub(super) async fn activate_validated_adt_source(
    sap: &mut SapClient,
    target: ValidatedAdtEditTarget,
    journal: Option<&Journal>,
) -> Result<AdtSourceActivationResult, AdtSourceActivationError> {
    let identity = target.identity;
    let transport = target.transport;

    let inactive_exists = probe_inactive_adt_source(sap, &identity.object_uri)
        .await
        .map_err(AdtSourceActivationError::InactiveVersionProbe)?;
    if !inactive_exists {
        return Err(AdtSourceActivationError::NoInactiveVersion {
            identity: Box::new(identity),
        });
    }

    sap.establish_csrf_session().await.map_err(|source| {
        AdtSourceActivationError::Precheck(AdtSourceCheckError::Sap {
            identity: Box::new(identity.clone()),
            version: AdtSourceVersion::Inactive,
            source,
        })
    })?;
    let (inactive, precheck) = read_and_precheck_inactive_source(sap, &identity).await?;

    if let Some(transport) = &transport {
        attach_adt_object_to_transport(sap, &identity.object_uri, transport)
            .await
            .map_err(AdtSourceActivationError::TransportAttachment)?;
    }

    let entry = match journal {
        Some(journal) => Some(
            // The one thing an undo of this activation needs, and the one thing
            // nothing read before: what the active version was beforehand.
            // Costs a request, so it is only made when it will be recorded.
            journal
                .begin(
                    journal_object(&identity),
                    JournalOperation::activate(),
                    transport.clone(),
                    read_active_source_if_any(sap, &identity).await,
                    Some(inactive.snapshot.source.clone()),
                )
                .map_err(AdtSourceActivationError::Journal)?,
        ),
        None => None,
    };

    let response = match post_activation(sap, &identity.object_uri, &identity.name).await {
        Ok(response) => response,
        Err(source) => {
            // Nothing was published, so the entry records a refusal.
            // Deliberately ignored: a cleanup failure must not replace the
            // failure that caused it.
            let _ = resolve(journal, entry, Resolution::Failed);
            return Err(AdtSourceActivationError::ActivationRequest {
                identity: Box::new(identity),
                source,
            });
        }
    };

    // SAP's activation response is advisory: activationExecuted="false" has
    // been observed after a successful activation, and malformed XML does not
    // prove failure. Keep the parse result until the inactive-state and active-
    // source checks establish whether a response problem is fatal.
    let parsed_response = parse_activation_response(&response);

    let active_result = match verify_activation_post_state(sap, &identity).await {
        Ok(active_result) => active_result,
        Err(error) => {
            // SAP accepted it and the post-state could not be established.
            // Deliberately ignored: a cleanup failure must not replace the
            // failure that caused it.
            let _ = resolve(journal, entry, Resolution::Unverified(None));
            return Err(error);
        }
    };

    let Some(active_result) = active_result else {
        // Deliberately ignored: a cleanup failure must not replace the cause.
        let _ = resolve(journal, entry, Resolution::Failed);
        return match parsed_response {
            Ok(parsed) => Err(AdtSourceActivationError::ActivationRefused {
                sap_reported_activation_executed: parsed.activation_executed,
                messages: parsed.messages,
            }),
            Err(error) => Err(AdtSourceActivationError::ActivationResponseInvalid(error)),
        };
    };
    let active = match active_result {
        Ok(active) => active,
        Err(source) => {
            // Deliberately ignored: a cleanup failure must not replace the
            // failure that caused it.
            let _ = resolve(journal, entry, Resolution::Unverified(None));
            return Err(AdtSourceActivationError::ActiveSourceRead {
                identity: Box::new(identity),
                source,
            });
        }
    };
    if active.snapshot.sha256 != inactive.snapshot.sha256 {
        // Deliberately ignored: a cleanup failure must not replace the cause.
        let _ = resolve(journal, entry, Resolution::Unverified(None));
        return Err(AdtSourceActivationError::VerificationMismatch {
            identity: Box::new(identity),
            inactive_sha256: inactive.snapshot.sha256,
            active_sha256: active.snapshot.sha256,
        });
    }
    let journal_entry_incomplete = resolve(
        journal,
        entry,
        Resolution::Succeeded(Some(&active.snapshot.source)),
    );

    // Post-state now proves success, so preserve malformed response XML as metadata.
    let (activation_response_parsed, parsed) = parsed_response.map_or_else(
        |_| (false, ParsedActivationResponse::default()),
        |parsed| (true, parsed),
    );
    Ok(AdtSourceActivationResult {
        journal_entry_incomplete,
        identity,
        transport,
        precheck,
        inactive: inactive.snapshot,
        active: active.snapshot,
        sap_reported_activation_executed: parsed.activation_executed,
        activation_response_parsed,
        activation_messages: parsed.messages,
    })
}

/// Snapshots the inactive source and syntax-checks it concurrently.
///
/// Refuses before any mutation when the check reports errors: activation must
/// never be attempted on source that will not compile.
fn journal_object(identity: &EditableAdtSourceIdentity) -> EntryObject {
    EntryObject {
        object_type: AdtObjectFamily::Source(identity.object_type),
        name: identity.name.clone(),
        uri: identity.object_uri.clone(),
        // Fractal reads and writes a class's `main` include only.
        source_part: None,
    }
}

/// The active source, or `None` when the object has never been activated.
///
/// A missing active version is an ordinary state for a newly created object,
/// not a failure, so it is recorded as an absence rather than stopping the
/// activation.
async fn read_active_source_if_any(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
) -> Option<String> {
    read_adt_source_for_edit(
        sap,
        identity.object_type,
        &identity.name,
        AdtSourceVersion::Active,
    )
    .await
    .ok()
    .map(|read| read.snapshot.source)
}

async fn read_and_precheck_inactive_source(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
) -> Result<(AdtSourceReadResult, AdtSourceCheckResult), AdtSourceActivationError> {
    let inactive_read = async {
        read_adt_source_for_edit(
            sap,
            identity.object_type,
            &identity.name,
            AdtSourceVersion::Inactive,
        )
        .await
        .map_err(AdtSourceActivationError::InactiveSourceRead)
    };
    let precheck_run = async {
        let precheck =
            check_adt_source_by_identity(sap, identity, AdtSourceVersion::Inactive, Some(true))
                .await
                .map_err(AdtSourceActivationError::Precheck)?;
        if precheck.clean {
            Ok(precheck)
        } else {
            Err(AdtSourceActivationError::PrecheckRejected {
                identity: Box::new(identity.clone()),
                errors: precheck.errors,
                warnings: precheck.warnings,
                messages: precheck.messages,
            })
        }
    };
    tokio::try_join!(inactive_read, precheck_run)
}

/// Reads the active source and re-probes the inactive worklist together.
///
/// Returns the active-source result only when the inactive version is gone.
/// The probe is decisive: if that version survives, activation did not happen
/// and the active body is irrelevant, so the race returns without waiting for
/// it. If the active read finishes first, its result is held until the probe
/// establishes whether activation occurred.
async fn verify_activation_post_state(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
) -> Result<Option<Result<AdtSourceReadResult, AdtSourceReadError>>, AdtSourceActivationError> {
    let active_read = read_adt_source_for_edit(
        sap,
        identity.object_type,
        &identity.name,
        AdtSourceVersion::Active,
    );
    let inactive_probe = probe_inactive_adt_source(sap, &identity.object_uri);
    tokio::pin!(active_read, inactive_probe);

    Ok(tokio::select! {
        biased;
        active_result = &mut active_read => {
            let inactive_still_exists = inactive_probe
                .await
                .map_err(|source| AdtSourceActivationError::PostActivationProbe {
                    identity: Box::new(identity.clone()),
                    source,
                })?;
            (!inactive_still_exists).then_some(active_result)
        }
        probe_result = &mut inactive_probe => {
            let inactive_still_exists =
                probe_result.map_err(|source| AdtSourceActivationError::PostActivationProbe {
                    identity: Box::new(identity.clone()),
                    source,
                })?;
            if inactive_still_exists {
                None
            } else {
                Some(active_read.await)
            }
        }
    })
}

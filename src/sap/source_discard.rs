use thiserror::Error;

use crate::suggested_command;

use super::{
    client::{SapClient, SapClientError},
    edit_session::{
        AdtEditSessionError, AdtObjectLock, acquire_adt_object_lock,
        read_adt_source_in_stateful_session, release_adt_object_lock, write_adt_source,
    },
    editable_source::{
        AdtEditTargetValidationError, AdtSourceReadError, AdtSourceReadResult, AdtSourceSnapshot,
        AdtSourceVersion, EditableAdtObjectType, EditableAdtSourceIdentity, ValidatedAdtEditTarget,
        validate_adt_edit_target,
    },
    source_activation::{AdtSourceActivationError, activate_validated_adt_source},
    source_check::{AdtInactiveSourceProbeError, probe_inactive_adt_source},
};
use crate::reportable_error::{ReportableError, sap_http_status};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtInactiveSourceDiscardRequest {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub transport: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtInactiveSourceDiscardResult {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub discarded: AdtSourceSnapshot,
    pub active_before: AdtSourceSnapshot,
    pub restored_inactive: AdtSourceSnapshot,
    pub active_after: AdtSourceSnapshot,
    pub activation_response_parsed: bool,
    pub sap_reported_activation_executed: Option<bool>,
}

#[derive(Debug, Error)]
pub enum AdtInactiveSourceDiscardError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error("ADT edit session failed while discarding inactive source: {0}")]
    Session(#[source] AdtEditSessionError),
    #[error("could not determine whether the locked object has inactive source: {0}")]
    InactiveVersionProbe(#[source] AdtInactiveSourceProbeError),
    #[error("{object_type} object '{name}' has no inactive source to discard")]
    NoInactiveVersion {
        object_type: &'static str,
        name: String,
    },
    #[error("could not read the active source while the discard lock was held: {0}")]
    ActiveSourceRead(#[source] AdtSourceReadError),
    #[error("could not read the inactive source while the discard lock was held: {0}")]
    InactiveSourceRead(#[source] AdtSourceReadError),
    #[error("could not re-read the restored inactive source while its lock was held: {0}")]
    RestoredSourceRead(#[source] AdtSourceReadError),
    #[error(
        "SAP stored restored inactive source SHA-256 {restored_sha256}, which does not match active source SHA-256 {active_sha256}"
    )]
    RestoreVerificationMismatch {
        identity: Box<EditableAdtSourceIdentity>,
        active_sha256: String,
        restored_sha256: String,
    },
    #[error("active source was restored over inactive source, but activation failed: {0}")]
    RestoredSourceActivation(#[source] AdtSourceActivationError),
    #[error(
        "discard activation changed active source SHA-256 from {before_sha256} to {after_sha256}"
    )]
    ActiveSourceChanged {
        identity: Box<EditableAdtSourceIdentity>,
        before_sha256: String,
        after_sha256: String,
    },
}

impl AdtInactiveSourceDiscardError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::ActiveSourceRead(error)
            | Self::InactiveSourceRead(error)
            | Self::RestoredSourceRead(error) => error.sap_error(),
            Self::Session(error) => error.sap_error(),
            Self::InactiveVersionProbe(error) => error.sap_error(),
            Self::RestoredSourceActivation(error) => error.sap_error(),
            Self::Validation(_)
            | Self::NoInactiveVersion { .. }
            | Self::RestoreVerificationMismatch { .. }
            | Self::ActiveSourceChanged { .. } => None,
        }
    }
}

impl ReportableError for AdtInactiveSourceDiscardError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Session(
                AdtEditSessionError::LockFailed { .. }
                | AdtEditSessionError::LockHandleMissing { .. },
            ) => "edit_discard_lock_failed",
            Self::Session(AdtEditSessionError::SourceWriteFailed { .. }) => {
                "edit_discard_restore_write_failed"
            }
            Self::Session(AdtEditSessionError::UnlockFailed(_)) => "edit_discard_unlock_failed",
            Self::InactiveVersionProbe(_) => "edit_discard_inactive_probe_failed",
            Self::NoInactiveVersion { .. } => "edit_discard_no_inactive_source",
            Self::ActiveSourceRead(_) => "edit_discard_active_read_failed",
            Self::InactiveSourceRead(_) => "edit_discard_inactive_read_failed",
            Self::RestoredSourceRead(_) => "edit_discard_restore_read_failed",
            Self::RestoreVerificationMismatch { .. } => "edit_discard_restore_mismatch",
            Self::RestoredSourceActivation(_) => "edit_discard_activation_failed",
            Self::ActiveSourceChanged { .. } => "edit_discard_active_source_changed",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::ActiveSourceRead(error)
            | Self::InactiveSourceRead(error)
            | Self::RestoredSourceRead(error) => error.hint()?,
            Self::Validation(AdtEditTargetValidationError::InvalidTransport(_)) => {
                "Use a parent transport request containing 1-20 ASCII letters or digits, for example DE3K900575."
                    .to_owned()
            }
            Self::Validation(error) => error.hint()?,
            Self::Session(AdtEditSessionError::UnlockFailed(_)) => {
                "The inactive source now contains the previous active source, but it was not activated. Release the lock, inspect the inactive version, and activate it to finish the discard."
                    .to_owned()
            }
            Self::Session(error) => error.hint()?,
            Self::InactiveVersionProbe(error) => error.hint()?,
            Self::NoInactiveVersion { .. } => {
                "There are no visible unactivated changes to discard.".to_owned()
            }
            Self::RestoreVerificationMismatch { identity, .. } => format!(
                "The inactive source was overwritten but not activated because SAP did not preserve the active bytes exactly. Inspect both versions before continuing, starting with `{}`.",
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Inactive.as_str())
            ),
            Self::RestoredSourceActivation(error) => format!(
                "The inactive source now contains the previous active source, but activation did not complete. {}",
                error.hint().unwrap_or_default()
            ),
            Self::ActiveSourceChanged { identity, .. } => format!(
                "Do not retry blindly: a discard must preserve active source exactly, so inspect the object history, starting with `{}`.",
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Active.as_str())
            ),
        })
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// A failed unlock after the restore write returns `None` even though its
    /// remedy is known: finishing the discard requires `fractal edit activate`,
    /// a mutation, so that instruction stays in prose where a caller decides.
    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::RestoreVerificationMismatch { identity, .. } => {
                Some(suggested_command::edit_read(
                    identity.object_type.as_str(),
                    &identity.name,
                    AdtSourceVersion::Inactive.as_str(),
                ))
            }
            Self::ActiveSourceChanged { identity, .. } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                AdtSourceVersion::Active.as_str(),
            )),
            Self::ActiveSourceRead(error)
            | Self::InactiveSourceRead(error)
            | Self::RestoredSourceRead(error) => error.suggested_command(),
            Self::RestoredSourceActivation(error) => error.suggested_command(),
            Self::Validation(_)
            | Self::Session(_)
            | Self::InactiveVersionProbe(_)
            | Self::NoInactiveVersion { .. } => None,
        }
    }
}

struct PreparedDiscard {
    inactive_before: AdtSourceReadResult,
    active_before: AdtSourceReadResult,
    restored_inactive: AdtSourceReadResult,
}

/// Discards inactive changes by restoring and activating the current active source.
///
/// ADT's `inactiveobjects?action=delete` operation only removes an object from
/// the current user's inactive-object worklist; it does not delete the stored
/// inactive source. Fractal therefore locks the object, snapshots both source
/// versions, writes the active bytes over the inactive version, verifies that
/// write while still locked, unlocks, and activates the restored source.
///
/// This workflow deliberately requires an active version. New objects that
/// have never been activated cannot be safely discarded as source edits.
///
/// # Errors
///
/// Returns [`AdtInactiveSourceDiscardError`] for validation, lock/session,
/// source restore, activation, verification, or cleanup failures.
pub async fn discard_inactive_adt_source(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &AdtInactiveSourceDiscardRequest,
) -> Result<AdtInactiveSourceDiscardResult, AdtInactiveSourceDiscardError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    let identity = target.identity;
    let transport = target.transport;

    let lock = acquire_adt_object_lock(sap, &identity.object_uri, transport.as_deref())
        .await
        .map_err(AdtInactiveSourceDiscardError::Session)?;
    let preparation = prepare_discard_while_locked(sap, &identity, &lock).await;
    let unlock = release_adt_object_lock(sap, &identity.object_uri, &lock).await;

    let prepared = match preparation {
        Err(primary) => {
            // Preserve the operation failure while still attempting lock cleanup.
            let _ = unlock;
            return Err(primary);
        }
        Ok(prepared) => {
            unlock.map_err(AdtInactiveSourceDiscardError::Session)?;
            prepared
        }
    };

    // The transport was already applied by the restore lock/write cycle. Passing
    // it again would acquire a redundant second transport-qualified lock.
    let activation = activate_validated_adt_source(
        sap,
        ValidatedAdtEditTarget {
            identity: identity.clone(),
            transport: None,
        },
    )
    .await
    .map_err(AdtInactiveSourceDiscardError::RestoredSourceActivation)?;

    if activation.active.sha256 != prepared.active_before.snapshot.sha256 {
        return Err(AdtInactiveSourceDiscardError::ActiveSourceChanged {
            identity: Box::new(identity.clone()),
            before_sha256: prepared.active_before.snapshot.sha256,
            after_sha256: activation.active.sha256,
        });
    }

    Ok(AdtInactiveSourceDiscardResult {
        identity,
        transport,
        discarded: prepared.inactive_before.snapshot,
        active_before: prepared.active_before.snapshot,
        restored_inactive: prepared.restored_inactive.snapshot,
        active_after: activation.active,
        activation_response_parsed: activation.activation_response_parsed,
        sap_reported_activation_executed: activation.sap_reported_activation_executed,
    })
}

async fn prepare_discard_while_locked(
    sap: &mut SapClient,
    identity: &EditableAdtSourceIdentity,
    lock: &AdtObjectLock,
) -> Result<PreparedDiscard, AdtInactiveSourceDiscardError> {
    let inactive_exists = probe_inactive_adt_source(sap, &identity.object_uri)
        .await
        .map_err(AdtInactiveSourceDiscardError::InactiveVersionProbe)?;
    if !inactive_exists {
        return Err(AdtInactiveSourceDiscardError::NoInactiveVersion {
            object_type: identity.object_type.as_str(),
            name: identity.name.clone(),
        });
    }

    let active_read = read_adt_source_in_stateful_session(sap, identity, AdtSourceVersion::Active);
    let inactive_read =
        read_adt_source_in_stateful_session(sap, identity, AdtSourceVersion::Inactive);
    let (active_before, inactive_before) = tokio::join!(active_read, inactive_read);
    let active_before = active_before.map_err(AdtInactiveSourceDiscardError::ActiveSourceRead)?;
    let inactive_before =
        inactive_before.map_err(AdtInactiveSourceDiscardError::InactiveSourceRead)?;

    write_adt_source(sap, identity, lock, &active_before.snapshot.source)
        .await
        .map_err(AdtInactiveSourceDiscardError::Session)?;
    let restored_inactive =
        read_adt_source_in_stateful_session(sap, identity, AdtSourceVersion::Inactive)
            .await
            .map_err(AdtInactiveSourceDiscardError::RestoredSourceRead)?;

    if restored_inactive.snapshot.sha256 != active_before.snapshot.sha256 {
        return Err(AdtInactiveSourceDiscardError::RestoreVerificationMismatch {
            identity: Box::new(identity.clone()),
            active_sha256: active_before.snapshot.sha256,
            restored_sha256: restored_inactive.snapshot.sha256,
        });
    }

    Ok(PreparedDiscard {
        inactive_before,
        active_before,
        restored_inactive,
    })
}

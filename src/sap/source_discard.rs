use super::package_authorization::{PackageAuthorizationError, authorize_object_package};
use crate::config::EditPolicy;
use thiserror::Error;

use crate::suggested_command;

use super::{
    adt_version::AdtVersion,
    class_include::ClassInclude,
    client::{SapClient, SapClientError},
    edit_session::{
        AdtEditSessionError, AdtObjectLock, acquire_adt_object_lock,
        read_adt_source_in_stateful_session, release_adt_object_lock, write_adt_source,
    },
    editable_source::{
        AdtEditTargetValidationError, AdtSourceReadError, AdtSourceReadResult, AdtSourceSnapshot,
        EditableAdtObjectType, EditableAdtSourceIdentity, ValidatedAdtEditTarget,
        validate_adt_edit_target,
    },
    source_activation::{
        AdtSourceActivationError, AdtSourceActivationResult, activate_validated_adt_source,
    },
    worklist::{AdtInactiveSourceProbeError, read_worklist},
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::source_change::same_except_trailing_whitespace;

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
    /// Every stream whose pending work was discarded: the object's one, or
    /// each include of a class the caller had staged.
    pub streams: Vec<DiscardedStream>,
    pub activation_response_parsed: bool,
    pub sap_reported_activation_executed: Option<bool>,
    /// The discard landed, but its lock could not be released.
    ///
    /// Reported as success with a caveat rather than as a failure: the work is
    /// done, and saying otherwise would invite a retry that then fails on the
    /// stuck lock. The caller still has to clear it, because it blocks the next
    /// edit of this object.
    pub still_locked: bool,
}

/// One stream a discard put back to its active version.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscardedStream {
    /// The class include, or `None` for an object with one stream.
    pub include: Option<ClassInclude>,
    pub discarded: AdtSourceSnapshot,
    pub active_before: AdtSourceSnapshot,
    pub restored_inactive: AdtSourceSnapshot,
    pub active_after: AdtSourceSnapshot,
}

#[derive(Debug, Error)]
pub enum AdtInactiveSourceDiscardError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
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
    /// A class include was restored, but the activation did not report
    /// publishing it, so whether its pending work is gone cannot be shown.
    #[error(
        "{} {}'s {} include was restored, but the activation did not publish it",
        identity.object_type.as_str(),
        identity.name,
        identity.include.map_or("main", ClassInclude::as_str)
    )]
    RestoredStreamNotPublished {
        identity: Box<EditableAdtSourceIdentity>,
    },
    /// The operation failed and its lock could not be released. Wraps the
    /// cause, so the reported code, status, and message are unchanged.
    #[error(transparent)]
    AbandonedLock(Box<Self>),
}

impl AdtInactiveSourceDiscardError {
    #[must_use]
    pub fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::PackageNotAllowed(error) => error.sap_error(),
            Self::AbandonedLock(primary) => primary.sap_error(),
            Self::ActiveSourceRead(error)
            | Self::InactiveSourceRead(error)
            | Self::RestoredSourceRead(error) => error.sap_error(),
            Self::Session(error) => error.sap_error(),
            Self::InactiveVersionProbe(error) => error.sap_error(),
            Self::RestoredSourceActivation(error) => error.sap_error(),
            Self::Validation(_)
            | Self::NoInactiveVersion { .. }
            | Self::RestoreVerificationMismatch { .. }
            | Self::ActiveSourceChanged { .. }
            | Self::RestoredStreamNotPublished { .. } => None,
        }
    }
}

impl ReportableError for AdtInactiveSourceDiscardError {
    fn code(&self) -> &'static str {
        match self {
            Self::AbandonedLock(primary) => primary.code(),
            Self::Validation(error) => error.code(),
            Self::PackageNotAllowed(error) => error.code(),
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
            Self::RestoredStreamNotPublished { .. } => "edit_discard_include_not_published",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::AbandonedLock(primary) => format!(
                "{} Releasing its lock also failed, so the object is still locked: clear the lock before retrying, or the next attempt will fail on the lock rather than the original cause.",
                primary.hint().unwrap_or_default()
            ),
            Self::ActiveSourceRead(error)
            | Self::InactiveSourceRead(error)
            | Self::RestoredSourceRead(error) => error.hint()?,
            Self::Validation(AdtEditTargetValidationError::InvalidTransport(_)) => {
                "Use a parent transport request containing 1-20 ASCII letters or digits, for example AB1K900575."
                    .to_owned()
            }
            Self::Validation(error) => error.hint()?,
            Self::PackageNotAllowed(error) => error.hint()?,
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
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtVersion::Inactive.as_str())
            ),
            Self::RestoredSourceActivation(error) => format!(
                "The inactive source now contains the previous active source, but activation did not complete. {}",
                error.hint().unwrap_or_default()
            ),
            Self::RestoredStreamNotPublished { .. } => {
                "The include's active version was written back as pending, then the class was activated without it. The class document shows which includes are still pending.".to_owned()
            }
            Self::ActiveSourceChanged { identity, .. } => format!(
                "Do not retry blindly: a discard must preserve active source exactly, so inspect the object history, starting with `{}`.",
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtVersion::Active.as_str())
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
            Self::PackageNotAllowed(error) => error.suggested_command(),
            Self::AbandonedLock(primary) => primary.suggested_command(),
            Self::RestoreVerificationMismatch { identity, .. } => {
                Some(suggested_command::edit_read(
                    identity.object_type.as_str(),
                    &identity.name,
                    AdtVersion::Inactive.as_str(),
                ))
            }
            Self::ActiveSourceChanged { identity, .. } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                AdtVersion::Active.as_str(),
            )),
            Self::RestoredStreamNotPublished { identity } => {
                Some(suggested_command::object_xml(&identity.object_uri))
            }
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

/// One stream restored under the lock, before the activation.
struct PreparedStream {
    identity: EditableAdtSourceIdentity,
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
/// For a class, every include the caller has staged is restored, not only
/// `main`: the activation publishes the caller's whole worklist, so an include
/// left out would be published rather than discarded. Other users' staged
/// includes are not touched, and the activation leaves them alone.
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
    policy: &EditPolicy,
    request: &AdtInactiveSourceDiscardRequest,
) -> Result<AdtInactiveSourceDiscardResult, AdtInactiveSourceDiscardError> {
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
    let identity = target.identity;
    let transport = target.transport;

    let lock = acquire_adt_object_lock(sap, &identity.object_uri, transport.as_deref())
        .await
        .map_err(AdtInactiveSourceDiscardError::Session)?;
    let preparation = prepare_discard_while_locked(sap, &identity, &lock).await;
    let unlock = release_adt_object_lock(sap, &identity.object_uri, &lock).await;

    let prepared = match preparation {
        Err(primary) => {
            // The failure that stopped the discard stays the reported cause,
            // but a lock that outlived it is state the caller has to act on.
            return Err(if unlock.is_err() {
                AdtInactiveSourceDiscardError::AbandonedLock(Box::new(primary))
            } else {
                primary
            });
        }
        Ok(prepared) => prepared,
    };
    let still_locked = unlock.is_err();

    // The transport was already applied by the restore lock/write cycle. Passing
    // it again would acquire a redundant second transport-qualified lock.
    let activation = activate_validated_adt_source(
        sap,
        ValidatedAdtEditTarget {
            identity: identity.clone(),
            transport: None,
            policy: policy.clone(),
        },
        // A discard restores the active version over the inactive one, so its
        // net effect on the active version is nil. Nothing to journal.
        None,
    )
    .await
    .map_err(AdtInactiveSourceDiscardError::RestoredSourceActivation)?;

    let mut streams = Vec::with_capacity(prepared.len());
    for stream in prepared {
        let Some(active_after) = active_after(&activation, &stream.identity) else {
            return Err(AdtInactiveSourceDiscardError::RestoredStreamNotPublished {
                identity: Box::new(stream.identity),
            });
        };
        if !same_except_trailing_whitespace(
            &active_after.source,
            &stream.active_before.snapshot.source,
        ) {
            return Err(AdtInactiveSourceDiscardError::ActiveSourceChanged {
                identity: Box::new(stream.identity),
                before_sha256: stream.active_before.snapshot.sha256,
                after_sha256: active_after.sha256,
            });
        }
        streams.push(DiscardedStream {
            include: stream.identity.include,
            discarded: stream.inactive_before.snapshot,
            active_before: stream.active_before.snapshot,
            restored_inactive: stream.restored_inactive.snapshot,
            active_after,
        });
    }

    Ok(AdtInactiveSourceDiscardResult {
        identity,
        transport,
        streams,
        activation_response_parsed: activation.activation_response_parsed,
        sap_reported_activation_executed: activation.sap_reported_activation_executed,
        still_locked,
    })
}

/// What one discarded stream is active as now, taken from reads the activation
/// already made rather than read again.
///
/// The activation reads two things back. `active` is always `source/main`: for
/// a single-source object that is the whole object. `published` exists only
/// for a class, with one after-read per include it published. A class stream
/// comes from `published`, and is `None` if the activation did not publish it;
/// falling back to `active` there would compare an include against `main`.
fn active_after(
    activation: &AdtSourceActivationResult,
    stream: &EditableAdtSourceIdentity,
) -> Option<AdtSourceSnapshot> {
    stream.include.map_or_else(
        || Some(activation.active.clone()),
        |include| {
            activation
                .published
                .iter()
                .find(|published| published.include == include)
                .map(|published| published.active.clone())
        },
    )
}

async fn prepare_discard_while_locked(
    sap: &mut SapClient,
    identity: &EditableAdtSourceIdentity,
    lock: &AdtObjectLock,
) -> Result<Vec<PreparedStream>, AdtInactiveSourceDiscardError> {
    let worklist = read_worklist(sap)
        .await
        .map_err(AdtInactiveSourceDiscardError::InactiveVersionProbe)?;
    let inactive_exists = worklist
        .contains(&identity.object_uri)
        .map_err(AdtInactiveSourceDiscardError::InactiveVersionProbe)?;
    if !inactive_exists {
        return Err(AdtInactiveSourceDiscardError::NoInactiveVersion {
            object_type: identity.object_type.as_str(),
            name: identity.name.clone(),
        });
    }

    // For a class, the streams the caller has staged. A worklist that names
    // the class but none of its streams is taken to mean `main`, which is what
    // a discard has always restored.
    let streams = if identity.object_type == EditableAdtObjectType::Class {
        let mut staged = worklist
            .staged_by_caller(&identity.object_uri)
            .map_err(AdtInactiveSourceDiscardError::InactiveVersionProbe)?;
        if staged.is_empty() {
            staged.push(ClassInclude::Main);
        }
        staged
            .into_iter()
            .map(|include| identity.for_include(include))
            .collect()
    } else {
        vec![identity.clone()]
    };

    let mut prepared = Vec::with_capacity(streams.len());
    for stream in streams {
        prepared.push(restore_stream_while_locked(sap, stream, lock).await?);
    }
    Ok(prepared)
}

/// Writes one stream's active source over its inactive one, and proves it.
async fn restore_stream_while_locked(
    sap: &mut SapClient,
    identity: EditableAdtSourceIdentity,
    lock: &AdtObjectLock,
) -> Result<PreparedStream, AdtInactiveSourceDiscardError> {
    let active_read = read_adt_source_in_stateful_session(sap, &identity, AdtVersion::Active);
    let inactive_read = read_adt_source_in_stateful_session(sap, &identity, AdtVersion::Inactive);
    let (active_before, inactive_before) = tokio::join!(active_read, inactive_read);
    let active_before = active_before.map_err(AdtInactiveSourceDiscardError::ActiveSourceRead)?;
    let inactive_before =
        inactive_before.map_err(AdtInactiveSourceDiscardError::InactiveSourceRead)?;

    write_adt_source(sap, &identity, lock, &active_before.snapshot.source)
        .await
        .map_err(AdtInactiveSourceDiscardError::Session)?;
    let restored_inactive =
        read_adt_source_in_stateful_session(sap, &identity, AdtVersion::Inactive)
            .await
            .map_err(AdtInactiveSourceDiscardError::RestoredSourceRead)?;

    // Exact but for trailing whitespace: writing a class include's generated
    // template back has been seen to drop its trailing line break. A
    // line-ending change still refuses.
    if !same_except_trailing_whitespace(
        &restored_inactive.snapshot.source,
        &active_before.snapshot.source,
    ) {
        return Err(AdtInactiveSourceDiscardError::RestoreVerificationMismatch {
            identity: Box::new(identity),
            active_sha256: active_before.snapshot.sha256,
            restored_sha256: restored_inactive.snapshot.sha256,
        });
    }

    Ok(PreparedStream {
        identity,
        inactive_before,
        active_before,
        restored_inactive,
    })
}

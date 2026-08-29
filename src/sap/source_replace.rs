use thiserror::Error;

use super::{
    client::{SapClient, SapClientError},
    edit_session::AdtEditSessionError,
    editable_source::{
        AdtEditTargetValidationError, AdtSourceReadError, AdtSourceSnapshot, AdtSourceVersion,
        EditableAdtObjectType, EditableAdtSourceIdentity, ValidatedAdtEditTarget,
        read_adt_source_for_edit, validate_adt_edit_target,
    },
    inactive_source_save::{
        InactiveSourceSaveError, PlannedInactiveSourceChange, save_inactive_source_atomically,
    },
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::source_change::{SourceChangePlanError, SourceReplacementPlan, plan_source_replacement};
use crate::suggested_command;

/// One complete source replacement to preview or save as inactive source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceReplacementRequest {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub replacement_source: String,
    pub expected_sha256: Option<String>,
    pub transport: Option<String>,
}

/// A non-mutating complete-source replacement plan based on source read from SAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceReplacementPreview {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub original: AdtSourceSnapshot,
    pub replacement: AdtSourceSnapshot,
}

/// Source versions observed before and after a complete inactive-source replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceReplacementWriteResult {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub original: AdtSourceSnapshot,
    pub replacement: AdtSourceSnapshot,
    pub stored: AdtSourceSnapshot,
    pub sap_normalized_source: bool,
}

/// A failure during complete inactive-source replacement or preview.
#[derive(Debug, Error)]
pub enum AdtSourceReplacementError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error("ADT edit session failed during complete-source replacement: {0}")]
    Session(#[source] AdtEditSessionError),
    #[error("could not read current source while the replacement lock was held: {0}")]
    LockedSourceRead(#[source] AdtSourceReadError),
    #[error("could not read current source for replacement preview: {0}")]
    PreviewSourceRead(#[source] AdtSourceReadError),
    #[error("{source}")]
    Replacement {
        identity: Box<EditableAdtSourceIdentity>,
        source: SourceChangePlanError,
    },
    #[error(
        "complete source was written and unlocked, but SAP's stored source could not be read: {source}"
    )]
    StoredSourceRead {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: AdtSourceReadError,
    },
}

impl AdtSourceReplacementError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::LockedSourceRead(error)
            | Self::PreviewSourceRead(error)
            | Self::StoredSourceRead { source: error, .. } => error.sap_error(),
            Self::Session(error) => error.sap_error(),
            Self::Validation(_) | Self::Replacement { .. } => None,
        }
    }
}

impl ReportableError for AdtSourceReplacementError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Replacement { source, .. } => source.code(),
            Self::Session(
                AdtEditSessionError::LockFailed { .. }
                | AdtEditSessionError::LockHandleMissing { .. },
            ) => "edit_source_replacement_lock_failed",
            Self::Session(AdtEditSessionError::SourceWriteFailed { .. }) => {
                "edit_source_replacement_write_failed"
            }
            Self::Session(AdtEditSessionError::UnlockFailed(_)) => {
                "edit_source_replacement_unlock_failed"
            }
            Self::LockedSourceRead(_) => "edit_source_replacement_locked_read_failed",
            Self::PreviewSourceRead(_) => "edit_source_replacement_preview_read_failed",
            Self::StoredSourceRead { .. } => "edit_source_replacement_verification_failed",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::LockedSourceRead(error) | Self::PreviewSourceRead(error) => error.hint()?,
            Self::Validation(error) => error.hint()?,
            // The pure planner cannot name the object; this layer validated it.
            Self::Replacement {
                identity,
                source:
                    source @ (SourceChangePlanError::SourceHashMismatch { .. }
                    | SourceChangePlanError::SourceReplacementNoChanges),
            } => format!(
                "{} Run `{}` to read the current source.",
                source.hint().unwrap_or_default(),
                suggested_command::edit_read(
                    identity.object_type.as_str(),
                    &identity.name,
                    AdtSourceVersion::Inactive.as_str()
                )
            ),
            Self::Replacement { source, .. } => source.hint()?,
            Self::Session(error) => error.hint()?,
            Self::StoredSourceRead { identity, .. } => format!(
                "The write and unlock succeeded, but its stored result could not be verified; re-read the inactive source before making another change. Run `{}`.",
                suggested_command::edit_read(
                    identity.object_type.as_str(),
                    &identity.name,
                    AdtSourceVersion::Inactive.as_str()
                )
            ),
        })
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// Lock and write failures return `None`: their remedy is to retry the
    /// write, which must never appear in a field a caller may execute.
    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Replacement {
                identity,
                source:
                    SourceChangePlanError::SourceHashMismatch { .. }
                    | SourceChangePlanError::SourceReplacementNoChanges,
            }
            | Self::StoredSourceRead { identity, .. } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                AdtSourceVersion::Inactive.as_str(),
            )),
            Self::LockedSourceRead(error) | Self::PreviewSourceRead(error) => {
                error.suggested_command()
            }
            Self::Replacement { .. } | Self::Validation(_) | Self::Session(_) => None,
        }
    }
}

/// Plans a complete inactive-source replacement without locking or writing.
///
/// The preview is based on the inactive source returned at the time of this
/// call. A later write acquires a lock and repeats the source read and plan.
///
/// # Errors
///
/// Returns [`AdtSourceReplacementError`] for validation, source-read, stale
/// revision, blank source, or no-change failures.
pub async fn preview_adt_source_replacement(
    sap: &SapClient,
    customer_namespaces: &[String],
    request: &AdtSourceReplacementRequest,
) -> Result<AdtSourceReplacementPreview, AdtSourceReplacementError> {
    let target = validate_replacement_request(customer_namespaces, request)?;
    let identity = target.identity;
    let transport = target.transport;
    let original = read_adt_source_for_edit(
        sap,
        identity.object_type,
        &identity.name,
        AdtSourceVersion::Inactive,
    )
    .await
    .map_err(AdtSourceReplacementError::PreviewSourceRead)?;
    let plan = plan_replacement(&original.snapshot.source, request, &identity)?;

    Ok(AdtSourceReplacementPreview {
        identity,
        transport,
        original: original.snapshot,
        replacement: AdtSourceSnapshot::from_parts(
            plan.replacement_source,
            plan.replacement_sha256,
            plan.replacement_bytes,
        ),
    })
}

/// Replaces complete inactive source through a guarded ADT lock/read/write cycle.
///
/// This operation only saves inactive source; it never activates the object.
/// The current source is read after locking, the optional expected hash is
/// checked against that locked source, and unlock is attempted after every
/// post-lock outcome. SAP's stored source is re-read after unlock so callers can
/// detect backend normalization.
///
/// # Errors
///
/// Returns [`AdtSourceReplacementError`] for validation, lock/session, stale
/// revision, blank or unchanged source, write, unlock, or verification failures.
pub async fn replace_adt_source_atomically(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &AdtSourceReplacementRequest,
) -> Result<AdtSourceReplacementWriteResult, AdtSourceReplacementError> {
    let target = validate_replacement_request(customer_namespaces, request)?;
    let saved = save_inactive_source_atomically(sap, &target, |original| {
        let plan = plan_source_replacement(
            &original.source,
            &request.replacement_source,
            request.expected_sha256.as_deref(),
        )?;
        Ok(PlannedInactiveSourceChange {
            proposed: AdtSourceSnapshot::from_parts(
                plan.replacement_source,
                plan.replacement_sha256,
                plan.replacement_bytes,
            ),
            metadata: (),
        })
    })
    .await
    .map_err(|error| map_replacement_save_error(error, &target.identity))?;
    let identity = target.identity;
    let transport = target.transport;
    let sap_normalized_source = saved.stored.source != saved.proposed.source;

    Ok(AdtSourceReplacementWriteResult {
        identity,
        transport,
        original: saved.original,
        replacement: saved.proposed,
        stored: saved.stored,
        sap_normalized_source,
    })
}

fn validate_replacement_request(
    customer_namespaces: &[String],
    request: &AdtSourceReplacementRequest,
) -> Result<ValidatedAdtEditTarget, AdtSourceReplacementError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    if request.replacement_source.trim().is_empty() {
        return Err(AdtSourceReplacementError::Replacement {
            identity: Box::new(target.identity),
            source: SourceChangePlanError::BlankReplacementSource,
        });
    }
    Ok(target)
}

fn plan_replacement(
    original_source: &str,
    request: &AdtSourceReplacementRequest,
    identity: &EditableAdtSourceIdentity,
) -> Result<SourceReplacementPlan, AdtSourceReplacementError> {
    plan_source_replacement(
        original_source,
        &request.replacement_source,
        request.expected_sha256.as_deref(),
    )
    .map_err(|source| AdtSourceReplacementError::Replacement {
        identity: Box::new(identity.clone()),
        source,
    })
}

fn map_replacement_save_error(
    error: InactiveSourceSaveError<SourceChangePlanError>,
    identity: &EditableAdtSourceIdentity,
) -> AdtSourceReplacementError {
    match error {
        InactiveSourceSaveError::Session(error) => AdtSourceReplacementError::Session(error),
        InactiveSourceSaveError::LockedSourceRead(error) => {
            AdtSourceReplacementError::LockedSourceRead(error)
        }
        InactiveSourceSaveError::Plan(source) => AdtSourceReplacementError::Replacement {
            identity: Box::new(identity.clone()),
            source,
        },
        InactiveSourceSaveError::StoredSourceRead(source) => {
            AdtSourceReplacementError::StoredSourceRead {
                identity: Box::new(identity.clone()),
                source,
            }
        }
    }
}

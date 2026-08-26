use thiserror::Error;

use super::{
    client::{SapClient, SapError},
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
use crate::source_change::{SourceChangePlanError, SourceReplacementPlan, plan_source_replacement};

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
    pub original_sha256: String,
    pub replacement_sha256: String,
    pub original_bytes: usize,
    pub replacement_bytes: usize,
    pub replacement_source: String,
}

/// Source versions observed before and after a complete inactive-source replacement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceReplacementWriteResult {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub original_sha256: String,
    pub replacement_sha256: String,
    pub stored_sha256: String,
    pub original_bytes: usize,
    pub replacement_bytes: usize,
    pub stored_bytes: usize,
    pub replacement_source: String,
    pub stored_source: String,
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
    #[error(transparent)]
    Replacement(SourceChangePlanError),
    #[error(
        "complete source was written and unlocked, but SAP's stored source could not be read: {0}"
    )]
    StoredSourceRead(#[source] AdtSourceReadError),
}

impl AdtSourceReplacementError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Replacement(error) => error.code(),
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
            Self::StoredSourceRead(_) => "edit_source_replacement_verification_failed",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::LockedSourceRead(error)
            | Self::PreviewSourceRead(error)
            | Self::StoredSourceRead(error) => error.hint(),
            Self::Validation(error) => error.hint(),
            Self::Replacement(error) => error.hint(),
            Self::Session(error) => error.hint(),
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::LockedSourceRead(error)
            | Self::PreviewSourceRead(error)
            | Self::StoredSourceRead(error) => error.sap_error(),
            Self::Session(error) => error.sap_error(),
            Self::Validation(_) | Self::Replacement(_) => None,
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
    let plan = plan_replacement(&original.source, request)?;

    Ok(AdtSourceReplacementPreview {
        identity,
        transport,
        original_sha256: plan.original_sha256,
        replacement_sha256: plan.replacement_sha256,
        original_bytes: plan.original_bytes,
        replacement_bytes: plan.replacement_bytes,
        replacement_source: plan.replacement_source,
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
    .map_err(map_replacement_save_error)?;
    let identity = target.identity;
    let transport = target.transport;
    let sap_normalized_source = saved.stored.source != saved.proposed.source;

    Ok(AdtSourceReplacementWriteResult {
        identity,
        transport,
        original_sha256: saved.original.sha256,
        replacement_sha256: saved.proposed.sha256,
        stored_sha256: saved.stored.sha256,
        original_bytes: saved.original.bytes,
        replacement_bytes: saved.proposed.bytes,
        stored_bytes: saved.stored.bytes,
        replacement_source: saved.proposed.source,
        stored_source: saved.stored.source,
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
        return Err(AdtSourceReplacementError::Replacement(
            SourceChangePlanError::BlankReplacementSource,
        ));
    }
    Ok(target)
}

fn plan_replacement(
    original_source: &str,
    request: &AdtSourceReplacementRequest,
) -> Result<SourceReplacementPlan, AdtSourceReplacementError> {
    plan_source_replacement(
        original_source,
        &request.replacement_source,
        request.expected_sha256.as_deref(),
    )
    .map_err(AdtSourceReplacementError::Replacement)
}

fn map_replacement_save_error(
    error: InactiveSourceSaveError<SourceChangePlanError>,
) -> AdtSourceReplacementError {
    match error {
        InactiveSourceSaveError::Session(error) => AdtSourceReplacementError::Session(error),
        InactiveSourceSaveError::LockedSourceRead(error) => {
            AdtSourceReplacementError::LockedSourceRead(error)
        }
        InactiveSourceSaveError::Plan(error) => AdtSourceReplacementError::Replacement(error),
        InactiveSourceSaveError::StoredSourceRead(error) => {
            AdtSourceReplacementError::StoredSourceRead(error)
        }
    }
}

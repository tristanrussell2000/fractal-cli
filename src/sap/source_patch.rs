use thiserror::Error;

use super::editable_source::{
    AdtEditTargetValidationError, AdtSourceReadError, AdtSourceVersion, EditableAdtObjectType,
    EditableAdtSourceIdentity, read_adt_source_for_edit,
};
use super::{
    client::{SapClient, SapError},
    edit_session::AdtEditSessionError,
    editable_source::{AdtSourceSnapshot, validate_adt_edit_target},
    inactive_source_save::{
        InactiveSourceSaveError, PlannedInactiveSourceChange, save_inactive_source_atomically,
    },
};
use crate::source_change::{SourceChangePlanError, plan_patch};
use crate::suggested_command;

/// One exact source replacement to preview or perform against an ADT object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourcePatchRequest {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub find: String,
    pub replace: String,
    pub expected_sha256: Option<String>,
    pub transport: Option<String>,
}

/// A non-mutating patch plan based on the source currently returned by SAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourcePatchPreview {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub original: AdtSourceSnapshot,
    pub proposed: AdtSourceSnapshot,
    pub replacements: usize,
}

/// The source versions observed before and after an atomic ADT patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourcePatchWriteResult {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub original: AdtSourceSnapshot,
    pub proposed: AdtSourceSnapshot,
    pub stored: AdtSourceSnapshot,
    pub replacements: usize,
}

/// A failure during the guarded ADT lock/read/patch/write/unlock workflow.
#[derive(Debug, Error)]
pub enum AdtSourcePatchError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error(transparent)]
    Session(#[from] AdtEditSessionError),
    #[error("could not read the current editable source while its ADT lock was held: {0}")]
    LockedSourceRead(#[source] AdtSourceReadError),
    #[error("could not read the current editable source for a patch preview: {0}")]
    PreviewSourceRead(#[source] AdtSourceReadError),
    #[error("{source}")]
    Patch {
        identity: Box<EditableAdtSourceIdentity>,
        source: SourceChangePlanError,
    },
    #[error(
        "the patched source was written, but SAP's stored source could not be re-read: {source}"
    )]
    StoredSourceRead {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: AdtSourceReadError,
    },
}

impl AdtSourcePatchError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Session(error) => error.code(),
            Self::Patch { source, .. } => source.code(),
            Self::LockedSourceRead(_) => "edit_locked_source_read_failed",
            Self::PreviewSourceRead(_) => "edit_preview_source_read_failed",
            Self::StoredSourceRead { .. } => "edit_source_verification_failed",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Validation(error) => error.hint(),
            Self::Session(error) => error.hint(),
            Self::LockedSourceRead(error) | Self::PreviewSourceRead(error) => error.hint(),
            // The pure planner cannot name the object, so the operation error —
            // the layer that validated the identity — adds the exact command
            // that produces a usable anchor or a current revision hash.
            Self::Patch { identity, source } => match source {
                SourceChangePlanError::AnchorNotFound
                | SourceChangePlanError::AnchorAmbiguous { .. }
                | SourceChangePlanError::SourceHashMismatch { .. } => format!(
                    "{} Run `{}` to read the current source.",
                    source.hint(),
                    suggested_command::edit_read(
                        identity.object_type.as_str(),
                        &identity.name,
                        AdtSourceVersion::Inactive.as_str()
                    )
                ),
                _ => source.hint(),
            },
            Self::StoredSourceRead { identity, .. } => format!(
                "The write and unlock succeeded, but its stored result could not be verified; re-read the inactive source before making another change. Run `{}`.",
                suggested_command::edit_read(
                    identity.object_type.as_str(),
                    &identity.name,
                    AdtSourceVersion::Inactive.as_str()
                )
            ),
        }
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// Lock and write failures return `None`: their remedy is to retry the
    /// write, which must never appear in a field a caller may execute.
    #[must_use]
    pub fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Patch {
                identity,
                source:
                    SourceChangePlanError::AnchorNotFound
                    | SourceChangePlanError::AnchorAmbiguous { .. }
                    | SourceChangePlanError::SourceHashMismatch { .. },
            }
            | Self::StoredSourceRead { identity, .. } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                AdtSourceVersion::Inactive.as_str(),
            )),
            Self::LockedSourceRead(error) | Self::PreviewSourceRead(error) => {
                error.suggested_command()
            }
            Self::Patch { .. } | Self::Validation(_) | Self::Session(_) => None,
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::LockedSourceRead(error)
            | Self::PreviewSourceRead(error)
            | Self::StoredSourceRead { source: error, .. } => error.sap_error(),
            Self::Session(error) => error.sap_error(),
            Self::Validation(_) | Self::Patch { .. } => None,
        }
    }
}

/// Applies one exact literal patch while holding a native ADT object lock.
///
/// The current editable source is read only after the lock is acquired. An
/// expected SHA-256 is therefore optional: when supplied it provides an
/// explicit stale-read check, while ordinary callers can safely patch the
/// locked source without carrying revision state between commands.
///
/// Unlock is attempted after every operation that follows successful lock
/// acquisition. If both the operation and unlock fail, the operation error is
/// returned because it is the primary cause. After a successful write and
/// unlock, the source is fetched again so the returned hash describes what SAP
/// actually stored, including any normalization performed by the backend.
///
/// # Errors
///
/// Returns [`AdtSourcePatchError`] for identity or namespace validation,
/// lock/session failures, stale revisions, unsafe patch anchors, source writes,
/// unlock failures, or failed post-write verification.
pub async fn patch_adt_source_atomically(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &AdtSourcePatchRequest,
) -> Result<AdtSourcePatchWriteResult, AdtSourcePatchError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    let saved = save_inactive_source_atomically(sap, &target, |original| {
        let expected_sha256 = request
            .expected_sha256
            .as_deref()
            .unwrap_or(&original.sha256);
        let plan = plan_patch(
            &original.source,
            &request.find,
            &request.replace,
            expected_sha256,
        )?;
        Ok(PlannedInactiveSourceChange {
            proposed: AdtSourceSnapshot::from_parts(
                plan.updated_source,
                plan.updated_sha256,
                plan.updated_bytes,
            ),
            metadata: plan.replacements,
        })
    })
    .await
    .map_err(|error| map_patch_save_error(error, &target.identity))?;
    let identity = target.identity;
    let transport = target.transport;

    Ok(AdtSourcePatchWriteResult {
        identity,
        transport,
        original: saved.original,
        proposed: saved.proposed,
        stored: saved.stored,
        replacements: saved.metadata,
    })
}

/// Plans one exact source patch without locking, writing, or activating.
///
/// The preview is based on the inactive source requested at the time of this
/// call. A later write still acquires a lock and repeats the read and planning
/// steps, so preview success is not a promise that a subsequent patch can be
/// applied unchanged.
///
/// # Errors
///
/// Returns [`AdtSourcePatchError`] for identity or namespace validation,
/// source reads, stale revisions, or unsafe patch anchors.
pub async fn preview_adt_source_patch(
    sap: &SapClient,
    customer_namespaces: &[String],
    request: &AdtSourcePatchRequest,
) -> Result<AdtSourcePatchPreview, AdtSourcePatchError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    let identity = target.identity;
    let transport = target.transport;
    let original = read_adt_source_for_edit(
        sap,
        identity.object_type,
        &identity.name,
        AdtSourceVersion::Inactive,
    )
    .await
    .map_err(AdtSourcePatchError::PreviewSourceRead)?;
    let expected_sha256 = request
        .expected_sha256
        .as_deref()
        .unwrap_or(&original.snapshot.sha256);
    let plan = plan_patch(
        &original.snapshot.source,
        &request.find,
        &request.replace,
        expected_sha256,
    )
    .map_err(|source| AdtSourcePatchError::Patch {
        identity: Box::new(identity.clone()),
        source,
    })?;

    Ok(AdtSourcePatchPreview {
        identity,
        transport,
        original: original.snapshot,
        proposed: AdtSourceSnapshot::from_parts(
            plan.updated_source,
            plan.updated_sha256,
            plan.updated_bytes,
        ),
        replacements: plan.replacements,
    })
}

fn map_patch_save_error(
    error: InactiveSourceSaveError<SourceChangePlanError>,
    identity: &EditableAdtSourceIdentity,
) -> AdtSourcePatchError {
    match error {
        InactiveSourceSaveError::Session(error) => AdtSourcePatchError::Session(error),
        InactiveSourceSaveError::LockedSourceRead(error) => {
            AdtSourcePatchError::LockedSourceRead(error)
        }
        InactiveSourceSaveError::Plan(source) => AdtSourcePatchError::Patch {
            identity: Box::new(identity.clone()),
            source,
        },
        InactiveSourceSaveError::StoredSourceRead(source) => {
            AdtSourcePatchError::StoredSourceRead {
                identity: Box::new(identity.clone()),
                source,
            }
        }
    }
}

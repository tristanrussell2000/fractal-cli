use thiserror::Error;

pub use super::editable_source::{
    AdtEditTargetValidationError, AdtSourceReadError, AdtSourceReadResult, AdtSourceVersion,
    EditableAdtObjectType, EditableAdtSourceTargetError, TransportRequestError,
    read_adt_source_for_edit,
};
use super::{
    client::{SapClient, SapError},
    edit_session::{
        AdtEditSessionError, AdtObjectLock, acquire_adt_object_lock,
        read_adt_source_in_stateful_session, release_adt_object_lock, write_adt_source,
    },
    editable_source::{EditableAdtSourceIdentity, validate_adt_edit_target},
};
use crate::source_change::{PatchPlan, SourceChangePlanError, plan_patch};

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
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
    pub transport: Option<String>,
    pub original_sha256: String,
    pub proposed_sha256: String,
    pub original_bytes: usize,
    pub proposed_bytes: usize,
    pub replacements: usize,
    pub proposed_source: String,
}

/// The source versions observed before and after an atomic ADT patch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourcePatchWriteResult {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
    pub transport: Option<String>,
    pub original_sha256: String,
    pub proposed_sha256: String,
    pub stored_sha256: String,
    pub original_bytes: usize,
    pub proposed_bytes: usize,
    pub stored_bytes: usize,
    pub replacements: usize,
    pub proposed_source: String,
    pub stored_source: String,
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
    #[error(transparent)]
    Patch(SourceChangePlanError),
    #[error("the patched source was written, but SAP's stored source could not be re-read: {0}")]
    StoredSourceRead(#[source] AdtSourceReadError),
}

impl AdtSourcePatchError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Session(error) => error.code(),
            Self::Patch(error) => error.code(),
            Self::LockedSourceRead(_) => "edit_locked_source_read_failed",
            Self::PreviewSourceRead(_) => "edit_preview_source_read_failed",
            Self::StoredSourceRead(_) => "edit_source_verification_failed",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Validation(error) => error.hint(),
            Self::Session(error) => error.hint(),
            Self::LockedSourceRead(error) | Self::PreviewSourceRead(error) => error.hint(),
            Self::Patch(error) => error.hint(),
            Self::StoredSourceRead(_) => {
                "The write and unlock succeeded, but its stored result could not be verified; re-read the inactive source before making another change."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::LockedSourceRead(error)
            | Self::PreviewSourceRead(error)
            | Self::StoredSourceRead(error) => error.sap_error(),
            Self::Session(error) => error.sap_error(),
            Self::Validation(_) | Self::Patch(_) => None,
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
    let identity = target.identity;
    let transport = target.transport;

    let lock = acquire_adt_object_lock(sap, &identity.object_uri, transport.as_deref()).await?;
    let operation = patch_source_while_locked(sap, &identity, &lock, request).await;
    let unlock = release_adt_object_lock(sap, &identity.object_uri, &lock).await;

    let (original, plan) = match operation {
        Err(primary) => {
            // Cleanup errors must not replace the error that caused the write
            // workflow to abort, but release was still attempted above.
            let _ = unlock;
            return Err(primary);
        }
        Ok(value) => {
            unlock?;
            value
        }
    };

    let stored = read_adt_source_for_edit(
        sap,
        identity.object_type,
        &identity.name,
        AdtSourceVersion::Inactive,
    )
    .await
    .map_err(AdtSourcePatchError::StoredSourceRead)?;

    Ok(AdtSourcePatchWriteResult {
        object_type: identity.object_type,
        name: identity.name,
        object_uri: identity.object_uri,
        source_uri: identity.source_uri,
        transport,
        original_sha256: original.sha256,
        proposed_sha256: plan.updated_sha256,
        stored_sha256: stored.sha256,
        original_bytes: original.bytes,
        proposed_bytes: plan.updated_bytes,
        stored_bytes: stored.bytes,
        replacements: plan.replacements,
        proposed_source: plan.updated_source,
        stored_source: stored.source,
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
        .unwrap_or(&original.sha256);
    let plan = plan_patch(
        &original.source,
        &request.find,
        &request.replace,
        expected_sha256,
    )
    .map_err(AdtSourcePatchError::Patch)?;

    Ok(AdtSourcePatchPreview {
        object_type: identity.object_type,
        name: identity.name,
        object_uri: identity.object_uri,
        source_uri: identity.source_uri,
        transport,
        original_sha256: original.sha256,
        proposed_sha256: plan.updated_sha256,
        original_bytes: original.bytes,
        proposed_bytes: plan.updated_bytes,
        replacements: plan.replacements,
        proposed_source: plan.updated_source,
    })
}

async fn patch_source_while_locked(
    sap: &mut SapClient,
    identity: &EditableAdtSourceIdentity,
    lock: &AdtObjectLock,
    request: &AdtSourcePatchRequest,
) -> Result<(AdtSourceReadResult, PatchPlan), AdtSourcePatchError> {
    let original = read_adt_source_in_stateful_session(sap, identity, AdtSourceVersion::Inactive)
        .await
        .map_err(AdtSourcePatchError::LockedSourceRead)?;
    let expected_sha256 = request
        .expected_sha256
        .as_deref()
        .unwrap_or(&original.sha256);
    let plan = plan_patch(
        &original.source,
        &request.find,
        &request.replace,
        expected_sha256,
    )
    .map_err(AdtSourcePatchError::Patch)?;

    write_adt_source(sap, identity, lock, &plan.updated_source).await?;
    Ok((original, plan))
}

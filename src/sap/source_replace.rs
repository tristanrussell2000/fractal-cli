use thiserror::Error;

use super::{
    client::{SapClient, SapError},
    edit::{
        AdtObjectLock, AdtSourcePatchError, AdtSourceReadError, AdtSourceReadResult,
        AdtSourceVersion, EditableAdtObjectIdentity, EditableAdtObjectType,
        acquire_adt_object_lock, canonicalize_transport_request, editable_object_identity,
        read_adt_source_for_edit, read_adt_source_in_stateful_session, release_adt_object_lock,
        write_adt_source,
    },
};
use crate::edit::{
    EditError, SourceReplacementPlan, plan_source_replacement, validate_customer_namespace,
};

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
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
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
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
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
    #[error("invalid complete-source replacement object: {0}")]
    InvalidObject(#[source] AdtSourceReadError),
    #[error(transparent)]
    Namespace(EditError),
    #[error("invalid transport request '{0}'")]
    InvalidTransportRequest(String),
    #[error("could not acquire an ADT lock for complete-source replacement: {0}")]
    Lock(#[source] AdtSourcePatchError),
    #[error("could not read current source while the replacement lock was held: {0}")]
    LockedSourceRead(#[source] AdtSourceReadError),
    #[error("could not read current source for replacement preview: {0}")]
    PreviewSourceRead(#[source] AdtSourceReadError),
    #[error(transparent)]
    Replacement(EditError),
    #[error("could not write complete replacement source through its ADT lock: {0}")]
    SourceWrite(#[source] AdtSourcePatchError),
    #[error("complete source was written, but its ADT lock could not be released: {0}")]
    Unlock(#[source] AdtSourcePatchError),
    #[error(
        "complete source was written and unlocked, but SAP's stored source could not be read: {0}"
    )]
    StoredSourceRead(#[source] AdtSourceReadError),
}

impl AdtSourceReplacementError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidObject(error) => error.code(),
            Self::Namespace(error) | Self::Replacement(error) => error.code(),
            Self::InvalidTransportRequest(_) => "invalid_transport_request",
            Self::Lock(_) => "edit_source_replacement_lock_failed",
            Self::LockedSourceRead(_) => "edit_source_replacement_locked_read_failed",
            Self::PreviewSourceRead(_) => "edit_source_replacement_preview_read_failed",
            Self::SourceWrite(_) => "edit_source_replacement_write_failed",
            Self::Unlock(_) => "edit_source_replacement_unlock_failed",
            Self::StoredSourceRead(_) => "edit_source_replacement_verification_failed",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::InvalidObject(error)
            | Self::LockedSourceRead(error)
            | Self::PreviewSourceRead(error)
            | Self::StoredSourceRead(error) => error.hint(),
            Self::Namespace(error) | Self::Replacement(error) => error.hint(),
            Self::InvalidTransportRequest(_) => {
                "Use a transport request identifier containing 1-20 ASCII letters or digits, for example DE3K900575."
                    .to_owned()
            }
            Self::Lock(error) | Self::SourceWrite(error) | Self::Unlock(error) => error.hint(),
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::InvalidObject(error)
            | Self::LockedSourceRead(error)
            | Self::PreviewSourceRead(error)
            | Self::StoredSourceRead(error) => error.sap_error(),
            Self::Lock(error) | Self::SourceWrite(error) | Self::Unlock(error) => error.sap_error(),
            Self::Namespace(_) | Self::InvalidTransportRequest(_) | Self::Replacement(_) => None,
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
    let (identity, transport) = validate_replacement_request(customer_namespaces, request)?;
    let original = read_adt_source_for_edit(
        sap,
        identity.object_type,
        &identity.name,
        AdtSourceVersion::Inactive,
    )
    .await
    .map_err(AdtSourceReplacementError::PreviewSourceRead)?;
    let plan = plan_replacement(&original, request)?;

    Ok(AdtSourceReplacementPreview {
        object_type: identity.object_type,
        name: identity.name,
        object_uri: identity.object_uri,
        source_uri: identity.source_uri,
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
    let (identity, transport) = validate_replacement_request(customer_namespaces, request)?;
    let lock = acquire_adt_object_lock(sap, &identity.object_uri, transport.as_deref())
        .await
        .map_err(AdtSourceReplacementError::Lock)?;
    let operation = replace_source_while_locked(sap, &identity, &lock, request).await;
    let unlock = release_adt_object_lock(sap, &identity.object_uri, &lock).await;

    let (original, plan) = match operation {
        Err(primary) => {
            // Keep the operation failure primary while still attempting cleanup.
            let _ = unlock;
            return Err(primary);
        }
        Ok(value) => {
            unlock.map_err(AdtSourceReplacementError::Unlock)?;
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
    .map_err(AdtSourceReplacementError::StoredSourceRead)?;
    let sap_normalized_source = stored.source != plan.replacement_source;

    Ok(AdtSourceReplacementWriteResult {
        object_type: identity.object_type,
        name: identity.name,
        object_uri: identity.object_uri,
        source_uri: identity.source_uri,
        transport,
        original_sha256: original.sha256,
        replacement_sha256: plan.replacement_sha256,
        stored_sha256: stored.sha256,
        original_bytes: original.bytes,
        replacement_bytes: plan.replacement_bytes,
        stored_bytes: stored.bytes,
        replacement_source: plan.replacement_source,
        stored_source: stored.source,
        sap_normalized_source,
    })
}

fn validate_replacement_request(
    customer_namespaces: &[String],
    request: &AdtSourceReplacementRequest,
) -> Result<(EditableAdtObjectIdentity, Option<String>), AdtSourceReplacementError> {
    let identity = editable_object_identity(request.object_type, &request.name)
        .map_err(AdtSourceReplacementError::InvalidObject)?;
    validate_customer_namespace(&identity.name, customer_namespaces)
        .map_err(AdtSourceReplacementError::Namespace)?;
    let transport = canonicalize_transport_request(request.transport.as_deref())
        .map_err(AdtSourceReplacementError::InvalidTransportRequest)?;
    if request.replacement_source.trim().is_empty() {
        return Err(AdtSourceReplacementError::Replacement(
            EditError::BlankReplacementSource,
        ));
    }
    Ok((identity, transport))
}

fn plan_replacement(
    original: &AdtSourceReadResult,
    request: &AdtSourceReplacementRequest,
) -> Result<SourceReplacementPlan, AdtSourceReplacementError> {
    plan_source_replacement(
        &original.source,
        &request.replacement_source,
        request.expected_sha256.as_deref(),
    )
    .map_err(AdtSourceReplacementError::Replacement)
}

async fn replace_source_while_locked(
    sap: &mut SapClient,
    identity: &EditableAdtObjectIdentity,
    lock: &AdtObjectLock,
    request: &AdtSourceReplacementRequest,
) -> Result<(AdtSourceReadResult, SourceReplacementPlan), AdtSourceReplacementError> {
    let original = read_adt_source_in_stateful_session(sap, identity, AdtSourceVersion::Inactive)
        .await
        .map_err(AdtSourceReplacementError::LockedSourceRead)?;
    let plan = plan_replacement(&original, request)?;
    write_adt_source(sap, identity, lock, &plan.replacement_source)
        .await
        .map_err(AdtSourceReplacementError::SourceWrite)?;
    Ok((original, plan))
}

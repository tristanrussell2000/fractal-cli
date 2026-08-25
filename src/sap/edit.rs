use reqwest::header::HeaderValue;
use thiserror::Error;

pub use super::editable_source::{
    AdtEditTargetValidationError, AdtSourceReadError, AdtSourceReadResult, AdtSourceVersion,
    EditableAdtObjectType, EditableAdtSourceTargetError, TransportRequestError,
    read_adt_source_for_edit,
};
use super::{
    client::{SapClient, SapError},
    editable_source::{
        EditableAdtSourceIdentity, read_adt_source, stateful_session_headers,
        validate_adt_edit_target,
    },
};
use crate::source_change::{PatchPlan, SourceChangePlanError, plan_patch};

const LOCK_RESULT_MEDIA_TYPE: &str =
    "application/vnd.sap.as+xml; charset=UTF-8; dataname=com.sap.adt.lock.Result";

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
    #[error("could not acquire an ADT edit lock: {source}")]
    Lock {
        transport: Option<String>,
        #[source]
        source: SapError,
    },
    #[error("SAP returned an edit lock response without a lock handle: {response_excerpt}")]
    LockHandleMissing { response_excerpt: String },
    #[error("could not read the current editable source while its ADT lock was held: {0}")]
    LockedSourceRead(#[source] AdtSourceReadError),
    #[error("could not read the current editable source for a patch preview: {0}")]
    PreviewSourceRead(#[source] AdtSourceReadError),
    #[error(transparent)]
    Patch(SourceChangePlanError),
    #[error("could not write the patched source through its ADT lock: {source}")]
    SourceWrite {
        transport: Option<String>,
        #[source]
        source: SapError,
    },
    #[error("the patch operation completed, but the ADT object lock could not be released: {0}")]
    Unlock(#[source] SapError),
    #[error("the patched source was written, but SAP's stored source could not be re-read: {0}")]
    StoredSourceRead(#[source] AdtSourceReadError),
}

impl AdtSourcePatchError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::Patch(error) => error.code(),
            Self::Lock { .. } => "edit_lock_failed",
            Self::LockHandleMissing { .. } => "edit_lock_response_invalid",
            Self::LockedSourceRead(_) => "edit_locked_source_read_failed",
            Self::PreviewSourceRead(_) => "edit_preview_source_read_failed",
            Self::SourceWrite { .. } => "edit_source_write_failed",
            Self::Unlock(_) => "edit_unlock_failed",
            Self::StoredSourceRead(_) => "edit_source_verification_failed",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Validation(error) => error.hint(),
            Self::LockedSourceRead(error) | Self::PreviewSourceRead(error) => error.hint(),
            Self::Patch(error) => error.hint(),
            Self::Lock { transport, source } => transport_failure_hint(transport.as_deref(), source)
                .unwrap_or_else(|| {
                    "Close any editor or process holding the object lock, then retry the source operation."
                        .to_owned()
                }),
            Self::LockHandleMissing { .. } => {
                "SAP accepted the lock request but returned an unexpected ADT lock payload."
                    .to_owned()
            }
            Self::SourceWrite { transport, source } => {
                transport_failure_hint(transport.as_deref(), source)
                    .unwrap_or_else(|| source.hint().to_owned())
            }
            Self::Unlock(_) => {
                "The source operation may have succeeded, but the SAP lock may remain; close or unlock the object before retrying."
                    .to_owned()
            }
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
            Self::Lock { source, .. } | Self::SourceWrite { source, .. } | Self::Unlock(source) => {
                Some(source)
            }
            Self::Validation(_) | Self::LockHandleMissing { .. } | Self::Patch(_) => None,
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
    let original = read_adt_source(
        sap,
        identity,
        AdtSourceVersion::Inactive,
        stateful_session_headers(),
    )
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct AdtObjectLock {
    handle: String,
    transport: Option<String>,
}

pub(super) async fn acquire_adt_object_lock(
    sap: &mut SapClient,
    object_uri: &str,
    transport: Option<&str>,
) -> Result<AdtObjectLock, AdtSourcePatchError> {
    let response = match request_adt_object_lock(sap, object_uri, transport).await {
        Ok(response) => response,
        Err(error)
            if transport
                .is_some_and(|transport| is_same_transport_lock_conflict(&error, transport)) =>
        {
            request_adt_object_lock(sap, object_uri, None)
                .await
                .map_err(|source| AdtSourcePatchError::Lock {
                    transport: transport.map(str::to_owned),
                    source,
                })?
        }
        Err(source) => {
            return Err(AdtSourcePatchError::Lock {
                transport: transport.map(str::to_owned),
                source,
            });
        }
    };
    let handle =
        parse_lock_handle(&response).ok_or_else(|| AdtSourcePatchError::LockHandleMissing {
            response_excerpt: response.chars().take(400).collect(),
        })?;
    Ok(AdtObjectLock {
        handle,
        transport: transport.map(str::to_owned),
    })
}

async fn request_adt_object_lock(
    sap: &mut SapClient,
    object_uri: &str,
    transport: Option<&str>,
) -> Result<String, SapError> {
    let mut headers = stateful_session_headers();
    headers.insert("Accept", HeaderValue::from_static(LOCK_RESULT_MEDIA_TYPE));
    let mut query = vec![("_action", "LOCK"), ("accessMode", "MODIFY")];
    if let Some(transport) = transport {
        query.push(("corrNr", transport));
    }
    sap.post_text(object_uri, &query, None, headers).await
}

pub(super) async fn release_adt_object_lock(
    sap: &mut SapClient,
    object_uri: &str,
    lock: &AdtObjectLock,
) -> Result<(), AdtSourcePatchError> {
    sap.post_text(
        object_uri,
        &[("_action", "UNLOCK"), ("lockHandle", &lock.handle)],
        None,
        stateful_session_headers(),
    )
    .await
    .map(|_| ())
    .map_err(AdtSourcePatchError::Unlock)
}

pub(super) async fn attach_adt_object_to_transport(
    sap: &mut SapClient,
    object_uri: &str,
    transport: &str,
) -> Result<(), AdtSourcePatchError> {
    let lock = acquire_adt_object_lock(sap, object_uri, Some(transport)).await?;
    release_adt_object_lock(sap, object_uri, &lock).await
}

pub(super) async fn write_adt_source(
    sap: &mut SapClient,
    identity: &EditableAdtSourceIdentity,
    lock: &AdtObjectLock,
    source: &str,
) -> Result<(), AdtSourcePatchError> {
    let mut headers = stateful_session_headers();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static("text/plain; charset=utf-8"),
    );
    let mut query = vec![("lockHandle", lock.handle.as_str())];
    if let Some(transport) = &lock.transport {
        query.push(("corrNr", transport));
    }
    sap.put_text(&identity.source_uri, &query, source, headers)
        .await
        .map(|_| ())
        .map_err(|source| AdtSourcePatchError::SourceWrite {
            transport: lock.transport.clone(),
            source,
        })
}

fn is_same_transport_lock_conflict(error: &SapError, transport: &str) -> bool {
    let Some(message) = sap_error_message(error) else {
        return false;
    };
    message.to_ascii_uppercase().contains("ALREADY LOCKED")
        && transport_request_from_sap_error(error)
            .is_some_and(|existing| existing.eq_ignore_ascii_case(transport))
}

fn transport_failure_hint(transport: Option<&str>, error: &SapError) -> Option<String> {
    let existing = transport_request_from_sap_error(error)?;
    match transport {
        None => Some(format!(
            "SAP reports that the object belongs to transport {existing}. Retry with --transport {existing} if that is the intended parent change request."
        )),
        Some(requested) if requested.eq_ignore_ascii_case(&existing) => Some(format!(
            "SAP still refused transport {existing}. Confirm that it is the parent change request rather than a task and that your user may write to it."
        )),
        Some(requested) => Some(format!(
            "SAP reports that the object belongs to transport {existing}, not the requested transport {requested}. Use the correct parent request or reassign the object in SAP."
        )),
    }
}

fn transport_request_from_sap_error(error: &SapError) -> Option<String> {
    let message = sap_error_message(error)?.to_ascii_uppercase();
    if !message.contains("LOCKED") {
        return None;
    }
    ["REQUEST ", "TRANSPORT "].into_iter().find_map(|marker| {
        let after_marker = message.split_once(marker)?.1;
        let candidate = after_marker
            .chars()
            .take_while(char::is_ascii_alphanumeric)
            .collect::<String>();
        (!candidate.is_empty()).then_some(candidate)
    })
}

fn sap_error_message(error: &SapError) -> Option<&str> {
    match error {
        SapError::Http { message, .. } => Some(message),
        _ => None,
    }
}

fn parse_lock_handle(response: &str) -> Option<String> {
    let document = roxmltree::Document::parse(response).ok()?;
    document.descendants().find_map(|node| {
        if !node.is_element() {
            return None;
        }
        if node.tag_name().name().eq_ignore_ascii_case("LOCK_HANDLE") {
            return node
                .text()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_owned);
        }
        node.attributes().find_map(|attribute| {
            attribute
                .name()
                .eq_ignore_ascii_case("lockHandle")
                .then(|| attribute.value().trim())
                .filter(|value| !value.is_empty())
                .map(str::to_owned)
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_lock_handles_from_element_text_or_attributes() {
        assert_eq!(
            parse_lock_handle("<lock><LOCK_HANDLE> handle-1 </LOCK_HANDLE></lock>").as_deref(),
            Some("handle-1")
        );
        assert_eq!(
            parse_lock_handle(r#"<lock lockHandle="handle-2"/>"#).as_deref(),
            Some("handle-2")
        );
    }

    #[test]
    fn rejects_missing_blank_and_malformed_lock_handles() {
        for response in [
            "<lock/>",
            "<lock><LOCK_HANDLE> </LOCK_HANDLE></lock>",
            "not XML",
        ] {
            assert_eq!(parse_lock_handle(response), None);
        }
    }
}

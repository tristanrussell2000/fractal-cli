use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    client::{SapClient, SapError},
    editable_source::{
        AdtSourceReadError, AdtSourceReadResult, AdtSourceVersion, EditableAdtSourceIdentity,
        read_adt_source,
    },
};

const STATEFUL_SESSION_HEADER: &str = "X-sap-adt-sessiontype";
const LOCK_RESULT_MEDIA_TYPE: &str =
    "application/vnd.sap.as+xml; charset=UTF-8; dataname=com.sap.adt.lock.Result";

/// A failure in the reusable native ADT lock/write/unlock session machinery.
#[derive(Debug, Error)]
pub enum AdtEditSessionError {
    #[error("could not acquire an ADT edit lock: {source}")]
    LockFailed {
        transport: Option<String>,
        #[source]
        source: SapError,
    },
    #[error("SAP returned an edit lock response without a lock handle: {response_excerpt}")]
    LockHandleMissing { response_excerpt: String },
    #[error("could not write source through its ADT lock: {source}")]
    SourceWriteFailed {
        transport: Option<String>,
        #[source]
        source: SapError,
    },
    #[error("the ADT object lock could not be released: {0}")]
    UnlockFailed(#[source] SapError),
}

impl AdtEditSessionError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::LockFailed { .. } => "edit_lock_failed",
            Self::LockHandleMissing { .. } => "edit_lock_response_invalid",
            Self::SourceWriteFailed { .. } => "edit_source_write_failed",
            Self::UnlockFailed(_) => "edit_unlock_failed",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::LockFailed { transport, source } => {
                transport_failure_hint(transport.as_deref(), source).unwrap_or_else(|| {
                    "Close any editor or process holding the object lock, then retry the source operation."
                        .to_owned()
                })
            }
            Self::LockHandleMissing { .. } => {
                "SAP accepted the lock request but returned an unexpected ADT lock payload."
                    .to_owned()
            }
            Self::SourceWriteFailed { transport, source } => {
                transport_failure_hint(transport.as_deref(), source)
                    .unwrap_or_else(|| source.hint().to_owned())
            }
            Self::UnlockFailed(_) => {
                "The source operation may have succeeded, but the SAP lock may remain; close or unlock the object before retrying."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::LockFailed { source, .. } | Self::SourceWriteFailed { source, .. } => {
                Some(source)
            }
            Self::UnlockFailed(source) => Some(source),
            Self::LockHandleMissing { .. } => None,
        }
    }
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
) -> Result<AdtObjectLock, AdtEditSessionError> {
    let response = match request_adt_object_lock(sap, object_uri, transport).await {
        Ok(response) => response,
        Err(error)
            if transport
                .is_some_and(|transport| is_same_transport_lock_conflict(&error, transport)) =>
        {
            request_adt_object_lock(sap, object_uri, None)
                .await
                .map_err(|source| AdtEditSessionError::LockFailed {
                    transport: transport.map(str::to_owned),
                    source,
                })?
        }
        Err(source) => {
            return Err(AdtEditSessionError::LockFailed {
                transport: transport.map(str::to_owned),
                source,
            });
        }
    };
    let handle =
        parse_lock_handle(&response).ok_or_else(|| AdtEditSessionError::LockHandleMissing {
            response_excerpt: response.chars().take(400).collect(),
        })?;
    Ok(AdtObjectLock {
        handle,
        transport: transport.map(str::to_owned),
    })
}

pub(super) async fn release_adt_object_lock(
    sap: &mut SapClient,
    object_uri: &str,
    lock: &AdtObjectLock,
) -> Result<(), AdtEditSessionError> {
    sap.post_text(
        object_uri,
        &[("_action", "UNLOCK"), ("lockHandle", &lock.handle)],
        None,
        stateful_session_headers(),
    )
    .await
    .map(|_| ())
    .map_err(AdtEditSessionError::UnlockFailed)
}

pub(super) async fn attach_adt_object_to_transport(
    sap: &mut SapClient,
    object_uri: &str,
    transport: &str,
) -> Result<(), AdtEditSessionError> {
    let lock = acquire_adt_object_lock(sap, object_uri, Some(transport)).await?;
    release_adt_object_lock(sap, object_uri, &lock).await
}

pub(super) async fn write_adt_source(
    sap: &mut SapClient,
    identity: &EditableAdtSourceIdentity,
    lock: &AdtObjectLock,
    source: &str,
) -> Result<(), AdtEditSessionError> {
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
        .map_err(|source| AdtEditSessionError::SourceWriteFailed {
            transport: lock.transport.clone(),
            source,
        })
}

pub(super) async fn read_adt_source_in_stateful_session(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
    version: AdtSourceVersion,
) -> Result<AdtSourceReadResult, AdtSourceReadError> {
    read_adt_source(sap, identity, version, stateful_session_headers()).await
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

fn stateful_session_headers() -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        STATEFUL_SESSION_HEADER,
        HeaderValue::from_static("stateful"),
    );
    headers
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

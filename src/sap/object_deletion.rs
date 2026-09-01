//! Deletion of editable ADT objects, guarded by where-used.
//!
//! This is the only destructive command in the CLI, so the safe path is the
//! default: an object that anything still references is refused, and the
//! caller has to say `--force` to override. Success means the object is
//! *gone*, proven by a read-back that must report not-found — ADT has already
//! been observed answering 200 to a destructive request that did nothing
//! (`activation?method=discard`), so a status code is not evidence.
//!
//! There is no confirmation flag. A flag that must be supplied up front is not
//! a pause for thought; it becomes boilerplate the caller always passes. The
//! guards that do work are the explicit `delete` verb, the customer-namespace
//! check, the where-used refusal, and the read-back.

use thiserror::Error;

use super::{
    client::{SapClient, SapClientError},
    edit_session::{
        AdtEditSessionError, acquire_adt_object_lock, release_adt_object_lock,
        stateful_session_headers,
    },
    editable_source::{
        AdtEditTargetValidationError, EditableAdtSourceIdentity, validate_adt_edit_target,
    },
    object_usages::{ObjectUsagesError, UsageReference, get_object_usages},
};
use crate::{
    reportable_error::{ReportableError, sap_http_status},
    suggested_command,
};

/// One object to delete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtObjectDeletionRequest {
    pub object_type: super::editable_source::EditableAdtObjectType,
    pub name: String,
    pub transport: Option<String>,
    /// Delete even though other objects still reference this one.
    pub force: bool,
}

/// What a deletion would do, without doing any of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtObjectDeletionPreview {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub direct_usages: Vec<String>,
    pub would_delete: bool,
}

/// A deletion that has been carried out and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtObjectDeletionResult {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub direct_usages: Vec<String>,
    pub forced: bool,
}

/// A failure while deleting an editable ADT object.
#[derive(Debug, Error)]
pub enum AdtObjectDeletionError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error("could not determine what references this object: {0}")]
    UsageCheck(#[source] ObjectUsagesError),
    #[error("{name} is still referenced by {} object(s)", usages.len())]
    ObjectInUse {
        identity: Box<EditableAdtSourceIdentity>,
        name: String,
        usages: Vec<String>,
    },
    #[error("ADT edit session failed while deleting: {0}")]
    Session(#[source] AdtEditSessionError),
    #[error("the ADT delete request failed: {source}")]
    DeleteRequest {
        identity: Box<EditableAdtSourceIdentity>,
        /// The delete failed *and* the lock could not be released, so the
        /// object is still locked. The delete failure stays the reported cause,
        /// but the caller has to clear the lock before anything else will work.
        still_locked: bool,
        #[source]
        source: SapClientError,
    },
    #[error("SAP accepted the delete request, but the object still exists")]
    NotDeleted {
        identity: Box<EditableAdtSourceIdentity>,
    },
    #[error("SAP accepted the delete request, but its result could not be verified: {source}")]
    Verification {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: SapClientError,
    },
}

impl ReportableError for AdtObjectDeletionError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::UsageCheck(_) => "edit_delete_usage_check_failed",
            Self::ObjectInUse { .. } => "edit_delete_object_in_use",
            Self::Session(_) => "edit_delete_lock_failed",
            Self::DeleteRequest { .. } => "edit_delete_request_failed",
            Self::NotDeleted { .. } => "edit_delete_not_verified",
            Self::Verification { .. } => "edit_delete_verification_failed",
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::UsageCheck(error) => error.status(),
            Self::Session(error) => error.status(),
            Self::DeleteRequest { source, .. } | Self::Verification { source, .. } => {
                sap_http_status(Some(source))
            }
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Validation(error) => return error.hint(),
            Self::UsageCheck(error) => return error.hint(),
            Self::ObjectInUse { usages, .. } => format!(
                "Deleting it would break {}. Remove those references first, or pass --force if they are already dead.",
                summarize(usages)
            ),
            Self::Session(error) => return error.hint(),
            Self::DeleteRequest {
                source,
                still_locked,
                ..
            } => {
                let mut hint = format!(
                    "The object was not deleted. {}",
                    source.hint().unwrap_or_default()
                );
                if *still_locked {
                    hint.push_str(
                        " Releasing its lock also failed, so the object is still locked: clear the lock before retrying, or the next attempt will fail on the lock rather than the original cause.",
                    );
                }
                hint
            }
            Self::NotDeleted { .. } => {
                "SAP reported success but the object is still readable. Do not retry blindly; inspect it in ADT, because a partial delete may have left it in an inconsistent state."
                    .to_owned()
            }
            Self::Verification { .. } => {
                "The delete may have succeeded. Check whether the object still exists before retrying."
                    .to_owned()
            }
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            // Show the caller exactly what is holding the object.
            Self::ObjectInUse { identity, .. } => Some(format!(
                "fractal object usages {} --direct-results",
                identity.object_uri
            )),
            Self::NotDeleted { identity, .. } | Self::Verification { identity, .. } => {
                Some(suggested_command::object_xml(&identity.object_uri))
            }
            _ => None,
        }
    }
}

/// Reports what a deletion would do, without locking or deleting anything.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation failures or a where-used
/// lookup that could not be completed.
pub async fn preview_adt_object_deletion(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &AdtObjectDeletionRequest,
) -> Result<AdtObjectDeletionPreview, AdtObjectDeletionError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    let direct_usages = direct_usages(sap, &target.identity).await?;

    Ok(AdtObjectDeletionPreview {
        would_delete: request.force || direct_usages.is_empty(),
        identity: target.identity,
        transport: target.transport,
        direct_usages,
    })
}

/// Deletes one object and proves it is gone.
///
/// Refuses when other objects still reference this one unless `force` is set.
/// The lock is released only when the delete fails: a successful delete removes
/// the object the lock was taken on, so there is nothing left to unlock.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation, a failed where-used
/// lookup, remaining references, lock failures, a rejected delete, or an object
/// that is still readable afterwards.
pub async fn delete_adt_object(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &AdtObjectDeletionRequest,
) -> Result<AdtObjectDeletionResult, AdtObjectDeletionError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    let identity = target.identity;
    let transport = target.transport;

    let direct_usages = direct_usages(sap, &identity).await?;
    if !direct_usages.is_empty() && !request.force {
        return Err(AdtObjectDeletionError::ObjectInUse {
            name: identity.name.clone(),
            identity: Box::new(identity),
            usages: direct_usages,
        });
    }

    let lock = acquire_adt_object_lock(sap, &identity.object_uri, transport.as_deref())
        .await
        .map_err(AdtObjectDeletionError::Session)?;

    let mut query = vec![("lockHandle", lock.handle())];
    if let Some(transport) = &transport {
        query.push(("corrNr", transport.as_str()));
    }
    let deleted = sap
        .delete(&identity.object_uri, &query, stateful_session_headers())
        .await;

    if let Err(source) = deleted {
        // The object still exists, so its lock still means something. The
        // delete failure stays the reported cause — a cleanup failure must not
        // mask it — but whether the lock survived is state the caller needs,
        // so it is carried rather than discarded.
        let still_locked = release_adt_object_lock(sap, &identity.object_uri, &lock)
            .await
            .is_err();
        return Err(AdtObjectDeletionError::DeleteRequest {
            identity: Box::new(identity),
            still_locked,
            source,
        });
    }

    verify_object_is_gone(sap, &identity).await?;

    Ok(AdtObjectDeletionResult {
        identity,
        transport,
        direct_usages,
        forced: request.force,
    })
}

/// Confirms the object can no longer be read.
///
/// Not-found is the success case. Anything else means the delete did not do
/// what SAP said it did, or that we cannot tell.
async fn verify_object_is_gone(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
) -> Result<(), AdtObjectDeletionError> {
    match sap.get_text(&identity.object_uri).await {
        Ok(_) => Err(AdtObjectDeletionError::NotDeleted {
            identity: Box::new(identity.clone()),
        }),
        Err(source) if source.is_not_found() => Ok(()),
        Err(source) => Err(AdtObjectDeletionError::Verification {
            identity: Box::new(identity.clone()),
            source,
        }),
    }
}

/// The objects SAP reports as genuine references, ignoring hierarchy context.
async fn direct_usages(
    sap: &mut SapClient,
    identity: &EditableAdtSourceIdentity,
) -> Result<Vec<String>, AdtObjectDeletionError> {
    let references = get_object_usages(sap, &identity.object_uri)
        .await
        .map_err(AdtObjectDeletionError::UsageCheck)?;
    Ok(references
        .iter()
        .filter(|reference| reference.direct_result)
        .map(describe_reference)
        .collect())
}

fn describe_reference(reference: &UsageReference) -> String {
    reference
        .name
        .clone()
        .unwrap_or_else(|| reference.uri.clone())
}

fn summarize(usages: &[String]) -> String {
    const SHOWN: usize = 5;
    let shown = usages.iter().take(SHOWN).cloned().collect::<Vec<_>>();
    match usages.len().checked_sub(SHOWN) {
        Some(remaining) if remaining > 0 => format!("{} and {remaining} more", shown.join(", ")),
        _ => shown.join(", "),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_a_long_reference_list_without_flooding_the_hint() {
        let many = (1..=8).map(|n| format!("ZCL_USER_{n}")).collect::<Vec<_>>();

        assert_eq!(
            summarize(&many),
            "ZCL_USER_1, ZCL_USER_2, ZCL_USER_3, ZCL_USER_4, ZCL_USER_5 and 3 more"
        );
        assert_eq!(summarize(&many[..2]), "ZCL_USER_1, ZCL_USER_2");
    }

    #[test]
    fn falls_back_to_the_uri_when_sap_omits_a_reference_name() {
        let unnamed = UsageReference {
            uri: "/sap/bc/adt/oo/classes/zcl_caller#start=1".to_owned(),
            parent_uri: None,
            name: None,
            object_type: None,
            package: None,
            direct_result: true,
        };

        assert_eq!(
            describe_reference(&unnamed),
            "/sap/bc/adt/oo/classes/zcl_caller#start=1"
        );
    }
}

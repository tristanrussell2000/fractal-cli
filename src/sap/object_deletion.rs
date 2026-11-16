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

use super::package_authorization::{PackageAuthorizationError, authorize_object_package};
use crate::config::EditPolicy;
use thiserror::Error;

use super::{
    adt_object_identity::AdtObjectIdentity,
    adt_response::parse_adt_document,
    adt_version::AdtVersion,
    class_include::ClassInclude,
    client::{SapClient, SapClientError},
    edit_session::{
        AdtEditSessionError, acquire_adt_object_lock, release_adt_object_lock,
        stateful_session_headers,
    },
    editable_source::{
        AdtEditTargetValidationError, AdtSourceReadError, EditableAdtObjectType,
        read_adt_include_source_for_edit, read_adt_source_for_edit, validate_adt_edit_target,
    },
    find_non_empty_attribute,
    metadata_document::strip_navigation_links,
    metadata_object::MetadataAdtObjectType,
    object_family::AdtObjectFamily,
    object_usages::{ObjectUsagesError, UsageReference, get_object_usages},
    package_authorization::package_of_object_xml,
};
use crate::journal::JournalError;
use crate::journal::entry::{JournalEntry, RecordedObject, Stream};
use crate::journal::recorder::{AfterImages, Journal, Resolution, resolve};
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
    pub identity: AdtObjectIdentity,
    pub transport: Option<String>,
    pub direct_usages: Vec<String>,
    pub would_delete: bool,
}

/// A deletion that has been carried out and verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtObjectDeletionResult {
    /// The object is gone and its journal entry could not be completed, so this
    /// names the entry left at `pending`. It still holds the content, which is
    /// the part that matters.
    pub journal_entry_incomplete: Option<String>,
    pub identity: AdtObjectIdentity,
    pub transport: Option<String>,
    pub direct_usages: Vec<String>,
    pub forced: bool,
    /// The object is gone but its lock could not be released, so the name
    /// stays blocked for anyone recreating it until SAP's session times out.
    pub name_still_locked: bool,
}

/// A failure while deleting an editable ADT object.
#[derive(Debug, Error)]
pub enum AdtObjectDeletionError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
    #[error("could not determine what references this object: {0}")]
    UsageCheck(#[source] ObjectUsagesError),
    #[error("{name} is still referenced by {} object(s)", usages.len())]
    ObjectInUse {
        identity: Box<AdtObjectIdentity>,
        name: String,
        usages: Vec<String>,
    },
    #[error("ADT edit session failed while deleting: {0}")]
    Session(#[source] AdtEditSessionError),
    /// SAP refuses to delete a binding whose service is still published.
    #[error("{} cannot be deleted while its service is published", identity.name)]
    BindingPublished {
        identity: Box<AdtObjectIdentity>,
        #[source]
        source: SapClientError,
    },
    #[error("the ADT delete request failed: {source}")]
    DeleteRequest {
        identity: Box<AdtObjectIdentity>,
        #[source]
        source: SapClientError,
    },
    #[error("SAP accepted the delete request, but the object still exists")]
    NotDeleted { identity: Box<AdtObjectIdentity> },
    #[error("SAP accepted the delete request, but its result could not be verified: {source}")]
    Verification {
        identity: Box<AdtObjectIdentity>,
        #[source]
        source: SapClientError,
    },
    /// The journal could not be written. Fatal, because this is the only copy
    /// of what is about to be destroyed.
    #[error(transparent)]
    Journal(#[from] JournalError),
    /// The content could not be read, so there would be no before-image.
    #[error("could not read {name} before deleting it: {source}")]
    ContentUnreadable {
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("could not read the source of {name} before deleting it: {source}")]
    SourceUnreadable {
        name: String,
        #[source]
        source: super::editable_source::AdtSourceReadError,
    },
    /// The operation failed and its lock could not be released. Wraps the
    /// cause, so the reported code, status, and message are unchanged.
    #[error(transparent)]
    AbandonedLock(Box<Self>),
}

impl ReportableError for AdtObjectDeletionError {
    fn code(&self) -> &'static str {
        match self {
            Self::AbandonedLock(primary) => primary.code(),
            Self::Validation(error) => error.code(),
            Self::PackageNotAllowed(error) => error.code(),
            Self::UsageCheck(_) => "edit_delete_usage_check_failed",
            Self::ObjectInUse { .. } => "edit_delete_object_in_use",
            Self::Session(_) => "edit_delete_lock_failed",
            Self::BindingPublished { .. } => "edit_delete_binding_published",
            Self::DeleteRequest { .. } => "edit_delete_request_failed",
            Self::NotDeleted { .. } => "edit_delete_not_verified",
            Self::Verification { .. } => "edit_delete_verification_failed",
            Self::Journal(error) => error.code(),
            Self::ContentUnreadable { .. } | Self::SourceUnreadable { .. } => {
                "edit_delete_content_unreadable"
            }
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::AbandonedLock(primary) => primary.status(),
            Self::UsageCheck(error) => error.status(),
            Self::PackageNotAllowed(error) => error.status(),
            Self::Session(error) => error.status(),
            Self::DeleteRequest { source, .. }
            | Self::BindingPublished { source, .. }
            | Self::Verification { source, .. } => sap_http_status(Some(source)),
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Validation(error) => return error.hint(),
            Self::PackageNotAllowed(error) => return error.hint(),
            Self::UsageCheck(error) => return error.hint(),
            Self::ObjectInUse { usages, .. } => format!(
                "Deleting it would break {}. Remove those references first, or pass --force if they are already dead.",
                summarize(usages)
            ),
            Self::Session(error) => return error.hint(),
            Self::AbandonedLock(primary) => format!(
                "{} Releasing its lock also failed, so the object is still locked: clear the lock before retrying, or the next attempt will fail on the lock rather than the original cause.",
                primary.hint().unwrap_or_default()
            ),
            Self::BindingPublished { identity, .. } => format!(
                "The binding was not deleted. Unpublish it first with `fractal binding unpublish --name {}`, then delete it.",
                identity.name
            ),
            Self::DeleteRequest { source, .. } => format!(
                "The object was not deleted. {}",
                source.hint().unwrap_or_default()
            ),
            Self::NotDeleted { .. } => {
                "SAP reported success but the object is still readable. Do not retry blindly; inspect it in ADT, because a partial delete may have left it in an inconsistent state."
                    .to_owned()
            }
            Self::Journal(error) => return error.hint(),
            Self::ContentUnreadable { .. } | Self::SourceUnreadable { .. } => {
                "Nothing was deleted. The journal keeps the only copy of what a delete destroys, so a delete whose content cannot be read is refused; pass --no-journal to delete without that safety net."
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
            Self::AbandonedLock(primary) => primary.suggested_command(),
            Self::PackageNotAllowed(error) => error.suggested_command(),
            // Show the caller exactly what is holding the object.
            Self::ObjectInUse { identity, .. } => Some(format!(
                "fractal object usages {} --direct-results",
                identity.object_uri
            )),
            Self::NotDeleted { identity, .. } | Self::Verification { identity, .. } => {
                Some(suggested_command::object_xml(&identity.object_uri))
            }
            Self::BindingPublished { identity, .. } => {
                Some(suggested_command::binding_show(&identity.name))
            }
            _ => None,
        }
    }
}

/// SAP answers 412 with "Service endpoint of Service Binding … is published".
/// The status alone is a generic precondition failure, so the wording is
/// checked too.
fn is_published_binding_refusal(identity: &AdtObjectIdentity, source: &SapClientError) -> bool {
    matches!(
        identity.object_type,
        AdtObjectFamily::Metadata(MetadataAdtObjectType::ServiceBinding)
    ) && matches!(
        source,
        SapClientError::Http { status, message, .. }
            if status.as_u16() == 412 && message.to_ascii_lowercase().contains("is published")
    )
}

/// Reports what a deletion would do, without locking or deleting anything.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation failures or a where-used
/// lookup that could not be completed.
pub async fn preview_adt_object_deletion(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &AdtObjectDeletionRequest,
) -> Result<AdtObjectDeletionPreview, AdtObjectDeletionError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        policy,
        request.transport.as_deref(),
    )?;
    preview_validated_deletion(
        sap,
        policy,
        target.identity.into(),
        target.transport,
        request.force,
    )
    .await
}

/// [`preview_adt_object_deletion`] for an object whose identity is already
/// established, whatever family it belongs to.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] when the where-used lookup fails.
pub async fn preview_validated_deletion(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object: AdtObjectIdentity,
    transport: Option<String>,
    force: bool,
) -> Result<AdtObjectDeletionPreview, AdtObjectDeletionError> {
    authorize_object_package(sap, policy, &object.name, &object.object_uri).await?;
    let direct_usages = direct_usages(sap, &object).await?;

    Ok(AdtObjectDeletionPreview {
        would_delete: force || direct_usages.is_empty(),
        identity: object,
        transport,
        direct_usages,
    })
}

/// Deletes one object and proves it is gone.
///
/// Refuses when other objects still reference this one unless `force` is set.
/// The lock is released whether or not the delete succeeds. SAP's lock outlives
/// the object it was taken on: without the release, recreating the name is
/// refused with "User … is currently editing" until the session times out.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for validation, a failed where-used
/// lookup, remaining references, lock failures, a rejected delete, or an object
/// that is still readable afterwards.
pub async fn delete_adt_object(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &AdtObjectDeletionRequest,
    journal: Option<&Journal>,
) -> Result<AdtObjectDeletionResult, AdtObjectDeletionError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        policy,
        request.transport.as_deref(),
    )?;
    delete_validated_adt_object(
        sap,
        policy,
        target.identity.into(),
        target.transport,
        request.force,
        journal,
    )
    .await
}

/// [`delete_adt_object`] for an object whose identity and transport are already
/// established.
///
/// Validation is the only part of deleting that differs between object
/// families — namespace rules, transport rules, and how a URI is built. Once
/// those have been settled, every family is deleted the same way, so this is
/// the whole destructive path and the guards live here rather than being
/// reimplemented per family.
///
/// # Errors
///
/// Returns [`AdtObjectDeletionError`] for a failed where-used lookup, remaining
/// references, lock failures, a rejected delete, or an object that is still
/// readable afterwards.
pub async fn delete_validated_adt_object(
    sap: &mut SapClient,
    policy: &EditPolicy,
    identity: AdtObjectIdentity,
    transport: Option<String>,
    force: bool,
    journal: Option<&Journal>,
) -> Result<AdtObjectDeletionResult, AdtObjectDeletionError> {
    authorize_object_package(sap, policy, &identity.name, &identity.object_uri).await?;
    let direct_usages = direct_usages(sap, &identity).await?;
    if !direct_usages.is_empty() && !force {
        return Err(AdtObjectDeletionError::ObjectInUse {
            name: identity.name.clone(),
            identity: Box::new(identity),
            usages: direct_usages,
        });
    }

    let lock = acquire_adt_object_lock(sap, &identity.object_uri, transport.as_deref())
        .await
        .map_err(AdtObjectDeletionError::Session)?;

    // The before-image is read here and nowhere earlier: under the lock the
    // delete already holds, so what is recorded is what is destroyed rather
    // than what the object looked like a moment before somebody else touched
    // it. A failure to record it stops the delete, because this is the only
    // copy there will ever be.
    let entry = match journal {
        Some(journal) => match record_deletion(sap, journal, &identity, transport.as_deref()).await
        {
            Ok(entry) => Some(entry),
            Err(failure) => {
                return Err(release_lock_and_report(sap, &identity, &lock, failure).await);
            }
        },
        None => None,
    };

    let mut query = vec![("lockHandle", lock.handle())];
    if let Some(transport) = &transport {
        query.push(("corrNr", transport.as_str()));
    }
    let deleted = sap
        .delete(&identity.object_uri, &query, stateful_session_headers())
        .await;

    if let Err(source) = deleted {
        // Deliberately ignored: a cleanup failure must not replace the cause,
        // and the delete is the failure worth reporting.
        let _ = resolve(journal, entry, Resolution::Failed);
        // The object still exists, so its lock still means something. The
        // delete failure stays the reported cause — a cleanup failure must not
        // mask it — but whether the lock survived is state the caller needs,
        // so it is carried rather than discarded.
        let abandoned_lock = release_adt_object_lock(sap, &identity.object_uri, &lock)
            .await
            .is_err();
        let failure = if is_published_binding_refusal(&identity, &source) {
            AdtObjectDeletionError::BindingPublished {
                identity: Box::new(identity),
                source,
            }
        } else {
            AdtObjectDeletionError::DeleteRequest {
                identity: Box::new(identity),
                source,
            }
        };
        return Err(if abandoned_lock {
            AdtObjectDeletionError::AbandonedLock(Box::new(failure))
        } else {
            failure
        });
    }

    // The lock outlives the object it was taken on, and holds the name.
    let name_still_locked = release_adt_object_lock(sap, &identity.object_uri, &lock)
        .await
        .is_err();
    let gone = verify_object_is_gone(sap, &identity).await;
    // The object is gone either way the read-back landed, so the entry is
    // resolved before the result is reported.
    let nothing_after = AfterImages::new();
    let journal_entry_incomplete = resolve(
        journal,
        entry,
        if gone.is_ok() {
            Resolution::Succeeded(&nothing_after)
        } else {
            Resolution::Failed
        },
    );
    gone?;

    Ok(AdtObjectDeletionResult {
        journal_entry_incomplete,
        identity,
        transport,
        direct_usages,
        forced: force,
        name_still_locked,
    })
}

/// Records what is about to be destroyed, read under the delete's own lock.
///
/// The content is the object itself: source for a source object, the whole
/// document for a metadata one. Package and description come with it, because
/// recreating the object needs both and neither survives the delete.
async fn record_deletion(
    sap: &SapClient,
    journal: &Journal,
    identity: &AdtObjectIdentity,
    transport: Option<&str>,
) -> Result<JournalEntry, AdtObjectDeletionError> {
    let (content, metadata) = deletion_content(sap, identity).await?;
    let (package, description) = describe(&metadata);
    let includes = class_include_content(sap, identity).await?;

    Ok(journal.begin_delete(
        RecordedObject::for_family(
            identity.object_type,
            identity.name.clone(),
            identity.object_uri.clone(),
            |stream| match stream {
                Stream::Whole | Stream::Include(ClassInclude::Main) => Some(content.clone()),
                Stream::Include(include) => includes
                    .iter()
                    .find(|(candidate, _)| *candidate == include)
                    .and_then(|(_, content)| content.clone()),
            },
        ),
        package,
        description,
        transport.map(str::to_owned),
    )?)
}

/// A class's includes other than `main`, as they stand: the local types a
/// recreated class would otherwise silently lose. Empty for anything else.
///
/// An include the class does not have is recorded as absent; any other read
/// failure stops the delete, because this is the only copy there will ever be.
async fn class_include_content(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
) -> Result<Vec<(ClassInclude, Option<String>)>, AdtObjectDeletionError> {
    let AdtObjectFamily::Source(object_type @ EditableAdtObjectType::Class) = identity.object_type
    else {
        return Ok(Vec::new());
    };
    let mut includes = Vec::new();
    for include in ClassInclude::ALL {
        if include == ClassInclude::Main {
            continue;
        }
        let content = match read_adt_include_source_for_edit(
            sap,
            object_type,
            &identity.name,
            include,
            AdtVersion::Active,
        )
        .await
        {
            Ok(read) => Some(read.snapshot.source),
            Err(AdtSourceReadError::IncludeNotFound { .. }) => None,
            Err(source) => {
                return Err(AdtObjectDeletionError::SourceUnreadable {
                    name: identity.name.clone(),
                    source,
                });
            }
        };
        includes.push((include, content));
    }
    Ok(includes)
}

/// The content to keep, and the document to read the object's own fields from.
///
/// For a metadata object those are the same read: the document *is* the object.
/// A source object needs both, and the two are different resources.
///
/// Both name the active version. This content is the journal's restore image,
/// and a read naming no version is served somebody's pending edit whenever one
/// exists — which was never what ran.
async fn deletion_content(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
) -> Result<(String, String), AdtObjectDeletionError> {
    let unreadable = |source| AdtObjectDeletionError::ContentUnreadable {
        name: identity.name.clone(),
        source,
    };
    match identity.object_type {
        AdtObjectFamily::Source(object_type) => {
            let source =
                read_adt_source_for_edit(sap, object_type, &identity.name, AdtVersion::Active)
                    .await
                    .map_err(|source| AdtObjectDeletionError::SourceUnreadable {
                        name: identity.name.clone(),
                        source,
                    })?;
            let metadata = sap
                .get_text_with_query(
                    &identity.object_uri,
                    &[("version", AdtVersion::Active.as_str())],
                )
                .await
                .map_err(unreadable)?;
            Ok((source.snapshot.source, metadata))
        }
        AdtObjectFamily::Metadata(_) => {
            let document = sap
                .get_text_with_query(
                    &identity.object_uri,
                    &[("version", AdtVersion::Active.as_str())],
                )
                .await
                .map_err(unreadable)?;
            let document = strip_navigation_links(&document);
            Ok((document.clone(), document))
        }
    }
}

/// The package and description a restore needs, from the object's own document.
fn describe(metadata: &str) -> (Option<String>, Option<String>) {
    let package = package_of_object_xml(metadata).ok().flatten();
    let description = parse_adt_document(metadata)
        .ok()
        .and_then(|document| find_non_empty_attribute(document.root_element(), "description"));
    (package, description)
}

/// Releases the lock a refused delete still holds, keeping the original cause.
async fn release_lock_and_report(
    sap: &mut SapClient,
    identity: &AdtObjectIdentity,
    lock: &super::edit_session::AdtObjectLock,
    failure: AdtObjectDeletionError,
) -> AdtObjectDeletionError {
    if release_adt_object_lock(sap, &identity.object_uri, lock)
        .await
        .is_err()
    {
        AdtObjectDeletionError::AbandonedLock(Box::new(failure))
    } else {
        failure
    }
}

/// Confirms the object can no longer be read.
///
/// Not-found is the success case. Anything else means the delete did not do
/// what SAP said it did, or that we cannot tell.
async fn verify_object_is_gone(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
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
    identity: &AdtObjectIdentity,
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

    fn binding_identity() -> AdtObjectIdentity {
        AdtObjectIdentity {
            object_type: AdtObjectFamily::Metadata(MetadataAdtObjectType::ServiceBinding),
            name: "ZSAMPLE_UI_O4".to_owned(),
            object_uri: "/sap/bc/adt/businessservices/bindings/zsample_ui_o4".to_owned(),
            source_uri: None,
        }
    }

    fn http(status: u16, message: &str) -> SapClientError {
        SapClientError::Http {
            kind: crate::sap::client::SapHttpErrorKind::Other,
            status: reqwest::StatusCode::from_u16(status).unwrap(),
            url: "https://sap.example/x".to_owned(),
            message: message.to_owned(),
        }
    }

    #[test]
    fn recognises_sap_refusing_to_delete_a_published_binding() {
        let published = http(
            412,
            "Service endpoint of Service Binding ZSAMPLE_UI_O4 is published.",
        );
        assert!(is_published_binding_refusal(
            &binding_identity(),
            &published
        ));
        // A 412 is a generic precondition failure; only the wording makes it this one.
        assert!(!is_published_binding_refusal(
            &binding_identity(),
            &http(412, "Precondition failed")
        ));

        let error = AdtObjectDeletionError::BindingPublished {
            identity: Box::new(binding_identity()),
            source: published,
        };
        assert_eq!(error.code(), "edit_delete_binding_published");
        assert_eq!(error.status(), Some(412));
        assert!(
            error
                .hint()
                .unwrap()
                .contains("fractal binding unpublish --name ZSAMPLE_UI_O4")
        );
        assert_eq!(
            error.suggested_command().as_deref(),
            Some("fractal binding show --name ZSAMPLE_UI_O4")
        );
    }

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

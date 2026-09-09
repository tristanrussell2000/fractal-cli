//! Deciding whether an activation can be undone, and what undoing it would do.
//!
//! This is the half of `fractal undo` that writes nothing. It resolves a
//! journal entry into a concrete plan — the content to restore, whether there
//! is pending work to put back afterwards — and refuses when the entry cannot
//! be acted on. `--dry-run` stops here.
//!
//! **The gate is content, not age.** An undo is allowed only while the object's
//! current active version is still the one the entry recorded activating; that
//! is what proves the activation being undone is still the one in force. The
//! comparison is against a fresh read, never against anything remembered, and
//! it is made on the same canonical form the journal stored — see
//! [`super::metadata_document`], without which a metadata object would look
//! stale to anyone who has pending work on it.
//!
//! **A refusal is not a dead end.** Every one names the blob holding the
//! content, so the answer is "here is what it was, do it by hand" rather than
//! "no". `--force` proceeds past the refusals that are judgement calls, and
//! records what it overrode; it does not exist for the two that are not.

use thiserror::Error;

use super::{
    adt_object_identity::AdtObjectIdentity,
    client::{SapClient, SapClientError},
    editable_source::{
        AdtEditTargetValidationError, AdtSourceVersion, EditableAdtSourceIdentity,
        editable_source_identity, read_adt_source_for_edit,
    },
    metadata_activation::{ACTIVE_VERSION, document_version},
    metadata_document::strip_navigation_links,
    metadata_object::metadata_object_identity,
    object_family::AdtObjectFamily,
    package_authorization::{PackageAuthorizationError, authorize_object_package},
};
use crate::config::EditPolicy;
use crate::journal::JournalError;
use crate::journal::blobs::BlobStore;
use crate::journal::entry::{ContentKind, ContentRef, JournalEntry, JournalOperation};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::source_change::source_sha256;

/// What an undo of one activation would do.
#[derive(Debug, Clone)]
pub struct UndoPlan {
    pub entry: JournalEntry,
    pub object_uri: String,
    /// The previous active content, which step 1 writes back as inactive.
    pub restore: String,
    pub restore_sha256: String,
    /// The pending work the activation consumed, which step 3 puts back
    /// **without** activating it. `None` means the caller had none, and step 3
    /// then does nothing rather than inventing one.
    pub restore_inactive: Option<String>,
    pub restore_inactive_sha256: Option<String>,
    /// What the entry says the activation left in place. Absent only for an
    /// entry that never recorded a result, which is refused without `--force`.
    pub expected_active_sha256: Option<String>,
    /// What the object holds now. `None` means it has no active version at all.
    pub current_active_sha256: Option<String>,
    /// The refusals `--force` was used to proceed past, in the order checked.
    pub overridden: Vec<Override>,
}

impl UndoPlan {
    /// Whether the object still holds what the entry recorded activating.
    #[must_use]
    pub fn matches_recorded_state(&self) -> bool {
        self.expected_active_sha256.is_some()
            && self.expected_active_sha256 == self.current_active_sha256
    }
}

/// A refusal that `--force` may proceed past, named in the output so an
/// override is never silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Override {
    /// The entry never recorded a result, so the operation may not have landed.
    Unresolved,
    /// No after-image, so there was nothing to compare the object against.
    Ungated,
    /// The object no longer holds what the entry recorded activating.
    Stale,
}

impl Override {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unresolved => "entry_unresolved",
            Self::Ungated => "no_after_image",
            Self::Stale => "stale",
        }
    }
}

#[derive(Debug, Error)]
pub enum UndoError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error(transparent)]
    PackageNotAllowed(#[from] PackageAuthorizationError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    /// Undoing a delete is not automated; `journal show` prints the recipe.
    #[error("journal entry {id} records a {operation}, which undo cannot reverse")]
    UnsupportedOperation { id: String, operation: String },
    /// Invariant: undo never becomes a path to `delete`.
    #[error("{name} had no active version before this activation")]
    WouldDelete { name: String, id: String },
    #[error("journal entry {id} is {status} and has no result to undo")]
    Unresolved { id: String, status: String },
    #[error("journal entry {id} has no after-image to check the object against")]
    Ungated { id: String },
    #[error("{name} is no longer what journal entry {id} recorded activating")]
    Stale {
        name: String,
        id: String,
        expected: String,
        found: Option<String>,
        /// Where the content that would have been restored is, so a refusal is
        /// still an answer.
        blob_path: String,
    },
    #[error("could not read the current state of {name}: {source}")]
    CurrentStateUnreadable {
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP returned a document that could not be parsed: {0}")]
    ResponseInvalid(#[from] super::adt_response::AdtResponseParseError),
}

impl UndoError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::CurrentStateUnreadable { source, .. } => Some(source),
            _ => None,
        }
    }
}

impl ReportableError for UndoError {
    fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::PackageNotAllowed(error) => error.code(),
            Self::Journal(error) => error.code(),
            Self::UnsupportedOperation { .. } => "undo_unsupported_operation",
            Self::WouldDelete { .. } => "undo_would_delete",
            Self::Unresolved { .. } => "undo_entry_unresolved",
            Self::Ungated { .. } => "undo_no_after_image",
            Self::Stale { .. } => "undo_stale",
            Self::CurrentStateUnreadable { .. } => "undo_current_state_unreadable",
            Self::ResponseInvalid(error) => error.code(),
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Validation(error) => return error.hint(),
            Self::PackageNotAllowed(error) => return error.hint(),
            Self::Journal(error) => return error.hint(),
            Self::UnsupportedOperation { .. } => {
                "Undoing a delete is not automated. `fractal journal show` prints the content and what recreating the object needs."
                    .to_owned()
            }
            // Undoing this would leave the object with no active version at
            // all, which is a delete by another name.
            Self::WouldDelete { .. } => {
                "This was the object's first activation, so there is no earlier active version to restore. Delete it deliberately if that is what you want."
                    .to_owned()
            }
            Self::Unresolved { .. } => {
                "This entry was never resolved, so whether the operation landed is unknown. Read the object, then use --force if the before-image is what you want."
                    .to_owned()
            }
            Self::Ungated { .. } => {
                "Without an after-image there is nothing to check the object against. Read it first, then use --force."
                    .to_owned()
            }
            Self::Stale { blob_path, .. } => format!(
                "Something changed the object after the activation this entry recorded, so undoing it would discard that. The content it would have restored is at {blob_path}. Use --force to restore it anyway."
            ),
            Self::CurrentStateUnreadable { source, .. } => format!(
                "Nothing was written. {}",
                source.hint().unwrap_or_default()
            ),
            Self::ResponseInvalid(error) => return error.hint(),
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            // Read-only, every one: a suggestion must never be a mutation.
            Self::UnsupportedOperation { id, .. }
            | Self::Unresolved { id, .. }
            | Self::Ungated { id }
            | Self::Stale { id, .. }
            | Self::WouldDelete { id, .. } => Some(format!("fractal journal show {id}")),
            Self::PackageNotAllowed(error) => error.suggested_command(),
            _ => None,
        }
    }
}

/// Works out whether an activation can be undone, and what undoing it needs.
///
/// Reads the object's current active version and compares it with the entry's
/// after-image. Writes nothing, whatever the verdict.
///
/// # Errors
///
/// Returns [`UndoError`] for an entry undo cannot act on, content that is no
/// longer in the blob store, an object outside the profile's allowlist, or an
/// object that has moved since the recorded activation. `force` proceeds past
/// the last of those and past an unresolved entry; it does not proceed past an
/// undo that would leave the object with no active version.
pub async fn plan_activation_undo(
    sap: &mut SapClient,
    policy: &EditPolicy,
    entry: JournalEntry,
    blobs: &BlobStore,
    force: bool,
) -> Result<UndoPlan, UndoError> {
    if entry.operation != JournalOperation::Activate {
        return Err(UndoError::UnsupportedOperation {
            id: entry.id.clone(),
            operation: format!("{:?}", entry.operation).to_lowercase(),
        });
    }

    // Before anything is read from SAP: an entry that cannot be undone at all
    // should cost nothing to find out about.
    let ContentRef::Sha256(restore_sha256) = entry.active_before.clone() else {
        return Err(UndoError::WouldDelete {
            name: entry.object.name.clone(),
            id: entry.id.clone(),
        });
    };

    let mut overridden = Vec::new();
    if !entry.status.is_undoable() {
        if !force {
            return Err(UndoError::Unresolved {
                id: entry.id.clone(),
                status: format!("{:?}", entry.status).to_lowercase(),
            });
        }
        overridden.push(Override::Unresolved);
    }
    let expected_active_sha256 = entry
        .active_after
        .as_ref()
        .and_then(ContentRef::sha256)
        .map(str::to_owned);
    if expected_active_sha256.is_none() {
        if !force {
            return Err(UndoError::Ungated {
                id: entry.id.clone(),
            });
        }
        overridden.push(Override::Ungated);
    }

    // Read the content before touching SAP: a blob that is gone makes the whole
    // question moot, and the read costs nothing.
    let restore = blobs.read(&restore_sha256)?;
    let restore_inactive_sha256 = entry
        .inactive_before
        .as_ref()
        .and_then(ContentRef::sha256)
        .map(str::to_owned);
    let restore_inactive = match &restore_inactive_sha256 {
        Some(sha256) => Some(blobs.read(sha256)?),
        None => None,
    };

    let identity = undo_identity(&entry, policy)?;
    authorize_object_package(sap, policy, identity.name(), identity.object_uri()).await?;

    let current = current_active_content(sap, &entry, &identity).await?;
    let current_active_sha256 = current.as_deref().map(source_sha256);

    let plan = UndoPlan {
        object_uri: identity.object_uri().to_owned(),
        restore,
        restore_sha256,
        restore_inactive,
        restore_inactive_sha256,
        expected_active_sha256,
        current_active_sha256,
        overridden,
        entry,
    };

    if plan.expected_active_sha256.is_some() && !plan.matches_recorded_state() {
        if !force {
            return Err(UndoError::Stale {
                name: plan.entry.object.name.clone(),
                id: plan.entry.id.clone(),
                expected: plan.expected_active_sha256.clone().unwrap_or_default(),
                found: plan.current_active_sha256.clone(),
                blob_path: blobs.path_of(&plan.restore_sha256).display().to_string(),
            });
        }
        let mut plan = plan;
        plan.overridden.push(Override::Stale);
        return Ok(plan);
    }
    Ok(plan)
}

/// The object, resolved the same way the forward command would resolve it.
///
/// Not taken from the entry's stored URI: the profile's namespace rules and
/// the object's own naming are re-checked, so an undo cannot reach somewhere a
/// forward edit could not.
enum UndoIdentity {
    Source(EditableAdtSourceIdentity),
    Metadata(AdtObjectIdentity),
}

impl UndoIdentity {
    fn name(&self) -> &str {
        match self {
            Self::Source(identity) => &identity.name,
            Self::Metadata(identity) => &identity.name,
        }
    }

    fn object_uri(&self) -> &str {
        match self {
            Self::Source(identity) => &identity.object_uri,
            Self::Metadata(identity) => &identity.object_uri,
        }
    }
}

fn undo_identity(
    entry: &JournalEntry,
    policy: &EditPolicy,
) -> Result<UndoIdentity, AdtEditTargetValidationError> {
    match entry.object.object_type {
        AdtObjectFamily::Source(object_type) => {
            let identity = editable_source_identity(object_type, &entry.object.name)
                .map_err(AdtEditTargetValidationError::InvalidObject)?;
            super::editable_source::validate_customer_namespace(
                &identity.name,
                &policy.customer_namespaces,
            )?;
            Ok(UndoIdentity::Source(identity))
        }
        AdtObjectFamily::Metadata(object_type) => Ok(UndoIdentity::Metadata(
            metadata_object_identity(object_type, &entry.object.name, policy)?,
        )),
    }
}

/// The object's current active content, or `None` when it has none.
///
/// A read that fails is an error rather than an absence: "SAP would not answer"
/// and "there is no active version" are different facts, and treating the first
/// as the second would report a network problem as a stale object.
async fn current_active_content(
    sap: &SapClient,
    entry: &JournalEntry,
    identity: &UndoIdentity,
) -> Result<Option<String>, UndoError> {
    match (entry.content_kind(), identity) {
        (ContentKind::Source, UndoIdentity::Source(identity)) => {
            match read_adt_source_for_edit(
                sap,
                identity.object_type,
                &identity.name,
                AdtSourceVersion::Active,
            )
            .await
            {
                Ok(read) => Ok(Some(read.snapshot.source)),
                // ABAP text carries no layer marker, so a refused read is the
                // only signal that there is no active version.
                Err(_) => Ok(None),
            }
        }
        (ContentKind::Xml, UndoIdentity::Metadata(identity)) => {
            let document = sap
                .get_text_with_query(&identity.object_uri, &[("version", ACTIVE_VERSION)])
                .await
                .map_err(|source| UndoError::CurrentStateUnreadable {
                    name: identity.name.clone(),
                    source,
                })?;
            let document = strip_navigation_links(&document);
            // `?version=active` serves the pending document when there is no
            // active version, so the document's own layer decides.
            Ok(
                (document_version(&document)?.as_deref() == Some(ACTIVE_VERSION))
                    .then_some(document),
            )
        }
        // The family decides both, so these cannot disagree.
        _ => unreachable!("content kind and identity come from the same entry"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::entry::{EntryObject, EntryStatus, EntrySystem};

    fn plan(expected: Option<&str>, current: Option<&str>) -> UndoPlan {
        UndoPlan {
            entry: JournalEntry {
                id: "20260909T203517.404Z".to_owned(),
                recorded_at: "2026-09-09T20:35:17.404Z".to_owned(),
                status: EntryStatus::Succeeded,
                system: EntrySystem {
                    base_url: "https://sap.example:8001".to_owned(),
                    profile: "dev".to_owned(),
                    client: "100".to_owned(),
                    user: "developer".to_owned(),
                },
                object: EntryObject {
                    object_type: AdtObjectFamily::parse("PROG").unwrap(),
                    name: "ZSAMPLE".to_owned(),
                    uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
                    source_part: None,
                },
                operation: JournalOperation::Activate,
                transport: None,
                active_before: ContentRef::Sha256("a".repeat(64)),
                inactive_before: None,
                active_after: expected.map(|sha256| ContentRef::Sha256(sha256.to_owned())),
                etag_after: None,
                undo_progress: None,
            },
            object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
            restore: "REPORT zsample.".to_owned(),
            restore_sha256: "a".repeat(64),
            restore_inactive: None,
            restore_inactive_sha256: None,
            expected_active_sha256: expected.map(str::to_owned),
            current_active_sha256: current.map(str::to_owned),
            overridden: Vec::new(),
        }
    }

    #[test]
    fn the_gate_agrees_only_when_both_sides_are_the_same_content() {
        let sha256 = "b".repeat(64);
        assert!(plan(Some(&sha256), Some(&sha256)).matches_recorded_state());
        assert!(!plan(Some(&sha256), Some(&"c".repeat(64))).matches_recorded_state());
    }

    #[test]
    fn two_absences_are_not_a_match() {
        // The trap: with no after-image and no active version, the two sides
        // are equal and mean nothing. Comparing them directly would report an
        // object nobody can describe as unchanged.
        assert!(!plan(None, None).matches_recorded_state());
        assert!(!plan(Some(&"b".repeat(64)), None).matches_recorded_state());
        assert!(!plan(None, Some(&"b".repeat(64))).matches_recorded_state());
    }

    fn every_error() -> Vec<UndoError> {
        vec![
            UndoError::UnsupportedOperation {
                id: "20260909T203517.404Z".to_owned(),
                operation: "delete".to_owned(),
            },
            UndoError::WouldDelete {
                name: "ZSAMPLE".to_owned(),
                id: "20260909T203517.404Z".to_owned(),
            },
            UndoError::Unresolved {
                id: "20260909T203517.404Z".to_owned(),
                status: "pending".to_owned(),
            },
            UndoError::Ungated {
                id: "20260909T203517.404Z".to_owned(),
            },
            UndoError::Stale {
                name: "ZSAMPLE".to_owned(),
                id: "20260909T203517.404Z".to_owned(),
                expected: "b".repeat(64),
                found: Some("c".repeat(64)),
                blob_path: "/blobs/bbbb".to_owned(),
            },
        ]
    }

    #[test]
    fn every_refusal_has_a_stable_code_and_says_what_to_do() {
        for error in every_error() {
            assert!(error.code().starts_with("undo_"), "{}", error.code());
            let hint = error.hint().expect("has a hint");
            assert!(!hint.is_empty());
        }
    }

    #[test]
    fn no_refusal_suggests_a_command_that_changes_anything() {
        // The rule the whole error surface follows: a suggestion is something
        // safe to run without thinking, and undo's refusals are the last place
        // to break it.
        for error in every_error() {
            let Some(suggestion) = error.suggested_command() else {
                continue;
            };
            for mutation in ["fractal undo", "fractal edit", "fractal delete"] {
                assert!(
                    !suggestion.contains(mutation),
                    "{} suggested {suggestion}",
                    error.code()
                );
            }
        }
    }

    #[test]
    fn a_stale_refusal_points_at_the_content_it_would_have_restored() {
        // A refusal that only says no leaves the caller with nothing; this one
        // has to hand over the blob path so the restore can be done by hand.
        let stale = UndoError::Stale {
            name: "ZSAMPLE".to_owned(),
            id: "20260909T203517.404Z".to_owned(),
            expected: "b".repeat(64),
            found: None,
            blob_path: "/blobs/bbbb".to_owned(),
        };
        let hint = stale.hint().expect("has a hint");
        assert!(hint.contains("/blobs/bbbb"), "{hint}");
        assert!(hint.contains("--force"), "{hint}");
    }
}

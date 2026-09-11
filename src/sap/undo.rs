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
        AdtEditTargetValidationError, AdtSourceVersion, EditableAdtObjectType,
        editable_source_identity, read_adt_source_for_edit,
    },
    metadata_activation::{
        ACTIVE_VERSION, MetadataObjectActivationError, MetadataObjectActivationRequest,
        activate_metadata_object, document_version,
    },
    metadata_document::strip_navigation_links,
    metadata_object::{
        MetadataAdtObjectType, MetadataObjectWriteError, metadata_object_identity,
        write_metadata_object,
    },
    object_family::AdtObjectFamily,
    package_authorization::{PackageAuthorizationError, authorize_object_package},
    source_activation::{
        AdtSourceActivationError, AdtSourceActivationRequest, activate_adt_source,
    },
    source_check::{AdtInactiveSourceProbeError, probe_inactive_adt_source},
    source_replace::{
        AdtSourceReplacementError, AdtSourceReplacementRequest, replace_adt_source_atomically,
    },
};
use crate::config::EditPolicy;
use crate::journal::JournalError;
use crate::journal::blobs::BlobStore;
use crate::journal::entry::{ActivationUndoStep, ContentRef, EntryStatus, JournalEntry};
use crate::journal::recorder::Journal;
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::source_change::{SourceChangePlanError, source_sha256};

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
    /// Whether pending work would be **lost**. An undo overwrites the inactive
    /// layer, so pending work is at risk — but only when it is not one of the
    /// two versions the undo leaves behind.
    pub pending_work_at_risk: bool,
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
    /// Pending work exists now that step 1 would overwrite.
    PendingWork,
    /// The entry had already been undone.
    AlreadyUndone,
}

impl Override {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unresolved => "entry_unresolved",
            Self::Ungated => "no_after_image",
            Self::Stale => "stale",
            Self::PendingWork => "pending_work",
            Self::AlreadyUndone => "already_undone",
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
    #[error("journal entry {id} has already been undone")]
    AlreadyUndone { id: String, name: String },
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
    #[error("{name} has pending changes that undoing would replace")]
    PendingWork { name: String, id: String },
    #[error("could not establish whether {name} has pending changes: {source}")]
    PendingWorkProbe {
        name: String,
        #[source]
        source: AdtInactiveSourceProbeError,
    },
    #[error("could not read the current state of {name}: {source}")]
    CurrentStateUnreadable {
        name: String,
        #[source]
        source: SapClientError,
    },
    #[error("SAP returned a document that could not be parsed: {0}")]
    ResponseInvalid(#[from] super::adt_response::AdtResponseParseError),
    #[error("could not write the previous version back: {0}")]
    InactiveWrite(Box<AdtSourceReplacementError>),
    #[error("could not write the previous document back: {0}")]
    MetadataWrite(Box<MetadataObjectWriteError>),
    #[error("could not activate the restored version: {0}")]
    Activation(Box<AdtSourceActivationError>),
    #[error("could not activate the restored document: {0}")]
    MetadataActivation(Box<MetadataObjectActivationError>),
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
            Self::AlreadyUndone { .. } => "undo_already_undone",
            Self::Ungated { .. } => "undo_no_after_image",
            Self::Stale { .. } => "undo_stale",
            Self::PendingWork { .. } => "undo_pending_work",
            Self::PendingWorkProbe { .. } => "undo_pending_work_probe_failed",
            Self::CurrentStateUnreadable { .. } => "undo_current_state_unreadable",
            Self::ResponseInvalid(error) => error.code(),
            Self::InactiveWrite(error) => error.code(),
            Self::MetadataWrite(error) => error.code(),
            Self::Activation(error) => error.code(),
            Self::MetadataActivation(error) => error.code(),
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
            // Redoing is not built yet, so say what the object holds rather
            // than pointing at a command that does not exist.
            Self::AlreadyUndone { name, .. } => format!(
                "{name} is already back at the version this entry replaced. Putting the activation back is a redo, which is not built yet: activate the object again, or use --force to run the undo a second time."
            ),
            Self::Ungated { .. } => {
                "Without an after-image there is nothing to check the object against. Read it first, then use --force."
                    .to_owned()
            }
            Self::Stale { blob_path, .. } => format!(
                "Something changed the object after the activation this entry recorded, so undoing it would discard that. The content it would have restored is at {blob_path}. Use --force to restore it anyway."
            ),
            Self::PendingWork { .. } => {
                "You have pending changes on this object. Undoing writes the previous active version over them, and then restores the pending version this entry recorded, not yours. Activate or discard them first, or use --force."
                    .to_owned()
            }
            Self::PendingWorkProbe { .. } => {
                "SAP could not say whether this object has pending changes, so nothing was written."
                    .to_owned()
            }
            Self::CurrentStateUnreadable { source, .. } => format!(
                "Nothing was written. {}",
                source.hint().unwrap_or_default()
            ),
            Self::ResponseInvalid(error) => return error.hint(),
            Self::InactiveWrite(error) => return error.hint(),
            Self::MetadataWrite(error) => return error.hint(),
            Self::Activation(error) => return error.hint(),
            Self::MetadataActivation(error) => return error.hint(),
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            // Read-only, every one: a suggestion must never be a mutation.
            Self::UnsupportedOperation { id, .. }
            | Self::Unresolved { id, .. }
            | Self::Ungated { id }
            | Self::AlreadyUndone { id, .. }
            | Self::Stale { id, .. }
            | Self::PendingWork { id, .. }
            | Self::WouldDelete { id, .. } => Some(format!("fractal journal show {id}")),
            Self::PackageNotAllowed(error) => error.suggested_command(),
            Self::InactiveWrite(error) => error.suggested_command(),
            Self::MetadataWrite(error) => error.suggested_command(),
            Self::Activation(error) => error.suggested_command(),
            Self::MetadataActivation(error) => error.suggested_command(),
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
    if !entry.operation.is_activation() {
        return Err(UndoError::UnsupportedOperation {
            id: entry.id.clone(),
            operation: entry.operation.as_str().to_owned(),
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
        // Named apart from the unresolved case: an already-undone entry is
        // also stale by construction, and "you undid this already" says more
        // than either of the refusals that would otherwise fire.
        let already_undone = entry.status == EntryStatus::Undone;
        if !force {
            return Err(if already_undone {
                UndoError::AlreadyUndone {
                    id: entry.id.clone(),
                    name: entry.object.name.clone(),
                }
            } else {
                UndoError::Unresolved {
                    id: entry.id.clone(),
                    status: format!("{:?}", entry.status).to_lowercase(),
                }
            });
        }
        overridden.push(if already_undone {
            Override::AlreadyUndone
        } else {
            Override::Unresolved
        });
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
    authorize_object_package(sap, policy, &identity.name, &identity.object_uri).await?;

    let current = current_active_content(sap, &identity).await?;
    let current_active_sha256 = current.as_deref().map(source_sha256);

    // The two versions an undo leaves behind: what it activates, and what it
    // puts back as pending.
    let pending_work_at_risk = pending_work_at_risk(
        sap,
        &entry,
        &identity,
        [Some(&restore_sha256), restore_inactive_sha256.as_deref()],
    )
    .await?;

    let plan = UndoPlan {
        object_uri: identity.object_uri.clone(),
        restore,
        restore_sha256,
        restore_inactive,
        restore_inactive_sha256,
        expected_active_sha256,
        current_active_sha256,
        pending_work_at_risk,
        overridden,
        entry,
    };

    let mut plan = plan;
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
        plan.overridden.push(Override::Stale);
    }

    // Separate from staleness, and checked after it: the active version can be
    // exactly what the entry recorded while somebody has staged pending work
    // that is not the version this undo would put back.
    if plan.pending_work_at_risk {
        if !force {
            return Err(UndoError::PendingWork {
                name: plan.entry.object.name.clone(),
                id: plan.entry.id.clone(),
            });
        }
        plan.overridden.push(Override::PendingWork);
    }
    Ok(plan)
}

/// Resolves the object the same way the forward command would.
///
/// Not taken from the entry's stored URI: the profile's namespace rules and the
/// object's own naming are re-checked, so an undo cannot reach somewhere a
/// forward edit could not.
///
/// [`AdtObjectIdentity`] carries the family as a typed value, so every step
/// below routes on `identity.object_type` and the family cannot disagree with
/// the identity it came from.
fn undo_identity(
    entry: &JournalEntry,
    policy: &EditPolicy,
) -> Result<AdtObjectIdentity, AdtEditTargetValidationError> {
    match entry.object.object_type {
        AdtObjectFamily::Source(object_type) => {
            let identity = editable_source_identity(object_type, &entry.object.name)
                .map_err(AdtEditTargetValidationError::InvalidObject)?;
            super::editable_source::validate_customer_namespace(
                &identity.name,
                &policy.customer_namespaces,
            )?;
            Ok(identity.into())
        }
        AdtObjectFamily::Metadata(object_type) => {
            metadata_object_identity(object_type, &entry.object.name, policy)
        }
    }
}

/// Whether undoing would destroy pending work.
///
/// An undo leaves two versions behind: the one it activates, and the one it
/// puts back in the inactive layer. Pending work is lost only when it is
/// neither of those — content that survives as the active version has not been
/// destroyed, it has been published.
///
/// The case this actually admits is running the same undo twice: the second
/// finds the pending version the first restored, which is exactly what it would
/// restore again. (It was first derived from the redo-through-a-new-entry path,
/// which no longer exists; the rule outlived it because it never depended on
/// it.)
///
/// A resumed undo is never at risk: what the probe would find is the inactive
/// version its own earlier run left behind.
async fn pending_work_at_risk(
    sap: &SapClient,
    entry: &JournalEntry,
    identity: &AdtObjectIdentity,
    survives: [Option<&str>; 2],
) -> Result<bool, UndoError> {
    if entry.undo_progress().is_some() {
        return Ok(false);
    }
    let pending = probe_inactive_adt_source(sap, &identity.object_uri)
        .await
        .map_err(|source| UndoError::PendingWorkProbe {
            name: identity.name.clone(),
            source,
        })?;
    if !pending {
        return Ok(false);
    }
    let current = current_inactive_content(sap, identity)
        .await?
        .as_deref()
        .map(source_sha256);
    Ok(!survives.contains(&current.as_deref()))
}

/// The object's pending content, read the same way its active content is.
async fn current_inactive_content(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
) -> Result<Option<String>, UndoError> {
    match identity.object_type {
        AdtObjectFamily::Source(object_type) => Ok(read_adt_source_for_edit(
            sap,
            object_type,
            &identity.name,
            AdtSourceVersion::Inactive,
        )
        .await
        .ok()
        .map(|read| read.snapshot.source)),
        AdtObjectFamily::Metadata(_) => {
            let document = sap
                .get_text_with_query(&identity.object_uri, &[("version", "inactive")])
                .await
                .map_err(|source| UndoError::CurrentStateUnreadable {
                    name: identity.name.clone(),
                    source,
                })?;
            Ok(Some(strip_navigation_links(&document)))
        }
    }
}

/// The object's current active content, or `None` when it has none.
///
/// A read that fails is an error rather than an absence: "SAP would not answer"
/// and "there is no active version" are different facts, and treating the first
/// as the second would report a network problem as a stale object.
async fn current_active_content(
    sap: &SapClient,
    identity: &AdtObjectIdentity,
) -> Result<Option<String>, UndoError> {
    match identity.object_type {
        AdtObjectFamily::Source(object_type) => {
            match read_adt_source_for_edit(
                sap,
                object_type,
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
        AdtObjectFamily::Metadata(_) => {
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
    }
}

/// What an undo actually did.
#[derive(Debug, Clone)]
pub struct UndoOutcome {
    pub entry_id: String,
    pub name: String,
    pub object_uri: String,
    /// The content now active again.
    pub restored_sha256: String,
    /// Whether step 3 put pending work back.
    pub inactive_restored: bool,
    /// Steps this run performed. Shorter than three when a previous run got
    /// part way.
    pub steps_run: Vec<ActivationUndoStep>,
    #[allow(clippy::struct_field_names)]
    pub resumed_from: Option<ActivationUndoStep>,
    /// A write landed and its lock could not be released. Reported, never
    /// treated as failure: the same answer `edit set` gives, for the same
    /// reason.
    pub still_locked: bool,
    /// Every step landed and the entry could not be marked as undone. Rerunning
    /// is safe: the steps are idempotent and converge.
    pub journal_entry_incomplete: bool,
}

/// Performs the undo: write the previous active version as inactive, activate
/// it, and put back the pending work the original activation consumed.
///
/// Every step goes through the forward code path for that step, so each
/// inherits its pre-check, transport handling and verification. No new entry is
/// written: the entry being undone is marked [`EntryStatus::Undone`], so the
/// journal holds one entry per logical change rather than a chain of undos.
///
/// Steps are idempotent and the entry records how far it got, so a rerun after
/// a failure continues rather than restarting.
///
/// # Errors
///
/// Returns [`UndoError`] when a write, the activation, or recording progress
/// fails. Whatever fails, the steps that already landed stay recorded.
pub async fn undo_activation(
    sap: &mut SapClient,
    policy: &EditPolicy,
    plan: &UndoPlan,
    transport: Option<&str>,
    journal: &Journal,
) -> Result<UndoOutcome, UndoError> {
    let identity = undo_identity(&plan.entry, policy)?;
    let mut entry = plan.entry.clone();
    let resumed_from = entry.undo_progress();
    let mut steps_run = Vec::new();
    let mut still_locked = false;

    let mut wrote_something = true;
    if !already_done(resumed_from, ActivationUndoStep::WroteInactive) {
        let write = write_inactive(sap, policy, &identity, &plan.restore, transport).await?;
        wrote_something = write.changed;
        still_locked |= write.still_locked;
        record_progress(journal, &mut entry, ActivationUndoStep::WroteInactive)?;
        steps_run.push(ActivationUndoStep::WroteInactive);
    }

    if !already_done(resumed_from, ActivationUndoStep::Activated) {
        activate_restored(sap, policy, &identity, transport, wrote_something).await?;
        record_progress(journal, &mut entry, ActivationUndoStep::Activated)?;
        steps_run.push(ActivationUndoStep::Activated);
    }

    // Step 3, and the reason an undo is three steps rather than one: the
    // activation consumed the caller's pending work, and restoring only the
    // active version would discard it silently.
    let mut inactive_restored = false;
    if let Some(pending) = &plan.restore_inactive
        && !already_done(resumed_from, ActivationUndoStep::RestoredInactive)
    {
        let write = write_inactive(sap, policy, &identity, pending, transport).await?;
        still_locked |= write.still_locked;
        record_progress(journal, &mut entry, ActivationUndoStep::RestoredInactive)?;
        steps_run.push(ActivationUndoStep::RestoredInactive);
        inactive_restored = true;
    }

    // Only now: a partway failure leaves the entry resolved and its progress
    // recorded, so a rerun resumes rather than treating the undo as finished.
    //
    // Failing to write it does **not** fail the undo. All three steps have
    // landed, so an error would report failure for work that is done; the entry
    // simply keeps its old status, and rerunning converges because the steps
    // are idempotent.
    entry.undone();
    let journal_entry_incomplete = journal.entries().update(&entry).is_err();

    Ok(UndoOutcome {
        entry_id: entry.id,
        name: identity.name.clone(),
        object_uri: identity.object_uri.clone(),
        restored_sha256: plan.restore_sha256.clone(),
        inactive_restored,
        steps_run,
        resumed_from,
        still_locked,
        journal_entry_incomplete,
    })
}

/// The steps in order, so "already done" is a comparison rather than a list of
/// special cases.
const fn rank(step: ActivationUndoStep) -> u8 {
    match step {
        ActivationUndoStep::WroteInactive => 1,
        ActivationUndoStep::Activated => 2,
        ActivationUndoStep::RestoredInactive => 3,
    }
}

fn already_done(progress: Option<ActivationUndoStep>, step: ActivationUndoStep) -> bool {
    progress.is_some_and(|done| rank(done) >= rank(step))
}

/// Records how far the undo got, so a rerun resumes.
fn record_progress(
    journal: &Journal,
    entry: &mut JournalEntry,
    step: ActivationUndoStep,
) -> Result<(), UndoError> {
    entry.record_undo_step(step);
    journal.entries().update(entry)?;
    Ok(())
}

struct InactiveWrite {
    changed: bool,
    still_locked: bool,
}

/// Writes one version as the inactive version, through the ordinary write path
/// for its family.
async fn write_inactive(
    sap: &mut SapClient,
    policy: &EditPolicy,
    identity: &AdtObjectIdentity,
    content: &str,
    transport: Option<&str>,
) -> Result<InactiveWrite, UndoError> {
    match identity.object_type {
        AdtObjectFamily::Source(object_type) => {
            write_inactive_source(sap, policy, object_type, identity, content, transport).await
        }
        AdtObjectFamily::Metadata(object_type) => {
            write_inactive_document(sap, policy, object_type, identity, content, transport).await
        }
    }
}

/// Writes the whole document back, which for this family is the whole object.
///
/// The document the journal holds is the canonical one, stripped of its
/// `atom:link` navigation; SAP regenerates those on the next read, verified
/// live — see [`super::metadata_document`].
async fn write_inactive_document(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object_type: MetadataAdtObjectType,
    identity: &AdtObjectIdentity,
    xml: &str,
    transport: Option<&str>,
) -> Result<InactiveWrite, UndoError> {
    let written = write_metadata_object(
        sap,
        policy,
        object_type,
        &identity.name,
        xml,
        transport,
        // The gate is on the active version and has already been checked; the
        // inactive layer is what this deliberately overwrites.
        None,
    )
    .await
    .map_err(|error| UndoError::MetadataWrite(Box::new(error)))?;
    Ok(InactiveWrite {
        // This family reports an unchanged write rather than refusing it, so
        // the rerun case needs no special handling here.
        changed: written.changed,
        still_locked: written.still_locked,
    })
}

/// Writes one version as the inactive source.
///
/// Content already identical to what is stored is **not** a failure here, even
/// though `edit set` reports it as one: for an undo it means the step has
/// already been taken, which is exactly what a rerun should find.
async fn write_inactive_source(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object_type: EditableAdtObjectType,
    identity: &AdtObjectIdentity,
    source: &str,
    transport: Option<&str>,
) -> Result<InactiveWrite, UndoError> {
    let request = AdtSourceReplacementRequest {
        object_type,
        name: identity.name.clone(),
        replacement_source: source.to_owned(),
        // The gate is on the active version and has already been checked; the
        // inactive layer is what this deliberately overwrites.
        expected_sha256: None,
        transport: transport.map(str::to_owned),
    };
    match replace_adt_source_atomically(sap, policy, &request).await {
        Ok(result) => Ok(InactiveWrite {
            changed: true,
            still_locked: result.still_locked,
        }),
        Err(AdtSourceReplacementError::Replacement {
            source: SourceChangePlanError::SourceReplacementNoChanges,
            ..
        }) => Ok(InactiveWrite {
            changed: false,
            still_locked: false,
        }),
        Err(error) => Err(UndoError::InactiveWrite(Box::new(error))),
    }
}

/// Activates the restored version, through the ordinary activation path for its
/// family, and **without journaling it**.
///
/// The entry being undone records both halves — its after-image is what was
/// active before the undo — so a second entry would only grow the journal by a
/// row per undo, describing a pending version the undo manufactured rather than
/// one anybody staged.
///
/// `wrote_something` decides how "there is nothing to activate" is read. After
/// a write that changed the inactive layer it is a real failure. After a write
/// that found the content already in place it means an earlier run activated
/// it, so the step is already taken.
async fn activate_restored(
    sap: &mut SapClient,
    policy: &EditPolicy,
    identity: &AdtObjectIdentity,
    transport: Option<&str>,
    wrote_something: bool,
) -> Result<(), UndoError> {
    match identity.object_type {
        AdtObjectFamily::Source(object_type) => {
            activate_restored_source(
                sap,
                policy,
                object_type,
                identity,
                transport,
                wrote_something,
            )
            .await
        }
        AdtObjectFamily::Metadata(object_type) => {
            activate_restored_document(
                sap,
                policy,
                object_type,
                identity,
                transport,
                wrote_something,
            )
            .await
        }
    }
}

/// The metadata activation runs no syntax pre-check — there is no source to
/// check — and proves success from the document's own layer plus the inactive
/// list, exactly as a forward activation does.
async fn activate_restored_document(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object_type: MetadataAdtObjectType,
    identity: &AdtObjectIdentity,
    transport: Option<&str>,
    wrote_something: bool,
) -> Result<(), UndoError> {
    let request = MetadataObjectActivationRequest {
        object_type,
        name: identity.name.clone(),
        transport: transport.map(str::to_owned),
    };
    match activate_metadata_object(sap, policy, &request, None).await {
        Ok(_) => Ok(()),
        Err(MetadataObjectActivationError::NoInactiveVersion { .. }) if !wrote_something => Ok(()),
        Err(error) => Err(UndoError::MetadataActivation(Box::new(error))),
    }
}

async fn activate_restored_source(
    sap: &mut SapClient,
    policy: &EditPolicy,
    object_type: EditableAdtObjectType,
    identity: &AdtObjectIdentity,
    transport: Option<&str>,
    wrote_something: bool,
) -> Result<(), UndoError> {
    let request = AdtSourceActivationRequest {
        object_type,
        name: identity.name.clone(),
        transport: transport.map(str::to_owned),
    };
    // Deliberately unjournalled. The entry being undone records both halves —
    // its after-image is what was active before the undo — so a second entry
    // would only grow the journal by one row per undo, and it would describe a
    // pending version the undo manufactured rather than one anybody staged.
    match activate_adt_source(sap, policy, &request, None).await {
        Ok(_) => Ok(()),
        Err(AdtSourceActivationError::NoInactiveVersion { .. }) if !wrote_something => Ok(()),
        Err(error) => Err(UndoError::Activation(Box::new(error))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::entry::{EntryObject, EntryStatus, EntrySystem, JournalOperation};

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
                operation: JournalOperation::activate(),
                transport: None,
                active_before: ContentRef::Sha256("a".repeat(64)),
                inactive_before: None,
                active_after: expected.map(|sha256| ContentRef::Sha256(sha256.to_owned())),
                etag_after: None,
            },
            object_uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
            restore: "REPORT zsample.".to_owned(),
            restore_sha256: "a".repeat(64),
            restore_inactive: None,
            restore_inactive_sha256: None,
            expected_active_sha256: expected.map(str::to_owned),
            current_active_sha256: current.map(str::to_owned),
            pending_work_at_risk: false,
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

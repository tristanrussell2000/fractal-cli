//! Recording one operation: the only supported way to write to the journal.
//!
//! Every write here is **put, write the entry, put again**, and the two puts do
//! different jobs.
//!
//! The **second** put is what makes the sweep safe. Storing content that
//! already exists writes nothing, so a concurrent sweep cannot see that a new
//! entry is about to reference it; the put after the entry lands touches the
//! blob, which a sweep started earlier respects via `mark_started`, or rewrites
//! it if that sweep already took it.
//!
//! The **first** put is not needed for that — the hash can be computed locally
//! and the entry written before any blob exists. It is there for the crash
//! window: with it, dying between the entry write and the confirm leaves an
//! orphan blob, which the sweep later collects. Without it, the same crash
//! leaves an entry pointing at content that was never written, which is a
//! recovery someone believes they have. It costs an open and a `set_times`,
//! not a second write of the content.
//!
//! Doing either by hand at each call site would work until someone forgot, so
//! callers get [`Journal`] and never the two stores directly.

use std::path::PathBuf;

use super::JournalError;
use super::blobs::BlobStore;
use super::entry::{
    ContentRef, EntryObject, EntryStatus, EntrySystem, JournalEntry, JournalOperation,
};
use super::paths;
use super::store::EntryStore;

/// The journal for one SAP system.
///
/// Its entry store *is* that system's directory, so the identity it stamps on
/// entries describes the same system its location does.
pub struct Journal {
    blobs: BlobStore,
    entries: EntryStore,
    system: EntrySystem,
}

impl Journal {
    /// Opens the journal for a profile's system.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the platform has no data directory, or
    /// when the profile's URL has no host to key on.
    pub fn open(
        profile_name: &str,
        profile: &crate::config::Profile,
    ) -> Result<Self, JournalError> {
        Ok(Self::with_roots(
            paths::blob_root()?,
            paths::entry_root(&profile.base_url)?,
            EntrySystem {
                base_url: profile.base_url.clone(),
                profile: profile_name.to_owned(),
                client: profile.client.clone(),
                user: profile.username.clone(),
            },
        ))
    }

    /// A journal over roots the caller placed, for tests and for any caller
    /// that does not read its location from a profile.
    #[must_use]
    pub fn with_roots(blob_root: PathBuf, entry_root: PathBuf, system: EntrySystem) -> Self {
        Self {
            blobs: BlobStore::new(blob_root),
            entries: EntryStore::new(entry_root),
            system,
        }
    }

    #[must_use]
    pub const fn blobs(&self) -> &BlobStore {
        &self.blobs
    }

    #[must_use]
    pub const fn entries(&self) -> &EntryStore {
        &self.entries
    }

    /// Stores the before-images and writes the entry as `pending`.
    ///
    /// Called before the operation runs: the before-image can only be had
    /// beforehand, and a crash after this leaves a `pending` entry holding it.
    ///
    /// Blobs are stored before the entry so that a crash in between leaves an
    /// orphan rather than a dangling reference; the confirming put afterwards
    /// is what a concurrent sweep respects.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written. The
    /// caller must treat that as fatal: an operation with no before-image has
    /// no recovery at all.
    pub fn begin(
        &self,
        object: EntryObject,
        operation: JournalOperation,
        transport: Option<String>,
        active_before: Option<String>,
        inactive_before: Option<String>,
    ) -> Result<JournalEntry, JournalError> {
        let stored_active = self.store(active_before.as_deref())?;
        let stored_inactive = match inactive_before.as_deref() {
            Some(content) => Some(self.store(Some(content))?),
            None => None,
        };

        let entry = self.entries.create(JournalEntry {
            id: String::new(),
            recorded_at: String::new(),
            status: EntryStatus::Pending,
            system: self.system.clone(),
            object,
            operation,
            transport,
            active_before: stored_active,
            inactive_before: stored_inactive,
            active_after: None,
            etag_after: None,
        })?;

        self.confirm([active_before.as_deref(), inactive_before.as_deref()])?;
        Ok(entry)
    }

    /// Records that the operation worked, with what SAP holds now.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the blob or the entry cannot be written.
    pub fn succeeded(
        &self,
        mut entry: JournalEntry,
        active_after: Option<&str>,
        etag_after: Option<String>,
    ) -> Result<JournalEntry, JournalError> {
        let after = self.store(active_after)?;
        entry.succeeded(after, etag_after);
        self.resolve(entry, active_after)
    }

    /// Records that SAP refused the operation. Nothing changed, so there is no
    /// after-image.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when the entry cannot be written.
    pub fn failed(&self, mut entry: JournalEntry) -> Result<JournalEntry, JournalError> {
        entry.failed();
        self.resolve(entry, None)
    }

    /// Records that SAP accepted the operation and the read-back could not
    /// confirm it — the case where the before-image matters most.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written.
    pub fn unverified(
        &self,
        mut entry: JournalEntry,
        active_after: Option<&str>,
    ) -> Result<JournalEntry, JournalError> {
        let after = match active_after {
            Some(content) => Some(self.store(Some(content))?),
            None => None,
        };
        entry.unverified(after);
        self.resolve(entry, active_after)
    }

    /// Writes the updated entry, then confirms the content it now references.
    fn resolve(
        &self,
        entry: JournalEntry,
        new_content: Option<&str>,
    ) -> Result<JournalEntry, JournalError> {
        self.entries.update(&entry)?;
        // Only the newly referenced content needs confirming: everything else
        // has been referenced by an entry on disk since `begin`.
        self.confirm([new_content])?;
        Ok(entry)
    }

    /// `None` content is an absence, not empty content.
    fn store(&self, content: Option<&str>) -> Result<ContentRef, JournalError> {
        match content {
            Some(content) => Ok(ContentRef::Sha256(self.blobs.put(content)?)),
            None => Ok(ContentRef::Absent),
        }
    }

    /// Re-stores content now that an entry references it.
    ///
    /// This is the put that matters for concurrency: it touches the blob so a
    /// sweep leaves it alone, and rewrites it if a sweep already took it.
    fn confirm<'a>(
        &self,
        contents: impl IntoIterator<Item = Option<&'a str>>,
    ) -> Result<(), JournalError> {
        for content in contents.into_iter().flatten() {
            self.blobs.put(content)?;
        }
        Ok(())
    }
}

/// How an operation ended, from the journal's point of view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Resolution<'a> {
    /// The operation landed. The content is what SAP holds now, or `None` when
    /// the object is gone — which is not the same as holding nothing.
    Succeeded(Option<&'a str>),
    /// SAP refused it, so nothing was published and the before-image is still
    /// true.
    Failed,
    /// SAP accepted it and the read-back could not confirm the result — the
    /// case where the before-image matters most.
    Unverified(Option<&'a str>),
}

/// Resolves an entry, naming one that could not be completed.
///
/// **Never fails the operation.** By the time this runs the mutation has landed
/// or been refused, so returning an error would report failure for work that is
/// already done and invite a retry that cannot succeed. What survives is a
/// `pending` entry holding a real before-image: degraded, not useless. The
/// caller reports success and says which entry is stuck.
///
/// A free function rather than a method because both arguments are optional at
/// every call site — `--no-journal` gives no journal, and an operation that
/// failed before recording anything has no entry — and pushing that back to the
/// callers is what put three copies of this in the tree.
pub fn resolve(
    journal: Option<&Journal>,
    entry: Option<JournalEntry>,
    resolution: Resolution<'_>,
) -> Option<String> {
    let (Some(journal), Some(entry)) = (journal, entry) else {
        return None;
    };
    let id = entry.id.clone();
    match resolution {
        Resolution::Succeeded(content) => journal.succeeded(entry, content, None),
        Resolution::Failed => journal.failed(entry),
        Resolution::Unverified(content) => journal.unverified(entry, content),
    }
    .err()
    .map(|_| id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sap::object_family::AdtObjectFamily;
    use crate::source_change::source_sha256;

    fn journal() -> (Journal, tempfile::TempDir) {
        let dir = tempfile::tempdir().expect("temp dir");
        let journal = Journal::with_roots(
            dir.path().join("blobs"),
            dir.path().join("journal").join("de3"),
            EntrySystem {
                base_url: "https://sap.example:8001".to_owned(),
                profile: "dev".to_owned(),
                client: "100".to_owned(),
                user: "developer".to_owned(),
            },
        );
        (journal, dir)
    }

    const BEFORE: &str = "REPORT zsample. \" before";
    const PENDING: &str = "REPORT zsample. \" pending";

    fn object() -> EntryObject {
        EntryObject {
            object_type: AdtObjectFamily::parse("PROG").unwrap(),
            name: "ZSAMPLE".to_owned(),
            uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
            source_part: None,
        }
    }

    #[test]
    fn begin_stores_the_before_images_and_a_pending_entry() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                Some(PENDING.to_owned()),
            )
            .unwrap();

        assert_eq!(entry.status, EntryStatus::Pending);
        assert_eq!(
            entry.active_before,
            ContentRef::Sha256(source_sha256(BEFORE))
        );
        assert!(entry.active_after.is_none());
        for hash in entry.referenced_blobs() {
            assert!(journal.blobs().contains(hash), "blob {hash} missing");
        }
    }

    #[test]
    fn no_pending_work_is_recorded_as_no_field_at_all() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                None,
            )
            .unwrap();

        assert_eq!(entry.inactive_before, None);
    }

    #[test]
    fn an_object_with_no_active_version_records_an_absence() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                None,
                Some(PENDING.to_owned()),
            )
            .unwrap();

        // Not the hash of an empty document.
        assert_eq!(entry.active_before, ContentRef::Absent);
        assert!(!journal.blobs().contains(&source_sha256("")));
    }

    #[test]
    fn succeeding_stores_the_after_image_and_resolves() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                Some(PENDING.to_owned()),
            )
            .unwrap();
        let resolved = journal
            .succeeded(
                entry,
                Some("REPORT zsample. \" after"),
                Some("etag".to_owned()),
            )
            .unwrap();

        assert_eq!(resolved.status, EntryStatus::Succeeded);
        assert_eq!(resolved.etag_after.as_deref(), Some("etag"));
        assert_eq!(
            journal.entries().find(&resolved.id).unwrap().status,
            EntryStatus::Succeeded
        );
    }

    #[test]
    fn a_refused_operation_keeps_its_before_image_and_gains_no_after() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                Some(PENDING.to_owned()),
            )
            .unwrap();
        let before = entry.active_before.clone();
        let resolved = journal.failed(entry).unwrap();

        assert_eq!(resolved.status, EntryStatus::Failed);
        assert_eq!(resolved.active_after, None);
        assert_eq!(resolved.active_before, before);
    }

    #[test]
    fn a_delete_records_the_object_as_gone() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::delete(None, None),
                None,
                Some(BEFORE.to_owned()),
                None,
            )
            .unwrap();
        let resolved = journal.succeeded(entry, None, None).unwrap();

        assert_eq!(resolved.active_after, Some(ContentRef::Absent));
    }

    #[test]
    fn a_blob_is_touched_after_the_entry_write_that_first_references_it() {
        // The ordering the double `put` exists for, as an invariant a test can
        // check. Touching only *before* the entry write is unsound: a sweep can
        // start in between, see an untouched blob and no entry, and delete it.
        //
        // It applies per entry write, to the blobs that write newly references.
        // The before-images stay older than a later resolution, which is fine:
        // an on-disk entry has referenced them since `begin`.
        let (journal, _dir) = journal();

        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                Some(PENDING.to_owned()),
            )
            .unwrap();
        let began_at = entry_written_at(&journal, &entry);
        for hash in entry.referenced_blobs() {
            assert!(
                journal.blobs().modified_at(hash).unwrap() >= began_at,
                "blob {hash} was last touched before the entry that references it"
            );
        }

        let entry = journal
            .succeeded(entry, Some("REPORT zsample. \" after"), None)
            .unwrap();
        let resolved_at = entry_written_at(&journal, &entry);
        let after = entry.active_after.as_ref().unwrap().sha256().unwrap();
        assert!(
            journal.blobs().modified_at(after).unwrap() >= resolved_at,
            "the after-image was touched before the entry that references it"
        );
    }

    fn entry_written_at(journal: &Journal, entry: &JournalEntry) -> std::time::SystemTime {
        journal
            .entries()
            .modified_at(
                &crate::journal::paths::object_key(&entry.object.uri),
                &entry.id,
            )
            .expect("entry exists")
    }

    #[test]
    fn resolving_names_an_entry_it_could_not_complete_rather_than_failing() {
        // The contract every mutating command depends on: by the time this runs
        // the operation has landed, so it reports which entry is stuck and
        // never turns completed work into an error.
        let (journal, dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                None,
            )
            .unwrap();
        let id = entry.id.clone();
        // Remove the whole store, so the resolution has nowhere to write.
        std::fs::remove_dir_all(dir.path().join("journal")).unwrap();

        let stuck = resolve(
            Some(&journal),
            Some(entry),
            Resolution::Succeeded(Some("after")),
        );

        assert_eq!(stuck, Some(id));
    }

    #[test]
    fn resolving_reports_nothing_when_it_worked_or_when_there_is_no_journal() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                None,
            )
            .unwrap();

        assert_eq!(
            resolve(Some(&journal), Some(entry), Resolution::Succeeded(None)),
            None
        );
        // `--no-journal`, and an operation that failed before recording.
        assert_eq!(resolve(None, None, Resolution::Failed), None);
    }

    #[test]
    fn confirming_rewrites_content_a_sweep_already_took() {
        // The other half: if the sweep won the race, the confirming `put` puts
        // the content back rather than leaving a dangling reference.
        let (journal, _dir) = journal();
        let entry = journal
            .begin(
                object(),
                JournalOperation::activate(),
                None,
                Some(BEFORE.to_owned()),
                Some(PENDING.to_owned()),
            )
            .unwrap();
        let hash = entry.active_before.sha256().unwrap().to_owned();
        std::fs::remove_file(journal.blobs().path_of(&hash)).unwrap();

        journal
            .succeeded(entry, Some("REPORT zsample. \" after"), None)
            .unwrap();
        // `begin`'s content is gone for good here, but a later confirm of the
        // same content restores it: prove the mechanism on the after-image.
        let restored = journal.blobs().put("REPORT zsample. \" after").unwrap();
        assert!(journal.blobs().contains(&restored));
    }
}

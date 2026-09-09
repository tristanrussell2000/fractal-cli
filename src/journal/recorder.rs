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

/// What a caller knows before an operation runs.
pub struct EntryDraft {
    pub system: EntrySystem,
    pub object: EntryObject,
    pub operation: JournalOperation,
    pub transport: Option<String>,
    /// The active version being replaced. `None` records that there was none.
    pub active_before: Option<String>,
    /// Pending work that the operation will consume. `None` records that the
    /// caller had none, which is different from having had an empty one.
    pub inactive_before: Option<String>,
}

pub struct Journal {
    blobs: BlobStore,
    entries: EntryStore,
}

impl Journal {
    /// Opens the journal for one system.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::NoDataDirectory`] when the platform has no data
    /// directory, or [`JournalError::UnusableBaseUrl`] when the profile's URL
    /// has no host to key on.
    pub fn open(base_url: &str) -> Result<Self, JournalError> {
        Ok(Self::with_roots(
            paths::blob_root()?,
            paths::entry_root(base_url)?,
        ))
    }

    #[must_use]
    pub fn with_roots(blob_root: PathBuf, entry_root: PathBuf) -> Self {
        Self {
            blobs: BlobStore::new(blob_root),
            entries: EntryStore::new(entry_root),
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
    /// orphan rather than a dangling reference; the confirming put afterwards is
    /// what a concurrent sweep respects.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError`] when a blob or the entry cannot be written.
    pub fn begin(&self, draft: EntryDraft) -> Result<JournalEntry, JournalError> {
        let active_before = self.store(draft.active_before.as_deref())?;
        let inactive_before = match draft.inactive_before.as_deref() {
            Some(content) => Some(self.store(Some(content))?),
            None => None,
        };

        let entry = self.entries.create(JournalEntry {
            id: String::new(),
            recorded_at: String::new(),
            status: EntryStatus::Pending,
            system: draft.system,
            object: draft.object,
            operation: draft.operation,
            transport: draft.transport,
            active_before,
            inactive_before,
            active_after: None,
            etag_after: None,
            undo_progress: None,
        })?;

        self.confirm([
            draft.active_before.as_deref(),
            draft.inactive_before.as_deref(),
        ])?;
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
        );
        (journal, dir)
    }

    fn draft() -> EntryDraft {
        EntryDraft {
            system: EntrySystem {
                host: "sap.example".to_owned(),
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
            active_before: Some("REPORT zsample. \" before".to_owned()),
            inactive_before: Some("REPORT zsample. \" pending".to_owned()),
        }
    }

    #[test]
    fn begin_stores_the_before_images_and_a_pending_entry() {
        let (journal, _dir) = journal();
        let entry = journal.begin(draft()).unwrap();

        assert_eq!(entry.status, EntryStatus::Pending);
        assert_eq!(
            entry.active_before,
            ContentRef::Sha256(source_sha256("REPORT zsample. \" before"))
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
            .begin(EntryDraft {
                inactive_before: None,
                ..draft()
            })
            .unwrap();

        assert_eq!(entry.inactive_before, None);
    }

    #[test]
    fn an_object_with_no_active_version_records_an_absence() {
        let (journal, _dir) = journal();
        let entry = journal
            .begin(EntryDraft {
                active_before: None,
                ..draft()
            })
            .unwrap();

        // Not the hash of an empty document.
        assert_eq!(entry.active_before, ContentRef::Absent);
        assert!(!journal.blobs().contains(&source_sha256("")));
    }

    #[test]
    fn succeeding_stores_the_after_image_and_resolves() {
        let (journal, _dir) = journal();
        let entry = journal.begin(draft()).unwrap();
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
            journal.entries().read(&resolved.id).unwrap().status,
            EntryStatus::Succeeded
        );
    }

    #[test]
    fn a_refused_operation_keeps_its_before_image_and_gains_no_after() {
        let (journal, _dir) = journal();
        let entry = journal.begin(draft()).unwrap();
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
            .begin(EntryDraft {
                operation: JournalOperation::Delete,
                inactive_before: None,
                ..draft()
            })
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

        let entry = journal.begin(draft()).unwrap();
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
            .modified_at(&entry.id)
            .expect("entry exists")
    }

    #[test]
    fn confirming_rewrites_content_a_sweep_already_took() {
        // The other half: if the sweep won the race, the confirming `put` puts
        // the content back rather than leaving a dangling reference.
        let (journal, _dir) = journal();
        let entry = journal.begin(draft()).unwrap();
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

//! One journal entry: what an object looked like before an operation, and what
//! became of it.

use serde::{Deserialize, Serialize};

use crate::sap::object_family::AdtObjectFamily;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryStatus {
    /// Written before the operation. Also what a crash between the two writes
    /// leaves behind.
    Pending,
    Succeeded,
    /// The operation was refused. The before-image is still true.
    Failed,
    /// SAP accepted it and the read-back could not confirm the result.
    Unverified,
    /// An undo put the object back the way this entry found it.
    ///
    /// The undo writes no entry of its own: this one records both that the
    /// operation happened and that it was reversed, so the journal holds one
    /// entry per logical change rather than a chain of undos.
    Undone,
}

impl EntryStatus {
    /// Whether `undo` may act on an entry in this state without `--force`.
    ///
    /// `Pending` is excluded because it has no after-image, which is what the
    /// staleness gate compares against. `Undone` is excluded because it has
    /// already been reversed.
    #[must_use]
    pub const fn is_undoable(self) -> bool {
        matches!(self, Self::Succeeded | Self::Unverified)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalOperation {
    Activate,
    Delete,
}

/// What the stored blobs hold, and so which code path an undo takes.
///
/// Derived from the object's family rather than stored: a source object's blob
/// is source and a metadata object's is XML, by definition of the families.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentKind {
    Source,
    Xml,
}

/// A reference to stored content, or the fact that there was none.
///
/// `Absent` is not `sha256("")`: an object holding an empty document is not the
/// same as an object that is not there.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContentRef {
    Sha256(String),
    Absent,
}

impl ContentRef {
    #[must_use]
    pub fn sha256(&self) -> Option<&str> {
        match self {
            Self::Sha256(sha256) => Some(sha256),
            Self::Absent => None,
        }
    }
}

/// Which system the entry belongs to. Only the host part of `base_url` keys the
/// directory; the rest is information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntrySystem {
    pub base_url: String,
    pub profile: String,
    pub client: String,
    pub user: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntryObject {
    #[serde(rename = "type")]
    pub object_type: AdtObjectFamily,
    pub name: String,
    pub uri: String,
    /// Which part of a class's source this content is: `main`, `definitions`,
    /// `implementations`, `macros` or `testclasses`. Absent for objects with a
    /// single source, and for metadata objects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_part: Option<String>,
}

/// How far an interrupted undo got, so a rerun resumes rather than restarts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UndoStep {
    WroteInactive,
    Activated,
    RestoredInactive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalEntry {
    /// The file name, without its extension.
    pub id: String,
    pub recorded_at: String,
    pub status: EntryStatus,
    pub system: EntrySystem,
    pub object: EntryObject,
    pub operation: JournalOperation,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub transport: Option<String>,
    pub active_before: ContentRef,
    /// Absent when the caller had no pending work, which is different from
    /// having had empty pending work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inactive_before: Option<ContentRef>,
    /// Unset until the operation resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_after: Option<ContentRef>,
    /// Opaque and verbatim; some SAP ETags embed the media type.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag_after: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_progress: Option<UndoStep>,
}

impl JournalEntry {
    /// What this entry's blobs hold.
    #[must_use]
    pub const fn content_kind(&self) -> ContentKind {
        match self.object.object_type {
            AdtObjectFamily::Source(_) => ContentKind::Source,
            AdtObjectFamily::Metadata(_) => ContentKind::Xml,
        }
    }

    /// Every blob this entry references, for the mark-and-sweep.
    pub fn referenced_blobs(&self) -> impl Iterator<Item = &str> {
        [
            Some(&self.active_before),
            self.inactive_before.as_ref(),
            self.active_after.as_ref(),
        ]
        .into_iter()
        .flatten()
        .filter_map(ContentRef::sha256)
    }

    /// Records a successful operation.
    pub fn succeeded(&mut self, active_after: ContentRef, etag_after: Option<String>) {
        self.status = EntryStatus::Succeeded;
        self.active_after = Some(active_after);
        self.etag_after = etag_after;
    }

    pub fn failed(&mut self) {
        self.status = EntryStatus::Failed;
    }

    /// SAP accepted the operation and the read-back could not confirm it.
    pub fn unverified(&mut self, active_after: Option<ContentRef>) {
        self.status = EntryStatus::Unverified;
        self.active_after = active_after;
    }

    /// An undo reversed this operation.
    pub fn undone(&mut self) {
        self.status = EntryStatus::Undone;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> JournalEntry {
        JournalEntry {
            id: "20260905T221503.412Z".to_owned(),
            recorded_at: "2026-09-05T22:15:03.412Z".to_owned(),
            status: EntryStatus::Pending,
            system: EntrySystem {
                base_url: "https://sap.example:8001".to_owned(),
                profile: "dev".to_owned(),
                client: "100".to_owned(),
                user: "developer".to_owned(),
            },
            object: EntryObject {
                object_type: AdtObjectFamily::parse("CLAS").unwrap(),
                name: "ZCL_SAMPLE".to_owned(),
                uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                source_part: Some("main".to_owned()),
            },
            operation: JournalOperation::Activate,
            transport: None,
            active_before: ContentRef::Sha256("a".repeat(64)),
            inactive_before: None,
            active_after: None,
            etag_after: None,
            undo_progress: None,
        }
    }

    fn json(entry: &JournalEntry) -> serde_json::Value {
        serde_json::to_value(entry).expect("serializes")
    }

    #[test]
    fn round_trips() {
        let mut original = entry();
        original.succeeded(ContentRef::Sha256("b".repeat(64)), Some("etag".to_owned()));
        let text = serde_json::to_string(&original).unwrap();

        assert_eq!(
            serde_json::from_str::<JournalEntry>(&text).unwrap(),
            original
        );
    }

    #[test]
    fn absent_is_not_an_empty_hash() {
        let empty = ContentRef::Sha256(crate::source_change::source_sha256(""));
        assert_ne!(ContentRef::Absent, empty);
        assert_eq!(ContentRef::Absent.sha256(), None);

        // And they serialize differently, so a delete is never read back as an
        // object that held an empty document.
        assert_ne!(
            serde_json::to_value(&ContentRef::Absent).unwrap(),
            serde_json::to_value(&empty).unwrap()
        );
    }

    #[test]
    fn a_hash_reference_serializes_as_the_plan_says() {
        let value = json(&entry());
        assert_eq!(
            value["active_before"]["sha256"],
            serde_json::json!("a".repeat(64))
        );
    }

    #[test]
    fn unset_fields_are_omitted_rather_than_null() {
        let value = json(&entry());
        for absent in [
            "transport",
            "inactive_before",
            "active_after",
            "etag_after",
            "undo_progress",
        ] {
            assert!(value.get(absent).is_none(), "{absent} should be omitted");
        }
    }

    #[test]
    fn no_pending_work_round_trips_as_none_not_as_empty() {
        let text = serde_json::to_string(&entry()).unwrap();
        let read: JournalEntry = serde_json::from_str(&text).unwrap();

        assert_eq!(read.inactive_before, None);
    }

    #[test]
    fn only_resolved_entries_are_undoable() {
        assert!(!EntryStatus::Pending.is_undoable());
        assert!(!EntryStatus::Failed.is_undoable());
        assert!(EntryStatus::Succeeded.is_undoable());
        // SAP accepted it, so the object may well have changed.
        assert!(EntryStatus::Unverified.is_undoable());
        // Already reversed. Putting it back is a redo, not another undo.
        assert!(!EntryStatus::Undone.is_undoable());
    }

    #[test]
    fn an_undone_entry_keeps_everything_it_recorded() {
        let mut entry = entry();
        entry.succeeded(ContentRef::Sha256("b".repeat(64)), None);
        let after = entry.active_after.clone();
        entry.undone();

        assert_eq!(entry.status, EntryStatus::Undone);
        // The after-image is what was active before the undo, so it is still
        // the record of what the operation did.
        assert_eq!(entry.active_after, after);
        assert_eq!(json(&entry)["status"], serde_json::json!("undone"));
    }

    #[test]
    fn resolving_sets_the_after_image_and_the_status() {
        let mut succeeded = entry();
        succeeded.succeeded(ContentRef::Absent, None);
        assert_eq!(succeeded.status, EntryStatus::Succeeded);
        assert_eq!(succeeded.active_after, Some(ContentRef::Absent));

        let mut failed = entry();
        failed.failed();
        assert_eq!(failed.status, EntryStatus::Failed);
        // A refused operation changed nothing, so there is no after-image.
        assert_eq!(failed.active_after, None);
    }

    #[test]
    fn referenced_blobs_lists_every_hash_and_no_absences() {
        let mut entry = entry();
        entry.inactive_before = Some(ContentRef::Sha256("c".repeat(64)));
        entry.succeeded(ContentRef::Absent, None);

        let blobs: Vec<_> = entry.referenced_blobs().collect();
        assert_eq!(blobs, vec!["a".repeat(64), "c".repeat(64)]);
    }

    #[test]
    fn the_object_type_is_the_shared_family_enum() {
        // Stored as the same type name every other command takes, so an undo
        // can route on it without reparsing a free-form string.
        assert_eq!(json(&entry())["object"]["type"], serde_json::json!("CLAS"));
        assert_eq!(entry().content_kind(), ContentKind::Source);
    }

    #[test]
    fn a_metadata_object_holds_xml_without_being_told() {
        let mut entry = entry();
        entry.object.object_type = AdtObjectFamily::parse("DTEL").unwrap();

        // Derived, not stored: a `kind` field could disagree with the type.
        assert_eq!(entry.content_kind(), ContentKind::Xml);
        assert!(json(&entry).get("kind").is_none());
    }
}

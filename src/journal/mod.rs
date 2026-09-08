//! The edit journal: a local record of what an object looked like before
//! Fractal changed it.
//!
//! Two stores, deliberately separate: content-addressed [`blobs`] shared by
//! every system, and entries filed per system under [`paths::entry_root`]. An
//! entry references blobs by hash and never embeds content, so identical
//! content is stored once, a blob can be verified by rehashing it, and blobs
//! can only ever be removed by sweeping from the surviving entries.

pub mod blobs;
pub mod entry;
pub mod paths;
pub mod store;

use std::path::PathBuf;

use thiserror::Error;

use crate::reportable_error::ReportableError;

#[derive(Debug, Error)]
pub enum JournalError {
    #[error("could not determine the platform data directory")]
    NoDataDirectory,
    #[error("'{0}' has no host to key a journal on")]
    UnusableBaseUrl(String),
    #[error("could not write {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not read {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("nothing is stored under {sha256}")]
    BlobMissing { sha256: String },
    #[error("the content stored under {sha256} now hashes to {actual}")]
    BlobCorrupt { sha256: String, actual: String },
    #[error("no journal entry {id}")]
    EntryMissing { id: String },
    #[error("journal entry {id} is not valid JSON: {source}")]
    EntryInvalid {
        id: String,
        source: serde_json::Error,
    },
    #[error("too many journal entries claim {stamp}")]
    IdExhausted { stamp: String },
}

impl ReportableError for JournalError {
    fn code(&self) -> &'static str {
        match self {
            Self::NoDataDirectory => "journal_directory_unavailable",
            Self::UnusableBaseUrl(_) => "journal_unusable_base_url",
            Self::Write { .. } => "journal_write_error",
            Self::Read { .. } => "journal_read_error",
            Self::BlobMissing { .. } => "journal_blob_missing",
            Self::BlobCorrupt { .. } => "journal_blob_corrupt",
            Self::EntryMissing { .. } => "journal_entry_missing",
            Self::EntryInvalid { .. } => "journal_entry_invalid",
            Self::IdExhausted { .. } => "journal_id_exhausted",
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::NoDataDirectory => {
                "This platform exposes no per-user data directory, so the journal has nowhere to live."
                    .to_owned()
            }
            Self::UnusableBaseUrl(_) => {
                "Check the profile's base_url: it needs a host, as in https://sap.example:8001."
                    .to_owned()
            }
            Self::Write { .. } => {
                "Check that the directory exists, is writable, and has space.".to_owned()
            }
            Self::Read { .. } => "Check that the file exists and is readable.".to_owned(),
            // Both of these mean the recorded content is gone. Say so plainly:
            // the entry's metadata may still be worth reading even though what
            // it points at cannot be restored.
            Self::BlobMissing { .. } => {
                "The entry survives but the content it points at does not, so it cannot be restored from the journal."
                    .to_owned()
            }
            Self::BlobCorrupt { .. } => {
                "The stored content no longer matches its hash, so it has been altered or damaged and will not be restored from the journal."
                    .to_owned()
            }
            Self::EntryMissing { .. } => {
                "List what the journal holds with `fractal journal list`.".to_owned()
            }
            Self::EntryInvalid { .. } => {
                "The entry file is damaged. Other entries are unaffected and still listed.".to_owned()
            }
            Self::IdExhausted { .. } => {
                "Too many entries landed in one millisecond, which suggests something is retrying in a loop."
                    .to_owned()
            }
        })
    }
}

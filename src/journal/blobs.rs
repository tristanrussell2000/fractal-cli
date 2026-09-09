//! The content-addressed store the journal keeps its before- and after-images
//! in.
//!
//! Blobs are named by the SHA-256 of their content, which makes them immutable,
//! deduplicated, and verifiable without a second stored field: rehashing a blob
//! is a stronger corruption check than any length or checksum kept beside it.
//!
//! Kept separate from the entries deliberately. Entries reference blobs by
//! hash, so two entries that saw the same content store it once, and a future
//! read cache can point at the same store without a migration. That also makes
//! the one dangerous operation obvious: a blob may only be deleted by sweeping
//! from the surviving entries, never as a side effect of removing one.

use std::path::{Path, PathBuf};
use std::time::SystemTime;

use sha2::{Digest, Sha256};

use super::JournalError;
#[cfg(test)]
use crate::reportable_error::ReportableError;
use crate::source_change::source_sha256;

/// Prefix for the temporary file a blob is written to before it is renamed
/// into place, so a partially written blob is never visible under its hash.
const TEMP_PREFIX: &str = ".tmp-";

pub struct BlobStore {
    root: PathBuf,
}

impl BlobStore {
    #[must_use]
    pub const fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The path a blob has, whether or not it exists yet.
    #[must_use]
    pub fn path_of(&self, sha256: &str) -> PathBuf {
        self.root.join(sha256)
    }

    #[must_use]
    pub fn contains(&self, sha256: &str) -> bool {
        self.path_of(sha256).is_file()
    }

    /// Stores content and returns its hash.
    ///
    /// Storing the same content twice is free and writes nothing the second
    /// time: the hash already names the file that holds it.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Write`] when the store directory cannot be
    /// created or the blob cannot be written.
    pub fn put(&self, content: &str) -> Result<String, JournalError> {
        let sha256 = source_sha256(content);
        let destination = self.path_of(&sha256);
        if destination.is_file() {
            // Touch it. A caller that stores content it did not write is still
            // taking a reference, and a sweep has no other way to see that: the
            // dedup writes nothing. Best effort — a sweep that misses the touch
            // deletes a blob the caller then rewrites.
            let _ = std::fs::File::options()
                .write(true)
                .open(&destination)
                .and_then(|file| {
                    file.set_times(std::fs::FileTimes::new().set_modified(SystemTime::now()))
                });
            return Ok(sha256);
        }

        create_private_dir(&self.root)?;

        // Write to a temporary name and rename into place. A reader that finds
        // a file under its hash must be able to trust its whole content, and a
        // rename is the only way to promise that: an interrupted write leaves
        // the temporary file behind instead of a truncated blob.
        let temporary = self.root.join(format!("{TEMP_PREFIX}{sha256}"));
        write_private_file(&temporary, content)?;
        finish_put(&temporary, &destination)?;
        Ok(sha256)
    }

    /// Reads a blob back and verifies it still hashes to its own name.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::BlobMissing`] when nothing is stored under that
    /// hash, [`JournalError::Read`] when it cannot be read, and
    /// [`JournalError::BlobCorrupt`] when the content no longer matches the
    /// hash it is filed under.
    pub fn read(&self, sha256: &str) -> Result<String, JournalError> {
        let path = self.path_of(sha256);
        if !path.is_file() {
            return Err(JournalError::BlobMissing {
                sha256: sha256.to_owned(),
            });
        }
        let content = std::fs::read_to_string(&path).map_err(|source| JournalError::Read {
            path: path.clone(),
            source,
        })?;
        // The name is the hash, so checking costs one hash and catches every
        // way a blob can rot. Nothing else in the journal is verifiable this
        // cheaply, which is the whole reason the store is content-addressed.
        let actual = source_sha256(&content);
        if actual == sha256 {
            Ok(content)
        } else {
            Err(JournalError::BlobCorrupt {
                sha256: sha256.to_owned(),
                actual,
            })
        }
    }

    /// When a blob was last stored or re-referenced.
    ///
    /// Read immediately before deleting it, never collected up front: a blob
    /// re-referenced mid-sweep must be seen as fresh.
    #[must_use]
    pub fn modified_at(&self, sha256: &str) -> Option<SystemTime> {
        std::fs::metadata(self.path_of(sha256))
            .and_then(|metadata| metadata.modified())
            .ok()
    }

    /// Every hash currently stored, for the sweep that collects unreferenced
    /// blobs.
    ///
    /// Temporary files are skipped: a half-written blob is not a blob, and
    /// treating one as garbage-collectable content would be wrong in both
    /// directions — it is neither restorable nor referenced. They are cleared
    /// separately by `sweep_temporaries`, which can afford to be careful about
    /// whether another process is mid-write.
    ///
    /// # Errors
    ///
    /// Returns [`JournalError::Read`] when the store directory cannot be
    /// listed. A store that does not exist yet is empty, not an error.
    pub fn hashes(&self) -> Result<Vec<String>, JournalError> {
        if !self.root.is_dir() {
            return Ok(Vec::new());
        }
        let entries = std::fs::read_dir(&self.root).map_err(|source| JournalError::Read {
            path: self.root.clone(),
            source,
        })?;
        let mut hashes = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|source| JournalError::Read {
                path: self.root.clone(),
                source,
            })?;
            let name = entry.file_name().to_string_lossy().into_owned();
            if !name.starts_with(TEMP_PREFIX) && is_sha256_name(&name) {
                hashes.push(name);
            }
        }
        Ok(hashes)
    }
}

/// Renames a written temporary into place.
///
/// Two processes storing the same content share a temporary name, since it is
/// derived from the hash. Whichever renames second finds the temporary already
/// gone and fails — but the blob is there, put by the other one, so that is a
/// success and not a write error. Content cannot differ: the path is the hash.
fn finish_put(temporary: &Path, destination: &Path) -> Result<(), JournalError> {
    match std::fs::rename(temporary, destination) {
        Ok(()) => Ok(()),
        Err(_) if destination.is_file() => Ok(()),
        Err(source) => {
            // A leftover is not lost space forever: the next `put` of this
            // content reuses the name. `sweep_temporaries` clears the rest.
            let _ = std::fs::remove_file(temporary);
            Err(JournalError::Write {
                path: destination.to_path_buf(),
                source,
            })
        }
    }
}

fn is_sha256_name(name: &str) -> bool {
    name.len() == Sha256::output_size() * 2 && name.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Creates a directory only this user can enter.
///
/// The journal holds customer ABAP source, so the mode is set when the
/// directory is created rather than afterwards; the gap between the two would
/// be a window where it is world-readable. Windows has no equivalent call, and
/// inherits the ACL of the per-user data directory instead.
pub(super) fn create_private_dir(path: &Path) -> Result<(), JournalError> {
    if path.is_dir() {
        return Ok(());
    }
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|source| JournalError::Write {
        path: path.to_path_buf(),
        source,
    })
}

/// Writes a file only this user can read, created with that mode rather than
/// chmod-ed into it afterwards, for the same reason as the directory.
pub(super) fn write_private_file(path: &Path, content: &str) -> Result<(), JournalError> {
    use std::io::Write as _;

    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|source| JournalError::Write {
        path: path.to_path_buf(),
        source,
    })?;
    file.write_all(content.as_bytes())
        .map_err(|source| JournalError::Write {
            path: path.to_path_buf(),
            source,
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (BlobStore, tempfile::TempDir) {
        // The TempDir is returned so it outlives the store: dropping it wipes
        // the directory out from under the test.
        let dir = tempfile::tempdir().expect("temp dir");
        (BlobStore::new(dir.path().join("blobs")), dir)
    }

    #[test]
    fn stores_content_under_its_hash_and_reads_it_back() {
        let (blobs, _dir) = store();
        let sha = blobs.put("REPORT zsample.").unwrap();

        assert!(blobs.contains(&sha));
        assert_eq!(blobs.read(&sha).unwrap(), "REPORT zsample.");
        assert_eq!(sha, crate::source_change::source_sha256("REPORT zsample."));
    }

    #[test]
    fn identical_content_is_stored_once() {
        let (blobs, _dir) = store();
        let first = blobs.put("same").unwrap();
        let second = blobs.put("same").unwrap();

        assert_eq!(first, second);
        assert_eq!(blobs.hashes().unwrap().len(), 1);
    }

    #[test]
    fn different_content_does_not_collide() {
        let (blobs, _dir) = store();
        blobs.put("one").unwrap();
        blobs.put("two").unwrap();

        assert_eq!(blobs.hashes().unwrap().len(), 2);
    }

    #[test]
    fn a_missing_blob_says_so_rather_than_reading_as_empty() {
        let (blobs, _dir) = store();
        let error = blobs.read(&"a".repeat(64)).unwrap_err();

        assert_eq!(error.code(), "journal_blob_missing");
    }

    #[test]
    fn a_blob_that_no_longer_matches_its_hash_is_refused() {
        let (blobs, _dir) = store();
        let sha = blobs.put("original").unwrap();
        // Simulate rot: the file under this hash no longer holds that content.
        std::fs::write(blobs.path_of(&sha), "tampered").unwrap();

        let error = blobs.read(&sha).unwrap_err();
        assert_eq!(error.code(), "journal_blob_corrupt");
    }

    #[test]
    fn a_store_that_does_not_exist_yet_is_empty_rather_than_an_error() {
        let (blobs, _dir) = store();
        assert!(blobs.hashes().unwrap().is_empty());
        assert!(!blobs.contains(&"b".repeat(64)));
    }

    #[test]
    fn leftover_temporary_files_are_not_mistaken_for_blobs() {
        let (blobs, _dir) = store();
        let sha = blobs.put("real").unwrap();
        // What an interrupted write leaves behind.
        std::fs::write(blobs.path_of(&format!("{TEMP_PREFIX}{sha}")), "half").unwrap();
        std::fs::write(blobs.path_of("not-a-hash"), "junk").unwrap();

        assert_eq!(blobs.hashes().unwrap(), vec![sha]);
    }

    #[test]
    fn losing_the_race_to_store_identical_content_is_not_an_error() {
        // Both processes derive the same temporary name from the hash. The one
        // that renames second finds it gone; the blob is still correctly there.
        let (blobs, _dir) = store();
        let sha = blobs.put("shared").unwrap();
        let temporary = blobs.path_of(&format!("{TEMP_PREFIX}{sha}"));

        assert!(!temporary.exists());
        finish_put(&temporary, &blobs.path_of(&sha)).unwrap();
        assert_eq!(blobs.read(&sha).unwrap(), "shared");
    }

    #[test]
    fn a_rename_that_leaves_no_blob_behind_is_still_an_error() {
        let (blobs, _dir) = store();
        let error = finish_put(
            &blobs.path_of(".tmp-missing"),
            &blobs.path_of(&"c".repeat(64)),
        )
        .unwrap_err();

        assert_eq!(error.code(), "journal_write_error");
    }

    #[test]
    fn empty_content_is_a_blob_like_any_other() {
        // Distinct from "no blob at all", which is how a delete records that
        // an object no longer exists.
        let (blobs, _dir) = store();
        let sha = blobs.put("").unwrap();

        assert!(blobs.contains(&sha));
        assert_eq!(blobs.read(&sha).unwrap(), "");
    }

    #[cfg(unix)]
    #[test]
    fn blobs_and_their_directory_are_private_to_this_user() {
        use std::os::unix::fs::PermissionsExt as _;

        let (blobs, _dir) = store();
        let sha = blobs.put("customer source").unwrap();

        let file_mode = std::fs::metadata(blobs.path_of(&sha))
            .unwrap()
            .permissions()
            .mode();
        let dir_mode = std::fs::metadata(&blobs.root).unwrap().permissions().mode();
        assert_eq!(file_mode & 0o777, 0o600, "blob mode {file_mode:o}");
        assert_eq!(dir_mode & 0o777, 0o700, "directory mode {dir_mode:o}");
    }
}

//! Where the journal lives, and how one SAP system is told from another.

use std::fmt::Write as _;
use std::path::PathBuf;

use directories::ProjectDirs;
use sha2::{Digest, Sha256};

use super::JournalError;

const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "issi";
const APPLICATION: &str = "fractal";

/// How many hex characters of the host digest go in a system key.
///
/// Enough that two hosts cannot realistically collide, short enough that the
/// readable prefix stays the part a person actually reads.
const HOST_DIGEST_LENGTH: usize = 12;

/// The root of everything this module writes.
///
/// The OS **data** directory, not the config directory. Config is small,
/// hand-edited and something people paste into bug reports; journals are large,
/// machine-written, and belong in neither category.
///
/// # Errors
///
/// Returns [`JournalError::NoDataDirectory`] when the platform exposes no
/// per-user data directory.
pub fn journal_home() -> Result<PathBuf, JournalError> {
    let dirs = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .ok_or(JournalError::NoDataDirectory)?;
    Ok(dirs.data_local_dir().to_path_buf())
}

/// Where content-addressed blobs live, shared by every system.
///
/// # Errors
///
/// Returns [`JournalError::NoDataDirectory`] when the platform exposes no
/// per-user data directory.
pub fn blob_root() -> Result<PathBuf, JournalError> {
    Ok(journal_home()?.join("blobs"))
}

/// Where every system's entries live.
///
/// # Errors
///
/// Returns [`JournalError::NoDataDirectory`] when the platform exposes no
/// per-user data directory.
pub fn journal_root() -> Result<PathBuf, JournalError> {
    Ok(journal_home()?.join("journal"))
}

/// Where one system's entries live.
///
/// # Errors
///
/// Returns [`JournalError::NoDataDirectory`] when the platform exposes no
/// per-user data directory, or [`JournalError::UnusableBaseUrl`] when the
/// profile's URL has no host to key on.
pub fn entry_root(base_url: &str) -> Result<PathBuf, JournalError> {
    Ok(journal_root()?.join(system_key(base_url)?))
}

/// The directory name for one SAP system.
///
/// Keyed on **the host alone**:
///
/// - Not the profile name: profiles get renamed, and two can point at one
///   system. A rename must not orphan the history.
///
/// The name is a readable host prefix plus a digest. The prefix is for the
/// person who opens the directory looking for something to recover; the digest
/// is what actually keeps two hosts apart.
///
/// # Errors
///
/// Returns [`JournalError::UnusableBaseUrl`] when the URL cannot be parsed or
/// carries no host.
pub fn system_key(base_url: &str) -> Result<String, JournalError> {
    let host = host_of(base_url)?;
    let mut key = sanitize(&host);
    key.push('-');
    let digest = Sha256::digest(host.as_bytes());
    for byte in digest.iter().take(HOST_DIGEST_LENGTH / 2) {
        let _ = write!(key, "{byte:02x}");
    }
    Ok(key)
}

/// The directory name for one object inside a system's journal.
///
/// A readable prefix and a digest, for the same reasons as [`system_key`]: ADT
/// URIs contain `/`, and a registered namespace contains more of them.
///
/// Keyed on the object URI alone. `source_part` is deliberately not in it: a
/// change spanning several of a class's includes is one activation and wants
/// one entry, not one per include.
#[must_use]
pub fn object_key(object_uri: &str) -> String {
    let uri = object_uri.trim().to_ascii_lowercase();
    let prefix = uri
        .rsplit('/')
        .find(|segment| !segment.is_empty())
        .unwrap_or("object");
    let mut key = sanitize(prefix);
    key.push('-');
    let digest = Sha256::digest(uri.as_bytes());
    for byte in digest.iter().take(HOST_DIGEST_LENGTH / 2) {
        let _ = write!(key, "{byte:02x}");
    }
    key
}

fn host_of(base_url: &str) -> Result<String, JournalError> {
    let url = url::Url::parse(base_url.trim())
        .map_err(|_| JournalError::UnusableBaseUrl(base_url.to_owned()))?;
    url.host_str()
        .map(str::to_ascii_lowercase)
        .ok_or_else(|| JournalError::UnusableBaseUrl(base_url.to_owned()))
}

/// Reduces a host to characters every supported filesystem accepts.
///
/// Windows is the strict one: it rejects `: * ? " < > |` outright, which an
/// IPv6 literal is full of.
fn sanitize(host: &str) -> String {
    let sanitized: String = host
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || character == '.' || character == '-' {
                character
            } else {
                '_'
            }
        })
        .collect();
    // A host of nothing but separators would otherwise produce a name starting
    // with the digest separator.
    if sanitized.trim_matches(['.', '-', '_']).is_empty() {
        "host".to_owned()
    } else {
        sanitized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_system_reached_two_ways_keeps_one_history() {
        // Different scheme, port, path and case — same host, so the same
        // history. A profile rename or a second profile must not orphan it.
        let key = system_key("https://de3.example.com:8001").unwrap();
        assert_eq!(system_key("https://DE3.example.com:44300/").unwrap(), key);
        assert_eq!(system_key("http://de3.example.com").unwrap(), key);
    }

    #[test]
    fn different_hosts_are_different_systems() {
        assert_ne!(
            system_key("https://de3.example.com:8001").unwrap(),
            system_key("https://qe2.example.com:8001").unwrap()
        );
    }

    #[test]
    fn the_key_is_readable_and_still_unambiguous() {
        let key = system_key("https://de3.example.com:8001").unwrap();
        assert!(key.starts_with("de3.example.com-"), "{key}");
        assert_eq!(key.len(), "de3.example.com-".len() + HOST_DIGEST_LENGTH);
    }

    #[test]
    fn hosts_that_sanitize_alike_stay_apart() {
        // Both collapse to the same readable prefix; only the digest separates
        // them, which is why the digest is there.
        let first = system_key("https://[2001:db8::1]:8001").unwrap();
        let second = system_key("https://[2001:db8::2]:8001").unwrap();
        assert_ne!(first, second);
        for key in [&first, &second] {
            assert!(
                !key.contains(':'),
                "a Windows filesystem would reject this: {key}"
            );
        }
    }

    #[test]
    fn one_object_keys_the_same_whatever_case_it_is_written_in() {
        let key = object_key("/sap/bc/adt/oo/classes/zcl_sample");
        assert_eq!(object_key("/sap/bc/adt/oo/classes/ZCL_SAMPLE"), key);
        assert!(key.starts_with("zcl_sample-"), "{key}");
    }

    #[test]
    fn different_objects_and_families_stay_apart() {
        // Same name, different collection, so a different object.
        assert_ne!(
            object_key("/sap/bc/adt/oo/classes/zsample"),
            object_key("/sap/bc/adt/programs/programs/zsample")
        );
    }

    #[test]
    fn a_registered_namespace_produces_no_separators() {
        let key = object_key("/sap/bc/adt/oo/classes/%2facme%2fzsample");
        assert!(!key.contains('/'), "{key}");
        assert!(!key.contains('%'), "{key}");
    }

    #[test]
    fn a_url_with_no_host_is_refused_rather_than_keyed_oddly() {
        for bad in ["", "not a url", "file:///tmp/x"] {
            assert!(system_key(bad).is_err(), "{bad}");
        }
    }
}

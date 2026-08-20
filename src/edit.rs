use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::pattern::glob_matches;

const SHA256_HEX_LENGTH: usize = 64;

/// A fully validated source change that has not been written to SAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PatchPlan {
    pub updated_source: String,
    pub original_sha256: String,
    pub updated_sha256: String,
    pub original_bytes: usize,
    pub updated_bytes: usize,
    pub replacements: usize,
}

/// A deterministic failure while validating or planning a source edit.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EditError {
    #[error("expected SHA-256 must contain exactly 64 hexadecimal characters")]
    InvalidExpectedSha256,
    #[error("source changed since it was read: expected SHA-256 {expected}, found {actual}")]
    SourceHashMismatch { expected: String, actual: String },
    #[error("patch find text cannot be empty")]
    EmptyFind,
    #[error("patch find text was not found in the source")]
    AnchorNotFound,
    #[error("patch find text matched {occurrences} times")]
    AnchorAmbiguous { occurrences: usize },
    #[error("the patch would not change the source")]
    NoChanges,
    #[error("object '{name}' is outside the configured customer namespaces")]
    ObjectOutsideCustomerNamespaces {
        name: String,
        namespaces: Vec<String>,
    },
}

impl EditError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidExpectedSha256 => "invalid_expected_sha256",
            Self::SourceHashMismatch { .. } => "source_hash_mismatch",
            Self::EmptyFind => "empty_patch_anchor",
            Self::AnchorNotFound => "patch_anchor_not_found",
            Self::AnchorAmbiguous { .. } => "patch_anchor_ambiguous",
            Self::NoChanges => "patch_no_change",
            Self::ObjectOutsideCustomerNamespaces { .. } => "object_outside_customer_namespaces",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::InvalidExpectedSha256 => {
                "Use the SHA-256 returned by the source-read operation.".to_owned()
            }
            Self::SourceHashMismatch { .. } => {
                "Re-read the object and reapply the edit to the current source.".to_owned()
            }
            Self::EmptyFind => "Provide non-empty literal text to replace.".to_owned(),
            Self::AnchorNotFound => {
                "Copy the exact anchor from the source-read result, including its whitespace and line endings."
                    .to_owned()
            }
            Self::AnchorAmbiguous { .. } => {
                "Include more surrounding source so the literal anchor matches exactly once.".to_owned()
            }
            Self::NoChanges => "Use replacement text that differs from the anchor.".to_owned(),
            Self::ObjectOutsideCustomerNamespaces { namespaces, .. } if namespaces.is_empty() => {
                "Configure at least one customer namespace on the selected profile before editing."
                    .to_owned()
            }
            Self::ObjectOutsideCustomerNamespaces { namespaces, .. } => format!(
                "Only objects matching these configured patterns may be edited: {}.",
                namespaces.join(", ")
            ),
        }
    }
}

/// Returns the lowercase SHA-256 of a UTF-8 source string.
#[must_use]
pub fn source_sha256(source: &str) -> String {
    let digest = Sha256::digest(source.as_bytes());
    let mut hash = String::with_capacity(SHA256_HEX_LENGTH);
    for byte in digest {
        let _ = write!(hash, "{byte:02x}");
    }
    hash
}

/// Verifies that an object belongs to one of the configured customer namespaces.
///
/// Matching is case-insensitive and uses the same `*` glob behavior as object
/// search. An empty pattern list denies every object.
///
/// # Errors
///
/// Returns [`EditError::ObjectOutsideCustomerNamespaces`] when no configured
/// pattern matches the complete object name.
pub fn validate_customer_namespace(name: &str, namespaces: &[String]) -> Result<(), EditError> {
    if namespaces.iter().any(|pattern| glob_matches(pattern, name)) {
        return Ok(());
    }

    Err(EditError::ObjectOutsideCustomerNamespaces {
        name: name.to_owned(),
        namespaces: namespaces.to_vec(),
    })
}

/// Plans one exact literal replacement against a known source version.
///
/// This function does not perform I/O. The find text must occur exactly once,
/// and `expected_sha256` must match the supplied source before a plan is
/// returned.
///
/// # Errors
///
/// Returns [`EditError`] when the expected hash is invalid or stale, the find
/// text is empty, absent, or ambiguous, or the replacement produces no change.
pub fn plan_patch(
    source: &str,
    find: &str,
    replace: &str,
    expected_sha256: &str,
) -> Result<PatchPlan, EditError> {
    if find.is_empty() {
        return Err(EditError::EmptyFind);
    }
    if !is_sha256(expected_sha256) {
        return Err(EditError::InvalidExpectedSha256);
    }

    let original_sha256 = source_sha256(source);
    if !expected_sha256.eq_ignore_ascii_case(&original_sha256) {
        return Err(EditError::SourceHashMismatch {
            expected: expected_sha256.to_ascii_lowercase(),
            actual: original_sha256,
        });
    }

    let occurrences = source.match_indices(find).count();
    match occurrences {
        0 => return Err(EditError::AnchorNotFound),
        1 => {}
        _ => return Err(EditError::AnchorAmbiguous { occurrences }),
    }

    let updated_source = source.replacen(find, replace, 1);
    if updated_source == source {
        return Err(EditError::NoChanges);
    }

    Ok(PatchPlan {
        original_sha256,
        updated_sha256: source_sha256(&updated_source),
        original_bytes: source.len(),
        updated_bytes: updated_source.len(),
        updated_source,
        replacements: 1,
    })
}

fn is_sha256(value: &str) -> bool {
    value.len() == SHA256_HEX_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SOURCE: &str = "REPORT zsample.\nWRITE 'before'.\n";

    fn source_hash() -> String {
        source_sha256(SOURCE)
    }

    #[test]
    fn plans_one_exact_replacement() {
        let plan = plan_patch(SOURCE, "'before'", "'after'", &source_hash()).unwrap();

        assert_eq!(plan.updated_source, "REPORT zsample.\nWRITE 'after'.\n");
        assert_eq!(plan.original_sha256, source_hash());
        assert_eq!(plan.updated_sha256, source_sha256(&plan.updated_source));
        assert_eq!(plan.original_bytes, SOURCE.len());
        assert_eq!(plan.updated_bytes, plan.updated_source.len());
        assert_eq!(plan.replacements, 1);
    }

    #[test]
    fn accepts_an_uppercase_expected_hash() {
        let hash = source_hash().to_ascii_uppercase();

        assert!(plan_patch(SOURCE, "before", "after", &hash).is_ok());
    }

    #[test]
    fn calculates_sha256_over_utf8_bytes() {
        assert_eq!(
            source_sha256("hello"),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert_ne!(source_sha256("é"), source_sha256("e"));
    }

    #[test]
    fn rejects_an_invalid_expected_hash() {
        assert_eq!(
            plan_patch(SOURCE, "before", "after", "not-a-hash"),
            Err(EditError::InvalidExpectedSha256)
        );
    }

    #[test]
    fn rejects_a_stale_expected_hash_before_matching_the_anchor() {
        let stale = source_sha256("older source");
        let error = plan_patch(SOURCE, "missing", "after", &stale).unwrap_err();

        assert!(matches!(error, EditError::SourceHashMismatch { .. }));
        assert_eq!(error.code(), "source_hash_mismatch");
    }

    #[test]
    fn rejects_an_empty_anchor() {
        assert_eq!(
            plan_patch(SOURCE, "", "after", &source_hash()),
            Err(EditError::EmptyFind)
        );
    }

    #[test]
    fn rejects_a_missing_anchor() {
        assert_eq!(
            plan_patch(SOURCE, "not present", "after", &source_hash()),
            Err(EditError::AnchorNotFound)
        );
    }

    #[test]
    fn rejects_an_ambiguous_anchor() {
        let source = "WRITE value.\nWRITE value.\n";
        let error = plan_patch(source, "WRITE", "SKIP", &source_sha256(source)).unwrap_err();

        assert_eq!(error, EditError::AnchorAmbiguous { occurrences: 2 });
        assert!(error.hint().contains("exactly once"));
    }

    #[test]
    fn rejects_a_no_op_replacement() {
        assert_eq!(
            plan_patch(SOURCE, "before", "before", &source_hash()),
            Err(EditError::NoChanges)
        );
    }

    #[test]
    fn accepts_plain_and_registered_customer_namespaces() {
        let namespaces = vec!["Z*".to_owned(), "/ACME/*".to_owned()];

        assert!(validate_customer_namespace("zsample", &namespaces).is_ok());
        assert!(validate_customer_namespace("/acme/example", &namespaces).is_ok());
    }

    #[test]
    fn rejects_objects_outside_customer_namespaces() {
        let namespaces = vec!["Z*".to_owned(), "Y*".to_owned()];
        let error = validate_customer_namespace("SAP_STANDARD", &namespaces).unwrap_err();

        assert!(matches!(
            error,
            EditError::ObjectOutsideCustomerNamespaces { .. }
        ));
        assert_eq!(error.code(), "object_outside_customer_namespaces");
        assert!(error.hint().contains("Z*"));
    }

    #[test]
    fn empty_namespace_configuration_fails_closed() {
        let error = validate_customer_namespace("Z_SAMPLE", &[]).unwrap_err();

        assert!(error.hint().contains("Configure at least one"));
    }
}

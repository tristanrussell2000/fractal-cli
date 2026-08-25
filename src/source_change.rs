use std::fmt::Write as _;

use sha2::{Digest, Sha256};
use thiserror::Error;

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

/// A fully validated complete-source replacement that has not been written to SAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceReplacementPlan {
    pub replacement_source: String,
    pub original_sha256: String,
    pub replacement_sha256: String,
    pub original_bytes: usize,
    pub replacement_bytes: usize,
}

/// A deterministic failure while planning a source change.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum SourceChangePlanError {
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
    #[error("complete replacement source cannot be blank")]
    BlankReplacementSource,
    #[error("the complete replacement source is identical to the current source")]
    SourceReplacementNoChanges,
}

impl SourceChangePlanError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidExpectedSha256 => "invalid_expected_sha256",
            Self::SourceHashMismatch { .. } => "source_hash_mismatch",
            Self::EmptyFind => "empty_patch_anchor",
            Self::AnchorNotFound => "patch_anchor_not_found",
            Self::AnchorAmbiguous { .. } => "patch_anchor_ambiguous",
            Self::NoChanges => "patch_no_change",
            Self::BlankReplacementSource => "blank_replacement_source",
            Self::SourceReplacementNoChanges => "source_replacement_no_change",
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
            Self::BlankReplacementSource => {
                "Provide the complete non-blank source for the object.".to_owned()
            }
            Self::SourceReplacementNoChanges => {
                "The supplied complete source already matches SAP's current inactive source."
                    .to_owned()
            }
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

/// Plans one exact literal replacement against a known source version.
///
/// This function does not perform I/O. The find text must occur exactly once,
/// and `expected_sha256` must match the supplied source before a plan is
/// returned.
///
/// # Errors
///
/// Returns [`SourceChangePlanError`] when the expected hash is invalid or stale, the find
/// text is empty, absent, or ambiguous, or the replacement produces no change.
pub fn plan_patch(
    source: &str,
    find: &str,
    replace: &str,
    expected_sha256: &str,
) -> Result<PatchPlan, SourceChangePlanError> {
    if find.is_empty() {
        return Err(SourceChangePlanError::EmptyFind);
    }
    if !is_sha256(expected_sha256) {
        return Err(SourceChangePlanError::InvalidExpectedSha256);
    }

    let original_sha256 = source_sha256(source);
    if !expected_sha256.eq_ignore_ascii_case(&original_sha256) {
        return Err(SourceChangePlanError::SourceHashMismatch {
            expected: expected_sha256.to_ascii_lowercase(),
            actual: original_sha256,
        });
    }

    let occurrences = source.match_indices(find).count();
    match occurrences {
        0 => return Err(SourceChangePlanError::AnchorNotFound),
        1 => {}
        _ => return Err(SourceChangePlanError::AnchorAmbiguous { occurrences }),
    }

    let updated_source = source.replacen(find, replace, 1);
    if updated_source == source {
        return Err(SourceChangePlanError::NoChanges);
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

/// Plans replacement of an object's complete source against a known source version.
///
/// This function does not perform I/O. Blank source and byte-identical
/// replacements are rejected. When `expected_sha256` is supplied, it must be a
/// valid SHA-256 matching the current source; otherwise the caller's locked read
/// is treated as the authoritative baseline.
///
/// # Errors
///
/// Returns [`SourceChangePlanError`] when replacement source is blank or unchanged, or when
/// the optional expected hash is invalid or stale.
pub fn plan_source_replacement(
    current_source: &str,
    replacement_source: &str,
    expected_sha256: Option<&str>,
) -> Result<SourceReplacementPlan, SourceChangePlanError> {
    if replacement_source.trim().is_empty() {
        return Err(SourceChangePlanError::BlankReplacementSource);
    }

    let original_sha256 = source_sha256(current_source);
    if let Some(expected_sha256) = expected_sha256 {
        if !is_sha256(expected_sha256) {
            return Err(SourceChangePlanError::InvalidExpectedSha256);
        }
        if !expected_sha256.eq_ignore_ascii_case(&original_sha256) {
            return Err(SourceChangePlanError::SourceHashMismatch {
                expected: expected_sha256.to_ascii_lowercase(),
                actual: original_sha256,
            });
        }
    }

    if replacement_source == current_source {
        return Err(SourceChangePlanError::SourceReplacementNoChanges);
    }

    Ok(SourceReplacementPlan {
        replacement_source: replacement_source.to_owned(),
        original_sha256,
        replacement_sha256: source_sha256(replacement_source),
        original_bytes: current_source.len(),
        replacement_bytes: replacement_source.len(),
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
            Err(SourceChangePlanError::InvalidExpectedSha256)
        );
    }

    #[test]
    fn rejects_a_stale_expected_hash_before_matching_the_anchor() {
        let stale = source_sha256("older source");
        let error = plan_patch(SOURCE, "missing", "after", &stale).unwrap_err();

        assert!(matches!(
            error,
            SourceChangePlanError::SourceHashMismatch { .. }
        ));
        assert_eq!(error.code(), "source_hash_mismatch");
    }

    #[test]
    fn rejects_an_empty_anchor() {
        assert_eq!(
            plan_patch(SOURCE, "", "after", &source_hash()),
            Err(SourceChangePlanError::EmptyFind)
        );
    }

    #[test]
    fn rejects_a_missing_anchor() {
        assert_eq!(
            plan_patch(SOURCE, "not present", "after", &source_hash()),
            Err(SourceChangePlanError::AnchorNotFound)
        );
    }

    #[test]
    fn rejects_an_ambiguous_anchor() {
        let source = "WRITE value.\nWRITE value.\n";
        let error = plan_patch(source, "WRITE", "SKIP", &source_sha256(source)).unwrap_err();

        assert_eq!(
            error,
            SourceChangePlanError::AnchorAmbiguous { occurrences: 2 }
        );
        assert!(error.hint().contains("exactly once"));
    }

    #[test]
    fn rejects_a_no_op_replacement() {
        assert_eq!(
            plan_patch(SOURCE, "before", "before", &source_hash()),
            Err(SourceChangePlanError::NoChanges)
        );
    }

    #[test]
    fn plans_a_complete_source_replacement_without_requiring_a_hash() {
        let replacement = "REPORT zsample.\nWRITE 'after'.\n";
        let plan = plan_source_replacement(SOURCE, replacement, None).unwrap();

        assert_eq!(plan.replacement_source, replacement);
        assert_eq!(plan.original_sha256, source_hash());
        assert_eq!(plan.replacement_sha256, source_sha256(replacement));
        assert_eq!(plan.original_bytes, SOURCE.len());
        assert_eq!(plan.replacement_bytes, replacement.len());
    }

    #[test]
    fn complete_source_replacement_honors_an_optional_expected_hash() {
        let replacement = "REPORT zsample.\nWRITE 'after'.\n";
        assert!(
            plan_source_replacement(
                SOURCE,
                replacement,
                Some(&source_hash().to_ascii_uppercase())
            )
            .is_ok()
        );

        let error =
            plan_source_replacement(SOURCE, replacement, Some(&source_sha256("older source")))
                .unwrap_err();
        assert!(matches!(
            error,
            SourceChangePlanError::SourceHashMismatch { .. }
        ));
    }

    #[test]
    fn rejects_blank_or_unchanged_complete_source() {
        assert_eq!(
            plan_source_replacement(SOURCE, " \n\t", None),
            Err(SourceChangePlanError::BlankReplacementSource)
        );
        assert_eq!(
            plan_source_replacement(SOURCE, SOURCE, None),
            Err(SourceChangePlanError::SourceReplacementNoChanges)
        );
    }
}

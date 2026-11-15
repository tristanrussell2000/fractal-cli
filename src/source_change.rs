use std::fmt::Write as _;

use crate::reportable_error::ReportableError;
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
    /// Set when the find and replace text were rewritten to the source's line
    /// endings before matching; see [`plan_patch`].
    pub adapted_line_endings: Option<LineEnding>,
}

/// The line-ending convention a stored source uses throughout.
///
/// SAP stores ABAP source with CRLF and the CDS family (DDLS, BDEF, DCLS,
/// DDLX, SRVD) with LF, and a caller writing a multi-line patch types LF.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineEnding {
    Lf,
    CrLf,
}

impl LineEnding {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Lf => "lf",
            Self::CrLf => "crlf",
        }
    }

    /// The convention `source` uses on every line, or `None` when it has no
    /// line break at all or mixes the two.
    #[must_use]
    pub fn uniform_in(source: &str) -> Option<Self> {
        let breaks = source.matches('\n').count();
        if breaks == 0 {
            return None;
        }
        match source.matches("\r\n").count() {
            0 => Some(Self::Lf),
            crlf if crlf == breaks => Some(Self::CrLf),
            _ => None,
        }
    }

    /// Rewrites every line break in `text` to this convention.
    #[must_use]
    pub fn apply(self, text: &str) -> String {
        let lf = text.replace("\r\n", "\n");
        match self {
            Self::Lf => lf,
            Self::CrLf => lf.replace('\n', "\r\n"),
        }
    }
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

impl ReportableError for SourceChangePlanError {
    fn code(&self) -> &'static str {
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

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::InvalidExpectedSha256 => {
                "Use the SHA-256 returned by the source-read operation.".to_owned()
            }
            Self::SourceHashMismatch { .. } => {
                "Re-read the object and reapply the edit to the current source.".to_owned()
            }
            Self::EmptyFind => "Provide non-empty literal text to replace.".to_owned(),
            Self::AnchorNotFound => {
                "Copy the anchor from the source-read result, including its whitespace and indentation. Line endings are matched to the source's own when it uses one convention throughout."
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
        })
    }
}

/// Verifies an optional expected SHA-256 against content just read from SAP.
///
/// The stale-read guard on its own, separated from patch and replacement
/// planning because a whole-document write has no plan: it only needs to know
/// that nobody changed the document between the read and the write.
///
/// # Errors
///
/// Returns [`SourceChangePlanError::InvalidExpectedSha256`] for a malformed
/// hash, or [`SourceChangePlanError::SourceHashMismatch`] when the content has
/// changed since the caller read it.
pub fn verify_expected_sha256(
    expected_sha256: Option<&str>,
    content: &str,
) -> Result<(), SourceChangePlanError> {
    let Some(expected_sha256) = expected_sha256 else {
        return Ok(());
    };
    if !is_sha256(expected_sha256) {
        return Err(SourceChangePlanError::InvalidExpectedSha256);
    }
    let actual = source_sha256(content);
    if !expected_sha256.eq_ignore_ascii_case(&actual) {
        return Err(SourceChangePlanError::SourceHashMismatch {
            expected: expected_sha256.to_ascii_lowercase(),
            actual,
        });
    }
    Ok(())
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

/// Plans one literal replacement against a known source version.
///
/// This function does not perform I/O. The find text must occur exactly once,
/// and `expected_sha256` must match the supplied source before a plan is
/// returned.
///
/// Line endings are the one thing not matched literally. When the source uses
/// one convention throughout, the replacement is written in it, and a find
/// text that does not occur as given is retried in it — so a multi-line
/// anchor typed with LF matches ABAP source SAP stores with CRLF. The plan
/// says when that retry was what matched.
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

    // An exact match always wins, so a caller who copied the stored line
    // endings gets exactly what they asked for. Otherwise, when the source
    // uses one convention throughout, the find and replace text are rewritten
    // to it: SAP would normalize the replacement on save anyway, and an anchor
    // typed with LF can never match CRLF source byte for byte.
    let convention = LineEnding::uniform_in(source);
    let adapted = convention.filter(|ending| !source.contains(find) && ending.apply(find) != find);
    let (find, replace) = match (adapted, convention) {
        (Some(ending), _) => (ending.apply(find), ending.apply(replace)),
        (None, Some(ending)) => (find.to_owned(), ending.apply(replace)),
        (None, None) => (find.to_owned(), replace.to_owned()),
    };

    let occurrences = source.match_indices(find.as_str()).count();
    match occurrences {
        0 => return Err(SourceChangePlanError::AnchorNotFound),
        1 => {}
        _ => return Err(SourceChangePlanError::AnchorAmbiguous { occurrences }),
    }

    let updated_source = source.replacen(find.as_str(), &replace, 1);
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
        adapted_line_endings: adapted,
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
    plan_source_replacement_allowing_blank(current_source, replacement_source, expected_sha256)
}

/// [`plan_source_replacement`] without the blank-source refusal, for the one
/// case where empty is the intended content: putting an include back to the
/// empty state SAP creates it in.
///
/// # Errors
///
/// Returns [`SourceChangePlanError`] when the replacement is unchanged, or when
/// the optional expected hash is invalid or stale.
pub fn plan_source_replacement_allowing_blank(
    current_source: &str,
    replacement_source: &str,
    expected_sha256: Option<&str>,
) -> Result<SourceReplacementPlan, SourceChangePlanError> {
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

/// Whether two sources are the same as SAP stores source, rather than as bytes.
///
/// What SAP stores is not byte-for-byte what was sent: it uses CRLF line
/// endings, and has been seen both to add a trailing line break and to drop
/// one. Any "is what is stored what I meant to store" check compares this way.
#[must_use]
pub fn same_source(left: &str, right: &str) -> bool {
    normalized_source(left) == normalized_source(right)
}

/// Whether two sources differ in nothing but trailing whitespace.
///
/// Narrower than [`same_source`]: a line-ending change still counts. For
/// checks that must preserve source exactly, where the one tolerated
/// difference is SAP dropping a trailing line break on save, as it does when a
/// class include's generated template is written back.
#[must_use]
pub fn same_except_trailing_whitespace(left: &str, right: &str) -> bool {
    left.trim_end() == right.trim_end()
}

fn normalized_source(source: &str) -> String {
    source.replace("\r\n", "\n").trim_end().to_owned()
}

fn is_sha256(value: &str) -> bool {
    value.len() == SHA256_HEX_LENGTH && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    #[test]
    fn an_absent_expected_hash_is_no_constraint() {
        assert!(verify_expected_sha256(None, "anything at all").is_ok());
    }

    #[test]
    fn a_matching_hash_passes_whatever_its_case() {
        let content = "<doma:domain/>";
        let hash = source_sha256(content);
        assert!(verify_expected_sha256(Some(&hash), content).is_ok());
        assert!(verify_expected_sha256(Some(&hash.to_ascii_uppercase()), content).is_ok());
    }

    #[test]
    fn a_changed_document_is_refused_and_names_both_hashes() {
        let hash = source_sha256("<doma:domain>old</doma:domain>");
        let error =
            verify_expected_sha256(Some(&hash), "<doma:domain>new</doma:domain>").unwrap_err();

        assert_eq!(error.code(), "source_hash_mismatch");
        let SourceChangePlanError::SourceHashMismatch { expected, actual } = &error else {
            panic!("expected a hash mismatch");
        };
        assert_eq!(expected, &hash);
        assert_ne!(actual, &hash);
    }

    #[test]
    fn a_malformed_hash_is_refused_before_it_is_compared() {
        // Too short, non-hex, and empty must all fail as malformed rather than
        // as a mismatch: the caller made a different mistake.
        for bad in ["abc", &"z".repeat(64), ""] {
            let error = verify_expected_sha256(Some(bad), "content").unwrap_err();
            assert_eq!(error.code(), "invalid_expected_sha256", "{bad}");
        }
    }

    use super::*;

    const SOURCE: &str = "REPORT zsample.\nWRITE 'before'.\n";

    fn source_hash() -> String {
        source_sha256(SOURCE)
    }

    /// How SAP serves ABAP source: CRLF on every line.
    const CRLF_SOURCE: &str = "METHOD get.\r\n  ENDMETHOD.\r\nMETHOD set.\r\n  ENDMETHOD.\r\n";

    fn plan(source: &str, find: &str, replace: &str) -> Result<PatchPlan, SourceChangePlanError> {
        plan_patch(source, find, replace, &source_sha256(source))
    }

    #[test]
    fn a_multi_line_lf_anchor_matches_crlf_source() {
        let plan = plan(
            CRLF_SOURCE,
            "METHOD set.\n  ENDMETHOD.",
            "METHOD set.\n    x = 1.\n  ENDMETHOD.",
        )
        .unwrap();

        assert_eq!(plan.adapted_line_endings, Some(LineEnding::CrLf));
        // The replacement is written in the source's convention too, so the
        // stored source stays uniform rather than mixed.
        assert_eq!(
            plan.updated_source,
            "METHOD get.\r\n  ENDMETHOD.\r\nMETHOD set.\r\n    x = 1.\r\n  ENDMETHOD.\r\n"
        );
    }

    #[test]
    fn an_anchor_that_matches_as_given_is_not_adapted() {
        let plan = plan(CRLF_SOURCE, "METHOD set.\r\n", "METHOD put.\r\n").unwrap();
        assert_eq!(plan.adapted_line_endings, None);
        assert!(plan.updated_source.contains("METHOD put.\r\n"));
    }

    #[test]
    fn a_single_line_anchor_still_gets_its_replacement_in_the_source_convention() {
        // The anchor needed no rewriting, so nothing is reported as adapted,
        // but the multi-line replacement is still written as CRLF.
        let plan = plan(CRLF_SOURCE, "METHOD set.", "METHOD set.\n  \" new").unwrap();
        assert_eq!(plan.adapted_line_endings, None);
        assert!(plan.updated_source.contains("METHOD set.\r\n  \" new\r\n"));
    }

    #[test]
    fn a_crlf_anchor_matches_lf_source() {
        // CDS sources keep LF; an anchor copied from somewhere that wrote CRLF
        // is rewritten the other way.
        let source = "define view entity ZI_X\n  as select from t000\n{\n  key mandt\n}\n";
        let plan = plan(
            source,
            "{\r\n  key mandt\r\n}",
            "{\r\n  key mandt as Client\r\n}",
        )
        .unwrap();
        assert_eq!(plan.adapted_line_endings, Some(LineEnding::Lf));
        assert!(plan.updated_source.contains("{\n  key mandt as Client\n}"));
        assert!(!plan.updated_source.contains('\r'));
    }

    #[test]
    fn mixed_line_endings_are_matched_literally() {
        // No single convention to rewrite to, so nothing is guessed.
        let source = "line one\r\nline two\nline three\n";
        assert_eq!(LineEnding::uniform_in(source), None);
        assert_eq!(
            plan(source, "line one\nline two", "x"),
            Err(SourceChangePlanError::AnchorNotFound)
        );
        assert!(plan(source, "line one\r\nline two", "x").is_ok());
    }

    #[test]
    fn ambiguity_is_judged_after_rewriting() {
        let source = "a\r\nb\r\na\r\nb\r\n";
        assert_eq!(
            plan(source, "a\nb", "c"),
            Err(SourceChangePlanError::AnchorAmbiguous { occurrences: 2 })
        );
    }

    #[test]
    fn a_replacement_that_differs_only_in_line_endings_changes_nothing() {
        assert_eq!(
            plan(CRLF_SOURCE, "METHOD set.\n", "METHOD set.\r\n"),
            Err(SourceChangePlanError::NoChanges)
        );
    }

    #[test]
    fn names_the_convention_a_source_uses() {
        assert_eq!(LineEnding::uniform_in("a\r\nb\r\n"), Some(LineEnding::CrLf));
        assert_eq!(LineEnding::uniform_in("a\nb"), Some(LineEnding::Lf));
        assert_eq!(LineEnding::uniform_in("one line"), None);
        assert_eq!(LineEnding::CrLf.apply("a\nb\r\nc"), "a\r\nb\r\nc");
        assert_eq!(LineEnding::Lf.apply("a\r\nb\nc"), "a\nb\nc");
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
        assert!(error.hint().unwrap().contains("exactly once"));
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
    fn compares_source_the_way_sap_stores_it() {
        let sent = "CLASS zcl_x DEFINITION.\nENDCLASS.";
        let stored = "CLASS zcl_x DEFINITION.\r\nENDCLASS.\r\n";
        assert!(same_source(sent, stored));
        // A trailing line break dropped on save, seen on a class include.
        assert!(same_source(stored, "CLASS zcl_x DEFINITION.\r\nENDCLASS."));
        assert!(!same_source(sent, "CLASS zcl_y DEFINITION."));
    }

    #[test]
    fn only_trailing_whitespace_is_tolerated_where_exactness_matters() {
        let active = "*\"* template\r\n*\"* more\r\n";
        assert!(same_except_trailing_whitespace(
            active,
            "*\"* template\r\n*\"* more"
        ));
        // A line-ending change is still a difference.
        assert!(!same_except_trailing_whitespace(
            active,
            "*\"* template\n*\"* more\n"
        ));
    }

    #[test]
    fn blank_source_is_accepted_only_when_asked_for() {
        assert!(plan_source_replacement(SOURCE, "", None).is_err());
        let plan = plan_source_replacement_allowing_blank(SOURCE, "", None).unwrap();
        assert_eq!(plan.replacement_bytes, 0);
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

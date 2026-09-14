//! Whether somebody else has an unactivated edit staged on an object.
//!
//! A write lands in the inactive layer, which is shared and last-write-wins, so
//! a caller who never saw the staged edit destroys it. The object document
//! answers both halves of the question in one read: `adtcore:version` says
//! whether unactivated content exists at all, and `adtcore:changedBy` says who
//! left it.

use thiserror::Error;

use super::{
    adt_response::{AdtResponseParseError, parse_adt_document},
    adt_version::AdtVersion,
    client::{SapClient, SapClientError},
    find_non_empty_attribute,
    metadata_document::declared_version,
};
use crate::reportable_error::{ReportableError, sap_http_status};
use crate::suggested_command;

const ACTIVE: &str = "active";

/// What a write should do about an edit somebody else has staged.
///
/// One value rather than a caller beside a `force` flag: a caller is only
/// meaningful when there is a check to run, and forcing makes the caller
/// irrelevant. As two fields either could be set against the other with
/// nothing to object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StagedEditPolicy {
    /// Refuse if the staged edit belongs to anybody but this user.
    Protect(String),
    /// Replace whatever is staged, whoever staged it.
    Overwrite,
}

/// An unactivated edit sitting on an object, and who left it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedWork {
    pub author: String,
}

#[derive(Debug, Error)]
pub enum StagedWorkError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error(transparent)]
    Parse(#[from] AdtResponseParseError),
    #[error("{author} has an unactivated edit staged on {object_type} {name}")]
    StagedByAnother {
        object_type: &'static str,
        name: String,
        author: String,
    },
}

impl StagedWorkError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            _ => None,
        }
    }
}

impl ReportableError for StagedWorkError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(error) => error.code(),
            Self::StagedByAnother { .. } => "edit_staged_by_another_user",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Sap(error) => error.hint()?,
            Self::Parse(error) => error.hint()?,
            Self::StagedByAnother { author, .. } => format!(
                "The inactive version holds {author}'s work, and a write replaces it whole. Read that version, fold your change into it, and pass its SHA-256 as --expected-sha256; or --force to overwrite it deliberately."
            ),
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::StagedByAnother {
                object_type, name, ..
            } => Some(suggested_command::edit_read(
                object_type,
                name,
                AdtVersion::Inactive.as_str(),
            )),
            Self::Sap(error) => error.suggested_command(),
            Self::Parse(_) => None,
        }
    }
}

/// Reads whoever has unactivated content staged on this object, if anybody.
///
/// `None` means the object has no unactivated content: asking for the inactive
/// layer of an object without one is answered with the active document, which
/// says so.
///
/// # Errors
///
/// Returns [`StagedWorkError`] when the object cannot be read or its document
/// does not parse.
pub async fn read_staged_work(
    sap: &SapClient,
    object_uri: &str,
) -> Result<Option<StagedWork>, StagedWorkError> {
    let xml = sap
        .get_text_with_query(object_uri, &[("version", AdtVersion::Inactive.as_str())])
        .await?;
    let document = parse_adt_document(&xml)?;
    let root = document.root_element();
    if declared_version(root).as_deref() == Some(ACTIVE) {
        return Ok(None);
    }
    Ok(find_non_empty_attribute(root, "changedBy").map(|author| StagedWork { author }))
}

/// Refuses a whole-document write that would replace somebody else's staged edit.
///
/// Reads nothing when the answer could not change the outcome, so the extra
/// request is paid for only where it can refuse. The caller's own staged edit
/// is not in the way: overwriting that is ordinary iteration.
///
/// # Errors
///
/// Returns [`StagedWorkError`] when the object cannot be read, or
/// [`StagedWorkError::StagedByAnother`] when another user's edit would be lost.
pub async fn refuse_when_staged_by_another(
    sap: &SapClient,
    object_uri: &str,
    policy: &StagedEditPolicy,
    object_type: &'static str,
    name: &str,
    asserted_hash: bool,
) -> Result<(), StagedWorkError> {
    // An asserted hash is the caller saying they have seen what is there; the
    // write path's own gate then decides whether they were right.
    let StagedEditPolicy::Protect(caller) = policy else {
        return Ok(());
    };
    if asserted_hash {
        return Ok(());
    }
    let Some(staged) = read_staged_work(sap, object_uri).await? else {
        return Ok(());
    };
    // Case-insensitively: SAP reports user ids uppercased, and a profile need
    // not spell its own that way.
    if staged.author.eq_ignore_ascii_case(caller) {
        return Ok(());
    }
    Err(StagedWorkError::StagedByAnother {
        object_type,
        name: name.to_owned(),
        author: staged.author,
    })
}

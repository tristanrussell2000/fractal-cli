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
    class_include::ClassInclude,
    client::{SapClient, SapClientError},
    find_attribute_value, find_non_empty_attribute,
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
    #[error(
        "{author} has an unactivated edit staged on {object_type} {name}{}",
        include.map_or_else(String::new, |include| format!("'s {} include", include.as_str()))
    )]
    StagedByAnother {
        object_type: &'static str,
        name: String,
        /// The class include the edit is staged in, for a class.
        include: Option<ClassInclude>,
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
                object_type,
                name,
                include,
                ..
            } => Some(include.map_or_else(
                || suggested_command::edit_read(object_type, name, AdtVersion::Inactive.as_str()),
                |include| {
                    suggested_command::edit_read_include(
                        object_type,
                        name,
                        include.as_str(),
                        AdtVersion::Inactive.as_str(),
                    )
                },
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

/// A class include with unactivated content, and who left it there.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedInclude {
    pub include: ClassInclude,
    pub author: Option<String>,
}

/// Which of a class's includes have unactivated content, whoever staged them.
///
/// Read from the class document, where each `class:include` carries its own
/// `adtcore:version` and `adtcore:changedBy`. Object-scoped, so it shows other
/// users' staged includes too. For what the caller alone has staged, which is
/// what an activation publishes, see [`super::worklist`].
///
/// # Errors
///
/// Returns [`StagedWorkError`] when the document cannot be read or parsed.
pub async fn read_staged_by_anyone(
    sap: &SapClient,
    class_uri: &str,
) -> Result<Vec<StagedInclude>, StagedWorkError> {
    let xml = sap
        .get_text_with_query(class_uri, &[("version", AdtVersion::Inactive.as_str())])
        .await?;
    staged_by_anyone_in(&xml)
}

fn staged_by_anyone_in(xml: &str) -> Result<Vec<StagedInclude>, StagedWorkError> {
    let document = parse_adt_document(xml)?;
    Ok(document
        .root_element()
        .children()
        .filter(|node| node.is_element() && node.tag_name().name() == "include")
        .filter(|node| declared_version(*node).as_deref() == Some("inactive"))
        .filter_map(|node| {
            let include = ClassInclude::parse(find_attribute_value(node, "includeType")?)?;
            Some(StagedInclude {
                include,
                author: find_non_empty_attribute(node, "changedBy"),
            })
        })
        .collect())
}

/// Whether a class has one of its includes yet, staged or active.
///
/// Only the test-classes include can be missing; the class document lists an
/// include once it exists.
///
/// # Errors
///
/// Returns [`StagedWorkError`] when the document cannot be read or parsed.
pub async fn class_has_include(
    sap: &SapClient,
    class_uri: &str,
    include: ClassInclude,
) -> Result<bool, StagedWorkError> {
    let xml = sap
        .get_text_with_query(class_uri, &[("version", AdtVersion::Inactive.as_str())])
        .await?;
    let document = parse_adt_document(&xml)?;
    Ok(document.root_element().children().any(|node| {
        node.is_element()
            && node.tag_name().name() == "include"
            && find_attribute_value(node, "includeType").and_then(ClassInclude::parse)
                == Some(include)
    }))
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
    target: StagedWorkTarget<'_>,
    policy: &StagedEditPolicy,
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
    // For a class, only the include being written. The class-level marker goes
    // inactive when *any* include is staged, but a write replaces one include
    // and cannot touch the others.
    let author = match target.include {
        Some(include) => read_staged_by_anyone(sap, target.object_uri)
            .await?
            .into_iter()
            .find(|staged| staged.include == include)
            .map(|staged| staged.author),
        None => read_staged_work(sap, target.object_uri)
            .await?
            .map(|staged| Some(staged.author)),
    };
    let Some(author) = author else {
        return Ok(());
    };
    // No author recorded is treated as somebody else's: refusing is the side
    // that can be recovered from.
    // Case-insensitively: SAP reports user ids uppercased, and a profile need
    // not spell its own that way.
    if author
        .as_deref()
        .is_some_and(|author| author.eq_ignore_ascii_case(caller))
    {
        return Ok(());
    }
    Err(StagedWorkError::StagedByAnother {
        object_type: target.object_type,
        name: target.name.to_owned(),
        include: target.include,
        author: author.unwrap_or_else(|| "another user".to_owned()),
    })
}

/// What a write is about to replace, for [`refuse_when_staged_by_another`].
#[derive(Debug, Clone, Copy)]
pub struct StagedWorkTarget<'a> {
    pub object_uri: &'a str,
    pub object_type: &'static str,
    pub name: &'a str,
    /// The class include being written, for a class.
    pub include: Option<ClassInclude>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_the_includes_still_staged_and_who_staged_them() {
        let staged = staged_by_anyone_in(
            r#"<class:abapClass xmlns:class="http://www.sap.com/adt/oo/classes" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:version="inactive"><class:include class:includeType="definitions" adtcore:version="active" adtcore:changedBy="DEVELOPER"/><class:include class:includeType="main" adtcore:version="inactive" adtcore:changedBy="COLLEAGUE"/></class:abapClass>"#,
        )
        .unwrap();
        assert_eq!(
            staged,
            vec![StagedInclude {
                include: ClassInclude::Main,
                author: Some("COLLEAGUE".to_owned()),
            }]
        );
    }
}

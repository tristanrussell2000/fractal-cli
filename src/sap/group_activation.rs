//! Activating several source objects in one request.
//!
//! Two new objects that reference each other, such as a root view with a
//! `composition` and its child with an `association to parent`, cannot be
//! activated one at a time: each one's syntax check fails because the other is
//! not active yet, and a check run over both still checks each against the
//! active versions. One activation request naming both activates both.
//!
//! - **No Fractal pre-check.** It cannot pass for such a pair. SAP's own
//!   checks run during the activation (`preauditRequested=true`).
//! - **Not all-or-nothing.** With one object broken, SAP answers "Activation
//!   was cancelled", claims `activationExecuted="true"`, leaves the broken one
//!   inactive, and still publishes the others' source while keeping them on
//!   the worklist. So nothing here trusts the answer: each object is proved
//!   from its own post-state, and its journal entry resolved by that alone.
//! - Running the same group again after a fix clears every object.

use std::time::{SystemTime, UNIX_EPOCH};

use thiserror::Error;

use super::{
    activation_request::{
        AdtActivationMessage, first_message_hint, parse_activation_response, post_activation_of,
    },
    adt_message_severity::AdtMessageSeverity,
    adt_version::AdtVersion,
    client::SapClient,
    edit_session::attach_adt_object_to_transport,
    editable_source::{
        AdtSourceSnapshot, EditableAdtObjectType, EditableAdtSourceIdentity,
        read_adt_source_for_edit, validate_adt_edit_target,
    },
    package_authorization::authorize_object_package,
    source_activation::{
        AdtSourceActivationError, PublishedInclude, activation_after, activation_before,
        publishing_includes, verify_activation_post_state, verify_published,
    },
    worklist::read_worklist,
};
use crate::config::EditPolicy;
use crate::journal::recorder::{AfterImages, Journal, Resolution, resolve};
use crate::reportable_error::ReportableError;
use crate::source_change::same_source;
use crate::suggested_command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupActivationRequest {
    pub objects: Vec<(EditableAdtObjectType, String)>,
    pub transport: Option<String>,
}

/// What one object of the group ended up as, read from SAP afterwards.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GroupOutcome {
    /// Off the worklist, and its active source is what was staged.
    Activated,
    /// Its staged source is now the active one, but SAP still lists it as
    /// inactive: the state a cancelled group activation leaves the objects
    /// that were not at fault in. Running the group again clears it.
    PublishedStillListed,
    /// Still inactive; SAP's messages for it say why.
    NotActivated,
    /// The post-state could not be read, so the outcome is unknown.
    Unverified,
}

impl GroupOutcome {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Activated => "activated",
            Self::PublishedStillListed => "published_still_listed",
            Self::NotActivated => "not_activated",
            Self::Unverified => "unverified",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupMember {
    pub identity: EditableAdtSourceIdentity,
    pub outcome: GroupOutcome,
    pub active_after: Option<AdtSourceSnapshot>,
    /// For a class, each include the activation published.
    pub published: Vec<PublishedInclude>,
    /// SAP's messages about this object.
    pub messages: Vec<AdtActivationMessage>,
    pub journal_entry_incomplete: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupActivationResult {
    /// The id the group's journal entries share, when journaled.
    pub group: Option<String>,
    pub transport: Option<String>,
    pub members: Vec<GroupMember>,
    /// Named, but with nothing staged, so left out of the request.
    pub nothing_to_activate: Vec<EditableAdtSourceIdentity>,
    pub sap_reported_activation_executed: Option<bool>,
    /// Messages SAP gave about none of the objects in particular.
    pub general_messages: Vec<AdtActivationMessage>,
}

/// One object's line in a refusal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemberSummary {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub outcome: GroupOutcome,
    pub first_error: Option<String>,
}

#[derive(Debug, Error)]
pub enum GroupActivationError {
    #[error("{name}: {source}")]
    Member {
        name: String,
        #[source]
        source: Box<AdtSourceActivationError>,
    },
    #[error("{name} is named more than once")]
    Duplicate { name: String },
    #[error("none of the objects has anything staged to activate: {}", names.join(", "))]
    NothingToActivate { names: Vec<String> },
    #[error("the ADT activation request failed: {0}")]
    Request(#[source] super::client::SapClientError),
    #[error("not every object activated: {}", describe(members))]
    Incomplete { members: Vec<MemberSummary> },
}

fn describe(members: &[MemberSummary]) -> String {
    members
        .iter()
        .map(|member| format!("{} {}", member.name, member.outcome.as_str()))
        .collect::<Vec<_>>()
        .join(", ")
}

impl ReportableError for GroupActivationError {
    fn code(&self) -> &'static str {
        match self {
            Self::Member { source, .. } => source.code(),
            Self::Duplicate { .. } => "edit_activation_duplicate_object",
            Self::NothingToActivate { .. } => "edit_activation_no_inactive_source",
            Self::Request(_) => "edit_activation_request_failed",
            Self::Incomplete { .. } => "edit_activation_incomplete",
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Member { source, .. } => source.status(),
            Self::Request(source) => crate::reportable_error::sap_http_status(Some(source)),
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Member { source, .. } => return source.hint(),
            Self::Duplicate { .. } => "Name each object once.".to_owned(),
            Self::NothingToActivate { .. } => {
                "Save an inactive change to at least one of them first.".to_owned()
            }
            Self::Request(_) => {
                "The request may have reached SAP. Check each object's state before retrying."
                    .to_owned()
            }
            Self::Incomplete { members } => first_message_hint(
                members
                    .iter()
                    .filter_map(|member| member.first_error.as_deref()),
                "Fix what SAP reports, then run the same command again: re-activating the group clears every object, including any left published but still listed.",
            ),
        })
    }

    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Member { source, .. } => source.suggested_command(),
            // The object that failed, ahead of one left only half-finished.
            Self::Incomplete { members } => members
                .iter()
                .find(|member| member.outcome == GroupOutcome::NotActivated)
                .or_else(|| {
                    members
                        .iter()
                        .find(|member| member.outcome != GroupOutcome::Activated)
                })
                .map(|member| {
                    suggested_command::edit_check(
                        member.object_type.as_str(),
                        &member.name,
                        AdtVersion::Inactive.as_str(),
                    )
                }),
            _ => None,
        }
    }
}

/// One object being activated, before the request.
struct Prepared {
    identity: EditableAdtSourceIdentity,
    staged: AdtSourceSnapshot,
    publishing: Vec<(super::class_include::ClassInclude, AdtSourceSnapshot)>,
    entry: Option<crate::journal::entry::JournalEntry>,
}

/// Activates several source objects in one request, and proves each.
///
/// # Errors
///
/// Returns [`GroupActivationError`] when any object fails validation or
/// preparation (nothing is activated then), when none has anything staged,
/// when the request fails, or when not every object ends up activated.
// One ordered protocol (validate, journal, activate, verify each); splitting
// it would scatter the steps that must happen in sequence.
#[allow(clippy::too_many_lines)]
pub async fn activate_adt_sources_together(
    sap: &mut SapClient,
    policy: &EditPolicy,
    request: &GroupActivationRequest,
    journal: Option<&Journal>,
) -> Result<GroupActivationResult, GroupActivationError> {
    let member = |name: &str, source: AdtSourceActivationError| GroupActivationError::Member {
        name: name.to_owned(),
        source: Box::new(source),
    };

    // Every object is validated before anything is read or written.
    let mut targets = Vec::with_capacity(request.objects.len());
    for (object_type, name) in &request.objects {
        let target =
            validate_adt_edit_target(*object_type, name, policy, request.transport.as_deref())
                .map_err(|error| member(name, error.into()))?;
        if targets.iter().any(
            |existing: &super::editable_source::ValidatedAdtEditTarget| {
                existing.identity.object_uri == target.identity.object_uri
            },
        ) {
            return Err(GroupActivationError::Duplicate {
                name: target.identity.name,
            });
        }
        authorize_object_package(
            sap,
            policy,
            &target.identity.name,
            &target.identity.object_uri,
        )
        .await
        .map_err(|error| member(name, error.into()))?;
        targets.push(target);
    }
    let transport = targets.first().and_then(|target| target.transport.clone());

    let worklist = read_worklist(sap).await.map_err(|error| {
        member(
            "worklist",
            AdtSourceActivationError::InactiveVersionProbe(error),
        )
    })?;
    let mut staged_targets = Vec::new();
    let mut nothing_to_activate = Vec::new();
    for target in targets {
        let listed = worklist
            .contains(&target.identity.object_uri)
            .map_err(|error| {
                member(
                    &target.identity.name,
                    AdtSourceActivationError::InactiveVersionProbe(error),
                )
            })?;
        if listed {
            staged_targets.push(target);
        } else {
            nothing_to_activate.push(target.identity);
        }
    }
    if staged_targets.is_empty() {
        return Err(GroupActivationError::NothingToActivate {
            names: nothing_to_activate
                .into_iter()
                .map(|identity| identity.name)
                .collect(),
        });
    }

    // Read and attach everything first, journal last: an object that cannot be
    // prepared refuses the group before any entry is opened.
    let group = journal.is_some().then(new_group_id);
    let mut prepared = Vec::with_capacity(staged_targets.len());
    for target in staged_targets {
        let identity = target.identity;
        let staged = read_adt_source_for_edit(
            sap,
            identity.object_type,
            &identity.name,
            AdtVersion::Inactive,
        )
        .await
        .map_err(|error| {
            member(
                &identity.name,
                AdtSourceActivationError::InactiveSourceRead(error),
            )
        })?
        .snapshot;
        let publishing = publishing_includes(sap, &identity, &worklist, &staged)
            .await
            .map_err(|error| member(&identity.name, error))?;
        if let Some(transport) = &transport {
            attach_adt_object_to_transport(sap, &identity.object_uri, transport)
                .await
                .map_err(|error| {
                    member(
                        &identity.name,
                        AdtSourceActivationError::TransportAttachment(error),
                    )
                })?;
        }
        prepared.push(Prepared {
            identity,
            staged,
            publishing,
            entry: None,
        });
    }
    if let Some(journal) = journal {
        for index in 0..prepared.len() {
            let object = &prepared[index];
            let opened = match activation_before(
                sap,
                &object.identity,
                &object.publishing,
                &object.staged.source,
            )
            .await
            {
                Ok(before) => journal
                    .begin_activation_in_group(before, transport.clone(), group.clone())
                    .map_err(AdtSourceActivationError::Journal),
                Err(error) => Err(error),
            };
            match opened {
                Ok(entry) => prepared[index].entry = Some(entry),
                Err(error) => {
                    // Nothing is activated, so the entries already opened
                    // record a refusal rather than staying pending. Ignored on
                    // failure: the cause is what has to be reported.
                    for earlier in &mut prepared[..index] {
                        let _ = resolve(Some(journal), earlier.entry.take(), Resolution::Failed);
                    }
                    return Err(member(&prepared[index].identity.name, error));
                }
            }
        }
    }

    let references: Vec<(&str, &str)> = prepared
        .iter()
        .map(|object| {
            (
                object.identity.object_uri.as_str(),
                object.identity.name.as_str(),
            )
        })
        .collect();
    let response = match post_activation_of(sap, &references).await {
        Ok(response) => response,
        Err(source) => {
            for object in prepared {
                // Deliberately ignored: a cleanup failure must not replace the cause.
                let _ = resolve(journal, object.entry, Resolution::Failed);
            }
            return Err(GroupActivationError::Request(source));
        }
    };
    let parsed = parse_activation_response(&response).unwrap_or_default();

    let mut members = Vec::with_capacity(prepared.len());
    for object in prepared {
        let messages: Vec<AdtActivationMessage> = parsed
            .messages
            .iter()
            .filter(|message| is_about(message, &object.identity))
            .cloned()
            .collect();
        members.push(settle(sap, journal, object, messages).await);
    }
    let general_messages = parsed
        .messages
        .iter()
        .filter(|message| {
            !members
                .iter()
                .any(|member| is_about(message, &member.identity))
        })
        .cloned()
        .collect();

    if members
        .iter()
        .all(|member| member.outcome == GroupOutcome::Activated)
    {
        return Ok(GroupActivationResult {
            group,
            transport,
            members,
            nothing_to_activate,
            sap_reported_activation_executed: parsed.activation_executed,
            general_messages,
        });
    }
    Err(GroupActivationError::Incomplete {
        members: members
            .into_iter()
            .map(|member| MemberSummary {
                object_type: member.identity.object_type,
                name: member.identity.name,
                outcome: member.outcome,
                first_error: member
                    .messages
                    .iter()
                    .find(|message| message.severity == AdtMessageSeverity::Error)
                    .map(|message| message.text.clone()),
            })
            .collect(),
    })
}

/// Decides one object's outcome from its own post-state, and resolves its
/// journal entry by that alone.
async fn settle(
    sap: &SapClient,
    journal: Option<&Journal>,
    object: Prepared,
    messages: Vec<AdtActivationMessage>,
) -> GroupMember {
    let Prepared {
        identity,
        staged,
        publishing,
        entry,
    } = object;
    let unverified = |identity: EditableAdtSourceIdentity, entry, messages| {
        let journal_entry_incomplete =
            resolve(journal, entry, Resolution::Unverified(&AfterImages::new()));
        GroupMember {
            identity,
            outcome: GroupOutcome::Unverified,
            active_after: None,
            published: Vec::new(),
            messages,
            journal_entry_incomplete,
        }
    };

    match verify_activation_post_state(sap, &identity).await {
        // Off the worklist: proved the same way a single activation is.
        Ok(Some(Ok(active))) => {
            match verify_published(sap, &identity, &publishing, &staged, &active.snapshot).await {
                Ok(published) => {
                    let after = activation_after(sap, &identity, &active.snapshot.source).await;
                    let journal_entry_incomplete =
                        resolve(journal, entry, Resolution::Succeeded(&after));
                    GroupMember {
                        identity,
                        outcome: GroupOutcome::Activated,
                        active_after: Some(active.snapshot),
                        published,
                        messages,
                        journal_entry_incomplete,
                    }
                }
                Err(_) => unverified(identity, entry, messages),
            }
        }
        // Still listed. Either nothing happened, or SAP published the source
        // and left the entry behind; only the active source tells which.
        Ok(None) => {
            let active = read_adt_source_for_edit(
                sap,
                identity.object_type,
                &identity.name,
                AdtVersion::Active,
            )
            .await
            .ok()
            .map(|read| read.snapshot);
            match active {
                Some(active) if same_source(&active.source, &staged.source) => {
                    let after = activation_after(sap, &identity, &active.source).await;
                    let journal_entry_incomplete =
                        resolve(journal, entry, Resolution::Unverified(&after));
                    GroupMember {
                        identity,
                        outcome: GroupOutcome::PublishedStillListed,
                        active_after: Some(active),
                        published: Vec::new(),
                        messages,
                        journal_entry_incomplete,
                    }
                }
                Some(active) => {
                    let journal_entry_incomplete = resolve(journal, entry, Resolution::Failed);
                    GroupMember {
                        identity,
                        outcome: GroupOutcome::NotActivated,
                        active_after: Some(active),
                        published: Vec::new(),
                        messages,
                        journal_entry_incomplete,
                    }
                }
                None => unverified(identity, entry, messages),
            }
        }
        Ok(Some(Err(_))) | Err(_) => unverified(identity, entry, messages),
    }
}

/// Whether an activation message is about this object.
///
/// SAP names the object in `href` (its URI, sometimes with `/source/main` and
/// a position) on every message it attributes, and leaves it empty on the
/// summary lines ("Activation was cancelled").
fn is_about(message: &AdtActivationMessage, identity: &EditableAdtSourceIdentity) -> bool {
    message.href.as_deref().is_some_and(|href| {
        let href = href.to_ascii_lowercase();
        let uri = identity.object_uri.to_ascii_lowercase();
        href == uri || href.starts_with(&format!("{uri}/")) || href.starts_with(&format!("{uri}#"))
    })
}

fn new_group_id() -> String {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis());
    format!("group-{millis}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str) -> EditableAdtSourceIdentity {
        EditableAdtSourceIdentity {
            object_type: EditableAdtObjectType::DdlSource,
            name: name.to_owned(),
            object_uri: format!("/sap/bc/adt/ddic/ddl/sources/{}", name.to_ascii_lowercase()),
            source_uri: format!(
                "/sap/bc/adt/ddic/ddl/sources/{}/source/main",
                name.to_ascii_lowercase()
            ),
            include: None,
        }
    }

    fn message(href: Option<&str>) -> AdtActivationMessage {
        AdtActivationMessage {
            severity: AdtMessageSeverity::Error,
            text: "x".to_owned(),
            line: None,
            object_description: None,
            href: href.map(str::to_owned),
        }
    }

    #[test]
    fn a_message_belongs_to_the_object_its_href_names() {
        let child = identity("ZSAMPLE_CHILD");
        assert!(is_about(
            &message(Some("/sap/bc/adt/ddic/ddl/sources/zsample_child")),
            &child
        ));
        assert!(is_about(
            &message(Some(
                "/sap/bc/adt/ddic/ddl/sources/zsample_child/source/main#start=9,6"
            )),
            &child
        ));
        // A name that only starts the same is another object.
        assert!(!is_about(
            &message(Some("/sap/bc/adt/ddic/ddl/sources/zsample_children")),
            &child
        ));
        assert!(!is_about(&message(None), &child));
    }

    #[test]
    fn an_incomplete_group_names_each_outcome_and_points_at_the_failure() {
        let error = GroupActivationError::Incomplete {
            members: vec![
                MemberSummary {
                    object_type: EditableAdtObjectType::DdlSource,
                    name: "ZSAMPLE_PARENT".to_owned(),
                    outcome: GroupOutcome::PublishedStillListed,
                    first_error: None,
                },
                MemberSummary {
                    object_type: EditableAdtObjectType::DdlSource,
                    name: "ZSAMPLE_CHILD".to_owned(),
                    outcome: GroupOutcome::NotActivated,
                    first_error: Some("The column sptxtx is unknown".to_owned()),
                },
            ],
        };
        assert_eq!(error.code(), "edit_activation_incomplete");
        assert!(
            error
                .to_string()
                .contains("ZSAMPLE_PARENT published_still_listed, ZSAMPLE_CHILD not_activated")
        );
        assert!(
            error
                .hint()
                .unwrap()
                .contains("The column sptxtx is unknown")
        );
        assert!(error.hint().unwrap().contains("run the same command again"));
        assert_eq!(
            error.suggested_command().as_deref(),
            Some("fractal edit check --type DDLS --name ZSAMPLE_CHILD --version inactive")
        );
    }
}

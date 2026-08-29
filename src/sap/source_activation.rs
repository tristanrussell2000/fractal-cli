use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use crate::suggested_command;

use super::{
    adt_message_severity::AdtMessageSeverity,
    client::{SapClient, SapClientError},
    edit_session::{AdtEditSessionError, attach_adt_object_to_transport},
    editable_source::{
        AdtEditTargetValidationError, AdtSourceReadError, AdtSourceSnapshot, AdtSourceVersion,
        EditableAdtObjectType, EditableAdtSourceIdentity, ValidatedAdtEditTarget,
        read_adt_source_for_edit, validate_adt_edit_target,
    },
    find_attribute_value,
    source_check::{
        AdtInactiveSourceProbeError, AdtSourceCheckError, AdtSourceCheckMessage,
        AdtSourceCheckResult, check_adt_source_by_identity, probe_inactive_adt_source,
    },
};

const ACTIVATION_PATH: &str = "/sap/bc/adt/activation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationRequest {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub transport: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationMessage {
    pub severity: AdtMessageSeverity,
    pub text: String,
    pub line: Option<usize>,
    pub object_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationResult {
    pub identity: EditableAdtSourceIdentity,
    pub transport: Option<String>,
    pub precheck: AdtSourceCheckResult,
    pub inactive: AdtSourceSnapshot,
    pub active: AdtSourceSnapshot,
    pub sap_reported_activation_executed: Option<bool>,
    pub activation_response_parsed: bool,
    pub activation_messages: Vec<AdtSourceActivationMessage>,
}

#[derive(Debug, Error)]
pub enum AdtSourceActivationError {
    #[error(transparent)]
    Validation(#[from] AdtEditTargetValidationError),
    #[error("could not determine whether the object has inactive source: {0}")]
    InactiveVersionProbe(#[source] AdtInactiveSourceProbeError),
    #[error("{} object '{}' has no inactive source to activate", identity.object_type.as_str(), identity.name)]
    NoInactiveVersion {
        identity: Box<EditableAdtSourceIdentity>,
    },
    #[error("could not read the inactive source before activation: {0}")]
    InactiveSourceRead(#[source] AdtSourceReadError),
    #[error("the inactive-source precheck could not run: {0}")]
    Precheck(#[source] AdtSourceCheckError),
    #[error("the inactive source has {errors} syntax error(s), so activation was not attempted")]
    PrecheckRejected {
        identity: Box<EditableAdtSourceIdentity>,
        errors: usize,
        warnings: usize,
        messages: Vec<AdtSourceCheckMessage>,
    },
    #[error("could not attach the object to the requested transport before activation: {0}")]
    TransportAttachment(#[source] AdtEditSessionError),
    #[error("the ADT activation request failed: {source}")]
    ActivationRequest {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: SapClientError,
    },
    #[error("SAP returned malformed activation XML and the inactive version still exists: {0}")]
    ActivationResponseInvalid(#[source] roxmltree::Error),
    #[error("SAP did not remove the inactive version, so activation was not completed")]
    ActivationRefused {
        sap_reported_activation_executed: Option<bool>,
        messages: Vec<AdtSourceActivationMessage>,
    },
    #[error(
        "SAP accepted the activation request, but the active source could not be read: {source}"
    )]
    ActiveSourceRead {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: AdtSourceReadError,
    },
    #[error(
        "SAP accepted the activation request, but its inactive-source state could not be verified: {source}"
    )]
    PostActivationProbe {
        identity: Box<EditableAdtSourceIdentity>,
        #[source]
        source: AdtInactiveSourceProbeError,
    },
    #[error(
        "activation removed the inactive version, but active source SHA-256 {active_sha256} does not match the pre-activation inactive source SHA-256 {inactive_sha256}"
    )]
    VerificationMismatch {
        identity: Box<EditableAdtSourceIdentity>,
        inactive_sha256: String,
        active_sha256: String,
    },
}

impl AdtSourceActivationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::Validation(error) => error.code(),
            Self::InactiveVersionProbe(_) => "edit_activation_inactive_probe_failed",
            Self::NoInactiveVersion { .. } => "edit_activation_no_inactive_source",
            Self::InactiveSourceRead(_) => "edit_activation_inactive_read_failed",
            Self::Precheck(_) => "edit_activation_precheck_failed",
            Self::PrecheckRejected { .. } => "edit_activation_precheck_rejected",
            Self::TransportAttachment(_) => "edit_activation_transport_failed",
            Self::ActivationRequest { .. } => "edit_activation_request_failed",
            Self::ActivationResponseInvalid(_) => "edit_activation_response_invalid",
            Self::ActivationRefused { .. } => "edit_activation_refused",
            Self::ActiveSourceRead { .. } => "edit_activation_active_read_failed",
            Self::PostActivationProbe { .. } => "edit_activation_verification_probe_failed",
            Self::VerificationMismatch { .. } => "edit_activation_verification_mismatch",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Validation(AdtEditTargetValidationError::InvalidTransport(_)) => {
                "Use a parent transport request containing 1-20 ASCII letters or digits, for example DE3K900575."
                    .to_owned()
            }
            Self::Validation(error) => error.hint(),
            Self::InactiveVersionProbe(error) => error.hint(),
            Self::NoInactiveVersion { identity } => format!(
                "Create or save an inactive change first. Run `{}` to inspect what is already live.",
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Active.as_str())
            ),
            Self::InactiveSourceRead(error) => error.hint(),
            Self::Precheck(error) => error.hint(),
            Self::PrecheckRejected {
                identity, messages, ..
            } => first_message_hint(
                messages.iter().map(|message| message.text.as_str()),
                &format!(
                    "Run `{}` to inspect every syntax finding.",
                    suggested_command::edit_check(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Inactive.as_str())
                ),
            ),
            Self::TransportAttachment(error) => error.hint(),
            Self::ActivationRequest { identity, .. } => format!(
                "The request may have reached SAP. Re-run `{}` before retrying activation.",
                suggested_command::edit_check(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Inactive.as_str())
            ),
            Self::ActivationResponseInvalid(_) => {
                "SAP did not clear the inactive version, and its response could not be interpreted; inspect the object in ADT before retrying."
                    .to_owned()
            }
            Self::ActivationRefused { messages, .. } => first_message_hint(
                messages.iter().map(|message| message.text.as_str()),
                "Review SAP's activation messages and the object's transport before retrying.",
            ),
            Self::ActiveSourceRead { identity, .. } | Self::PostActivationProbe { identity, .. } => {
                format!(
                    "Activation may have succeeded. Read both active source and the inactive-object state before retrying. Run `{}`.",
                    suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Active.as_str())
                )
            }
            Self::VerificationMismatch { identity, .. } => format!(
                "Do not retry blindly: SAP activated different source than the version Fractal prechecked. Review the object history, starting with `{}`.",
                suggested_command::edit_read(identity.object_type.as_str(), &identity.name, AdtSourceVersion::Active.as_str())
            ),
        }
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// Transport attachment failures return `None`: their remedy is to retry
    /// the activation with a different request, which is a mutation.
    #[must_use]
    pub fn suggested_command(&self) -> Option<String> {
        match self {
            Self::NoInactiveVersion { identity }
            | Self::ActiveSourceRead { identity, .. }
            | Self::PostActivationProbe { identity, .. }
            | Self::VerificationMismatch { identity, .. } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                AdtSourceVersion::Active.as_str(),
            )),
            Self::PrecheckRejected { identity, .. } | Self::ActivationRequest { identity, .. } => {
                Some(suggested_command::edit_check(
                    identity.object_type.as_str(),
                    &identity.name,
                    AdtSourceVersion::Inactive.as_str(),
                ))
            }
            Self::InactiveSourceRead(error) => error.suggested_command(),
            Self::Validation(_)
            | Self::InactiveVersionProbe(_)
            | Self::Precheck(_)
            | Self::TransportAttachment(_)
            | Self::ActivationResponseInvalid(_)
            | Self::ActivationRefused { .. } => None,
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::InactiveSourceRead(error) | Self::ActiveSourceRead { source: error, .. } => {
                error.sap_error()
            }
            Self::InactiveVersionProbe(error) => error.sap_error(),
            Self::ActivationRequest { source, .. } => Some(source),
            Self::PostActivationProbe { source: error, .. } => error.sap_error(),
            Self::Precheck(error) => error.sap_error(),
            Self::TransportAttachment(error) => error.sap_error(),
            Self::Validation(_)
            | Self::NoInactiveVersion { .. }
            | Self::PrecheckRejected { .. }
            | Self::ActivationResponseInvalid(_)
            | Self::ActivationRefused { .. }
            | Self::VerificationMismatch { .. } => None,
        }
    }
}

/// Activates one confirmed inactive source version through native ADT.
///
/// The workflow refuses to activate when no inactive version exists or when
/// the standalone syntax check finds errors. It snapshots the inactive source,
/// requests ADT activation with preaudit enabled, and then verifies both that
/// the inactive version disappeared and that the active source has the same
/// SHA-256 as the source inspected before activation.
///
/// When a transport is supplied, a lock/unlock cycle first attaches the object
/// to that parent request using the same transport behavior as source patching.
/// The activation request itself does not accept a transport parameter.
///
/// # Errors
///
/// Returns [`AdtSourceActivationError`] when validation, precheck, transport
/// attachment, activation, or post-activation verification fails.
pub async fn activate_adt_source(
    sap: &mut SapClient,
    customer_namespaces: &[String],
    request: &AdtSourceActivationRequest,
) -> Result<AdtSourceActivationResult, AdtSourceActivationError> {
    let target = validate_adt_edit_target(
        request.object_type,
        &request.name,
        customer_namespaces,
        request.transport.as_deref(),
    )?;
    activate_validated_adt_source(sap, target).await
}

pub(super) async fn activate_validated_adt_source(
    sap: &mut SapClient,
    target: ValidatedAdtEditTarget,
) -> Result<AdtSourceActivationResult, AdtSourceActivationError> {
    let identity = target.identity;
    let transport = target.transport;

    let inactive_exists = probe_inactive_adt_source(sap, &identity.object_uri)
        .await
        .map_err(AdtSourceActivationError::InactiveVersionProbe)?;
    if !inactive_exists {
        return Err(AdtSourceActivationError::NoInactiveVersion {
            identity: Box::new(identity),
        });
    }

    sap.establish_csrf_session().await.map_err(|source| {
        AdtSourceActivationError::Precheck(AdtSourceCheckError::Sap {
            identity: Box::new(identity.clone()),
            version: AdtSourceVersion::Inactive,
            source,
        })
    })?;
    let inactive_read = async {
        read_adt_source_for_edit(
            sap,
            identity.object_type,
            &identity.name,
            AdtSourceVersion::Inactive,
        )
        .await
        .map_err(AdtSourceActivationError::InactiveSourceRead)
    };
    let precheck_run = async {
        let precheck =
            check_adt_source_by_identity(sap, &identity, AdtSourceVersion::Inactive, Some(true))
                .await
                .map_err(AdtSourceActivationError::Precheck)?;
        if precheck.clean {
            Ok(precheck)
        } else {
            Err(AdtSourceActivationError::PrecheckRejected {
                identity: Box::new(identity.clone()),
                errors: precheck.errors,
                warnings: precheck.warnings,
                messages: precheck.messages,
            })
        }
    };
    let (inactive, precheck) = tokio::try_join!(inactive_read, precheck_run)?;

    if let Some(transport) = &transport {
        attach_adt_object_to_transport(sap, &identity.object_uri, transport)
            .await
            .map_err(AdtSourceActivationError::TransportAttachment)?;
    }

    let body = build_activation_request(&identity.object_uri, &identity.name);
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static("application/xml"));
    headers.insert("Accept", HeaderValue::from_static("application/xml"));
    let response = sap
        .post_text(
            ACTIVATION_PATH,
            &[("method", "activate"), ("preauditRequested", "true")],
            Some(&body),
            headers,
        )
        .await
        .map_err(|source| AdtSourceActivationError::ActivationRequest {
            identity: Box::new(identity.clone()),
            source,
        })?;

    // SAP's activation response is advisory: activationExecuted="false" has
    // been observed after a successful activation, and malformed XML does not
    // prove failure. Keep the parse result until the inactive-state and active-
    // source checks establish whether a response problem is fatal.
    let parsed_response = parse_activation_response(&response);

    let active_result = {
        let active_read = read_adt_source_for_edit(
            sap,
            identity.object_type,
            &identity.name,
            AdtSourceVersion::Active,
        );
        let inactive_probe = probe_inactive_adt_source(sap, &identity.object_uri);
        tokio::pin!(active_read, inactive_probe);

        // Poll both verification requests together. The inactive-object result
        // is decisive: when that version still exists, return without waiting
        // for the active-source body. If the active read finishes first, retain
        // its result until the probe establishes whether activation occurred.
        tokio::select! {
            biased;
            active_result = &mut active_read => {
                let inactive_still_exists = inactive_probe
                    .await
                    .map_err(|source| AdtSourceActivationError::PostActivationProbe {
                        identity: Box::new(identity.clone()),
                        source,
                    })?;
                (!inactive_still_exists).then_some(active_result)
            }
            probe_result = &mut inactive_probe => {
                let inactive_still_exists =
                    probe_result.map_err(|source| AdtSourceActivationError::PostActivationProbe {
                        identity: Box::new(identity.clone()),
                        source,
                    })?;
                if inactive_still_exists {
                    None
                } else {
                    Some(active_read.await)
                }
            }
        }
    };

    let Some(active_result) = active_result else {
        return match parsed_response {
            Ok(parsed) => Err(AdtSourceActivationError::ActivationRefused {
                sap_reported_activation_executed: parsed.activation_executed,
                messages: parsed.messages,
            }),
            Err(error) => Err(AdtSourceActivationError::ActivationResponseInvalid(error)),
        };
    };
    let active = active_result.map_err(|source| AdtSourceActivationError::ActiveSourceRead {
        identity: Box::new(identity.clone()),
        source,
    })?;
    if active.snapshot.sha256 != inactive.snapshot.sha256 {
        return Err(AdtSourceActivationError::VerificationMismatch {
            identity: Box::new(identity.clone()),
            inactive_sha256: inactive.snapshot.sha256,
            active_sha256: active.snapshot.sha256,
        });
    }

    // Post-state now proves success, so preserve malformed response XML as metadata.
    let (activation_response_parsed, parsed) = match parsed_response {
        Ok(parsed) => (true, parsed),
        Err(_) => (false, ParsedActivationResponse::default()),
    };
    Ok(AdtSourceActivationResult {
        identity,
        transport,
        precheck,
        inactive: inactive.snapshot,
        active: active.snapshot,
        sap_reported_activation_executed: parsed.activation_executed,
        activation_response_parsed,
        activation_messages: parsed.messages,
    })
}

fn build_activation_request(object_uri: &str, name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference adtcore:uri=\"{object_uri}\" adtcore:name=\"{name}\"/></adtcore:objectReferences>"
    )
}

#[derive(Default)]
struct ParsedActivationResponse {
    activation_executed: Option<bool>,
    messages: Vec<AdtSourceActivationMessage>,
}

fn parse_activation_response(response: &str) -> Result<ParsedActivationResponse, roxmltree::Error> {
    let document = roxmltree::Document::parse(response)?;
    let activation_executed = document.descendants().find_map(|node| {
        find_attribute_value(node, "activationExecuted").and_then(|value| {
            if value.eq_ignore_ascii_case("true") {
                Some(true)
            } else if value.eq_ignore_ascii_case("false") {
                Some(false)
            } else {
                None
            }
        })
    });
    let messages = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "msg")
        .filter_map(parse_activation_message)
        .collect();
    Ok(ParsedActivationResponse {
        activation_executed,
        messages,
    })
}

fn parse_activation_message(node: roxmltree::Node<'_, '_>) -> Option<AdtSourceActivationMessage> {
    let text = find_attribute_value(node, "shortText")
        .map(str::to_owned)
        .or_else(|| descendant_text(node, "shortText"))
        .or_else(|| descendant_text(node, "txt"))?;
    let text = text.trim();
    if text.is_empty() {
        return None;
    }
    // Activation messages have been observed carrying the severity code on
    // either attribute; checkrun messages only ever use `type`.
    let severity = AdtMessageSeverity::from_sap_code(
        find_attribute_value(node, "type")
            .or_else(|| find_attribute_value(node, "severity"))
            .unwrap_or_default(),
    );
    Some(AdtSourceActivationMessage {
        severity,
        text: text.to_owned(),
        line: find_attribute_value(node, "line").and_then(|line| line.parse().ok()),
        object_description: find_attribute_value(node, "objDescr")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned),
    })
}

fn descendant_text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    let descendant = node
        .descendants()
        .find(|descendant| descendant.is_element() && descendant.tag_name().name() == name)?;
    let text = descendant
        .descendants()
        .filter(|node| node.is_text())
        .filter_map(|node| node.text())
        .collect();
    Some(text)
}

fn first_message_hint<'a>(messages: impl Iterator<Item = &'a str>, fallback: &str) -> String {
    let summary = messages
        .filter(|message| !message.trim().is_empty())
        .take(3)
        .collect::<Vec<_>>()
        .join(" | ");
    if summary.is_empty() {
        fallback.to_owned()
    } else {
        format!("{summary}. {fallback}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_activation_flag_and_nested_messages() {
        let parsed = parse_activation_response(
            r#"<act:activationResult xmlns:act="http://www.sap.com/adt/activation" activationExecuted="false">
                <msg type="E" objDescr="Class ZCL_SAMPLE" line="17">
                    <shortText><txt>Expected &lt;identifier&gt;</txt></shortText>
                </msg>
                <msg severity="warning" shortText="Obsolete statement"/>
            </act:activationResult>"#,
        )
        .unwrap();

        assert_eq!(parsed.activation_executed, Some(false));
        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.messages[0].severity, AdtMessageSeverity::Error);
        assert_eq!(parsed.messages[0].text, "Expected <identifier>");
        assert_eq!(parsed.messages[0].line, Some(17));
        assert_eq!(
            parsed.messages[0].object_description.as_deref(),
            Some("Class ZCL_SAMPLE")
        );
        assert_eq!(parsed.messages[1].severity, AdtMessageSeverity::Warning);
    }

    #[test]
    fn treats_an_omitted_activation_flag_as_unknown() {
        let parsed = parse_activation_response("<activationResult/>").unwrap();

        assert_eq!(parsed.activation_executed, None);
        assert!(parsed.messages.is_empty());
    }
}

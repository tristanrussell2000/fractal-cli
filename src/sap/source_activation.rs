use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    client::{SapClient, SapError},
    edit::{
        AdtSourcePatchError, AdtSourceReadError, AdtSourceVersion, EditableAdtObjectType,
        attach_adt_object_to_transport, canonicalize_transport_request, editable_object_identity,
        read_adt_source_for_edit,
    },
    find_attribute_value,
    source_check::{
        AdtInactiveSourceProbeError, AdtSourceCheckError, AdtSourceCheckMessage,
        AdtSourceCheckResult, check_adt_source_by_identity, probe_inactive_adt_source,
    },
};
use crate::edit::{EditError, validate_customer_namespace};

const ACTIVATION_PATH: &str = "/sap/bc/adt/activation";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationRequest {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub transport: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdtSourceActivationMessageSeverity {
    Error,
    Warning,
    Info,
}

impl AdtSourceActivationMessageSeverity {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
            Self::Info => "info",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationMessage {
    pub severity: AdtSourceActivationMessageSeverity,
    pub text: String,
    pub line: Option<usize>,
    pub object_description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceActivationResult {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
    pub transport: Option<String>,
    pub precheck: AdtSourceCheckResult,
    pub inactive_sha256: String,
    pub inactive_bytes: usize,
    pub active_sha256: String,
    pub active_bytes: usize,
    pub sap_reported_activation_executed: Option<bool>,
    pub activation_response_parsed: bool,
    pub activation_messages: Vec<AdtSourceActivationMessage>,
}

#[derive(Debug, Error)]
pub enum AdtSourceActivationError {
    #[error("invalid activation object: {0}")]
    InvalidObject(#[source] AdtSourceReadError),
    #[error(transparent)]
    Namespace(EditError),
    #[error("invalid transport request '{0}'")]
    InvalidTransportRequest(String),
    #[error("could not determine whether the object has inactive source: {0}")]
    InactiveVersionProbe(#[source] AdtInactiveSourceProbeError),
    #[error("{object_type} object '{name}' has no inactive source to activate")]
    NoInactiveVersion {
        object_type: &'static str,
        name: String,
    },
    #[error("could not read the inactive source before activation: {0}")]
    InactiveSourceRead(#[source] AdtSourceReadError),
    #[error("the inactive-source precheck could not run: {0}")]
    Precheck(#[source] AdtSourceCheckError),
    #[error("the inactive source has {errors} syntax error(s), so activation was not attempted")]
    PrecheckRejected {
        errors: usize,
        warnings: usize,
        messages: Vec<AdtSourceCheckMessage>,
    },
    #[error("could not attach the object to the requested transport before activation: {0}")]
    TransportAttachment(#[source] AdtSourcePatchError),
    #[error("the ADT activation request failed: {0}")]
    ActivationRequest(#[source] SapError),
    #[error("SAP returned malformed activation XML and the inactive version still exists: {0}")]
    ActivationResponseInvalid(#[source] roxmltree::Error),
    #[error("SAP did not remove the inactive version, so activation was not completed")]
    ActivationRefused {
        sap_reported_activation_executed: Option<bool>,
        messages: Vec<AdtSourceActivationMessage>,
    },
    #[error("SAP accepted the activation request, but the active source could not be read: {0}")]
    ActiveSourceRead(#[source] AdtSourceReadError),
    #[error(
        "SAP accepted the activation request, but its inactive-source state could not be verified: {0}"
    )]
    PostActivationProbe(#[source] AdtInactiveSourceProbeError),
    #[error(
        "activation removed the inactive version, but active source SHA-256 {active_sha256} does not match the pre-activation inactive source SHA-256 {inactive_sha256}"
    )]
    VerificationMismatch {
        inactive_sha256: String,
        active_sha256: String,
    },
}

impl AdtSourceActivationError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidObject(error) => error.code(),
            Self::Namespace(error) => error.code(),
            Self::InvalidTransportRequest(_) => "invalid_transport_request",
            Self::InactiveVersionProbe(_) => "edit_activation_inactive_probe_failed",
            Self::NoInactiveVersion { .. } => "edit_activation_no_inactive_source",
            Self::InactiveSourceRead(_) => "edit_activation_inactive_read_failed",
            Self::Precheck(_) => "edit_activation_precheck_failed",
            Self::PrecheckRejected { .. } => "edit_activation_precheck_rejected",
            Self::TransportAttachment(_) => "edit_activation_transport_failed",
            Self::ActivationRequest(_) => "edit_activation_request_failed",
            Self::ActivationResponseInvalid(_) => "edit_activation_response_invalid",
            Self::ActivationRefused { .. } => "edit_activation_refused",
            Self::ActiveSourceRead(_) => "edit_activation_active_read_failed",
            Self::PostActivationProbe(_) => "edit_activation_verification_probe_failed",
            Self::VerificationMismatch { .. } => "edit_activation_verification_mismatch",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::InvalidObject(error) => error.hint(),
            Self::Namespace(error) => error.hint(),
            Self::InvalidTransportRequest(_) => {
                "Use a parent transport request containing 1-20 ASCII letters or digits, for example DE3K900575."
                    .to_owned()
            }
            Self::InactiveVersionProbe(error) => error.hint(),
            Self::NoInactiveVersion { .. } => {
                "Create or save an inactive change first; use `fractal edit read --version active` to inspect what is already live."
                    .to_owned()
            }
            Self::InactiveSourceRead(error) => error.hint(),
            Self::Precheck(error) => error.hint(),
            Self::PrecheckRejected { messages, .. } => first_message_hint(
                messages.iter().map(|message| message.text.as_str()),
                "Run `fractal edit check --version inactive` to inspect every syntax finding.",
            ),
            Self::TransportAttachment(error) => error.hint(),
            Self::ActivationRequest(_) => {
                "The request may have reached SAP. Re-run `fractal edit check --version inactive` before retrying activation."
                    .to_owned()
            }
            Self::ActivationResponseInvalid(_) => {
                "SAP did not clear the inactive version, and its response could not be interpreted; inspect the object in ADT before retrying."
                    .to_owned()
            }
            Self::ActivationRefused { messages, .. } => first_message_hint(
                messages.iter().map(|message| message.text.as_str()),
                "Review SAP's activation messages and the object's transport before retrying.",
            ),
            Self::ActiveSourceRead(_) | Self::PostActivationProbe(_) => {
                "Activation may have succeeded. Read both active source and the inactive-object state before retrying."
                    .to_owned()
            }
            Self::VerificationMismatch { .. } => {
                "Do not retry blindly: SAP activated different source than the version Fractal prechecked. Read the active and inactive versions and review the object history."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::InvalidObject(error)
            | Self::InactiveSourceRead(error)
            | Self::ActiveSourceRead(error) => error.sap_error(),
            Self::InactiveVersionProbe(error) => error.sap_error(),
            Self::ActivationRequest(error) => Some(error),
            Self::PostActivationProbe(error) => error.sap_error(),
            Self::Precheck(error) => error.sap_error(),
            Self::TransportAttachment(error) => error.sap_error(),
            Self::Namespace(_)
            | Self::InvalidTransportRequest(_)
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
    let identity = editable_object_identity(request.object_type, &request.name)
        .map_err(AdtSourceActivationError::InvalidObject)?;
    validate_customer_namespace(&identity.name, customer_namespaces)
        .map_err(AdtSourceActivationError::Namespace)?;
    let transport = canonicalize_transport_request(request.transport.as_deref())
        .map_err(AdtSourceActivationError::InvalidTransportRequest)?;

    let inactive_exists = probe_inactive_adt_source(sap, &identity.object_uri)
        .await
        .map_err(AdtSourceActivationError::InactiveVersionProbe)?;
    if !inactive_exists {
        return Err(AdtSourceActivationError::NoInactiveVersion {
            object_type: identity.object_type.as_str(),
            name: identity.name,
        });
    }

    let inactive = read_adt_source_for_edit(
        sap,
        identity.object_type,
        &identity.name,
        AdtSourceVersion::Inactive,
    )
    .await
    .map_err(AdtSourceActivationError::InactiveSourceRead)?;

    let precheck =
        check_adt_source_by_identity(sap, &identity, AdtSourceVersion::Inactive, Some(true))
            .await
            .map_err(AdtSourceActivationError::Precheck)?;
    if !precheck.clean {
        return Err(AdtSourceActivationError::PrecheckRejected {
            errors: precheck.errors,
            warnings: precheck.warnings,
            messages: precheck.messages,
        });
    }

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
        .map_err(AdtSourceActivationError::ActivationRequest)?;

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
                    .map_err(AdtSourceActivationError::PostActivationProbe)?;
                (!inactive_still_exists).then_some(active_result)
            }
            probe_result = &mut inactive_probe => {
                let inactive_still_exists = probe_result
                    .map_err(AdtSourceActivationError::PostActivationProbe)?;
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
    let active = active_result.map_err(AdtSourceActivationError::ActiveSourceRead)?;
    if active.sha256 != inactive.sha256 {
        return Err(AdtSourceActivationError::VerificationMismatch {
            inactive_sha256: inactive.sha256,
            active_sha256: active.sha256,
        });
    }

    // Post-state now proves success, so preserve malformed response XML as metadata.
    let (activation_response_parsed, parsed) = match parsed_response {
        Ok(parsed) => (true, parsed),
        Err(_) => (false, ParsedActivationResponse::default()),
    };
    Ok(AdtSourceActivationResult {
        object_type: identity.object_type,
        name: identity.name,
        object_uri: identity.object_uri,
        source_uri: identity.source_uri,
        transport,
        precheck,
        inactive_sha256: inactive.sha256,
        inactive_bytes: inactive.bytes,
        active_sha256: active.sha256,
        active_bytes: active.bytes,
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
    let raw_severity = find_attribute_value(node, "type")
        .or_else(|| find_attribute_value(node, "severity"))
        .unwrap_or_default()
        .to_ascii_uppercase();
    let severity = if raw_severity.starts_with('E') || raw_severity.starts_with('A') {
        AdtSourceActivationMessageSeverity::Error
    } else if raw_severity.starts_with('W') {
        AdtSourceActivationMessageSeverity::Warning
    } else {
        AdtSourceActivationMessageSeverity::Info
    };
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
        assert_eq!(
            parsed.messages[0].severity,
            AdtSourceActivationMessageSeverity::Error
        );
        assert_eq!(parsed.messages[0].text, "Expected <identifier>");
        assert_eq!(parsed.messages[0].line, Some(17));
        assert_eq!(
            parsed.messages[0].object_description.as_deref(),
            Some("Class ZCL_SAMPLE")
        );
        assert_eq!(
            parsed.messages[1].severity,
            AdtSourceActivationMessageSeverity::Warning
        );
    }

    #[test]
    fn treats_an_omitted_activation_flag_as_unknown() {
        let parsed = parse_activation_response("<activationResult/>").unwrap();

        assert_eq!(parsed.activation_executed, None);
        assert!(parsed.messages.is_empty());
    }
}

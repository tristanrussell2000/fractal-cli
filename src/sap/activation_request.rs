//! The ADT activation request, shared by every object family.
//!
//! Activation is the one mutating operation whose protocol does not vary by
//! family at all: the same endpoint, the same object-reference body, the same
//! checklist response. What differs is the identity, what can be checked
//! beforehand, and how success is proved afterwards, and those stay with each
//! family.

use reqwest::header::{HeaderMap, HeaderValue};

use super::{
    adt_message_severity::AdtMessageSeverity,
    client::{SapClient, SapClientError},
    find_attribute_value, find_non_empty_attribute,
};

pub(super) const ACTIVATION_PATH: &str = "/sap/bc/adt/activation";

/// One message SAP reported while activating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtActivationMessage {
    pub severity: AdtMessageSeverity,
    pub text: String,
    pub line: Option<usize>,
    pub object_description: Option<String>,
}

/// SAP's activation response, parsed.
///
/// `activation_executed` is **advisory and must never decide anything**. Both
/// failure modes have been observed live: `false` after an activation that
/// worked, and `true` after one that plainly did not — a data element pointing
/// at a missing domain came back `activationExecuted="true"` alongside error
/// messages saying "Activation was cancelled". Only the object's own post-state
/// settles it.
#[derive(Default)]
pub(super) struct ParsedActivationResponse {
    pub(super) activation_executed: Option<bool>,
    pub(super) messages: Vec<AdtActivationMessage>,
}

/// Posts the activation request for one object.
///
/// # Errors
///
/// Returns the underlying [`SapClientError`] when the request itself fails. A
/// successful response does **not** mean the object activated; the caller has
/// to prove that from the object's post-state.
pub(super) async fn post_activation(
    sap: &mut SapClient,
    object_uri: &str,
    name: &str,
) -> Result<String, SapClientError> {
    let body = build_activation_request(object_uri, name);
    let mut headers = HeaderMap::new();
    headers.insert("Content-Type", HeaderValue::from_static("application/xml"));
    headers.insert("Accept", HeaderValue::from_static("application/xml"));
    sap.post_text(
        ACTIVATION_PATH,
        &[("method", "activate"), ("preauditRequested", "true")],
        Some(&body),
        headers,
    )
    .await
}

pub(super) fn build_activation_request(object_uri: &str, name: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><adtcore:objectReferences xmlns:adtcore=\"http://www.sap.com/adt/core\"><adtcore:objectReference adtcore:uri=\"{object_uri}\" adtcore:name=\"{name}\"/></adtcore:objectReferences>"
    )
}

pub(super) fn parse_activation_response(
    response: &str,
) -> Result<ParsedActivationResponse, roxmltree::Error> {
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

fn parse_activation_message(node: roxmltree::Node<'_, '_>) -> Option<AdtActivationMessage> {
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
    Some(AdtActivationMessage {
        severity,
        text: text.to_owned(),
        line: find_attribute_value(node, "line").and_then(|line| line.parse().ok()),
        object_description: find_non_empty_attribute(node, "objDescr"),
    })
}

fn descendant_text(node: roxmltree::Node<'_, '_>, name: &str) -> Option<String> {
    let descendant = node
        .descendants()
        .find(|descendant| descendant.is_element() && descendant.tag_name().name() == name)?;
    let text = descendant
        .descendants()
        .filter(roxmltree::Node::is_text)
        .filter_map(|node| node.text())
        .collect();
    Some(text)
}

/// Summarizes up to three messages for an error hint, falling back when SAP
/// said nothing usable.
pub(super) fn first_message_hint<'a>(
    messages: impl Iterator<Item = &'a str>,
    fallback: &str,
) -> String {
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
    fn builds_the_object_reference_body() {
        let body =
            build_activation_request("/sap/bc/adt/ddic/dataelements/zsample_de", "ZSAMPLE_DE");
        assert!(body.contains("adtcore:uri=\"/sap/bc/adt/ddic/dataelements/zsample_de\""));
        assert!(body.contains("adtcore:name=\"ZSAMPLE_DE\""));
    }

    #[test]
    fn reads_the_executed_flag_and_the_messages() {
        let response = r#"<?xml version="1.0" encoding="utf-8"?>
<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist">
  <chkl:properties checkExecuted="true" activationExecuted="false"/>
  <msg objDescr="PROG ZSAMPLE" type="E" line="12"><shortText><txt>Syntax error</txt></shortText></msg>
</chkl:messages>"#;
        let parsed = parse_activation_response(response).expect("parses");

        assert_eq!(parsed.activation_executed, Some(false));
        assert_eq!(parsed.messages.len(), 1);
        assert_eq!(parsed.messages[0].severity, AdtMessageSeverity::Error);
        assert_eq!(parsed.messages[0].line, Some(12));
        assert_eq!(
            parsed.messages[0].object_description.as_deref(),
            Some("PROG ZSAMPLE")
        );
    }

    #[test]
    fn a_failed_activation_can_still_claim_it_executed() {
        // Observed live from a data element pointing at a missing domain. The
        // flag says one thing and the messages say the opposite, which is why
        // no caller may decide on the flag.
        let response = r#"<?xml version="1.0" encoding="utf-8"?>
<chkl:messages xmlns:chkl="http://www.sap.com/abapxml/checklist">
  <chkl:properties checkExecuted="true" activationExecuted="true" generationExecuted="false"/>
  <msg objDescr="" type="E" line="0"><shortText><txt>Activation was cancelled.</txt></shortText></msg>
  <msg objDescr="DTEL ZSAMPLE_DE" type="E" line="1"><shortText><txt>No active domain ZSAMPLE_DOM available</txt></shortText></msg>
</chkl:messages>"#;
        let parsed = parse_activation_response(response).expect("parses");

        assert_eq!(parsed.activation_executed, Some(true));
        assert_eq!(parsed.messages.len(), 2);
        assert!(
            parsed
                .messages
                .iter()
                .all(|message| message.severity == AdtMessageSeverity::Error)
        );
    }

    #[test]
    fn joins_up_to_three_messages_and_falls_back_when_there_are_none() {
        assert_eq!(
            first_message_hint(["one", "two"].into_iter(), "Try again."),
            "one | two. Try again."
        );
        assert_eq!(
            first_message_hint(["", "  "].into_iter(), "Try again."),
            "Try again."
        );
    }

    #[test]
    fn reads_severity_and_text_from_either_spelling() {
        // Moved from source_activation when the parser did: activation
        // messages carry severity on `type` or `severity`, and the text as a
        // nested element or an attribute.
        let parsed = parse_activation_response(
            r#"<act:activationResult xmlns:act="http://www.sap.com/adt/activation" activationExecuted="false">
                <msg type="E" objDescr="Class ZCL_SAMPLE" line="17">
                    <shortText><txt>Expected &lt;identifier&gt;</txt></shortText>
                </msg>
                <msg severity="warning" shortText="Obsolete statement"/>
            </act:activationResult>"#,
        )
        .unwrap();

        assert_eq!(parsed.messages.len(), 2);
        assert_eq!(parsed.messages[0].severity, AdtMessageSeverity::Error);
        assert_eq!(parsed.messages[0].text, "Expected <identifier>");
        assert_eq!(parsed.messages[0].line, Some(17));
        assert_eq!(
            parsed.messages[0].object_description.as_deref(),
            Some("Class ZCL_SAMPLE")
        );
        assert_eq!(parsed.messages[1].severity, AdtMessageSeverity::Warning);
        assert_eq!(parsed.messages[1].text, "Obsolete statement");
    }

    #[test]
    fn treats_an_omitted_activation_flag_as_unknown() {
        let parsed = parse_activation_response("<activationResult/>").unwrap();

        assert_eq!(parsed.activation_executed, None);
        assert!(parsed.messages.is_empty());
    }
}

use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    client::{SapClient, SapError},
    edit::{AdtSourceReadError, AdtSourceVersion, EditableAdtObjectType, editable_object_identity},
};

const CHECKRUNS_PATH: &str = "/sap/bc/adt/checkruns";
const INACTIVE_OBJECTS_PATH: &str = "/sap/bc/adt/activation/inactiveobjects";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdtSourceCheckSeverity {
    Error,
    Warning,
    Info,
}

impl AdtSourceCheckSeverity {
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
pub struct AdtSourceCheckMessage {
    pub severity: AdtSourceCheckSeverity,
    pub text: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceCheckResult {
    pub object_type: EditableAdtObjectType,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
    pub requested_version: AdtSourceVersion,
    pub check_executed: bool,
    pub inactive_version_exists: Option<bool>,
    pub clean: bool,
    pub errors: usize,
    pub warnings: usize,
    pub infos: usize,
    pub messages: Vec<AdtSourceCheckMessage>,
}

#[derive(Debug, Error)]
pub enum AdtSourceCheckError {
    #[error("invalid source-check object: {0}")]
    InvalidObject(#[source] AdtSourceReadError),
    #[error("SAP source check failed: {0}")]
    Sap(#[source] SapError),
    #[error("SAP returned malformed source-check XML: {0}")]
    Parse(#[source] roxmltree::Error),
}

impl AdtSourceCheckError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidObject(error) => error.code(),
            Self::Sap(_) => "edit_source_check_failed",
            Self::Parse(_) => "edit_source_check_response_invalid",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::InvalidObject(error) => error.hint(),
            Self::Sap(error) => error.hint().to_owned(),
            Self::Parse(_) => {
                "The SAP checkrun response did not match the expected ADT check-message XML."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::Sap(error) => Some(error),
            Self::InvalidObject(_) | Self::Parse(_) => None,
        }
    }
}

/// Checks one stored active or inactive source version through native ADT.
///
/// An inactive check first consults ADT's inactive-object list. SAP otherwise
/// checks nothing and can return a fabricated syntax error when no inactive
/// version exists. If the list itself cannot be read, the check still runs and
/// `inactive_version_exists` remains unknown.
///
/// # Errors
///
/// Returns [`AdtSourceCheckError`] for invalid object identity, SAP request
/// failures, or malformed checkrun XML.
pub async fn check_adt_stored_source(
    sap: &mut SapClient,
    object_type: EditableAdtObjectType,
    name: &str,
    version: AdtSourceVersion,
) -> Result<AdtSourceCheckResult, AdtSourceCheckError> {
    let identity =
        editable_object_identity(object_type, name).map_err(AdtSourceCheckError::InvalidObject)?;
    let inactive_version_exists = if version == AdtSourceVersion::Inactive {
        probe_inactive_version(sap, &identity.object_uri).await
    } else {
        None
    };

    if inactive_version_exists == Some(false) {
        let messages = vec![AdtSourceCheckMessage {
            severity: AdtSourceCheckSeverity::Info,
            text: "No inactive version exists; there are no unactivated changes to check. Use --version active to check what is live."
                .to_owned(),
            line: None,
        }];
        return Ok(AdtSourceCheckResult {
            object_type: identity.object_type,
            name: identity.name,
            object_uri: identity.object_uri,
            source_uri: identity.source_uri,
            requested_version: version,
            check_executed: false,
            inactive_version_exists,
            clean: true,
            errors: 0,
            warnings: 0,
            infos: 1,
            messages,
        });
    }

    let body = build_checkrun_request(&identity.source_uri, version);
    let mut headers = HeaderMap::new();
    headers.insert(
        "Content-Type",
        HeaderValue::from_static("application/vnd.sap.adt.checkobjects+xml"),
    );
    headers.insert(
        "Accept",
        HeaderValue::from_static("application/vnd.sap.adt.checkmessages+xml"),
    );
    let response = sap
        .post_text(
            CHECKRUNS_PATH,
            &[("reporters", "abapCheckRun")],
            Some(&body),
            headers,
        )
        .await
        .map_err(AdtSourceCheckError::Sap)?;
    let parsed = parse_checkrun_response(&response)?;

    Ok(AdtSourceCheckResult {
        object_type: identity.object_type,
        name: identity.name,
        object_uri: identity.object_uri,
        source_uri: identity.source_uri,
        requested_version: version,
        check_executed: true,
        inactive_version_exists,
        clean: parsed.errors == 0,
        errors: parsed.errors,
        warnings: parsed.warnings,
        infos: parsed.infos,
        messages: parsed.messages,
    })
}

async fn probe_inactive_version(sap: &SapClient, object_uri: &str) -> Option<bool> {
    let response = sap.get_text(INACTIVE_OBJECTS_PATH).await.ok()?;
    Some(
        response
            .to_ascii_lowercase()
            .contains(&object_uri.to_ascii_lowercase()),
    )
}

fn build_checkrun_request(source_uri: &str, version: AdtSourceVersion) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><chkrun:checkObjectList xmlns:chkrun=\"http://www.sap.com/adt/checkrun\" xmlns:adtcore=\"http://www.sap.com/adt/core\"><chkrun:checkObject adtcore:uri=\"{source_uri}\" chkrun:version=\"{}\"/></chkrun:checkObjectList>",
        version.as_str()
    )
}

struct ParsedCheckrunResponse {
    errors: usize,
    warnings: usize,
    infos: usize,
    messages: Vec<AdtSourceCheckMessage>,
}

fn parse_checkrun_response(response: &str) -> Result<ParsedCheckrunResponse, AdtSourceCheckError> {
    let document = roxmltree::Document::parse(response).map_err(AdtSourceCheckError::Parse)?;
    let messages = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "checkMessage")
        .filter_map(|node| {
            let text = attribute(node, "shortText")?.trim();
            if text.is_empty() {
                return None;
            }
            let raw_type = attribute(node, "type").unwrap_or_default();
            let raw_type = raw_type.to_ascii_uppercase();
            let severity = if raw_type.starts_with('E') || raw_type.starts_with('A') {
                AdtSourceCheckSeverity::Error
            } else if raw_type.starts_with('W') {
                AdtSourceCheckSeverity::Warning
            } else {
                AdtSourceCheckSeverity::Info
            };
            let line = attribute(node, "line").and_then(|line| line.parse().ok());
            Some(AdtSourceCheckMessage {
                severity,
                text: text.to_owned(),
                line,
            })
        })
        .collect::<Vec<_>>();
    let errors = messages
        .iter()
        .filter(|message| message.severity == AdtSourceCheckSeverity::Error)
        .count();
    let warnings = messages
        .iter()
        .filter(|message| message.severity == AdtSourceCheckSeverity::Warning)
        .count();
    let infos = messages.len() - errors - warnings;
    Ok(ParsedCheckrunResponse {
        errors,
        warnings,
        infos,
        messages,
    })
}

fn attribute<'a, 'input>(node: roxmltree::Node<'a, 'input>, name: &str) -> Option<&'a str> {
    node.attributes()
        .find(|attribute| attribute.name() == name)
        .map(|attribute| attribute.value())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_check_messages_and_decodes_xml_attributes() {
        let parsed = parse_checkrun_response(
            r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun">
                <chkrun:checkMessage chkrun:type="E" chkrun:shortText="Expected &lt;identifier&gt; &amp; value" chkrun:line="12"/>
                <chkrun:checkMessage chkrun:type="W" chkrun:shortText="Unused variable"/>
                <chkrun:checkMessage chkrun:type="I" chkrun:shortText="Check completed" chkrun:line="not-a-number"/>
            </chkrun:checkMessageList>"#,
        )
        .unwrap();

        assert_eq!(parsed.errors, 1);
        assert_eq!(parsed.warnings, 1);
        assert_eq!(parsed.infos, 1);
        assert_eq!(parsed.messages[0].text, "Expected <identifier> & value");
        assert_eq!(parsed.messages[0].line, Some(12));
        assert_eq!(parsed.messages[2].line, None);
    }

    #[test]
    fn accepts_a_well_formed_response_with_no_messages_as_clean() {
        let parsed = parse_checkrun_response(
            r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun"/>"#,
        )
        .unwrap();

        assert_eq!(parsed.errors, 0);
        assert!(parsed.messages.is_empty());
    }

    #[test]
    fn rejects_malformed_checkrun_xml() {
        assert!(matches!(
            parse_checkrun_response("<not-closed>"),
            Err(AdtSourceCheckError::Parse(_))
        ));
    }
}

use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use crate::suggested_command;

use super::{
    adt_message_severity::AdtMessageSeverity,
    client::{SapClient, SapError},
    editable_source::{
        AdtSourceVersion, EditableAdtObjectType, EditableAdtSourceIdentity,
        EditableAdtSourceTargetError, editable_source_identity,
    },
    find_attribute_value,
};

const CHECKRUNS_PATH: &str = "/sap/bc/adt/checkruns";
const INACTIVE_OBJECTS_PATH: &str = "/sap/bc/adt/activation/inactiveobjects";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceCheckMessage {
    pub severity: AdtMessageSeverity,
    pub text: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceCheckResult {
    pub identity: EditableAdtSourceIdentity,
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
    InvalidObject(#[source] EditableAdtSourceTargetError),
    #[error("SAP source check failed: {source}")]
    Sap {
        identity: Box<EditableAdtSourceIdentity>,
        version: AdtSourceVersion,
        #[source]
        source: SapError,
    },
    #[error("SAP returned malformed source-check XML: {source}")]
    Parse {
        identity: Box<EditableAdtSourceIdentity>,
        version: AdtSourceVersion,
        #[source]
        source: roxmltree::Error,
    },
}

#[derive(Debug, Error)]
pub enum AdtInactiveSourceProbeError {
    #[error("SAP inactive-object lookup failed: {0}")]
    Sap(#[from] SapError),
    #[error("SAP returned malformed inactive-object XML: {0}")]
    Parse(#[from] roxmltree::Error),
}

impl AdtInactiveSourceProbeError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::Sap(error) => Some(error),
            Self::Parse(_) => None,
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) => {
                "The SAP inactive-object response did not contain valid ADT XML.".to_owned()
            }
        }
    }
}

impl AdtSourceCheckError {
    #[must_use]
    pub const fn code(&self) -> &'static str {
        match self {
            Self::InvalidObject(error) => error.code(),
            Self::Sap { .. } => "edit_source_check_failed",
            Self::Parse { .. } => "edit_source_check_response_invalid",
        }
    }

    #[must_use]
    pub fn hint(&self) -> String {
        match self {
            Self::InvalidObject(error) => error.hint(),
            Self::Sap { source, .. } => source.hint(),
            Self::Parse { .. } => {
                "The SAP checkrun response did not match the expected ADT check-message XML."
                    .to_owned()
            }
        }
    }

    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapError> {
        match self {
            Self::Sap { source, .. } => Some(source),
            Self::InvalidObject(_) | Self::Parse { .. } => None,
        }
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// When the check itself cannot run, reading the version that was being
    /// checked is the remaining way to inspect the source.
    #[must_use]
    pub fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Sap {
                identity, version, ..
            }
            | Self::Parse {
                identity, version, ..
            } => Some(suggested_command::edit_read(
                identity.object_type.as_str(),
                &identity.name,
                version.as_str(),
            )),
            Self::InvalidObject(_) => None,
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
        editable_source_identity(object_type, name).map_err(AdtSourceCheckError::InvalidObject)?;
    let inactive_version_exists = if version == AdtSourceVersion::Inactive {
        probe_inactive_adt_source(sap, &identity.object_uri)
            .await
            .ok()
    } else {
        None
    };

    if inactive_version_exists == Some(false) {
        return check_adt_source_by_identity(sap, &identity, version, inactive_version_exists)
            .await;
    }
    sap.establish_csrf_session()
        .await
        .map_err(|source| AdtSourceCheckError::Sap {
            identity: Box::new(identity.clone()),
            version,
            source,
        })?;
    check_adt_source_by_identity(sap, &identity, version, inactive_version_exists).await
}

/// Runs a check after the caller has established CSRF/session state.
pub(super) async fn check_adt_source_by_identity(
    sap: &SapClient,
    identity: &EditableAdtSourceIdentity,
    version: AdtSourceVersion,
    inactive_version_exists: Option<bool>,
) -> Result<AdtSourceCheckResult, AdtSourceCheckError> {
    if inactive_version_exists == Some(false) {
        let messages = vec![AdtSourceCheckMessage {
            severity: AdtMessageSeverity::Info,
            text: "No inactive version exists; there are no unactivated changes to check. Use --version active to check what is live."
                .to_owned(),
            line: None,
        }];
        return Ok(AdtSourceCheckResult {
            identity: identity.clone(),
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
        .post_text_read_only(
            CHECKRUNS_PATH,
            &[("reporters", "abapCheckRun")],
            Some(&body),
            headers,
        )
        .await
        .map_err(|source| AdtSourceCheckError::Sap {
            identity: Box::new(identity.clone()),
            version,
            source,
        })?;
    let parsed = parse_checkrun_response(&response, identity, version)?;

    Ok(AdtSourceCheckResult {
        identity: identity.clone(),
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

pub(super) async fn probe_inactive_adt_source(
    sap: &SapClient,
    object_uri: &str,
) -> Result<bool, AdtInactiveSourceProbeError> {
    let response = sap.get_text(INACTIVE_OBJECTS_PATH).await?;
    inactive_object_response_contains_uri(&response, object_uri)
}

fn inactive_object_response_contains_uri(
    response: &str,
    object_uri: &str,
) -> Result<bool, AdtInactiveSourceProbeError> {
    let document = roxmltree::Document::parse(response)?;
    Ok(document.descendants().any(|node| {
        node.attributes().any(|attribute| {
            attribute.name().eq_ignore_ascii_case("uri")
                && attribute.value().eq_ignore_ascii_case(object_uri)
        })
    }))
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

fn parse_checkrun_response(
    response: &str,
    identity: &EditableAdtSourceIdentity,
    version: AdtSourceVersion,
) -> Result<ParsedCheckrunResponse, AdtSourceCheckError> {
    let document =
        roxmltree::Document::parse(response).map_err(|source| AdtSourceCheckError::Parse {
            identity: Box::new(identity.clone()),
            version,
            source,
        })?;
    let messages = document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "checkMessage")
        .filter_map(|node| {
            let text = find_attribute_value(node, "shortText")?.trim();
            if text.is_empty() {
                return None;
            }
            let severity = AdtMessageSeverity::from_sap_code(
                find_attribute_value(node, "type").unwrap_or_default(),
            );
            let line = find_attribute_value(node, "line").and_then(|line| line.parse().ok());
            Some(AdtSourceCheckMessage {
                severity,
                text: text.to_owned(),
                line,
            })
        })
        .collect::<Vec<_>>();
    let errors = messages
        .iter()
        .filter(|message| message.severity == AdtMessageSeverity::Error)
        .count();
    let warnings = messages
        .iter()
        .filter(|message| message.severity == AdtMessageSeverity::Warning)
        .count();
    let infos = messages.len() - errors - warnings;
    Ok(ParsedCheckrunResponse {
        errors,
        warnings,
        infos,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_identity() -> EditableAdtSourceIdentity {
        editable_source_identity(EditableAdtObjectType::Class, "zcl_sample").unwrap()
    }

    #[test]
    fn parses_check_messages_and_decodes_xml_attributes() {
        let parsed = parse_checkrun_response(
            r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun">
                <chkrun:checkMessage chkrun:type="E" chkrun:shortText="Expected &lt;identifier&gt; &amp; value" chkrun:line="12"/>
                <chkrun:checkMessage chkrun:type="W" chkrun:shortText="Unused variable"/>
                <chkrun:checkMessage chkrun:type="I" chkrun:shortText="Check completed" chkrun:line="not-a-number"/>
            </chkrun:checkMessageList>"#,
            &sample_identity(),
            AdtSourceVersion::Inactive,
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
            &sample_identity(),
            AdtSourceVersion::Inactive,
        )
        .unwrap();

        assert_eq!(parsed.errors, 0);
        assert!(parsed.messages.is_empty());
    }

    #[test]
    fn rejects_malformed_checkrun_xml() {
        assert!(matches!(
            parse_checkrun_response(
                "<not-closed>",
                &sample_identity(),
                AdtSourceVersion::Inactive,
            ),
            Err(AdtSourceCheckError::Parse { .. })
        ));
    }

    #[test]
    fn inactive_object_probe_matches_complete_uris_not_prefixes() {
        let response = r#"<adtcore:objectReferences xmlns:adtcore="http://www.sap.com/adt/core">
            <adtcore:objectReference adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample2"/>
        </adtcore:objectReferences>"#;

        assert!(
            !inactive_object_response_contains_uri(response, "/sap/bc/adt/oo/classes/zcl_sample")
                .unwrap()
        );
        assert!(
            inactive_object_response_contains_uri(response, "/SAP/BC/ADT/OO/CLASSES/ZCL_SAMPLE2")
                .unwrap()
        );
    }
}

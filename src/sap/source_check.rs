use reqwest::header::{HeaderMap, HeaderValue};
use thiserror::Error;

use crate::suggested_command;

use super::{
    adt_message_severity::AdtMessageSeverity,
    adt_version::AdtVersion,
    class_include::ClassInclude,
    client::{SapClient, SapClientError},
    editable_source::{
        EditableAdtObjectType, EditableAdtSourceIdentity, EditableAdtSourceTargetError,
        editable_source_identity,
    },
    find_attribute_value,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const CHECKRUNS_PATH: &str = "/sap/bc/adt/checkruns";
const INACTIVE_OBJECTS_PATH: &str = "/sap/bc/adt/activation/inactiveobjects";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceCheckMessage {
    pub severity: AdtMessageSeverity,
    pub text: String,
    /// The source the message is about, without its position fragment. A
    /// class check reports errors in every include, each at its own URI.
    pub uri: Option<String>,
    /// Which class stream `uri` is, when it is one.
    pub include: Option<ClassInclude>,
    /// Relative to the stream `uri` names, not to the object as a whole.
    pub line: Option<usize>,
    pub column: Option<usize>,
}

impl AdtSourceCheckMessage {
    /// `implementations line 9, column 9`, or as much of it as is known.
    #[must_use]
    pub fn location(&self) -> Option<String> {
        let include = self.include.map(ClassInclude::as_str);
        let position = match (self.line, self.column) {
            (Some(line), Some(column)) => Some(format!("line {line}, column {column}")),
            (Some(line), None) => Some(format!("line {line}")),
            (None, _) => None,
        };
        match (include, position) {
            (Some(include), Some(position)) => Some(format!("{include} {position}")),
            (Some(include), None) => Some(include.to_owned()),
            (None, position) => position,
        }
    }

    /// The text with its location in front, for a one-line summary.
    #[must_use]
    pub fn located_text(&self) -> String {
        self.location()
            .map_or_else(|| self.text.clone(), |at| format!("{at}: {}", self.text))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdtSourceCheckResult {
    pub identity: EditableAdtSourceIdentity,
    pub requested_version: AdtVersion,
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
        version: AdtVersion,
        #[source]
        source: SapClientError,
    },
    #[error("SAP returned malformed source-check XML: {source}")]
    Parse {
        identity: Box<EditableAdtSourceIdentity>,
        version: AdtVersion,
        #[source]
        source: roxmltree::Error,
    },
}

#[derive(Debug, Error)]
pub enum AdtInactiveSourceProbeError {
    #[error("SAP inactive-object lookup failed: {0}")]
    Sap(#[from] SapClientError),
    #[error("SAP returned malformed inactive-object XML: {0}")]
    Parse(#[from] roxmltree::Error),
}

impl AdtInactiveSourceProbeError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap(error) => Some(error),
            Self::Parse(_) => None,
        }
    }

    #[must_use]
    pub fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) => {
                Some("The SAP inactive-object response did not contain valid ADT XML.".to_owned())
            }
        }
    }
}

impl AdtSourceCheckError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap { source, .. } => Some(source),
            Self::InvalidObject(_) | Self::Parse { .. } => None,
        }
    }
}

impl ReportableError for AdtSourceCheckError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidObject(error) => error.code(),
            Self::Sap { .. } => "edit_source_check_failed",
            Self::Parse { .. } => "edit_source_check_response_invalid",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::InvalidObject(error) => error.hint()?,
            Self::Sap { source, .. } => source.hint()?,
            Self::Parse { .. } => {
                "The SAP checkrun response did not match the expected ADT check-message XML."
                    .to_owned()
            }
        })
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// When the check itself cannot run, reading the version that was being
    /// checked is the remaining way to inspect the source.
    fn suggested_command(&self) -> Option<String> {
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
    version: AdtVersion,
) -> Result<AdtSourceCheckResult, AdtSourceCheckError> {
    let identity =
        editable_source_identity(object_type, name).map_err(AdtSourceCheckError::InvalidObject)?;
    let inactive_version_exists = if version == AdtVersion::Inactive {
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
    version: AdtVersion,
    inactive_version_exists: Option<bool>,
) -> Result<AdtSourceCheckResult, AdtSourceCheckError> {
    if inactive_version_exists == Some(false) {
        let messages = vec![AdtSourceCheckMessage {
            severity: AdtMessageSeverity::Info,
            text: "No inactive version exists; there are no unactivated changes to check. Use --version active to check what is live."
                .to_owned(),
            uri: None,
            include: None,
            line: None,
            column: None,
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
    read_worklist(sap).await?.contains(object_uri)
}

/// The caller's inactive-object worklist, read once and asked several things.
pub(super) struct Worklist(String);

impl Worklist {
    /// Whether the object itself is on the worklist.
    pub(super) fn contains(&self, object_uri: &str) -> Result<bool, AdtInactiveSourceProbeError> {
        inactive_object_response_contains_uri(&self.0, object_uri)
    }

    /// Which of a class's includes the caller has staged. See
    /// [`staged_class_includes`].
    pub(super) fn staged_includes(
        &self,
        class_uri: &str,
    ) -> Result<Vec<ClassInclude>, AdtInactiveSourceProbeError> {
        staged_includes_in(&self.0, class_uri)
    }
}

pub(super) async fn read_worklist(
    sap: &SapClient,
) -> Result<Worklist, AdtInactiveSourceProbeError> {
    Ok(Worklist(sap.get_text(INACTIVE_OBJECTS_PATH).await?))
}

/// Which of a class's includes the caller has staged: what activating the
/// class will publish.
///
/// Read from the caller's own worklist, not from the class document, which
/// also shows includes other users have staged and which an activation leaves
/// alone. An include is listed under its own URI; `main` is listed by section
/// or method, as `source/main#type=…`.
///
/// # Errors
///
/// Returns [`AdtInactiveSourceProbeError`] when the worklist cannot be read.
pub(super) async fn staged_class_includes(
    sap: &SapClient,
    class_uri: &str,
) -> Result<Vec<ClassInclude>, AdtInactiveSourceProbeError> {
    read_worklist(sap).await?.staged_includes(class_uri)
}

fn staged_includes_in(
    response: &str,
    class_uri: &str,
) -> Result<Vec<ClassInclude>, AdtInactiveSourceProbeError> {
    let document = roxmltree::Document::parse(response)?;
    let mut staged: Vec<ClassInclude> = document
        .descendants()
        .filter_map(|node| find_attribute_value(node, "uri"))
        .filter_map(ClassInclude::split_source_uri)
        .filter(|(class, _)| class.eq_ignore_ascii_case(class_uri))
        .map(|(_, include)| include)
        .collect();
    staged.sort_unstable();
    staged.dedup();
    Ok(staged)
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

/// Where a check message points: `<source uri>#start=<line>,<column>`.
///
/// SAP sends the position only in this fragment; there is no separate line
/// attribute on the checkrun messages it returns.
struct MessageLocation {
    uri: String,
    include: Option<ClassInclude>,
    line: Option<usize>,
    column: Option<usize>,
}

impl MessageLocation {
    fn parse(value: &str) -> Self {
        let (uri, fragment) = value.split_once('#').unwrap_or((value, ""));
        let start = fragment
            .split(';')
            .find_map(|part| part.strip_prefix("start="));
        let (line, column) = start.map_or((None, None), |start| {
            let (line, column) = start.split_once(',').unwrap_or((start, ""));
            (line.parse().ok(), column.parse().ok())
        });
        Self {
            include: ClassInclude::split_source_uri(uri).map(|(_, include)| include),
            uri: uri.to_owned(),
            line,
            column,
        }
    }
}

fn build_checkrun_request(source_uri: &str, version: AdtVersion) -> String {
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
    version: AdtVersion,
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
            let location = find_attribute_value(node, "uri").map(MessageLocation::parse);
            let (uri, include, line, column) = location.map_or((None, None, None, None), |at| {
                (Some(at.uri), at.include, at.line, at.column)
            });
            let line =
                line.or_else(|| find_attribute_value(node, "line").and_then(|l| l.parse().ok()));
            Some(AdtSourceCheckMessage {
                severity,
                text: text.to_owned(),
                uri,
                include,
                line,
                column,
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
            AdtVersion::Inactive,
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
    fn reads_the_staged_includes_of_one_class_from_the_worklist() {
        let response = r#"<ioc:inactiveObjects xmlns:ioc="http://www.sap.com/abapxml/inactiveCtsObjects" xmlns:adtcore="http://www.sap.com/adt/core">
            <ioc:entry><ioc:object/><ioc:transport/></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample" adtcore:type="CLAS/OC"/></ioc:object></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/source/main#type=CLAS%2FOSU;name=ZCL_SAMPLE" adtcore:type="CLAS/OSU"/></ioc:object></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample/includes/implementations" adtcore:type="CLAS/OCN/implementations" adtcore:parentUri="/sap/bc/adt/oo/classes/zcl_sample"/></ioc:object></ioc:entry>
            <ioc:entry><ioc:object><ioc:ref adtcore:uri="/sap/bc/adt/oo/classes/zcl_sample2/includes/macros" adtcore:type="CLAS/OCN/macros"/></ioc:object></ioc:entry>
        </ioc:inactiveObjects>"#;

        assert_eq!(
            staged_includes_in(response, "/sap/bc/adt/oo/classes/zcl_sample").unwrap(),
            vec![ClassInclude::Main, ClassInclude::Implementations]
        );
    }

    #[test]
    fn locates_a_message_in_the_include_it_names() {
        // The shape SAP returns: the position lives only in the URI fragment.
        let parsed = parse_checkrun_response(
            r#"<chkrun:checkRunReports xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkReport chkrun:reporter="abapCheckRun" chkrun:triggeringUri="/sap/bc/adt/oo/classes/zcl_sample/source/main" chkrun:status="processed"><chkrun:checkMessageList><chkrun:checkMessage chkrun:uri="/sap/bc/adt/oo/classes/zcl_sample/includes/implementations#start=9,9" chkrun:type="E" chkrun:shortText="Field &quot;LV_MISSING&quot; is unknown."><atom:link href="art.syntax:GTU" rel="http://www.sap.com/adt/categories/quickfixes" xmlns:atom="http://www.w3.org/2005/Atom"/></chkrun:checkMessage></chkrun:checkMessageList></chkrun:checkReport></chkrun:checkRunReports>"#,
            &sample_identity(),
            AdtVersion::Inactive,
        )
        .unwrap();

        let message = &parsed.messages[0];
        assert_eq!(
            message.uri.as_deref(),
            Some("/sap/bc/adt/oo/classes/zcl_sample/includes/implementations")
        );
        assert_eq!(message.include, Some(ClassInclude::Implementations));
        assert_eq!(message.line, Some(9));
        assert_eq!(message.column, Some(9));
    }

    #[test]
    fn a_message_outside_any_class_has_no_include() {
        let parsed = parse_checkrun_response(
            r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun"><chkrun:checkMessage chkrun:uri="/sap/bc/adt/programs/programs/zsample/source/main#start=3,1" chkrun:type="E" chkrun:shortText="Syntax error"/></chkrun:checkMessageList>"#,
            &sample_identity(),
            AdtVersion::Inactive,
        )
        .unwrap();

        assert_eq!(parsed.messages[0].include, None);
        assert_eq!(parsed.messages[0].line, Some(3));
    }

    #[test]
    fn accepts_a_well_formed_response_with_no_messages_as_clean() {
        let parsed = parse_checkrun_response(
            r#"<chkrun:checkMessageList xmlns:chkrun="http://www.sap.com/adt/checkrun"/>"#,
            &sample_identity(),
            AdtVersion::Inactive,
        )
        .unwrap();

        assert_eq!(parsed.errors, 0);
        assert!(parsed.messages.is_empty());
    }

    #[test]
    fn rejects_malformed_checkrun_xml() {
        assert!(matches!(
            parse_checkrun_response("<not-closed>", &sample_identity(), AdtVersion::Inactive,),
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

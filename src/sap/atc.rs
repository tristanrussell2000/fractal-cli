//! ABAP Test Cockpit runs.
//!
//! Four requests: the system's check variant from the ATC customizing, a
//! worklist for that variant, a synchronous run over the objects, and the
//! worklist's findings.
//!
//! Traps, measured live:
//! - **No findings is not clean.** A run can answer with zero findings and an
//!   info such as `TOOL_FAILURE` ("Check not executable, due to missing
//!   prerequisites"). Such a run is reported as incomplete.
//! - **Objects without findings are left out of the result**, so every object
//!   asked about is listed, with none where nothing came back. A name that
//!   does not exist would look clean, so each one is checked first.
//! - **A class finding's line counts from its method**:
//!   `…/source/main#type=CLAS%2FOM;name=RUN;start=13` is line 13 of `RUN`,
//!   the `METHOD` line being 1. The class source is read to turn it into a
//!   line of the class.
//! - **ATC checks the active version.** Staged changes are not checked.
//! - The variant list endpoint answers with nothing, and the system's default
//!   (`DEFAULT`) is a classic Code Inspector variant, not an ATC variant
//!   object, so a variant can be named but not listed.

use std::collections::BTreeMap;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use thiserror::Error;

use super::{
    adt_version::AdtVersion,
    client::{SapClient, SapClientError},
    editable_source::{EditableAdtObjectType, editable_source_identity},
    find_attribute_value, find_children, find_non_empty_attribute,
    object_source::{ByteRangeOptions, get_source, resolve_source_target},
    worklist::list_inactive_objects,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const CUSTOMIZING_PATH: &str = "/sap/bc/adt/atc/customizing";
const WORKLISTS_PATH: &str = "/sap/bc/adt/atc/worklists";
const RUNS_PATH: &str = "/sap/bc/adt/atc/runs";
const WORKLIST_ACCEPT: &str = "application/atc.worklist.v1+xml";
const DOCUMENTATION_ACCEPT: &str = "application/vnd.sap.adt.docu.v1+html";
const MAXIMUM_VERDICTS: &str = "1000";

/// One object to check.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcTarget {
    /// `CLAS`, `DDLS`, `DEVC`, …
    pub object_type: String,
    pub name: String,
    uri: String,
}

impl AtcTarget {
    /// A source-based type, a function group (`FUGR`) or a package (`DEVC`).
    ///
    /// # Errors
    ///
    /// Returns [`AtcError::UnsupportedType`] or [`AtcError::InvalidName`].
    pub fn parse(object_type: &str, name: &str) -> Result<Self, AtcError> {
        let object_type = object_type.trim().to_ascii_uppercase();
        let name = name.trim().to_ascii_uppercase();
        let collection = match object_type.as_str() {
            "DEVC" => Some("/sap/bc/adt/packages"),
            "FUGR" => Some("/sap/bc/adt/functions/groups"),
            _ => None,
        };
        let uri = if let Some(collection) = collection {
            if name.is_empty()
                || !name
                    .chars()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '$'))
            {
                return Err(AtcError::InvalidName(name));
            }
            format!(
                "{collection}/{}",
                name.to_ascii_lowercase()
                    .replace('/', "%2f")
                    .replace('$', "%24")
            )
        } else {
            let editable = EditableAdtObjectType::parse(&object_type)
                .map_err(|_| AtcError::UnsupportedType(object_type.clone()))?;
            editable_source_identity(editable, &name)
                .map_err(|_| AtcError::InvalidName(name.clone()))?
                .object_uri
        };
        Ok(Self {
            object_type,
            name,
            uri,
        })
    }

    #[must_use]
    pub fn describe(&self) -> String {
        format!("{} {}", self.object_type, self.name)
    }

    fn is_package(&self) -> bool {
        self.object_type == "DEVC"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AtcRequest {
    pub targets: Vec<AtcTarget>,
    /// `None` for the system's own variant.
    pub variant: Option<String>,
    /// Also read each finding's documentation.
    pub explain: bool,
    /// The lowest priority that fails the run: 2 by default, 3 for strict.
    pub fail_on: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AtcSummary {
    pub variant: String,
    /// No finding at or above `fail_on`, and nothing kept a check from running.
    pub passed: bool,
    /// False when SAP reported a check it could not run.
    pub complete: bool,
    pub priority_1: usize,
    pub priority_2: usize,
    pub priority_3: usize,
    /// Every object asked about, and any a package brought in.
    pub objects: Vec<AtcObject>,
    /// What SAP said about the run itself, such as a check that could not run.
    pub infos: Vec<AtcInfo>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AtcObject {
    #[serde(rename = "type")]
    pub object_type: String,
    pub name: String,
    pub findings: Vec<AtcFinding>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AtcFinding {
    pub priority: u8,
    pub check: String,
    pub message: String,
    /// The source the finding is in, without the line.
    pub uri: Option<String>,
    /// Line of that source, counted from 1.
    pub line: Option<u32>,
    /// For a class, the method the finding is in, and the line within it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub method_line: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub documentation: Option<String>,
    #[serde(skip)]
    documentation_uri: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AtcInfo {
    pub kind: String,
    pub description: String,
}

impl AtcInfo {
    fn is_failure(&self) -> bool {
        let kind = self.kind.to_ascii_uppercase();
        kind.contains("FAILURE") || kind.contains("ERROR")
    }
}

#[derive(Debug, Error)]
pub enum AtcError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error("SAP returned malformed ATC XML: {0}")]
    Parse(#[from] roxmltree::Error),
    #[error("ATC cannot check type '{0}' here")]
    UnsupportedType(String),
    #[error("'{0}' is not a valid object name")]
    InvalidName(String),
    #[error("{0} does not exist")]
    TargetNotFound(String),
    #[error("the ATC customizing names no system check variant")]
    NoSystemVariant,
    #[error("SAP did not return a worklist id")]
    NoWorklist,
}

impl ReportableError for AtcError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(_) => "atc_response_invalid",
            Self::UnsupportedType(_) => "atc_unsupported_type",
            Self::InvalidName(_) => "invalid_object_name",
            Self::TargetNotFound(_) => "atc_target_not_found",
            Self::NoSystemVariant => "atc_no_system_variant",
            Self::NoWorklist => "atc_worklist_missing",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(match self {
            Self::Sap(error) => Some(error),
            _ => None,
        })
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) | Self::NoWorklist => {
                Some("SAP's ATC answer was not in the expected shape.".to_owned())
            }
            Self::UnsupportedType(_) => Some(
                "Use a source type (CLAS, INTF, PROG, DDLS, BDEF, SRVD, DDLX, DCLS, TABL), FUGR or DEVC."
                    .to_owned(),
            ),
            Self::InvalidName(_) => {
                Some("Use a repository name: letters, digits, underscore and slash.".to_owned())
            }
            Self::TargetNotFound(_) => Some(
                "Check the name; ATC would report a missing object as having no findings."
                    .to_owned(),
            ),
            Self::NoSystemVariant => Some("Name one with --variant.".to_owned()),
        }
    }
}

/// Runs ATC over the targets and gathers every finding.
///
/// # Errors
///
/// Returns [`AtcError`] when a target does not exist or a request fails.
pub async fn run_atc(sap: &mut SapClient, request: &AtcRequest) -> Result<AtcSummary, AtcError> {
    for target in &request.targets {
        match sap.get_text(&target.uri).await {
            Ok(_) => {}
            Err(error) if error.is_not_found() => {
                return Err(AtcError::TargetNotFound(target.describe()));
            }
            Err(error) => return Err(error.into()),
        }
    }
    let variant = match &request.variant {
        Some(variant) => variant.trim().to_ascii_uppercase(),
        None => system_variant(&sap.get_text(CUSTOMIZING_PATH).await?)?
            .ok_or(AtcError::NoSystemVariant)?,
    };

    let mut accept_text = HeaderMap::new();
    accept_text.insert(ACCEPT, HeaderValue::from_static("text/plain"));
    let worklist = sap
        .post_text(
            WORKLISTS_PATH,
            &[("checkVariant", variant.as_str())],
            None,
            accept_text,
        )
        .await?;
    let worklist = worklist.trim().to_owned();
    if worklist.is_empty() || worklist.contains('<') {
        return Err(AtcError::NoWorklist);
    }

    let mut xml = HeaderMap::new();
    xml.insert(CONTENT_TYPE, HeaderValue::from_static("application/xml"));
    xml.insert(ACCEPT, HeaderValue::from_static("application/xml"));
    let run = sap
        .post_text(
            RUNS_PATH,
            &[("worklistId", worklist.as_str())],
            Some(&run_body(&request.targets)),
            xml,
        )
        .await?;
    let infos = parse_run(&run)?;

    let mut accept_worklist = HeaderMap::new();
    accept_worklist.insert(ACCEPT, HeaderValue::from_static(WORKLIST_ACCEPT));
    let findings = sap
        .get_bytes_with_query_and_headers(
            &format!("{WORKLISTS_PATH}/{worklist}"),
            &[("includeExemptedFindings", "false")],
            accept_worklist,
        )
        .await?;
    let mut objects = parse_worklist(&String::from_utf8_lossy(&findings))?;

    place_method_findings(sap, &mut objects).await;
    if request.explain {
        explain(sap, &mut objects).await;
    }
    let notes = staged_notes(sap, &request.targets).await;
    Ok(summarize(
        variant,
        &request.targets,
        objects,
        infos,
        notes,
        request.fail_on,
    ))
}

fn system_variant(customizing: &str) -> Result<Option<String>, AtcError> {
    let document = roxmltree::Document::parse(customizing)?;
    Ok(document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "property")
        .find(|node| find_attribute_value(*node, "name") == Some("systemCheckVariant"))
        .and_then(|node| find_non_empty_attribute(node, "value")))
}

fn run_body(targets: &[AtcTarget]) -> String {
    let references: String = targets
        .iter()
        .fold(String::new(), |mut references, target| {
            references.push_str("<adtcore:objectReference adtcore:uri=\"");
            references.push_str(&target.uri);
            references.push_str("\"/>");
            references
        });
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<atc:run maximumVerdicts=\"{MAXIMUM_VERDICTS}\" xmlns:atc=\"http://www.sap.com/adt/atc\">\
<objectSets xmlns:adtcore=\"http://www.sap.com/adt/core\"><objectSet kind=\"inclusive\">\
<adtcore:objectReferences>{references}</adtcore:objectReferences>\
</objectSet></objectSets></atc:run>"
    )
}

/// The run's infos, without the per-priority counts, which the findings give.
fn parse_run(run: &str) -> Result<Vec<AtcInfo>, AtcError> {
    let document = roxmltree::Document::parse(run)?;
    Ok(document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "info")
        .filter_map(|info| {
            let text = |name: &str| {
                find_children(info, name)
                    .next()
                    .and_then(|node| node.text())
                    .map(|text| text.trim().to_owned())
            };
            let kind = text("type")?;
            (kind != "FINDING_STATS").then(|| AtcInfo {
                kind,
                description: text("description").unwrap_or_default(),
            })
        })
        .collect())
}

fn parse_worklist(xml: &str) -> Result<Vec<AtcObject>, AtcError> {
    let document = roxmltree::Document::parse(xml)?;
    let mut objects = Vec::new();
    for object in document
        .descendants()
        .filter(|node| node.is_element() && node.tag_name().name() == "object")
    {
        let findings = find_children(object, "findings")
            .flat_map(|findings| find_children(findings, "finding"))
            .map(|finding| {
                let attribute = |name: &str| find_non_empty_attribute(finding, name);
                let mut parsed = AtcFinding {
                    priority: attribute("priority")
                        .and_then(|priority| priority.parse().ok())
                        .unwrap_or(3),
                    check: attribute("checkTitle").unwrap_or_default(),
                    message: attribute("messageTitle").unwrap_or_default(),
                    uri: None,
                    line: None,
                    method: None,
                    method_line: None,
                    documentation: None,
                    documentation_uri: find_children(finding, "link")
                        .find(|link| {
                            find_attribute_value(*link, "rel")
                                .is_some_and(|rel| rel.ends_with("/documentation"))
                        })
                        .and_then(|link| find_non_empty_attribute(link, "href")),
                };
                if let Some(location) = attribute("location") {
                    apply_location(&mut parsed, &location);
                }
                parsed
            })
            .collect();
        objects.push(AtcObject {
            object_type: find_non_empty_attribute(object, "type").unwrap_or_default(),
            name: find_non_empty_attribute(object, "name").unwrap_or_default(),
            findings,
        });
    }
    Ok(objects)
}

/// `…#start=13` is a line; `…#type=CLAS%2FOM;name=RUN;start=13` is line 13
/// of method `RUN`, made absolute later from the class source.
fn apply_location(finding: &mut AtcFinding, location: &str) {
    let (uri, fragment) = location.split_once('#').unwrap_or((location, ""));
    finding.uri = Some(uri.to_owned());
    let mut kind = None;
    let mut name = None;
    let mut start = None;
    for part in fragment.split(';') {
        if let Some(value) = part.strip_prefix("type=") {
            kind = Some(value.replace("%2F", "/").replace("%2f", "/"));
        } else if let Some(value) = part.strip_prefix("name=") {
            name = Some(value.replace("%7e", "~").replace("%7E", "~"));
        } else if let Some(value) = part.strip_prefix("start=") {
            start = value.split(',').next().and_then(|line| line.parse().ok());
        }
    }
    match (kind.as_deref(), name) {
        (Some("CLAS/OM"), Some(method)) => {
            finding.method = Some(method);
            finding.method_line = start;
        }
        (None, _) => finding.line = start,
        _ => {}
    }
}

/// Reads each class source with method-relative findings once, and turns
/// those into lines of the source. A method that cannot be found keeps its
/// relative line only.
async fn place_method_findings(sap: &SapClient, objects: &mut [AtcObject]) {
    let mut sources: BTreeMap<String, Option<String>> = BTreeMap::new();
    for finding in objects
        .iter_mut()
        .flat_map(|object| object.findings.iter_mut())
    {
        let (Some(uri), Some(method), Some(method_line)) = (
            finding.uri.clone(),
            finding.method.clone(),
            finding.method_line,
        ) else {
            continue;
        };
        let object_uri = uri.trim_end_matches("/source/main").to_owned();
        if !sources.contains_key(&object_uri) {
            sources.insert(
                object_uri.clone(),
                read_active_source(sap, &object_uri).await,
            );
        }
        if let Some(Some(source)) = sources.get(&object_uri) {
            finding.line = method_start(source, &method).map(|start| start + method_line - 1);
        }
    }
}

async fn read_active_source(sap: &SapClient, object_uri: &str) -> Option<String> {
    let target = resolve_source_target(object_uri, None).ok()?;
    get_source(
        sap,
        &target,
        AdtVersion::Active,
        ByteRangeOptions::default(),
    )
    .await
    .ok()
    .map(|source| source.content)
}

/// The line of `METHOD name.`, counted from 1.
fn method_start(source: &str, method: &str) -> Option<u32> {
    source
        .lines()
        .position(|line| {
            let mut words = line.split_whitespace();
            words
                .next()
                .is_some_and(|word| word.eq_ignore_ascii_case("METHOD"))
                && words
                    .next()
                    .is_some_and(|word| word.trim_end_matches('.').eq_ignore_ascii_case(method))
        })
        .and_then(|index| u32::try_from(index + 1).ok())
}

async fn explain(sap: &SapClient, objects: &mut [AtcObject]) {
    for finding in objects
        .iter_mut()
        .flat_map(|object| object.findings.iter_mut())
    {
        let Some(uri) = finding.documentation_uri.clone() else {
            continue;
        };
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static(DOCUMENTATION_ACCEPT));
        if let Ok(bytes) = sap
            .get_bytes_with_query_and_headers(&uri, &[], headers)
            .await
        {
            finding.documentation = Some(html_to_text(&String::from_utf8_lossy(&bytes)));
        }
    }
}

/// The documentation as plain paragraphs: tags dropped, entities decoded,
/// whitespace collapsed, a line per block element.
fn html_to_text(html: &str) -> String {
    let mut text = String::new();
    let mut rest = html;
    while let Some(open) = rest.find('<') {
        text.push_str(&rest[..open]);
        let Some(close) = rest[open..].find('>') else {
            break;
        };
        let tag = rest[open + 1..open + close].to_ascii_lowercase();
        let name = tag
            .trim_start_matches('/')
            .split_whitespace()
            .next()
            .unwrap_or("");
        if matches!(name, "style" | "script") && !tag.starts_with('/') {
            let end = format!("</{name}");
            rest = rest[open..].find(&end).map_or("", |at| &rest[open + at..]);
            continue;
        }
        if matches!(
            name,
            "p" | "br" | "div" | "h1" | "h2" | "h3" | "h4" | "li" | "tr"
        ) {
            text.push('\n');
        }
        rest = &rest[open + close + 1..];
    }
    text.push_str(rest);
    let decoded = text
        .replace("&nbsp;", " ")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'")
        .replace("&amp;", "&");
    decoded
        .lines()
        .map(|line| line.split_whitespace().collect::<Vec<_>>().join(" "))
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

async fn staged_notes(sap: &SapClient, targets: &[AtcTarget]) -> Vec<String> {
    let Ok(inactive) = list_inactive_objects(sap).await else {
        return Vec::new();
    };
    targets
        .iter()
        .filter(|target| {
            inactive
                .iter()
                .any(|object| object.uri.eq_ignore_ascii_case(&target.uri))
        })
        .map(|target| {
            format!(
                "{} has staged changes; ATC checked its active version, not them.",
                target.describe()
            )
        })
        .collect()
}

fn summarize(
    variant: String,
    targets: &[AtcTarget],
    mut objects: Vec<AtcObject>,
    infos: Vec<AtcInfo>,
    mut notes: Vec<String>,
    fail_on: u8,
) -> AtcSummary {
    for target in targets.iter().filter(|target| !target.is_package()) {
        let listed = objects.iter().any(|object| {
            object.object_type.eq_ignore_ascii_case(&target.object_type)
                && object.name.eq_ignore_ascii_case(&target.name)
        });
        if !listed {
            objects.push(AtcObject {
                object_type: target.object_type.clone(),
                name: target.name.clone(),
                findings: Vec::new(),
            });
        }
    }
    if targets.iter().any(AtcTarget::is_package) {
        notes.push("A package's objects are listed only when they have findings.".to_owned());
    }
    let count = |priority: u8| {
        objects
            .iter()
            .flat_map(|object| &object.findings)
            .filter(|finding| finding.priority == priority)
            .count()
    };
    let complete = !infos.iter().any(AtcInfo::is_failure);
    let failing = objects
        .iter()
        .flat_map(|object| &object.findings)
        .any(|finding| finding.priority <= fail_on);
    AtcSummary {
        variant,
        passed: complete && !failing,
        complete,
        priority_1: count(1),
        priority_2: count(2),
        priority_3: count(3),
        objects,
        infos,
        notes,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORKLIST: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/atc/worklist.xml"
    ));
    const RUN: &str = include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/testdata/atc/run.xml"));
    const RUN_TOOL_FAILURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/atc/run_tool_failure.xml"
    ));
    const CLASS: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/atc/class.abap"
    ));
    const DOCUMENTATION: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/atc/documentation.html"
    ));

    fn class_target(name: &str) -> AtcTarget {
        AtcTarget::parse("CLAS", name).unwrap()
    }

    #[test]
    fn targets_map_to_the_uris_atc_takes() {
        assert_eq!(
            class_target("zsample_atchk").uri,
            "/sap/bc/adt/oo/classes/zsample_atchk"
        );
        assert_eq!(
            AtcTarget::parse("DEVC", "$tmp").unwrap().uri,
            "/sap/bc/adt/packages/%24tmp"
        );
        assert_eq!(
            AtcTarget::parse("DDLS", "/ACME/R_TRAVEL").unwrap().uri,
            "/sap/bc/adt/ddic/ddl/sources/%2facme%2fr_travel"
        );
        assert_eq!(
            AtcTarget::parse("MSAG", "ZSAMPLE").unwrap_err().code(),
            "atc_unsupported_type"
        );
    }

    #[test]
    fn reads_the_system_variant_from_the_customizing() {
        let customizing = r#"<atc:customizing xmlns:atc="http://www.sap.com/adt/atc"><properties><property name="ciCheckFlavour" value="true"/><property name="systemCheckVariant" value="DEFAULT"/></properties></atc:customizing>"#;
        assert_eq!(
            system_variant(customizing).unwrap().as_deref(),
            Some("DEFAULT")
        );
    }

    #[test]
    fn a_tool_failure_is_kept_and_the_counts_are_not() {
        assert!(parse_run(RUN).unwrap().is_empty());
        let infos = parse_run(RUN_TOOL_FAILURE).unwrap();
        assert_eq!(infos.len(), 1);
        assert!(infos[0].is_failure());
        assert_eq!(
            infos[0].description,
            "Check not executable, due to missing prerequisites"
        );
    }

    #[test]
    fn a_class_finding_is_placed_in_its_method_and_the_class() {
        let objects = parse_worklist(WORKLIST).unwrap();
        assert_eq!(objects.len(), 1);
        let findings = &objects[0].findings;
        assert_eq!(findings.len(), 2);

        let object_level = &findings[0];
        assert_eq!(
            object_level.uri.as_deref(),
            Some("/sap/bc/adt/oo/classes/zsample_atchk")
        );
        assert_eq!(object_level.line, Some(1));

        let text = &findings[1];
        assert_eq!(text.check, "Extended Program Check (SLIN)");
        assert!(text.message.contains("'No clients found!'"));
        assert_eq!(text.method.as_deref(), Some("RUN"));
        assert_eq!(text.method_line, Some(13));
        assert_eq!(text.line, None, "absolute only once the source is read");
        assert!(text.documentation_uri.is_some());

        let start = method_start(CLASS, "RUN").unwrap();
        assert_eq!(start, 7);
        let line = start + 13 - 1;
        assert_eq!(
            CLASS.lines().nth(line as usize - 1).unwrap().trim(),
            "MESSAGE 'No clients found!' TYPE 'I'."
        );
    }

    #[test]
    fn an_interface_method_keeps_its_tilde() {
        let mut finding = AtcFinding {
            priority: 3,
            check: String::new(),
            message: String::new(),
            uri: None,
            line: None,
            method: None,
            method_line: None,
            documentation: None,
            documentation_uri: None,
        };
        apply_location(
            &mut finding,
            "/sap/bc/adt/oo/classes/zsample/source/main#type=CLAS%2FOM;name=IF_OO_ADT_CLASSRUN%7eMAIN;start=51",
        );
        assert_eq!(finding.method.as_deref(), Some("IF_OO_ADT_CLASSRUN~MAIN"));
        let source = "CLASS zsample IMPLEMENTATION.\n  METHOD if_oo_adt_classrun~main.\n";
        assert_eq!(method_start(source, "IF_OO_ADT_CLASSRUN~MAIN"), Some(2));
    }

    #[test]
    fn every_object_asked_about_is_listed_and_failures_fail() {
        let targets = [class_target("ZSAMPLE_ATCHK"), class_target("ZSAMPLE_CLEAN")];
        let objects = parse_worklist(WORKLIST).unwrap();
        let summary = summarize(
            "DEFAULT".to_owned(),
            &targets,
            objects.clone(),
            Vec::new(),
            Vec::new(),
            2,
        );
        assert_eq!(summary.objects.len(), 2);
        assert!(summary.objects[1].findings.is_empty());
        assert_eq!(
            (summary.priority_1, summary.priority_2, summary.priority_3),
            (0, 0, 2)
        );
        assert!(summary.passed, "priority 3 does not fail by default");

        let strict = summarize(
            "DEFAULT".to_owned(),
            &targets,
            objects.clone(),
            Vec::new(),
            Vec::new(),
            3,
        );
        assert!(!strict.passed);

        let incomplete = summarize(
            "DEFAULT".to_owned(),
            &targets,
            Vec::new(),
            parse_run(RUN_TOOL_FAILURE).unwrap(),
            Vec::new(),
            2,
        );
        assert!(!incomplete.complete);
        assert!(
            !incomplete.passed,
            "nothing found is not clean when a check did not run"
        );
    }

    #[test]
    fn documentation_reads_as_text() {
        let text = html_to_text(DOCUMENTATION);
        assert!(
            text.contains("Finding can be suppressed with pragma ##NO_TEXT"),
            "{text}"
        );
        assert!(!text.contains('<'), "{text}");
    }
}

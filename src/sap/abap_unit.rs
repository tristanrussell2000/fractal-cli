//! Running ABAP Unit tests over ADT, and judging the result.
//!
//! One `POST /sap/bc/adt/abapunit/testruns` names the objects; SAP runs every
//! test inside that request and answers with a tree:
//!
//! ```text
//! runResult > program > testClasses > testClass > testMethods > testMethod
//! ```
//!
//! An alert can hang at any level, and where it hangs matters: on a method it
//! is a failed assertion or an exception; on a test class, `class_setup` or
//! `setup` failed and the class's methods may not be listed at all; on a
//! program, it does not compile (`hasSyntaxErrors`). So **green is not "no
//! failures"**: it needs at least one method to have passed, nothing skipped,
//! every named target run, nothing outside a selection run, and no blocking
//! alert anywhere. Every way of being unsure is not green: a false red costs a
//! look, a false green lets broken code through.
//!
//! Traps:
//! - A nonexistent object answers 200 with an empty result, the same bytes as
//!   "no tests". Every target's existence is checked first.
//! - SAP can list one method twice (a class over its duration limit lists each
//!   method again, as an `abortion` echoing the class's tolerable alert).
//!   Methods are merged by name, and a tolerable alert on every method of a
//!   class is reported once.
//! - A skipped test (`cl_abap_unit_assert=>skip`) is a tolerable `abortion`
//!   on that one method, the same kind as the duration echo. It is told apart
//!   by being on one method rather than class-wide, never by SAP's wording.
//! - Selecting a local test class or method is done with the workbench URIs
//!   SAP itself returns as `navigationUri`. The result is checked against the
//!   selection, so SAP ignoring it can never read as a pass.
//!
//! It runs code as the connection's user: `dangerous` tests may change data,
//! `critical` ones system settings, which is why critical and long tests only
//! run when asked for.

use std::collections::BTreeMap;
use std::time::Duration;

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use thiserror::Error;

use super::{
    adt_response::parse_adt_document,
    client::{SapClient, SapClientError},
    find_attribute_value,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const TESTRUNS_PATH: &str = "/sap/bc/adt/abapunit/testruns";
const RUN_CONFIGURATION: &str = "application/vnd.sap.adt.abapunit.testruns.config.v4+xml";
/// The v4 configuration is refused (406) unless the answer is v2.
const RUN_RESULT: &str = "application/vnd.sap.adt.abapunit.testruns.result.v2+xml";
/// A local class or method name is at most 30 characters.
const LOCAL_NAME_MAX: usize = 30;
/// What each list in a summary is cut to; totals are never cut.
pub const LIST_CAP: usize = 200;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitTargetType {
    Class,
    Program,
    FunctionGroup,
    Package,
}

impl UnitTargetType {
    /// Parses `CLAS`, `PROG`, `FUGR` or `DEVC`.
    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_uppercase().as_str() {
            "CLAS" => Some(Self::Class),
            "PROG" => Some(Self::Program),
            "FUGR" => Some(Self::FunctionGroup),
            "DEVC" => Some(Self::Package),
            _ => None,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Class => "CLAS",
            Self::Program => "PROG",
            Self::FunctionGroup => "FUGR",
            Self::Package => "DEVC",
        }
    }

    const fn collection(self) -> &'static str {
        match self {
            Self::Class => "/sap/bc/adt/oo/classes",
            Self::Program => "/sap/bc/adt/programs/programs",
            Self::FunctionGroup => "/sap/bc/adt/functions/groups",
            Self::Package => "/sap/bc/adt/packages",
        }
    }
}

/// One thing to test: an object, or for a class, one local test class and
/// optionally some of its methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitTarget {
    pub target_type: UnitTargetType,
    pub name: String,
    pub test_class: Option<String>,
    pub methods: Vec<String>,
}

impl UnitTarget {
    fn uri(&self) -> String {
        format!(
            "{}/{}",
            self.target_type.collection(),
            self.name
                .to_ascii_lowercase()
                .replace('/', "%2f")
                .replace('$', "%24")
        )
    }

    /// How the target reads in a result: `CLAS ZCL_X`, `CLAS ZCL_X LTC_Y`,
    /// `CLAS ZCL_X LTC_Y=>A,B`.
    #[must_use]
    pub fn describe(&self) -> String {
        let base = format!("{} {}", self.target_type.as_str(), self.name);
        match &self.test_class {
            None => base,
            Some(class) if self.methods.is_empty() => format!("{base} {class}"),
            Some(class) => format!("{base} {class}=>{}", self.methods.join(",")),
        }
    }

    /// What goes into the run's object set: the object, the selected local
    /// test class, or one reference per selected method, in the workbench
    /// form SAP returns as each class's and method's `navigationUri`.
    fn run_uris(&self) -> Vec<String> {
        let Some(class) = &self.test_class else {
            return vec![self.uri()];
        };
        let base = format!("{}/includes/testclasses", self.uri());
        if self.methods.is_empty() {
            return vec![format!("{base}#type=CLAS%2FOCL;name={class}")];
        }
        self.methods
            .iter()
            .map(|method| {
                let padded = format!("{class:<LOCAL_NAME_MAX$}{method}").replace(' ', "%20");
                format!("{base}#type=CLAS%2FOLD;name={padded}")
            })
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum RiskLevel {
    Harmless,
    Dangerous,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TestDuration {
    Short,
    Medium,
    Long,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitRunRequest {
    pub targets: Vec<UnitTarget>,
    pub risk_levels: Vec<RiskLevel>,
    pub durations: Vec<TestDuration>,
    pub timeout: Duration,
}

/// Where an alert points: the include's URI and line, which `edit read
/// --include` opens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitLocation {
    pub uri: String,
    pub line: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnitAlert {
    pub kind: String,
    pub severity: String,
    pub title: String,
    pub details: Vec<String>,
    pub location: Option<UnitLocation>,
    /// The program does not compile, so its tests were never looked for.
    pub syntax_errors: bool,
}

impl UnitAlert {
    /// A tolerable alert is a warning; critical and fatal ones fail, and so
    /// does anything without a recognisable severity, or a program that does
    /// not compile whatever severity SAP gave it.
    fn is_blocking(&self) -> bool {
        self.syntax_errors || !self.severity.eq_ignore_ascii_case("tolerable")
    }

    fn same_message(&self, other: &Self) -> bool {
        self.severity.eq_ignore_ascii_case(&other.severity)
            && self.title == other.title
            && self.details == other.details
    }
}

/// A method-level alert, with where it is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MethodAlert {
    pub program: String,
    pub test_class: String,
    pub method: String,
    pub alert: UnitAlert,
}

/// An alert above method level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScopedAlert {
    /// `run`, `program` or `class`.
    pub level: &'static str,
    pub program: Option<String>,
    pub test_class: Option<String>,
    pub alert: UnitAlert,
    /// Class level only: the same tolerable alert SAP also put on this many of
    /// the class's methods, reported once here instead.
    pub repeated_on_methods: Option<usize>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitTotals {
    pub programs: usize,
    pub test_classes: usize,
    pub methods: usize,
    pub passed: usize,
    pub failed: usize,
    pub errors: usize,
    pub skipped: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UnitRunSummary {
    pub green: bool,
    pub totals: UnitTotals,
    /// Failed assertions (`failed`) and exceptions (`errors`) in methods.
    pub failures: Vec<MethodAlert>,
    /// Alerts above method level, blocking or folded from every method.
    pub other_alerts: Vec<ScopedAlert>,
    /// Tolerable alerts on methods that passed.
    pub warnings: Vec<MethodAlert>,
    pub skipped: Vec<MethodAlert>,
    /// Named targets, or selected test classes or methods, that did not run.
    pub untested_targets: Vec<String>,
    /// Test classes or methods a selection run ran but did not ask for: SAP
    /// did not apply the selection.
    pub outside_selection: Vec<String>,
    /// program → test class → methods passed.
    pub passed_by_class: BTreeMap<String, BTreeMap<String, usize>>,
    /// How many entries each list lost to [`LIST_CAP`].
    pub omitted: BTreeMap<&'static str, usize>,
    pub notes: Vec<String>,
}

#[derive(Debug, Error)]
pub enum UnitTestError {
    #[error("{0}")]
    InvalidTarget(String),
    #[error("{target} does not exist")]
    TargetNotFound { target: String },
    #[error("could not check that {target} exists: {source}")]
    TargetUnreadable {
        target: String,
        #[source]
        source: SapClientError,
    },
    #[error("{target}: {what} is not a test {kind} of the class (it has: {})", available.join(", "))]
    SelectionUnknown {
        target: String,
        what: String,
        kind: &'static str,
        available: Vec<String>,
    },
    #[error("could not read the test classes of {target}: {reason}")]
    StructureUnreadable { target: String, reason: String },
    #[error("the test run failed: {0}")]
    Request(#[source] SapClientError),
    #[error("SAP's test result could not be parsed: {0}")]
    ResultInvalid(String),
}

impl ReportableError for UnitTestError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidTarget(_) => "unit_test_invalid_target",
            Self::TargetNotFound { .. } => "unit_test_target_not_found",
            Self::TargetUnreadable { .. } => "unit_test_target_unreadable",
            Self::SelectionUnknown { .. } => "unit_test_selection_unknown",
            Self::StructureUnreadable { .. } => "unit_test_structure_unreadable",
            Self::Request(_) => "unit_test_request_failed",
            Self::ResultInvalid(_) => "unit_test_result_invalid",
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::TargetUnreadable { source, .. } | Self::Request(source) => {
                sap_http_status(Some(source))
            }
            _ => None,
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::InvalidTarget(_) => "No tests were run.".to_owned(),
            Self::TargetNotFound { .. } => {
                "No tests were run. SAP answers a missing object exactly like one without tests, so this is checked first; check the name.".to_owned()
            }
            Self::TargetUnreadable { .. } | Self::StructureUnreadable { .. } => {
                "No tests were run.".to_owned()
            }
            Self::SelectionUnknown { .. } => {
                "No tests were run. Name a test class and methods from the list.".to_owned()
            }
            Self::Request(SapClientError::Network { .. }) => {
                "The tests may still be running in SAP. A large class can take minutes: raise --timeout, or run one test class at a time with --test-class.".to_owned()
            }
            Self::Request(source) => return source.hint(),
            Self::ResultInvalid(_) => {
                "The tests ran, but their result could not be read, so this is not a pass.".to_owned()
            }
        })
    }
}

/// Runs the tests and judges the result.
///
/// # Errors
///
/// Returns [`UnitTestError`] for a malformed target or selection, a target
/// that does not exist or cannot be checked, a selection naming a test class
/// or method the class does not have, a failed request, or a result that
/// cannot be parsed. A run that completes but is not green is a result, not
/// an error.
pub async fn run_unit_tests(
    sap: &mut SapClient,
    request: &UnitRunRequest,
) -> Result<UnitRunSummary, UnitTestError> {
    validate(&request.targets)?;
    for target in &request.targets {
        ensure_exists(sap, target).await?;
        if target.test_class.is_some() {
            check_selection(sap, target).await?;
        }
    }

    let body = run_configuration(request);
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static(RUN_CONFIGURATION));
    headers.insert(ACCEPT, HeaderValue::from_static(RUN_RESULT));
    let xml = sap
        .post_text_with_timeout(TESTRUNS_PATH, &[], Some(&body), headers, request.timeout)
        .await
        .map_err(UnitTestError::Request)?;
    summarize(&xml, &request.targets)
}

fn validate(targets: &[UnitTarget]) -> Result<(), UnitTestError> {
    let bad = |message: String| Err(UnitTestError::InvalidTarget(message));
    if targets.is_empty() {
        return bad("name at least one object to test".to_owned());
    }
    for target in targets {
        let name_ok = !target.name.is_empty()
            && target.name.len() <= 40
            && target
                .name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '$'));
        if !name_ok {
            return bad(format!("invalid object name '{}'", target.name));
        }
        let local_ok = |name: &str| {
            !name.is_empty()
                && name.len() <= LOCAL_NAME_MAX
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        match &target.test_class {
            None if !target.methods.is_empty() => {
                return bad(format!(
                    "{}: --method needs --test-class, the local test class it belongs to",
                    target.describe()
                ));
            }
            Some(_) if target.target_type != UnitTargetType::Class => {
                return bad(format!(
                    "{}: --test-class works on a class only",
                    target.describe()
                ));
            }
            Some(class) if !local_ok(class) => {
                return bad(format!("invalid test class name '{class}'"));
            }
            _ => {}
        }
        let mut seen = Vec::new();
        for method in &target.methods {
            if !local_ok(method) {
                return bad(format!("invalid test method name '{method}'"));
            }
            if seen.contains(&method) {
                return bad(format!("test method {method} is named twice"));
            }
            seen.push(method);
        }
    }
    // A package run would run the selected class whole, which the result
    // check could not tell apart from SAP ignoring the selection.
    if targets.iter().any(|t| t.test_class.is_some())
        && targets
            .iter()
            .any(|t| t.target_type == UnitTargetType::Package)
    {
        return bad(
            "a test-class selection cannot share a run with a package: the package may contain the class and run all of it. Run them separately".to_owned(),
        );
    }
    Ok(())
}

async fn ensure_exists(sap: &SapClient, target: &UnitTarget) -> Result<(), UnitTestError> {
    match sap.get_text(&target.uri()).await {
        Ok(_) => Ok(()),
        Err(error) if error.is_not_found() => Err(UnitTestError::TargetNotFound {
            target: format!("{} {}", target.target_type.as_str(), target.name),
        }),
        Err(source) => Err(UnitTestError::TargetUnreadable {
            target: format!("{} {}", target.target_type.as_str(), target.name),
            source,
        }),
    }
}

/// Refuses a selection naming a test class or method the class does not
/// have, listing the ones it does.
async fn check_selection(sap: &SapClient, target: &UnitTarget) -> Result<(), UnitTestError> {
    let unreadable = |reason: String| UnitTestError::StructureUnreadable {
        target: target.describe(),
        reason,
    };
    let xml = sap
        .get_text(&format!("{}/objectstructure", target.uri()))
        .await
        .map_err(|error| unreadable(error.to_string()))?;
    let structure = test_structure(&xml).map_err(unreadable)?;
    let Some(class) = &target.test_class else {
        return Ok(());
    };
    let Some(methods) = structure.get(class) else {
        return Err(UnitTestError::SelectionUnknown {
            target: format!("CLAS {}", target.name),
            what: class.clone(),
            kind: "class",
            available: structure.keys().cloned().collect(),
        });
    };
    if let Some(missing) = target
        .methods
        .iter()
        .find(|method| !methods.contains(method))
    {
        return Err(UnitTestError::SelectionUnknown {
            target: format!("CLAS {} {class}", target.name),
            what: missing.clone(),
            kind: "method",
            available: methods.clone(),
        });
    }
    Ok(())
}

/// Each local test class (`CLAS/OCL testclass="true"`) and its test methods
/// (`CLAS/OLD testmethod="true"`), from a class's object structure.
fn test_structure(xml: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let document = parse_adt_document(xml).map_err(|error| error.to_string())?;
    let mut classes: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for node in document.descendants().filter(|node| node.is_element()) {
        let kind = find_attribute_value(node, "type").unwrap_or_default();
        let name = find_attribute_value(node, "name")
            .unwrap_or_default()
            .to_ascii_uppercase();
        if kind == "CLAS/OCL" && find_attribute_value(node, "testclass") == Some("true") {
            classes.entry(name).or_default();
        } else if kind == "CLAS/OLD" && find_attribute_value(node, "testmethod") == Some("true") {
            let owner = find_attribute_value(node, "clif_name")
                .unwrap_or_default()
                .to_ascii_uppercase();
            classes.entry(owner).or_default().push(name);
        }
    }
    Ok(classes)
}

fn run_configuration(request: &UnitRunRequest) -> String {
    let flag = |on: bool| if on { "true" } else { "false" };
    let risk = |level| flag(request.risk_levels.contains(&level));
    let duration = |length| flag(request.durations.contains(&length));
    let references: String = request
        .targets
        .iter()
        .flat_map(UnitTarget::run_uris)
        .collect::<Vec<_>>()
        .into_iter()
        .fold(Vec::<String>::new(), |mut unique, uri| {
            if !unique.contains(&uri) {
                unique.push(uri);
            }
            unique
        })
        .iter()
        .map(|uri| format!("<adtcore:objectReference adtcore:uri=\"{}\"/>", escape(uri)))
        .collect();
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\
<aunit:runConfiguration xmlns:aunit=\"http://www.sap.com/adt/aunit\">\
<external><coverage active=\"false\"/></external>\
<options>\
<uriType value=\"semantic\"/>\
<testDeterminationStrategy sameProgram=\"true\" assignedTests=\"false\" appendAssignedTestsPreview=\"true\"/>\
<testRiskLevels harmless=\"{}\" dangerous=\"{}\" critical=\"{}\"/>\
<testDurations short=\"{}\" medium=\"{}\" long=\"{}\"/>\
<withNavigationUri enabled=\"true\"/>\
</options>\
<adtcore:objectSets xmlns:adtcore=\"http://www.sap.com/adt/core\">\
<objectSet kind=\"inclusive\"><adtcore:objectReferences>{references}</adtcore:objectReferences></objectSet>\
</adtcore:objectSets>\
</aunit:runConfiguration>",
        risk(RiskLevel::Harmless),
        risk(RiskLevel::Dangerous),
        risk(RiskLevel::Critical),
        duration(TestDuration::Short),
        duration(TestDuration::Medium),
        duration(TestDuration::Long),
    )
}

fn escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('"', "&quot;")
}

// ---------------------------------------------------------------------------
// The result
// ---------------------------------------------------------------------------

struct Method {
    name: String,
    alerts: Vec<UnitAlert>,
}

struct Class {
    name: String,
    alerts: Vec<UnitAlert>,
    methods: Vec<Method>,
}

struct Program {
    name: String,
    alerts: Vec<UnitAlert>,
    classes: Vec<Class>,
}

fn children<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &'a str,
) -> impl Iterator<Item = roxmltree::Node<'a, 'i>> + 'a {
    node.children()
        .filter(move |child| child.is_element() && child.tag_name().name() == name)
}

/// The alerts directly under `<alerts>` of this node, not of its descendants.
fn alerts_of(node: roxmltree::Node) -> Vec<UnitAlert> {
    children(node, "alerts")
        .flat_map(|alerts| children(alerts, "alert").collect::<Vec<_>>())
        .map(parse_alert)
        .collect()
}

fn parse_alert(node: roxmltree::Node) -> UnitAlert {
    let title = children(node, "title")
        .next()
        .and_then(|title| title.text())
        .unwrap_or_default()
        .trim()
        .to_owned();
    let mut details = Vec::new();
    if let Some(list) = children(node, "details").next() {
        flatten_details(list, 0, &mut details);
    }
    let location = children(node, "stack")
        .next()
        .and_then(|stack| children(stack, "stackEntry").next())
        .and_then(|entry| find_attribute_value(entry, "uri"))
        .map(|uri| {
            let (base, fragment) = uri.split_once('#').unwrap_or((uri, ""));
            let line = fragment
                .split(';')
                .find_map(|part| part.strip_prefix("start="))
                .and_then(|start| start.split(',').next())
                .and_then(|line| line.parse().ok());
            UnitLocation {
                uri: base.to_owned(),
                line,
            }
        });
    UnitAlert {
        kind: find_attribute_value(node, "kind")
            .unwrap_or("unknown")
            .to_owned(),
        severity: find_attribute_value(node, "severity")
            .unwrap_or("unknown")
            .to_owned(),
        title,
        details,
        location,
        syntax_errors: find_attribute_value(node, "hasSyntaxErrors") == Some("true"),
    }
}

fn flatten_details(list: roxmltree::Node, depth: usize, out: &mut Vec<String>) {
    for detail in children(list, "detail") {
        if let Some(text) = find_attribute_value(detail, "text").map(str::trim)
            && !text.is_empty()
        {
            out.push(format!("{}{text}", "  ".repeat(depth)));
        }
        if let Some(nested) = children(detail, "details").next() {
            flatten_details(nested, depth + 1, out);
        }
    }
}

fn name_of(node: roxmltree::Node) -> String {
    find_attribute_value(node, "name")
        .unwrap_or_default()
        .to_ascii_uppercase()
}

/// Parses the run result, merging classes and methods SAP listed twice.
fn parse_result(xml: &str) -> Result<(Vec<UnitAlert>, Vec<Program>), String> {
    let document = parse_adt_document(xml).map_err(|error| error.to_string())?;
    let root = document.root_element();
    if root.tag_name().name() != "runResult" {
        return Err(format!(
            "expected runResult, found {}",
            root.tag_name().name()
        ));
    }
    let mut programs: Vec<Program> = Vec::new();
    for node in children(root, "program") {
        let mut program = Program {
            name: name_of(node),
            alerts: alerts_of(node),
            classes: Vec::new(),
        };
        for class_node in children(node, "testClasses")
            .flat_map(|list| children(list, "testClass").collect::<Vec<_>>())
        {
            let name = name_of(class_node);
            let index = match program
                .classes
                .iter()
                .position(|class| class.name == name && !name.is_empty())
            {
                Some(index) => index,
                None => {
                    program.classes.push(Class {
                        name: name.clone(),
                        alerts: Vec::new(),
                        methods: Vec::new(),
                    });
                    program.classes.len() - 1
                }
            };
            let class = &mut program.classes[index];
            add_alerts(&mut class.alerts, alerts_of(class_node));
            for method_node in children(class_node, "testMethods")
                .flat_map(|list| children(list, "testMethod").collect::<Vec<_>>())
            {
                let method_name = name_of(method_node);
                let alerts = alerts_of(method_node);
                match class
                    .methods
                    .iter_mut()
                    .find(|method| method.name == method_name && !method_name.is_empty())
                {
                    Some(method) => add_alerts(&mut method.alerts, alerts),
                    None => class.methods.push(Method {
                        name: method_name,
                        alerts,
                    }),
                }
            }
        }
        programs.push(program);
    }
    Ok((alerts_of(root), programs))
}

fn add_alerts(into: &mut Vec<UnitAlert>, more: Vec<UnitAlert>) {
    for alert in more {
        if !into.contains(&alert) {
            into.push(alert);
        }
    }
}

/// Tolerable alerts on every method of a class, reported once at class level.
///
/// Applies when the class carries the same message itself, or has at least
/// two methods carrying it. A single method's tolerable alert stays its own,
/// which is how a skip on one method stays a skip.
fn class_wide(class: &Class) -> Vec<UnitAlert> {
    let Some(first) = class.methods.first() else {
        return Vec::new();
    };
    first
        .alerts
        .iter()
        .filter(|alert| !alert.is_blocking())
        .filter(|alert| {
            class
                .methods
                .iter()
                .all(|method| method.alerts.iter().any(|other| other.same_message(alert)))
        })
        .filter(|alert| {
            class.methods.len() >= 2 || class.alerts.iter().any(|other| other.same_message(alert))
        })
        .cloned()
        .collect()
}

/// Judges a run result against what was asked for.
///
/// # Errors
///
/// Returns [`UnitTestError::ResultInvalid`] when the result is not a run
/// result at all.
pub fn summarize(xml: &str, targets: &[UnitTarget]) -> Result<UnitRunSummary, UnitTestError> {
    let (run_alerts, programs) = parse_result(xml).map_err(UnitTestError::ResultInvalid)?;
    let mut summary = UnitRunSummary::default();

    for alert in run_alerts {
        summary.other_alerts.push(ScopedAlert {
            level: "run",
            program: None,
            test_class: None,
            alert,
            repeated_on_methods: None,
        });
    }
    summary.totals.programs = programs.len();
    for program in &programs {
        for alert in &program.alerts {
            summary.other_alerts.push(ScopedAlert {
                level: "program",
                program: Some(program.name.clone()),
                test_class: None,
                alert: alert.clone(),
                repeated_on_methods: None,
            });
        }
        for class in &program.classes {
            summary.totals.test_classes += 1;
            let wide = class_wide(class);
            for alert in &class.alerts {
                let repeated = wide.iter().any(|other| other.same_message(alert));
                summary.other_alerts.push(ScopedAlert {
                    level: "class",
                    program: Some(program.name.clone()),
                    test_class: Some(class.name.clone()),
                    alert: alert.clone(),
                    repeated_on_methods: repeated.then_some(class.methods.len()),
                });
            }
            for alert in &wide {
                if !class.alerts.iter().any(|other| other.same_message(alert)) {
                    summary.other_alerts.push(ScopedAlert {
                        level: "class",
                        program: Some(program.name.clone()),
                        test_class: Some(class.name.clone()),
                        alert: alert.clone(),
                        repeated_on_methods: Some(class.methods.len()),
                    });
                }
            }
            for method in &class.methods {
                summary.totals.methods += 1;
                let at = |alert: &UnitAlert| MethodAlert {
                    program: program.name.clone(),
                    test_class: class.name.clone(),
                    method: method.name.clone(),
                    alert: alert.clone(),
                };
                let own: Vec<&UnitAlert> = method
                    .alerts
                    .iter()
                    .filter(|alert| !wide.iter().any(|other| other.same_message(alert)))
                    .collect();
                if let Some(blocking) = own.iter().find(|alert| alert.is_blocking()) {
                    if blocking.kind.eq_ignore_ascii_case("failedAssertion") {
                        summary.totals.failed += 1;
                    } else {
                        summary.totals.errors += 1;
                    }
                    summary.failures.push(at(blocking));
                } else if let Some(skip) = own
                    .iter()
                    .find(|alert| alert.kind.eq_ignore_ascii_case("abortion"))
                {
                    summary.totals.skipped += 1;
                    summary.skipped.push(at(skip));
                } else {
                    summary.totals.passed += 1;
                    *summary
                        .passed_by_class
                        .entry(program.name.clone())
                        .or_default()
                        .entry(class.name.clone())
                        .or_default() += 1;
                    for warning in own {
                        summary.warnings.push(at(warning));
                    }
                }
            }
        }
    }

    check_targets(&mut summary, &programs, targets);

    let blocking_above_methods = summary
        .other_alerts
        .iter()
        .any(|scoped| scoped.alert.is_blocking());
    summary.green = summary.totals.passed > 0
        && summary.totals.failed == 0
        && summary.totals.errors == 0
        && summary.totals.skipped == 0
        && !blocking_above_methods
        && summary.untested_targets.is_empty()
        && summary.outside_selection.is_empty();
    if summary.totals.methods == 0 {
        summary.notes.push("No test ran. An object without tests, or with only tests outside the chosen risk levels and durations, runs nothing.".to_owned());
    }
    cap(&mut summary);
    Ok(summary)
}

/// Named classes and programs that ran nothing, and for a selection, what ran
/// that was not asked for or was asked for and did not run.
fn check_targets(summary: &mut UnitRunSummary, programs: &[Program], targets: &[UnitTarget]) {
    for target in targets {
        if !matches!(
            target.target_type,
            UnitTargetType::Class | UnitTargetType::Program
        ) {
            continue;
        }
        let program = programs
            .iter()
            .find(|program| program.name.eq_ignore_ascii_case(&target.name));
        let Some(class_name) = &target.test_class else {
            let ran = program.is_some_and(|program| {
                program
                    .classes
                    .iter()
                    .any(|class| !class.methods.is_empty())
            });
            if !ran {
                summary.untested_targets.push(target.describe());
            }
            continue;
        };
        let Some(program) = program else {
            summary.untested_targets.push(target.describe());
            continue;
        };
        for class in &program.classes {
            if !class.name.eq_ignore_ascii_case(class_name) {
                summary
                    .outside_selection
                    .push(format!("CLAS {} {}", target.name, class.name));
                continue;
            }
            if target.methods.is_empty() {
                continue;
            }
            for method in &class.methods {
                if !target.methods.contains(&method.name) {
                    summary.outside_selection.push(format!(
                        "CLAS {} {}=>{}",
                        target.name, class.name, method.name
                    ));
                }
            }
        }
        let class = program
            .classes
            .iter()
            .find(|class| class.name.eq_ignore_ascii_case(class_name));
        match class {
            None => summary.untested_targets.push(target.describe()),
            Some(class) => {
                for method in &target.methods {
                    if !class.methods.iter().any(|ran| &ran.name == method) {
                        summary
                            .untested_targets
                            .push(format!("CLAS {} {class_name}=>{method}", target.name));
                    }
                }
            }
        }
    }
}

fn cap(summary: &mut UnitRunSummary) {
    fn cut<T>(list: &mut Vec<T>, name: &'static str, omitted: &mut BTreeMap<&'static str, usize>) {
        if list.len() > LIST_CAP {
            omitted.insert(name, list.len() - LIST_CAP);
            list.truncate(LIST_CAP);
        }
    }
    let mut omitted = BTreeMap::new();
    cut(&mut summary.failures, "failures", &mut omitted);
    cut(&mut summary.other_alerts, "other_alerts", &mut omitted);
    cut(&mut summary.warnings, "warnings", &mut omitted);
    cut(&mut summary.skipped, "skipped", &mut omitted);
    summary.omitted = omitted;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn class_target(test_class: Option<&str>, methods: &[&str]) -> UnitTarget {
        UnitTarget {
            target_type: UnitTargetType::Class,
            name: "ZSAMPLE_AU".to_owned(),
            test_class: test_class.map(str::to_owned),
            methods: methods.iter().map(|m| (*m).to_owned()).collect(),
        }
    }

    /// A live answer, renamed: one class with a passing, a failing, an
    /// exception and a skipped method, and a second whose class_setup raised.
    const MIXED: &str = r#"<?xml version="1.0" encoding="utf-8"?><aunit:runResult xmlns:aunit="http://www.sap.com/adt/aunit"><program adtcore:uri="/sap/bc/adt/oo/classes/zsample_au" adtcore:type="CLAS/OC" adtcore:name="ZSAMPLE_AU" xmlns:adtcore="http://www.sap.com/adt/core"><testClasses><testClass adtcore:name="LTC_ADD"><testMethods><testMethod adtcore:name="FAILS" executionTime="0.05"><alerts><alert kind="failedAssertion" severity="critical"><title>Critical Assertion Error: 'one plus two is not four'</title><details><detail text="Different values"><details><detail text="Expected [4] Actual [3]"/></details></detail></details><stack><stackEntry adtcore:uri="/sap/bc/adt/oo/classes/zsample_au/includes/testclasses#start=15,0" adtcore:description="Include: &lt;ZSAMPLE_AU=====CCAU&gt; Line: &lt;15&gt; (FAILS)"/></stack></alert></alerts></testMethod><testMethod adtcore:name="PASSES" executionTime="0"/><testMethod adtcore:name="RAISES" executionTime="0.01"><alerts><alert kind="exception" severity="critical"><title>Exception Error &lt;COMPUTE_INT_ZERODIVIDE&gt;</title><details><detail text="Division by zero"/></details><stack><stackEntry adtcore:uri="/sap/bc/adt/oo/classes/zsample_au/includes/testclasses#start=19,0"/></stack></alert></alerts></testMethod><testMethod adtcore:name="SKIPS" executionTime="0"><alerts><alert kind="abortion" severity="tolerable"><title>Missing Prerequisites - not ready yet</title><details><detail text="Test execution skipped due to missing prerequisites"/></details><stack><stackEntry adtcore:uri="/sap/bc/adt/oo/classes/zsample_au/includes/testclasses#start=22,0"/></stack></alert></alerts></testMethod></testMethods></testClass><testClass adtcore:name="LTC_SETUP"><alerts><alert kind="exception" severity="critical"><title>Exception Error &lt;UNCAUGHT_EXCEPTION&gt;</title><details><detail text="Test failed in CLASS_SETUP or CLASS_CONSTRUCTOR"/></details><stack><stackEntry adtcore:uri="/sap/bc/adt/oo/classes/zsample_au/includes/testclasses#start=35,0"/></stack></alert></alerts></testClass></testClasses></program></aunit:runResult>"#;

    const PASSING: &str = r#"<aunit:runResult xmlns:aunit="http://www.sap.com/adt/aunit"><program adtcore:name="ZSAMPLE_AU" xmlns:adtcore="http://www.sap.com/adt/core"><testClasses><testClass adtcore:name="LTC_ADD"><testMethods><testMethod adtcore:name="PASSES" executionTime="0"/><testMethod adtcore:name="ALSO_PASSES" executionTime="0"/></testMethods></testClass></testClasses></program></aunit:runResult>"#;

    #[test]
    fn a_mixed_run_counts_each_outcome_and_is_not_green() {
        let summary = summarize(MIXED, &[class_target(None, &[])]).unwrap();
        assert!(!summary.green);
        assert_eq!(
            summary.totals,
            UnitTotals {
                programs: 1,
                test_classes: 2,
                methods: 4,
                passed: 1,
                failed: 1,
                errors: 1,
                skipped: 1,
            }
        );
        let failure = &summary.failures[0];
        assert_eq!(failure.method, "FAILS");
        assert_eq!(
            failure.alert.details,
            ["Different values", "  Expected [4] Actual [3]"]
        );
        assert_eq!(
            failure.alert.location,
            Some(UnitLocation {
                uri: "/sap/bc/adt/oo/classes/zsample_au/includes/testclasses".to_owned(),
                line: Some(15),
            })
        );
        // The skip is its own outcome, not a pass and not a failure.
        assert_eq!(summary.skipped[0].method, "SKIPS");
        // class_setup failed: no methods listed, a blocking class-level alert.
        let setup = summary
            .other_alerts
            .iter()
            .find(|alert| alert.test_class.as_deref() == Some("LTC_SETUP"))
            .unwrap();
        assert_eq!(setup.level, "class");
        assert!(setup.alert.is_blocking());
    }

    #[test]
    fn a_clean_run_is_green() {
        let summary = summarize(PASSING, &[class_target(None, &[])]).unwrap();
        assert!(summary.green);
        assert_eq!(summary.totals.passed, 2);
        assert_eq!(summary.passed_by_class["ZSAMPLE_AU"]["LTC_ADD"], 2);
    }

    #[test]
    fn an_empty_result_is_never_green() {
        let summary = summarize(
            r#"<aunit:runResult xmlns:aunit="http://www.sap.com/adt/aunit"/>"#,
            &[class_target(None, &[])],
        )
        .unwrap();
        assert!(!summary.green);
        assert_eq!(summary.untested_targets, ["CLAS ZSAMPLE_AU"]);
        assert!(!summary.notes.is_empty());
    }

    #[test]
    fn a_method_listed_twice_is_one_method_and_a_class_wide_echo_is_one_alert() {
        // A class past its duration limit: the class carries a tolerable
        // warning, and every method is listed again carrying it as an
        // abortion.
        let xml = r#"<aunit:runResult xmlns:aunit="http://www.sap.com/adt/aunit"><program adtcore:name="ZSAMPLE_AU" xmlns:adtcore="http://www.sap.com/adt/core"><testClasses><testClass adtcore:name="LTC_SLOW"><alerts><alert kind="warning" severity="tolerable"><title>Maximum allowed duration exceeded</title></alert></alerts><testMethods><testMethod adtcore:name="ONE" executionTime="30"/><testMethod adtcore:name="TWO" executionTime="31"/><testMethod adtcore:name="ONE"><alerts><alert kind="abortion" severity="tolerable"><title>Maximum allowed duration exceeded</title></alert></alerts></testMethod><testMethod adtcore:name="TWO"><alerts><alert kind="abortion" severity="tolerable"><title>Maximum allowed duration exceeded</title></alert></alerts></testMethod></testMethods></testClass></testClasses></program></aunit:runResult>"#;
        let summary = summarize(xml, &[class_target(None, &[])]).unwrap();
        assert_eq!(summary.totals.methods, 2);
        assert_eq!(summary.totals.passed, 2);
        assert_eq!(summary.totals.skipped, 0);
        assert!(summary.warnings.is_empty());
        assert_eq!(summary.other_alerts.len(), 1);
        assert_eq!(summary.other_alerts[0].repeated_on_methods, Some(2));
        assert!(summary.green);
    }

    #[test]
    fn a_program_that_does_not_compile_blocks_whatever_its_severity() {
        let xml = r#"<aunit:runResult xmlns:aunit="http://www.sap.com/adt/aunit"><program adtcore:name="ZSAMPLE_AU" xmlns:adtcore="http://www.sap.com/adt/core"><alerts><alert kind="warning" severity="tolerable" hasSyntaxErrors="true"><title>The program has syntax errors</title></alert></alerts></program></aunit:runResult>"#;
        let summary = summarize(xml, &[class_target(None, &[])]).unwrap();
        assert!(!summary.green);
        assert!(summary.other_alerts[0].alert.syntax_errors);
    }

    #[test]
    fn a_selection_is_checked_against_what_ran() {
        // Asked for one method; SAP ran the whole class.
        let summary = summarize(MIXED, &[class_target(Some("LTC_ADD"), &["PASSES"])]).unwrap();
        assert!(!summary.green);
        assert!(
            summary
                .outside_selection
                .contains(&"CLAS ZSAMPLE_AU LTC_ADD=>FAILS".to_owned())
        );
        assert!(
            summary
                .outside_selection
                .contains(&"CLAS ZSAMPLE_AU LTC_SETUP".to_owned())
        );

        let only_passes = r#"<aunit:runResult xmlns:aunit="http://www.sap.com/adt/aunit"><program adtcore:name="ZSAMPLE_AU" xmlns:adtcore="http://www.sap.com/adt/core"><testClasses><testClass adtcore:name="LTC_ADD"><testMethods><testMethod adtcore:name="PASSES"/></testMethods></testClass></testClasses></program></aunit:runResult>"#;
        let summary = summarize(
            only_passes,
            &[class_target(Some("LTC_ADD"), &["PASSES", "GONE"])],
        )
        .unwrap();
        assert!(!summary.green);
        assert_eq!(summary.untested_targets, ["CLAS ZSAMPLE_AU LTC_ADD=>GONE"]);
    }

    #[test]
    fn selection_uris_are_the_ones_sap_returns_as_navigation_uris() {
        let uris = class_target(Some("LTC_ADD"), &["PASSES"]).run_uris();
        assert_eq!(
            uris,
            [
                "/sap/bc/adt/oo/classes/zsample_au/includes/testclasses#type=CLAS%2FOLD;name=LTC_ADD%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20%20PASSES"
            ]
        );
        assert_eq!(
            class_target(Some("LTC_ADD"), &[]).run_uris(),
            ["/sap/bc/adt/oo/classes/zsample_au/includes/testclasses#type=CLAS%2FOCL;name=LTC_ADD"]
        );
    }

    #[test]
    fn malformed_targets_are_refused_before_anything_runs() {
        let package = UnitTarget {
            target_type: UnitTargetType::Package,
            name: "ZSAMPLE_PKG".to_owned(),
            test_class: None,
            methods: Vec::new(),
        };
        for targets in [
            vec![],
            vec![class_target(None, &["PASSES"])],
            vec![UnitTarget {
                test_class: Some("LTC".to_owned()),
                ..package.clone()
            }],
            vec![class_target(Some("LTC"), &["A", "A"])],
            vec![class_target(Some("LTC"), &[]), package],
        ] {
            assert_eq!(
                validate(&targets).unwrap_err().code(),
                "unit_test_invalid_target",
                "{targets:?}"
            );
        }
    }

    #[test]
    fn reads_test_classes_and_methods_from_the_object_structure() {
        let xml = r#"<abapsource:objectStructureElement xmlns:abapsource="http://www.sap.com/adt/abapsource" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_AU" adtcore:type="CLAS/OC"><abapsource:objectStructureElement adtcore:name="LTC_ADD" testclass="true" adtcore:type="CLAS/OCL"/><abapsource:objectStructureElement adtcore:type="CLAS/OLD" adtcore:name="PASSES" testmethod="true" clif_name="LTC_ADD"/><abapsource:objectStructureElement adtcore:type="CLAS/OLD" adtcore:name="HELPER" clif_name="LTC_ADD"/></abapsource:objectStructureElement>"#;
        let structure = test_structure(xml).unwrap();
        assert_eq!(structure["LTC_ADD"], ["PASSES"]);
    }

    #[test]
    fn the_run_configuration_names_the_chosen_levels() {
        let body = run_configuration(&UnitRunRequest {
            targets: vec![class_target(None, &[])],
            risk_levels: vec![RiskLevel::Harmless, RiskLevel::Dangerous],
            durations: vec![TestDuration::Short, TestDuration::Medium],
            timeout: Duration::from_secs(300),
        });
        assert!(
            body.contains(r#"<testRiskLevels harmless="true" dangerous="true" critical="false"/>"#)
        );
        assert!(body.contains(r#"<testDurations short="true" medium="true" long="false"/>"#));
        assert!(body.contains(r#"adtcore:uri="/sap/bc/adt/oo/classes/zsample_au""#));
    }
}

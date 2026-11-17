use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::time::Duration;

use serde::Serialize;

use super::{connect, parse_object_spec};
use crate::{
    cli::{DurationArg, RiskArg, TestArgs},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::reportable_error::ReportableError;
use fractal::sap::abap_unit::{
    MethodAlert, RiskLevel, ScopedAlert, TestDuration, UnitAlert, UnitRunRequest, UnitRunSummary,
    UnitTarget, UnitTargetType, run_unit_tests,
};

#[derive(Debug, thiserror::Error)]
#[error("unknown test target type '{0}'")]
pub struct TargetTypeError(String);

impl ReportableError for TargetTypeError {
    fn code(&self) -> &'static str {
        "unit_test_invalid_target"
    }

    fn hint(&self) -> Option<String> {
        Some("Use CLAS, PROG, FUGR or DEVC.".to_owned())
    }
}

#[derive(Debug, Serialize)]
pub struct LocationOutput {
    uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    line: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct AlertOutput {
    kind: String,
    severity: String,
    title: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    details: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    location: Option<LocationOutput>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    syntax_errors: bool,
}

impl From<UnitAlert> for AlertOutput {
    fn from(alert: UnitAlert) -> Self {
        Self {
            kind: alert.kind,
            severity: alert.severity,
            title: alert.title,
            details: alert.details,
            location: alert.location.map(|location| LocationOutput {
                uri: location.uri,
                line: location.line,
            }),
            syntax_errors: alert.syntax_errors,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct MethodAlertOutput {
    program: String,
    test_class: String,
    method: String,
    #[serde(flatten)]
    alert: AlertOutput,
}

impl From<MethodAlert> for MethodAlertOutput {
    fn from(alert: MethodAlert) -> Self {
        Self {
            program: alert.program,
            test_class: alert.test_class,
            method: alert.method,
            alert: alert.alert.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ScopedAlertOutput {
    level: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    program: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    test_class: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeated_on_methods: Option<usize>,
    #[serde(flatten)]
    alert: AlertOutput,
}

impl From<ScopedAlert> for ScopedAlertOutput {
    fn from(alert: ScopedAlert) -> Self {
        Self {
            level: alert.level,
            program: alert.program,
            test_class: alert.test_class,
            repeated_on_methods: alert.repeated_on_methods,
            alert: alert.alert.into(),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct TotalsOutput {
    programs: usize,
    test_classes: usize,
    methods: usize,
    passed: usize,
    failed: usize,
    errors: usize,
    skipped: usize,
}

#[derive(Debug, Serialize)]
pub struct UnitTestOutput {
    ok: bool,
    profile: String,
    /// The verdict. The exit code is 0 only when this is true.
    green: bool,
    targets: Vec<String>,
    totals: TotalsOutput,
    failures: Vec<MethodAlertOutput>,
    other_alerts: Vec<ScopedAlertOutput>,
    warnings: Vec<MethodAlertOutput>,
    skipped: Vec<MethodAlertOutput>,
    untested_targets: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    outside_selection: Vec<String>,
    passed_by_class: BTreeMap<String, BTreeMap<String, usize>>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    omitted: BTreeMap<&'static str, usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    notes: Vec<String>,
}

pub async fn unit_test(
    explicit_profile: Option<&str>,
    args: &TestArgs,
) -> Result<UnitTestOutput, Reported> {
    let targets = targets(args)?;
    let request = UnitRunRequest {
        targets: targets.clone(),
        risk_levels: args
            .risk
            .iter()
            .map(|risk| match risk {
                RiskArg::Harmless => RiskLevel::Harmless,
                RiskArg::Dangerous => RiskLevel::Dangerous,
                RiskArg::Critical => RiskLevel::Critical,
            })
            .collect(),
        durations: args
            .duration
            .iter()
            .map(|duration| match duration {
                DurationArg::Short => TestDuration::Short,
                DurationArg::Medium => TestDuration::Medium,
                DurationArg::Long => TestDuration::Long,
            })
            .collect(),
        timeout: Duration::from_secs(args.timeout),
    };
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let summary = run_unit_tests(&mut client, &request).await?;
    Ok(map_summary(
        profile_name,
        targets.iter().map(UnitTarget::describe).collect(),
        summary,
    ))
}

fn targets(args: &TestArgs) -> Result<Vec<UnitTarget>, Reported> {
    let target = |object_type: &str, name: &str| -> Result<UnitTarget, Reported> {
        let target_type = UnitTargetType::parse(object_type)
            .ok_or_else(|| TargetTypeError(object_type.to_owned()))?;
        Ok(UnitTarget {
            target_type,
            name: name.trim().to_ascii_uppercase(),
            test_class: None,
            methods: Vec::new(),
        })
    };
    if !args.objects.is_empty() {
        return args
            .objects
            .iter()
            .map(|spec| {
                let (object_type, name) = parse_object_spec(spec)?;
                target(&object_type, &name)
            })
            .collect();
    }
    let (Some(object_type), Some(name)) = (&args.object_type, &args.name) else {
        // clap requires both unless --object is given.
        return Ok(Vec::new());
    };
    let mut single = target(object_type, name)?;
    single.test_class = args
        .test_class
        .as_deref()
        .map(|class| class.trim().to_ascii_uppercase());
    single.methods = args
        .methods
        .iter()
        .map(|method| method.trim().to_ascii_uppercase())
        .collect();
    Ok(vec![single])
}

fn map_summary(profile: String, targets: Vec<String>, summary: UnitRunSummary) -> UnitTestOutput {
    let totals = summary.totals;
    UnitTestOutput {
        ok: true,
        profile,
        green: summary.green,
        targets,
        totals: TotalsOutput {
            programs: totals.programs,
            test_classes: totals.test_classes,
            methods: totals.methods,
            passed: totals.passed,
            failed: totals.failed,
            errors: totals.errors,
            skipped: totals.skipped,
        },
        failures: summary.failures.into_iter().map(Into::into).collect(),
        other_alerts: summary.other_alerts.into_iter().map(Into::into).collect(),
        warnings: summary.warnings.into_iter().map(Into::into).collect(),
        skipped: summary.skipped.into_iter().map(Into::into).collect(),
        untested_targets: summary.untested_targets,
        outside_selection: summary.outside_selection,
        passed_by_class: summary.passed_by_class,
        omitted: summary.omitted,
        notes: summary.notes,
    }
}

/// 0 when green; 1 when the run completed and is not green. A run that could
/// not be done is an error, exit 2, decided before this is asked.
pub fn exit_code(result: &UnitTestOutput) -> i32 {
    i32::from(!result.green)
}

pub fn print_unit_test(result: &UnitTestOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    print!("{}", render(result));
}

fn render(result: &UnitTestOutput) -> String {
    let mut text = String::new();
    let totals = &result.totals;
    let _ = writeln!(
        text,
        "{}: {} passed, {} failed, {} errors, {} skipped ({} methods in {} test classes)",
        if result.green { "GREEN" } else { "NOT GREEN" },
        totals.passed,
        totals.failed,
        totals.errors,
        totals.skipped,
        totals.methods,
        totals.test_classes
    );
    let line = |text: &mut String, label: &str, alert: &AlertOutput, at: String| {
        let location = alert
            .location
            .as_ref()
            .map(|location| match location.line {
                Some(line) => format!(" ({}:{line})", location.uri),
                None => format!(" ({})", location.uri),
            })
            .unwrap_or_default();
        let _ = writeln!(text, "{label} {at}: {}{location}", alert.title);
        for detail in &alert.details {
            let _ = writeln!(text, "    {detail}");
        }
    };
    for failure in &result.failures {
        line(
            &mut text,
            "FAIL",
            &failure.alert,
            format!("{}=>{}", failure.test_class, failure.method),
        );
    }
    for alert in result
        .other_alerts
        .iter()
        .filter(|alert| alert.alert.severity != "tolerable" || alert.alert.syntax_errors)
    {
        let at = alert
            .test_class
            .clone()
            .or_else(|| alert.program.clone())
            .unwrap_or_else(|| "run".to_owned());
        line(&mut text, "BLOCKED", &alert.alert, at);
    }
    for skipped in &result.skipped {
        line(
            &mut text,
            "SKIP",
            &skipped.alert,
            format!("{}=>{}", skipped.test_class, skipped.method),
        );
    }
    for untested in &result.untested_targets {
        let _ = writeln!(text, "NOT RUN {untested}");
    }
    for outside in &result.outside_selection {
        let _ = writeln!(text, "OUTSIDE SELECTION {outside}");
    }
    for note in &result.notes {
        let _ = writeln!(text, "note: {note}");
    }
    text
}

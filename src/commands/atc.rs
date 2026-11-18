use std::fmt::Write as _;

use serde::Serialize;

use super::{connect, parse_object_spec};
use crate::{
    cli::AtcArgs,
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::sap::atc::{AtcRequest, AtcSummary, AtcTarget, run_atc};

#[derive(Debug, Serialize)]
pub struct AtcOutput {
    ok: bool,
    profile: String,
    targets: Vec<String>,
    /// The lowest priority that fails the run.
    fail_on: u8,
    #[serde(flatten)]
    summary: AtcSummary,
}

pub async fn atc(explicit_profile: Option<&str>, args: &AtcArgs) -> Result<AtcOutput, Reported> {
    let targets = targets(args)?;
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let summary = run_atc(
        &mut client,
        &AtcRequest {
            targets: targets.clone(),
            variant: args.variant.clone(),
            explain: args.explain,
            fail_on: args.fail_on,
        },
    )
    .await?;
    Ok(AtcOutput {
        ok: true,
        profile: profile_name,
        targets: targets.iter().map(AtcTarget::describe).collect(),
        fail_on: args.fail_on,
        summary,
    })
}

fn targets(args: &AtcArgs) -> Result<Vec<AtcTarget>, Reported> {
    if !args.objects.is_empty() {
        return args
            .objects
            .iter()
            .map(|spec| {
                let (object_type, name) = parse_object_spec(spec)?;
                Ok(AtcTarget::parse(&object_type, &name)?)
            })
            .collect();
    }
    let (Some(object_type), Some(name)) = (&args.object_type, &args.name) else {
        // clap requires both unless --object is given.
        return Ok(Vec::new());
    };
    Ok(vec![AtcTarget::parse(object_type, name)?])
}

/// 0 when the run passed; 1 when it found something at `fail_on` or above,
/// or a check could not run. A run that could not be done is exit 2.
pub fn exit_code(result: &AtcOutput) -> i32 {
    i32::from(!result.summary.passed)
}

pub fn print_atc(result: &AtcOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    print!("{}", render(result));
}

fn render(result: &AtcOutput) -> String {
    let summary = &result.summary;
    let mut text = String::new();
    let total = summary.priority_1 + summary.priority_2 + summary.priority_3;
    let _ = writeln!(
        text,
        "{}: ATC (variant {}) found {total} finding{} — priority 1: {}, priority 2: {}, priority 3: {}",
        if summary.passed {
            "PASSED"
        } else {
            "NOT PASSED"
        },
        summary.variant,
        if total == 1 { "" } else { "s" },
        summary.priority_1,
        summary.priority_2,
        summary.priority_3
    );
    for info in &summary.infos {
        let _ = writeln!(text, "{}: {}", info.kind, info.description);
    }
    for object in &summary.objects {
        let _ = writeln!(text);
        let _ = writeln!(text, "{} {}", object.object_type, object.name);
        if object.findings.is_empty() {
            let _ = writeln!(text, "  no findings");
        }
        for finding in &object.findings {
            let place = match (finding.line, &finding.method, finding.method_line) {
                (Some(line), Some(method), Some(method_line)) => {
                    format!("line {line} (method {method}, line {method_line})")
                }
                (None, Some(method), Some(method_line)) => {
                    format!("method {method}, line {method_line}")
                }
                (Some(line), _, _) => format!("line {line}"),
                _ => String::new(),
            };
            let _ = writeln!(text, "  P{}  {place}  {}", finding.priority, finding.check);
            let _ = writeln!(text, "      {}", finding.message);
            if let Some(uri) = &finding.uri {
                let _ = writeln!(
                    text,
                    "      {uri}{}",
                    finding
                        .line
                        .map_or_else(String::new, |line| format!("#start={line}"))
                );
            }
            if let Some(documentation) = &finding.documentation {
                for line in documentation.lines() {
                    let _ = writeln!(text, "      | {line}");
                }
            }
        }
    }
    for note in &summary.notes {
        let _ = writeln!(text);
        let _ = writeln!(text, "note: {note}");
    }
    text
}

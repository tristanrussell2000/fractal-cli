use std::fmt::Write as _;

use serde::Serialize;

use super::connect;
use crate::{
    cli::{TransportCreateArgs, TransportListArgs, TransportShowArgs},
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::sap::transport::{
    CreatedTransportRequest, TransportObject, TransportRequest, TransportRequestDetail,
    TransportStatusFilter, TransportTarget, create_transport_request, list_transport_requests,
    show_transport_request,
};

#[derive(Debug, Serialize)]
pub struct TransportTaskOutput {
    number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TransportRequestOutput {
    number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    tasks: Vec<TransportTaskOutput>,
}

#[derive(Debug, Serialize)]
pub struct TransportListOutput {
    ok: bool,
    profile: String,
    user: String,
    status_filter: String,
    total: usize,
    requests: Vec<TransportRequestOutput>,
}

/// # Errors
///
/// Returns [`Reported`] when the profile cannot be opened or SAP rejects the
/// transport-organizer request.
pub async fn transport_list(
    explicit_profile: Option<&str>,
    args: &TransportListArgs,
) -> Result<TransportListOutput, Reported> {
    let (profile_name, profile, client) = connect(explicit_profile).await?;
    let user = args
        .user
        .clone()
        .unwrap_or_else(|| profile.username.clone())
        .to_uppercase();
    let status = if args.released {
        TransportStatusFilter::Released
    } else {
        TransportStatusFilter::Modifiable
    };
    let requests = list_transport_requests(&client, &user, status).await?;

    Ok(TransportListOutput {
        ok: true,
        profile: profile_name,
        user,
        status_filter: if args.released {
            "released".to_owned()
        } else {
            "modifiable".to_owned()
        },
        total: requests.len(),
        requests: requests.into_iter().map(map_request).collect(),
    })
}

#[derive(Debug, Serialize)]
pub struct TransportCreateOutput {
    ok: bool,
    profile: String,
    status: String,
    number: String,
    description: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    package: Option<String>,
    /// The system this request transports to; absent means local.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
    next_step: String,
}

/// # Errors
///
/// Returns [`Reported`] when the description is blank or SAP rejects the
/// creation request.
pub async fn transport_create(
    explicit_profile: Option<&str>,
    args: &TransportCreateArgs,
) -> Result<TransportCreateOutput, Reported> {
    let (profile_name, profile, mut client) = connect(explicit_profile).await?;
    // The new request is found by listing the profile user's own requests, so
    // creation is scoped to whoever is logged in.
    let owner = profile.username.to_uppercase();
    let created =
        create_transport_request(&mut client, &args.description, &args.package, &owner).await?;
    Ok(map_created_request(profile_name, created))
}

pub fn print_transport_create(result: &TransportCreateOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    let mut readable = String::new();
    let _ = writeln!(readable, "profile: {}", result.profile);
    let _ = writeln!(readable, "created: {}", result.number);
    let _ = writeln!(readable, "description: {}", result.description);
    if let Some(package) = &result.package {
        let _ = writeln!(readable, "package: {package}");
    }
    let _ = writeln!(
        readable,
        "target: {}",
        result.target.as_deref().unwrap_or("(none)")
    );
    if let Some(warning) = &result.warning {
        let _ = writeln!(readable, "warning: {warning}");
    }
    let _ = writeln!(readable, "next: {}", result.next_step);
    print!("{readable}");
}

fn map_created_request(profile: String, created: CreatedTransportRequest) -> TransportCreateOutput {
    let next_step = format!("fractal edit create --transport {} ...", created.number);
    // A local request is reported, not refused: it already exists, and it can
    // still hold objects. What it cannot do is ever be released, and nothing
    // else in the workflow would say so until the release failed.
    let warning = match &created.target {
        TransportTarget::System(_) => None,
        TransportTarget::Local => Some(format!(
            "SAP filed this request as local: it has no transport target and can never be released. \
             It derives the target from the package's transport layer, so {} has a layer that does \
             not route on this system. Use a package whose layer does if this request needs to move.",
            created.package.as_deref().unwrap_or("that package")
        )),
        // Not the same claim: the request was created but never seen, so
        // whether it has a target is unknown rather than known to be absent.
        TransportTarget::Unknown => Some(
            "The request was created but did not appear in your modifiable requests, so whether it \
             has a transport target is unknown. Run `fractal transport list` to check before \
             putting objects into it."
                .to_owned(),
        ),
    };
    TransportCreateOutput {
        ok: true,
        profile,
        status: "created".to_owned(),
        number: created.number,
        description: created.description,
        package: created.package,
        target: created.target.system().map(str::to_owned),
        warning,
        next_step,
    }
}

#[derive(Debug, Serialize)]
pub struct TransportObjectOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    program_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    object_type: Option<String>,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    workbench_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct TransportTaskDetailOutput {
    number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_type: Option<String>,
    objects: Vec<TransportObjectOutput>,
}

#[derive(Debug, Serialize)]
pub struct TransportShowOutput {
    ok: bool,
    profile: String,
    number: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    owner: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    total_objects: usize,
    objects: Vec<TransportObjectOutput>,
    tasks: Vec<TransportTaskDetailOutput>,
    /// Set when SAP answered with a different request than the one asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

/// # Errors
///
/// Returns [`Reported`] when the profile cannot be opened, the number is blank
/// or unknown, or SAP's response cannot be read.
pub async fn transport_show(
    explicit_profile: Option<&str>,
    args: &TransportShowArgs,
) -> Result<TransportShowOutput, Reported> {
    let (profile_name, _profile, client) = connect(explicit_profile).await?;
    let detail = show_transport_request(&client, &args.number).await?;
    Ok(map_request_detail(profile_name, &args.number, detail))
}

fn map_request_detail(
    profile: String,
    requested: &str,
    detail: TransportRequestDetail,
) -> TransportShowOutput {
    // SAP resolves a task number to its parent silently, so the number in the
    // answer is not always the number that was asked for. Saying so is the
    // difference between "this is your request" and "this is the request your
    // task belongs to".
    let requested = requested.trim().to_uppercase();
    let note = (requested != detail.number).then(|| {
        format!(
            "{requested} is a task; SAP answered with its parent request {}.",
            detail.number
        )
    });
    TransportShowOutput {
        ok: true,
        profile,
        number: detail.number,
        description: detail.description,
        owner: detail.owner,
        status: detail.status_text.or(detail.status),
        target: detail.target,
        total_objects: detail.objects.len(),
        objects: detail.objects.into_iter().map(map_object).collect(),
        tasks: detail
            .tasks
            .into_iter()
            .map(|task| TransportTaskDetailOutput {
                number: task.number,
                description: task.description,
                owner: task.owner,
                status: task.status_text.or(task.status),
                task_type: task.task_type,
                objects: task.objects.into_iter().map(map_object).collect(),
            })
            .collect(),
        note,
    }
}

fn map_object(object: TransportObject) -> TransportObjectOutput {
    TransportObjectOutput {
        program_id: object.program_id,
        object_type: object.object_type,
        name: object.name,
        workbench_type: object.workbench_type,
    }
}

pub fn print_transport_show(result: &TransportShowOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_transport_show_readable(result));
}

fn render_transport_show_readable(result: &TransportShowOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    if let Some(note) = &result.note {
        let _ = writeln!(output, "note: {note}");
    }
    let _ = writeln!(
        output,
        "{} {}",
        result.number,
        result.description.as_deref().unwrap_or("(no description)")
    );
    let _ = writeln!(
        output,
        "owner: {}   status: {}   target: {}",
        result.owner.as_deref().unwrap_or("(unknown)"),
        result.status.as_deref().unwrap_or("(unknown)"),
        // No target means local: it can hold objects but never be released.
        result.target.as_deref().unwrap_or("(none - local request)")
    );
    let _ = writeln!(output, "{} object(s)", result.total_objects);
    for object in &result.objects {
        let _ = writeln!(
            output,
            "- {} {}",
            object.object_type.as_deref().unwrap_or("?"),
            object.name
        );
    }
    for task in &result.tasks {
        let _ = writeln!(
            output,
            "  task {} {} ({} object(s))",
            task.number,
            task.description.as_deref().unwrap_or(""),
            task.objects.len()
        );
    }
    output
}

pub fn print_transport_list(result: &TransportListOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }
    print!("{}", render_transport_list_readable(result));
}

fn map_request(request: TransportRequest) -> TransportRequestOutput {
    TransportRequestOutput {
        number: request.number,
        description: request.description,
        owner: request.owner,
        status: request.status,
        target: request.target,
        tasks: request
            .tasks
            .into_iter()
            .map(|task| TransportTaskOutput {
                number: task.number,
                description: task.description,
                owner: task.owner,
                status: task.status,
            })
            .collect(),
    }
}

fn render_transport_list_readable(result: &TransportListOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "profile: {}", result.profile);
    let _ = writeln!(
        output,
        "{} {} request(s) for {}",
        result.total, result.status_filter, result.user
    );
    for request in &result.requests {
        let _ = writeln!(
            output,
            "- {} {}",
            request.number,
            request.description.as_deref().unwrap_or("(no description)")
        );
        for task in &request.tasks {
            let _ = writeln!(
                output,
                "    task {} {}",
                task.number,
                task.description.as_deref().unwrap_or("")
            );
        }
    }
    output
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, TransportCommand};
    use fractal::sap::transport::TransportTask;

    fn list_args(cli: Cli) -> TransportListArgs {
        let Command::Transport {
            command: TransportCommand::List(args),
        } = cli.command
        else {
            panic!("expected transport list command");
        };
        args
    }

    #[test]
    fn defaults_to_the_profiles_own_modifiable_requests() {
        let args = list_args(Cli::try_parse_from(["fractal", "transport", "list"]).unwrap());

        assert_eq!(args.user, None);
        assert!(!args.released);
    }

    #[test]
    fn parses_an_explicit_user_and_the_released_filter() {
        let args = list_args(
            Cli::try_parse_from([
                "fractal",
                "transport",
                "list",
                "--user",
                "OTHERDEV",
                "--released",
            ])
            .unwrap(),
        );

        assert_eq!(args.user.as_deref(), Some("OTHERDEV"));
        assert!(args.released);
    }

    fn sample_request() -> TransportRequest {
        TransportRequest {
            number: "AB1K900001".to_owned(),
            description: Some("Sample request".to_owned()),
            owner: Some("DEVELOPER".to_owned()),
            status: Some("D".to_owned()),
            target: Some("QA1".to_owned()),
            tasks: vec![TransportTask {
                number: "AB1K900002".to_owned(),
                description: Some("Sample task".to_owned()),
                owner: Some("DEVELOPER".to_owned()),
                status: Some("D".to_owned()),
            }],
        }
    }

    #[test]
    fn maps_requests_and_tasks_into_stable_json() {
        let output = TransportListOutput {
            ok: true,
            profile: "development".to_owned(),
            user: "DEVELOPER".to_owned(),
            status_filter: "modifiable".to_owned(),
            total: 1,
            requests: vec![map_request(sample_request())],
        };

        let json = serde_json::to_value(&output).unwrap();
        assert_eq!(json["total"], 1);
        assert_eq!(json["status_filter"], "modifiable");
        assert_eq!(json["requests"][0]["number"], "AB1K900001");
        assert_eq!(json["requests"][0]["tasks"][0]["number"], "AB1K900002");
    }

    #[test]
    fn omits_fields_sap_left_empty_rather_than_emitting_null() {
        let bare = TransportRequest {
            number: "AB1K900003".to_owned(),
            description: None,
            owner: None,
            status: None,
            target: None,
            tasks: Vec::new(),
        };

        let json = serde_json::to_value(map_request(bare)).unwrap();
        assert_eq!(json["number"], "AB1K900003");
        assert!(json.get("description").is_none());
        assert!(json.get("target").is_none());
        assert!(json["tasks"].as_array().unwrap().is_empty());
    }

    #[test]
    fn readable_output_lists_each_request_with_its_tasks() {
        let output = TransportListOutput {
            ok: true,
            profile: "development".to_owned(),
            user: "DEVELOPER".to_owned(),
            status_filter: "modifiable".to_owned(),
            total: 1,
            requests: vec![map_request(sample_request())],
        };

        let readable = render_transport_list_readable(&output);
        assert!(readable.contains("1 modifiable request(s) for DEVELOPER"));
        assert!(readable.contains("- AB1K900001 Sample request"));
        assert!(readable.contains("    task AB1K900002 Sample task"));
    }

    #[test]
    fn a_request_without_a_description_still_renders() {
        let output = TransportListOutput {
            ok: true,
            profile: "development".to_owned(),
            user: "DEVELOPER".to_owned(),
            status_filter: "modifiable".to_owned(),
            total: 1,
            requests: vec![map_request(TransportRequest {
                number: "AB1K900003".to_owned(),
                description: None,
                owner: None,
                status: None,
                target: None,
                tasks: Vec::new(),
            })],
        };

        assert!(render_transport_list_readable(&output).contains("- AB1K900003 (no description)"));
    }

    fn created(target: TransportTarget) -> TransportCreateOutput {
        map_created_request(
            "development".to_owned(),
            CreatedTransportRequest {
                number: "AB1K900005".to_owned(),
                description: "Widen the event log key".to_owned(),
                package: Some("ZDEMO".to_owned()),
                target,
            },
        )
    }

    #[test]
    fn warns_that_a_request_without_a_target_can_never_be_released() {
        // The request exists and can hold objects, so this is a warning rather
        // than a failure — but nothing else in the workflow would mention it
        // until the release failed.
        let output = created(TransportTarget::Local);

        let warning = output.warning.clone().unwrap();
        assert!(warning.contains("can never be released"));
        // Naming the package points at the cause: its transport layer.
        assert!(warning.contains("ZDEMO"));
        assert!(warning.contains("transport layer"));
    }

    #[test]
    fn an_unseen_request_is_not_reported_as_local() {
        // Both states have no target to print, but only one of them justifies
        // blaming the package's transport layer. Saying "local" about a
        // request that merely did not appear is a confident wrong answer.
        let output = created(TransportTarget::Unknown);

        assert_eq!(output.target, None);
        let warning = output.warning.clone().unwrap();
        assert!(warning.contains("unknown"));
        assert!(!warning.contains("can never be released"));
        assert!(!warning.contains("transport layer"));
    }

    #[test]
    fn a_request_with_a_target_carries_no_warning() {
        let output = created(TransportTarget::System("QA1".to_owned()));

        assert_eq!(output.target.as_deref(), Some("QA1"));
        assert!(output.warning.is_none());
    }

    fn detail(number: &str) -> TransportRequestDetail {
        TransportRequestDetail {
            number: number.to_owned(),
            description: Some("Widen the event log key".to_owned()),
            owner: Some("DEVELOPER".to_owned()),
            status: Some("D".to_owned()),
            status_text: Some("Modifiable".to_owned()),
            target: Some("QA1".to_owned()),
            objects: vec![TransportObject {
                program_id: Some("R3TR".to_owned()),
                object_type: Some("PROG".to_owned()),
                name: "ZEXAMPLE_REPORT".to_owned(),
                workbench_type: Some("PROG/P".to_owned()),
            }],
            tasks: Vec::new(),
        }
    }

    #[test]
    fn parses_the_request_number_as_a_positional_argument() {
        let cli = Cli::try_parse_from(["fractal", "transport", "show", "AB1K900001"]).unwrap();
        let Command::Transport {
            command: TransportCommand::Show(args),
        } = cli.command
        else {
            panic!("expected transport show command");
        };

        assert_eq!(args.number, "AB1K900001");
    }

    #[test]
    fn says_so_when_a_task_number_resolved_to_its_parent() {
        // SAP substitutes the parent silently, so without this the output
        // simply shows a different number than the one that was asked for.
        let output =
            map_request_detail("development".to_owned(), "ab1k900002", detail("AB1K900001"));

        let note = output.note.clone().unwrap();
        assert!(note.contains("AB1K900002"));
        assert!(note.contains("is a task"));
        assert!(note.contains("AB1K900001"));
    }

    #[test]
    fn asking_for_the_request_itself_carries_no_note() {
        let output = map_request_detail(
            "development".to_owned(),
            "  ab1k900001 ",
            detail("AB1K900001"),
        );

        assert_eq!(output.note, None);
        assert_eq!(output.total_objects, 1);
        assert_eq!(output.status.as_deref(), Some("Modifiable"));
    }

    #[test]
    fn renders_a_request_with_its_objects() {
        let readable = render_transport_show_readable(&map_request_detail(
            "development".to_owned(),
            "AB1K900001",
            detail("AB1K900001"),
        ));

        assert!(readable.contains("AB1K900001 Widen the event log key"));
        assert!(readable.contains("target: QA1"));
        assert!(readable.contains("1 object(s)"));
        assert!(readable.contains("- PROG ZEXAMPLE_REPORT"));
    }

    #[test]
    fn a_local_request_is_named_as_such_when_rendered() {
        let mut local = detail("AB1K900001");
        local.target = None;

        let readable = render_transport_show_readable(&map_request_detail(
            "development".to_owned(),
            "AB1K900001",
            local,
        ));

        assert!(readable.contains("target: (none - local request)"));
    }
}

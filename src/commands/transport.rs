use std::fmt::Write as _;

use serde::Serialize;

use super::connect;
use crate::{
    cli::TransportListArgs,
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::sap::transport::{TransportRequest, TransportStatusFilter, list_transport_requests};

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
            number: "DE3K900001".to_owned(),
            description: Some("Sample request".to_owned()),
            owner: Some("DEVELOPER".to_owned()),
            status: Some("D".to_owned()),
            target: Some("XE1".to_owned()),
            tasks: vec![TransportTask {
                number: "DE3K900002".to_owned(),
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
        assert_eq!(json["requests"][0]["number"], "DE3K900001");
        assert_eq!(json["requests"][0]["tasks"][0]["number"], "DE3K900002");
    }

    #[test]
    fn omits_fields_sap_left_empty_rather_than_emitting_null() {
        let bare = TransportRequest {
            number: "DE3K900003".to_owned(),
            description: None,
            owner: None,
            status: None,
            target: None,
            tasks: Vec::new(),
        };

        let json = serde_json::to_value(map_request(bare)).unwrap();
        assert_eq!(json["number"], "DE3K900003");
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
        assert!(readable.contains("- DE3K900001 Sample request"));
        assert!(readable.contains("    task DE3K900002 Sample task"));
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
                number: "DE3K900003".to_owned(),
                description: None,
                owner: None,
                status: None,
                target: None,
                tasks: Vec::new(),
            })],
        };

        assert!(render_transport_list_readable(&output).contains("- DE3K900003 (no description)"));
    }
}

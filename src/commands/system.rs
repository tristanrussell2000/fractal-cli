use serde::Serialize;

use crate::commands::connect;
use crate::output::{OutputFormat, print_result};
use crate::reported::Reported;
use fractal::{config, sap::client::DiscoveryResult};

#[derive(Debug, Serialize)]
pub struct SystemListResult {
    ok: bool,
    config_path: String,
    default_profile: Option<String>,
    profiles: Vec<ProfileSummary>,
}

#[derive(Debug, Serialize)]
struct ProfileSummary {
    name: String,
    base_url: String,
    client: String,
    username: String,
    insecure_tls: bool,
    customer_namespaces: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit_packages: Option<Vec<String>>,
    allow_temporary_package: bool,
}

#[derive(Debug, Serialize)]
pub struct SystemTestResult {
    ok: bool,
    profile: String,
    base_url: String,
    status: u16,
    csrf_token_received: bool,
    message: String,
}

pub fn print_system_list(result: &SystemListResult, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    println!("config: {}", result.config_path);
    match &result.default_profile {
        Some(profile) => println!("default profile: {profile}"),
        None => println!("default profile: (none)"),
    }

    if result.profiles.is_empty() {
        println!("profiles: (none)");
        return;
    }

    println!("profiles:");
    for profile in &result.profiles {
        let marker = if result.default_profile.as_deref() == Some(profile.name.as_str()) {
            "*"
        } else {
            " "
        };
        println!(
            "  {marker} {} — {} client {} user {}",
            profile.name, profile.base_url, profile.client, profile.username
        );
    }
}

pub fn system_list() -> Result<SystemListResult, Reported> {
    let loaded = config::load()?;
    let profiles = loaded
        .config
        .profiles
        .iter()
        .map(|(name, profile)| ProfileSummary {
            name: name.clone(),
            base_url: profile.base_url.clone(),
            client: profile.client.clone(),
            username: profile.username.clone(),
            insecure_tls: profile.insecure_tls,
            customer_namespaces: profile.customer_namespaces.clone(),
            edit_packages: profile.edit_packages.clone(),
            allow_temporary_package: profile.allow_temporary_package,
        })
        .collect();

    Ok(SystemListResult {
        ok: true,
        config_path: loaded.path.display().to_string(),
        default_profile: loaded.config.default_profile,
        profiles,
    })
}

pub async fn system_test(explicit_profile: Option<&str>) -> Result<SystemTestResult, Reported> {
    let (name, profile, mut client) = connect(explicit_profile).await?;
    let discovery = client.test_connection().await?;

    Ok(system_test_result(&name, &profile, &discovery))
}

fn system_test_result(
    name: &str,
    profile: &config::Profile,
    discovery: &DiscoveryResult,
) -> SystemTestResult {
    SystemTestResult {
        ok: true,
        profile: name.to_owned(),
        base_url: profile.base_url.clone(),
        status: discovery.status.as_u16(),
        csrf_token_received: discovery.csrf_token_received,
        message: "SAP ADT discovery endpoint is reachable and accepted the credentials.".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{Cli, Command, SystemCommand};

    #[test]
    fn parses_system_list_command_from_cli() {
        let cli = Cli::try_parse_from(["fractal", "system", "list"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::System {
                command: SystemCommand::List
            }
        ));
    }

    #[test]
    fn parses_system_test_command_from_cli() {
        let cli = Cli::try_parse_from(["fractal", "system", "test"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::System {
                command: SystemCommand::Test
            }
        ));
    }
}

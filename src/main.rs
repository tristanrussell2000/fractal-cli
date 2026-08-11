mod cli;
mod command_error;
mod commands;
mod output;

use clap::Parser;
use cli::{AuthCommand, Cli, Command, ObjectCommand, PackageCommand, SystemCommand};
use command_error::CommandError;
use commands::auth::{auth_list, auth_login, auth_remove};
use commands::object::{
    object_info, object_kinds, object_search, object_source, object_xml, print_object_kinds,
};
use commands::package::{package_items, package_tree};
use fractal::{
    config, credentials,
    sap::client::{DiscoveryResult, SapClient},
};
use output::{
    OutputFormat, default_output_format, print_result, run_and_print, run_and_print_async,
    run_and_print_with,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct SystemListResult {
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
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    let output = cli.output.unwrap_or_else(default_output_format);

    let exit_code = match &cli.command {
        Command::Auth {
            command: AuthCommand::Login(args),
        } => run_and_print(|| auth_login(args), output),
        Command::Auth {
            command: AuthCommand::List,
        } => run_and_print(auth_list, output),
        Command::Auth {
            command: AuthCommand::Remove(args),
        } => run_and_print(|| auth_remove(args), output),
        Command::System {
            command: SystemCommand::List,
        } => run_and_print_with(system_list, print_system_list, output),
        Command::System {
            command: SystemCommand::Test,
        } => run_and_print_async(|| system_test(cli.profile.as_deref()), output).await,
        Command::Object {
            command: ObjectCommand::Search(args),
        } => run_and_print_async(|| object_search(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Source(args),
        } => run_and_print_async(|| object_source(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Xml(args),
        } => run_and_print_async(|| object_xml(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Info(args),
        } => run_and_print_async(|| object_info(cli.profile.as_deref(), args), output).await,
        Command::Object {
            command: ObjectCommand::Kinds,
        } => run_and_print_with(object_kinds, print_object_kinds, output),
        Command::Package {
            command: PackageCommand::Tree(args),
        } => run_and_print_async(|| package_tree(cli.profile.as_deref(), args), output).await,
        Command::Package {
            command: PackageCommand::Items(args),
        } => run_and_print_async(|| package_items(cli.profile.as_deref(), args), output).await,
    };

    if exit_code != 0 {
        std::process::exit(exit_code);
    }
}

fn print_system_list(result: &SystemListResult, output: OutputFormat) {
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

fn system_list() -> Result<SystemListResult, CommandError> {
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
        })
        .collect();

    Ok(SystemListResult {
        ok: true,
        config_path: loaded.path.display().to_string(),
        default_profile: loaded.config.default_profile,
        profiles,
    })
}

#[derive(Debug, Serialize)]
struct SystemTestResult {
    ok: bool,
    profile: String,
    base_url: String,
    status: u16,
    csrf_token_received: bool,
    message: String,
}

async fn system_test(explicit_profile: Option<&str>) -> Result<SystemTestResult, CommandError> {
    let loaded = config::load()?;
    let (name, profile) = config::resolve_profile(&loaded.config, explicit_profile)?;
    let password = credentials::get_password(name)?;
    let mut client = SapClient::new(profile, password)?;
    let discovery = client.test_connection().await?;

    Ok(system_test_result(name, profile, &discovery))
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

    use super::*;

    #[test]
    fn parses_package_tree_options_from_cli() {
        let cli =
            Cli::try_parse_from(["fractal", "package", "tree", "ZAPP", "--no-recursive"]).unwrap();

        let Command::Package {
            command: PackageCommand::Tree(args),
        } = cli.command
        else {
            panic!("expected package tree command");
        };
        assert_eq!(args.name, "ZAPP");
        assert!(args.no_recursive);
    }

    #[test]
    fn parses_package_items_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "package",
            "items",
            "ZAPP",
            "--recursive",
            "--kind",
            "clas",
            "--object-type",
            "CLAS/OC",
            "--name-substring",
            "TEST",
            "--offset",
            "2",
            "--limit",
            "5",
        ])
        .unwrap();

        let Command::Package {
            command: PackageCommand::Items(args),
        } = cli.command
        else {
            panic!("expected package items command");
        };
        assert_eq!(args.name, "ZAPP");
        assert!(args.recursive);
        assert_eq!(args.kind.as_deref(), Some("clas"));
        assert_eq!(args.object_type.as_deref(), Some("CLAS/OC"));
        assert_eq!(args.name_substring.as_deref(), Some("TEST"));
        assert_eq!(args.offset, 2);
        assert_eq!(args.limit, 5);
    }
}

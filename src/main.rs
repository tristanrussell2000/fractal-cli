mod cli;
mod command_error;
mod output;

use std::io::Read;

use clap::Parser;
use cli::{
    AuthCommand, Cli, Command, LoginArgs, ObjectCommand, PackageCommand, ProfileArgs, SearchArgs,
    SystemCommand,
};
use command_error::CommandError;
use fractal::{
    config, credentials,
    sap::{
        adt::{ObjectSearchOptions, RepositoryKind, search_objects},
        client::{DiscoveryResult, SapClient},
    },
};
use output::{
    OutputFormat, default_output_format, print_result, run_and_print, run_and_print_async,
    run_and_print_with,
};
use serde::Serialize;

#[derive(Debug, Serialize)]
struct Placeholder {
    ok: bool,
    command: String,
    profile: Option<String>,
    message: String,
}

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

#[derive(Debug, Serialize)]
struct AuthLoginResult {
    ok: bool,
    profile: String,
    config_path: String,
    became_default: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct AuthListResult {
    ok: bool,
    config_path: String,
    default_profile: Option<String>,
    profiles: Vec<AuthProfileSummary>,
}

#[derive(Debug, Serialize)]
struct AuthProfileSummary {
    name: String,
    base_url: String,
    client: String,
    username: String,
    insecure_tls: bool,
    customer_namespaces: Vec<String>,
    credential: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_error: Option<String>,
}

#[derive(Debug, Serialize)]
struct AuthRemoveResult {
    ok: bool,
    profile: String,
    config_path: String,
    removed_default: bool,
    message: String,
}

#[derive(Debug, Serialize)]
struct ObjectSearchResultOutput {
    ok: bool,
    profile: String,
    query: String,
    package_patterns: Vec<String>,
    package_patterns_source: String,
    total_matching: usize,
    returned: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    sap_search_cap: usize,
    hits: Vec<ObjectSearchHitOutput>,
}

#[derive(Debug, Serialize)]
struct ObjectSearchHitOutput {
    name: String,
    kind: String,
    object_type: String,
    package: Option<String>,
    description: Option<String>,
    uri: Option<String>,
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
        _ => {
            let (command_name, message) = describe_command(&cli.command);
            let result = Placeholder {
                ok: false,
                command: command_name.to_owned(),
                profile: cli.profile,
                message: message.to_owned(),
            };
            print_result(&result, output);
            0
        }
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

    Ok(system_test_result(name, profile, discovery))
}

async fn object_search(
    explicit_profile: Option<&str>,
    args: &SearchArgs,
) -> Result<ObjectSearchResultOutput, CommandError> {
    let kind = args
        .kind
        .as_deref()
        .map(RepositoryKind::parse)
        .transpose()
        .map_err(|error| {
            CommandError::with_hint(
                "invalid_repository_kind",
                error.to_string(),
                "Use a kind such as CLAS, INTF, TABL, PROG, DDLS, or OTHER.",
            )
        })?;
    let loaded = config::load()?;
    let (profile_name, profile) = config::resolve_profile(&loaded.config, explicit_profile)?;
    let password = credentials::get_password(profile_name)?;
    let mut client = SapClient::new(profile, password)?;
    let explicit_patterns =
        (!args.package_patterns.is_empty()).then(|| args.package_patterns.clone());
    let effective_patterns = explicit_patterns
        .clone()
        .unwrap_or_else(|| profile.customer_namespaces.clone());
    let result = search_objects(
        &mut client,
        profile,
        &args.query,
        ObjectSearchOptions {
            package_patterns: explicit_patterns,
            kind,
            offset: args.offset,
            limit: Some(args.limit),
        },
    )
    .await?;
    let returned = result.hits.len();
    let next_offset = (args.offset + returned < result.total).then_some(args.offset + returned);

    Ok(ObjectSearchResultOutput {
        ok: true,
        profile: profile_name.to_owned(),
        query: args.query.clone(),
        package_patterns: effective_patterns,
        package_patterns_source: if args.package_patterns.is_empty() {
            "default".to_owned()
        } else {
            "explicit".to_owned()
        },
        total_matching: result.total,
        returned,
        offset: args.offset,
        limit: args.limit,
        next_offset,
        sap_search_cap: result.sap_search_cap,
        hits: result
            .hits
            .into_iter()
            .map(|hit| ObjectSearchHitOutput {
                name: hit.name,
                kind: hit.kind.as_str().to_owned(),
                object_type: hit.object_type,
                package: hit.package,
                description: hit.description,
                uri: hit.uri,
            })
            .collect(),
    })
}

fn system_test_result(
    name: &str,
    profile: &config::Profile,
    discovery: DiscoveryResult,
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

fn auth_list() -> Result<AuthListResult, CommandError> {
    let loaded = config::load()?;
    let profiles = loaded
        .config
        .profiles
        .iter()
        .map(|(name, profile)| {
            let (credential, credential_error) = match credentials::get_password(name) {
                Ok(_) => ("stored".to_owned(), None),
                Err(error @ credentials::CredentialError::Missing(_)) => {
                    ("missing".to_owned(), Some(error.to_string()))
                }
                Err(error) => ("unavailable".to_owned(), Some(error.to_string())),
            };

            AuthProfileSummary {
                name: name.clone(),
                base_url: profile.base_url.clone(),
                client: profile.client.clone(),
                username: profile.username.clone(),
                insecure_tls: profile.insecure_tls,
                customer_namespaces: profile.customer_namespaces.clone(),
                credential,
                credential_error,
            }
        })
        .collect();

    Ok(AuthListResult {
        ok: true,
        config_path: loaded.path.display().to_string(),
        default_profile: loaded.config.default_profile,
        profiles,
    })
}

fn auth_remove(args: &ProfileArgs) -> Result<AuthRemoveResult, CommandError> {
    let mut loaded = config::load()?;
    if !loaded.config.profiles.contains_key(&args.name) {
        return Err(CommandError::with_hint(
            "profile_not_found",
            format!("profile '{}' was not found", args.name),
            "Run `fractal auth list` to see configured profiles.",
        ));
    }

    let removed_default = loaded.config.default_profile.as_deref() == Some(args.name.as_str());
    credentials::delete_password(&args.name)?;
    config::remove_profile(&mut loaded.config, &args.name);
    let config_path = config::save(&loaded.config).map_err(|error| {
        CommandError::with_hint(
            "config_write_error_after_credential_removal",
            format!("credential removed, but profile config could not be saved: {error}"),
            "Retry `fractal auth remove` for this profile; credential deletion is idempotent.",
        )
    })?;

    let message = if removed_default {
        "Profile removed. No default profile is set; pass --profile <name> for commands or set a new default with `fractal auth login <name> --default`.".to_owned()
    } else {
        "Profile and credential removed.".to_owned()
    };

    Ok(AuthRemoveResult {
        ok: true,
        profile: args.name.clone(),
        config_path: config_path.display().to_string(),
        removed_default,
        message,
    })
}

fn auth_login(args: &LoginArgs) -> Result<AuthLoginResult, CommandError> {
    let password = if args.password_stdin {
        let mut password = String::new();
        std::io::stdin()
            .read_to_string(&mut password)
            .map_err(|error| CommandError::with_hint(
                "password_stdin_error",
                format!("could not read password from stdin: {error}"),
                "Provide the password through stdin or omit --password-stdin to use the secure prompt.",
            ))?;
        password.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        rpassword::prompt_password("Password: ").map_err(|error| {
            CommandError::from_message("password_prompt_error", error.to_string())
        })?
    };

    if password.is_empty() {
        return Err(CommandError::with_hint(
            "empty_password",
            "password cannot be empty",
            "Provide a non-empty password through the secure prompt or --password-stdin.",
        ));
    }

    let mut loaded = config::load()?;
    let profile = config::Profile {
        base_url: args.url.trim_end_matches('/').to_owned(),
        client: args.client.clone(),
        username: args.username.clone(),
        insecure_tls: args.insecure_tls,
        customer_namespaces: if args.namespace.is_empty() {
            vec!["Z*".to_owned(), "Y*".to_owned()]
        } else {
            args.namespace.clone()
        },
    };
    let became_default =
        config::update_profile(&mut loaded.config, args.name.clone(), profile, args.default);

    let config_path = config::save(&loaded.config).map_err(|error| {
        CommandError::with_hint(
            "config_write_error",
            format!("could not save profile config: {error}"),
            "Check that Fractal's application config directory is writable.",
        )
    })?;
    credentials::save_password(&args.name, &password).map_err(|error| {
        CommandError::with_hint(
            "credential_store_error_after_config_save",
            format!("profile config saved, but the credential could not be stored: {error}"),
            format!(
                "Run `fractal auth list` and retry `fractal auth login {}`.",
                args.name
            ),
        )
    })?;

    Ok(AuthLoginResult {
        ok: true,
        profile: args.name.clone(),
        config_path: config_path.display().to_string(),
        became_default,
        message: if became_default {
            "Profile saved and selected as the default profile.".to_owned()
        } else {
            "Profile saved; the existing default profile was preserved.".to_owned()
        },
    })
}

fn describe_command(command: &Command) -> (&'static str, &'static str) {
    match command {
        Command::Auth { command } => match command {
            AuthCommand::Login(_) => ("auth login", "Authentication is not implemented yet."),
            AuthCommand::List => ("auth list", "Profile listing is not implemented yet."),
            AuthCommand::Remove(_) => ("auth remove", "Profile removal is not implemented yet."),
        },
        Command::System { command } => match command {
            SystemCommand::List => ("system list", "System listing is not implemented yet."),
            SystemCommand::Test => ("system test", "SAP connectivity is not implemented yet."),
        },
        Command::Package { command } => match command {
            PackageCommand::Tree(_) => ("package tree", "Package browsing is not implemented yet."),
        },
        Command::Object { command } => match command {
            ObjectCommand::Search(_) => ("object search", "Object search is not implemented yet."),
            ObjectCommand::Source(_) => {
                ("object source", "Source retrieval is not implemented yet.")
            }
            ObjectCommand::Xml(_) => ("object xml", "Metadata retrieval is not implemented yet."),
        },
    }
}

use std::{fmt::Display, future::Future, io::Read};

use clap::{Args, Parser, Subcommand, ValueEnum};
use fractal::{
    config, credentials,
    sap::client::{DiscoveryResult, SapClient},
};
use serde::Serialize;

#[derive(Debug, Parser)]
#[command(
    name = "fractal",
    version,
    about = "Explore and edit SAP S/4HANA development systems"
)]
struct Cli {
    /// Select the SAP profile for this command.
    #[arg(long, global = true, env = "FRACTAL_PROFILE")]
    profile: Option<String>,

    /// Select the output format. Without this flag, TTY output is readable and non-TTY output is JSON.
    #[arg(long, value_enum, global = true)]
    output: Option<OutputFormat>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum OutputFormat {
    Json,
    Readable,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Manage saved SAP profiles and credentials.
    Auth {
        #[command(subcommand)]
        command: AuthCommand,
    },
    /// Inspect configured SAP environments.
    System {
        #[command(subcommand)]
        command: SystemCommand,
    },
    /// Browse ABAP packages.
    Package {
        #[command(subcommand)]
        command: PackageCommand,
    },
    /// Discover and inspect repository objects.
    Object {
        #[command(subcommand)]
        command: ObjectCommand,
    },
}

#[derive(Debug, Subcommand)]
enum AuthCommand {
    /// Save or update a profile and its password in the OS keychain.
    Login(LoginArgs),
    /// List saved profile names.
    List,
    /// Remove a saved profile and its keychain credential.
    Remove(ProfileArgs),
}

#[derive(Debug, Args)]
struct LoginArgs {
    /// Name used to select this profile, for example DE2_903.
    name: String,
    /// SAP base URL, for example https://sap.example:8001.
    #[arg(long)]
    url: String,
    /// SAP client, for example 903.
    #[arg(long)]
    client: String,
    /// SAP username.
    #[arg(long)]
    username: String,
    /// Allow invalid TLS certificates. Use only for development systems.
    #[arg(long, default_value_t = false)]
    insecure_tls: bool,
    /// Customer namespace patterns. Repeat for multiple patterns; defaults to Z* and Y*.
    #[arg(long)]
    namespace: Vec<String>,
    /// Make this profile the default, replacing the current default.
    #[arg(long, default_value_t = false)]
    default: bool,
    /// Read the password from standard input instead of prompting.
    #[arg(long, default_value_t = false)]
    password_stdin: bool,
}

#[derive(Debug, Args)]
struct ProfileArgs {
    /// Saved profile name.
    name: String,
}

#[derive(Debug, Subcommand)]
enum SystemCommand {
    /// List saved SAP environments.
    List,
    /// Verify connectivity and authentication for the selected profile.
    Test,
}

#[derive(Debug, Subcommand)]
enum PackageCommand {
    /// Walk a package and show its object tree.
    Tree(NameArgs),
}

#[derive(Debug, Subcommand)]
enum ObjectCommand {
    /// Search for repository objects by name.
    Search(SearchArgs),
    /// Read source for an ADT object URI.
    Source(UriArgs),
    /// Read metadata XML for an ADT object URI.
    Xml(UriArgs),
}

#[derive(Debug, Args)]
struct NameArgs {
    /// ABAP package name.
    name: String,
}

#[derive(Debug, Args)]
struct SearchArgs {
    /// Name substring or SAP-style pattern.
    query: String,
}

#[derive(Debug, Args)]
struct UriArgs {
    /// ADT object URI, without a source suffix.
    uri: String,
}

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
struct ErrorResult {
    ok: bool,
    error: String,
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

async fn run_and_print_async<T, E, F, Fut>(operation: F, output: OutputFormat) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    run_and_print_with_async(operation, print_result, output).await
}

async fn run_and_print_with_async<T, E, F, Fut, P>(
    operation: F,
    print: P,
    output: OutputFormat,
) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    P: FnOnce(&T, OutputFormat),
{
    match operation().await {
        Ok(result) => {
            print(&result, output);
            0
        }
        Err(error) => {
            print_error_message(&error.to_string(), output);
            2
        }
    }
}

fn run_and_print<T, E, F>(operation: F, output: OutputFormat) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Result<T, E>,
{
    run_and_print_with(operation, print_result, output)
}

fn run_and_print_with<T, E, F, P>(operation: F, print: P, output: OutputFormat) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Result<T, E>,
    P: FnOnce(&T, OutputFormat),
{
    match operation() {
        Ok(result) => {
            print(&result, output);
            0
        }
        Err(error) => {
            print_error_message(&error.to_string(), output);
            2
        }
    }
}

fn print_result<T: Serialize>(result: &T, output: OutputFormat) {
    match output {
        OutputFormat::Json => print_json(result),
        OutputFormat::Readable => {
            // Commands without a dedicated readable renderer use structured JSON for now.
            print_json(result);
        }
    }
}

fn print_json<T: Serialize>(result: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(result).expect("result serializes")
    );
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

fn system_list() -> Result<SystemListResult, config::ConfigError> {
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

async fn system_test(explicit_profile: Option<&str>) -> Result<SystemTestResult, String> {
    let loaded = config::load().map_err(|error| error.to_string())?;
    let (name, profile) = config::resolve_profile(&loaded.config, explicit_profile)
        .map_err(|error| error.to_string())?;
    let password = credentials::get_password(name).map_err(|error| error.to_string())?;
    let client = SapClient::new(profile, password).map_err(|error| error.to_string())?;
    let discovery = client
        .test_connection()
        .await
        .map_err(|error| error.to_string())?;

    Ok(system_test_result(name, profile, discovery))
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

fn print_error_message(error: &str, output: OutputFormat) {
    let result = ErrorResult {
        ok: false,
        error: error.to_owned(),
    };
    match output {
        OutputFormat::Json => println!(
            "{}",
            serde_json::to_string_pretty(&result).expect("error serializes")
        ),
        OutputFormat::Readable => eprintln!("error: {error}"),
    }
}

fn auth_list() -> Result<AuthListResult, String> {
    let loaded = config::load().map_err(|error| error.to_string())?;
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

fn auth_remove(args: &ProfileArgs) -> Result<AuthRemoveResult, String> {
    let mut loaded = config::load().map_err(|error| error.to_string())?;
    if !loaded.config.profiles.contains_key(&args.name) {
        return Err(format!("profile '{}' was not found", args.name));
    }

    let removed_default = loaded.config.default_profile.as_deref() == Some(args.name.as_str());
    credentials::delete_password(&args.name).map_err(|error| error.to_string())?;
    config::remove_profile(&mut loaded.config, &args.name);
    let config_path = config::save(&loaded.config).map_err(|error| {
        format!("credential removed, but profile config could not be saved: {error}")
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

fn auth_login(args: &LoginArgs) -> Result<AuthLoginResult, String> {
    let password = if args.password_stdin {
        let mut password = String::new();
        std::io::stdin()
            .read_to_string(&mut password)
            .map_err(|error| format!("could not read password from stdin: {error}"))?;
        password.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        rpassword::prompt_password("Password: ")
            .map_err(|error| format!("could not read password: {error}"))?
    };

    if password.is_empty() {
        return Err("password cannot be empty".to_owned());
    }

    let mut loaded = config::load().map_err(|error| error.to_string())?;
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

    let config_path = config::save(&loaded.config)
        .map_err(|error| format!("could not save profile config: {error}"))?;
    credentials::save_password(&args.name, &password).map_err(|error| {
        format!("profile config saved, but the credential could not be stored: {error}")
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

fn default_output_format() -> OutputFormat {
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        OutputFormat::Readable
    } else {
        OutputFormat::Json
    }
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

use std::io::Read;

use clap::{Args, Parser, Subcommand, ValueEnum};
use fractal::{config, credentials};
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

fn main() {
    let cli = Cli::parse();
    let output = cli.output.unwrap_or_else(default_output_format);

    let exit_code = match &cli.command {
        Command::Auth {
            command: AuthCommand::Login(args),
        } => match auth_login(args) {
            Ok(result) => {
                print_result(&result, output);
                0
            }
            Err(error) => {
                print_error_message(&error, output);
                2
            }
        },
        Command::System {
            command: SystemCommand::List,
        } => match config::load() {
            Ok(loaded) => {
                let result = SystemListResult {
                    ok: true,
                    config_path: loaded.path.display().to_string(),
                    default_profile: loaded.config.default_profile.clone(),
                    profiles: loaded
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
                        .collect(),
                };
                print_system_list(&result, output);
                0
            }
            Err(error) => {
                print_error(error, output);
                2
            }
        },
        Command::System {
            command: SystemCommand::Test,
        } => match config::load().and_then(|loaded| {
            let (name, profile) = config::resolve_profile(&loaded.config, cli.profile.as_deref())?;
            Ok((name.to_owned(), profile.base_url.clone()))
        }) {
            Ok((name, base_url)) => {
                let result = Placeholder {
                    ok: false,
                    command: "system test".to_owned(),
                    profile: Some(name),
                    message: format!(
                        "Profile resolved successfully, but SAP connectivity is not implemented yet ({base_url})."
                    ),
                };
                print_result(&result, output);
                0
            }
            Err(error) => {
                print_error(error, output);
                2
            }
        },
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

fn print_result<T: Serialize>(result: &T, output: OutputFormat) {
    match output {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(result).expect("result serializes")
            );
        }
        OutputFormat::Readable => {
            println!(
                "{}",
                serde_json::to_string_pretty(result).expect("result serializes")
            );
        }
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

fn print_error(error: config::ConfigError, output: OutputFormat) {
    print_error_message(&error.to_string(), output);
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
            AuthCommand::List => ("auth list", "Profile storage is not implemented yet."),
            AuthCommand::Remove(_) => ("auth remove", "Profile storage is not implemented yet."),
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

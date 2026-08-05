use clap::{Args, Parser, Subcommand, ValueEnum};
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
    /// Verify connectivity and authentication for a profile.
    Test(ProfileArgs),
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

fn main() {
    let cli = Cli::parse();
    let (command_name, message) = describe_command(&cli.command);

    let result = Placeholder {
        ok: false,
        command: command_name.to_string(),
        profile: cli.profile,
        message: message.to_owned(),
    };

    match cli.output.unwrap_or_else(default_output_format) {
        OutputFormat::Json => {
            println!(
                "{}",
                serde_json::to_string_pretty(&result).expect("placeholder serializes")
            );
        }
        OutputFormat::Readable => {
            println!("{}", result.message);
            if let Some(profile) = result.profile {
                println!("profile: {profile}");
            }
        }
    }
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
            SystemCommand::Test(_) => ("system test", "SAP connectivity is not implemented yet."),
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

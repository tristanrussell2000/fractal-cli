use clap::{Args, Parser, Subcommand};

use crate::output::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "fractal",
    version,
    about = "Explore and edit SAP S/4HANA development systems"
)]
pub(crate) struct Cli {
    /// Select the SAP profile for this command.
    #[arg(long, global = true, env = "FRACTAL_PROFILE")]
    pub(crate) profile: Option<String>,

    /// Select the output format. Without this flag, TTY output is readable and non-TTY output is JSON.
    #[arg(long, value_enum, global = true)]
    pub(crate) output: Option<OutputFormat>,

    #[command(subcommand)]
    pub(crate) command: Command,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
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
pub(crate) enum AuthCommand {
    /// Save or update a profile and its password in the OS keychain.
    Login(LoginArgs),
    /// List saved profile names.
    List,
    /// Remove a saved profile and its keychain credential.
    Remove(ProfileArgs),
}

#[derive(Debug, Args)]
pub(crate) struct LoginArgs {
    /// Name used to select this profile, for example DE2_903.
    pub(crate) name: String,
    /// SAP base URL, for example https://sap.example:8001.
    #[arg(long)]
    pub(crate) url: String,
    /// SAP client, for example 903.
    #[arg(long)]
    pub(crate) client: String,
    /// SAP username.
    #[arg(long)]
    pub(crate) username: String,
    /// Allow invalid TLS certificates. Use only for development systems.
    #[arg(long, default_value_t = false)]
    pub(crate) insecure_tls: bool,
    /// Customer namespace patterns. Repeat for multiple patterns; defaults to Z* and Y*.
    #[arg(long)]
    pub(crate) namespace: Vec<String>,
    /// Make this profile the default, replacing the current default.
    #[arg(long, default_value_t = false)]
    pub(crate) default: bool,
    /// Read the password from standard input instead of prompting.
    #[arg(long, default_value_t = false)]
    pub(crate) password_stdin: bool,
}

#[derive(Debug, Args)]
pub(crate) struct ProfileArgs {
    /// Saved profile name.
    pub(crate) name: String,
}

#[derive(Debug, Subcommand)]
pub(crate) enum SystemCommand {
    /// List saved SAP environments.
    List,
    /// Verify connectivity and authentication for the selected profile.
    Test,
}

#[derive(Debug, Subcommand)]
pub(crate) enum PackageCommand {
    /// Walk a package and show its object tree.
    Tree(NameArgs),
}

#[derive(Debug, Subcommand)]
pub(crate) enum ObjectCommand {
    /// Search for repository objects by name.
    Search(SearchArgs),
    /// Read source for an ADT object URI.
    Source(SourceArgs),
    /// Read metadata XML for an ADT object URI.
    Xml(UriArgs),
}

#[derive(Debug, Args)]
pub(crate) struct NameArgs {
    /// ABAP package name.
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub(crate) struct SearchArgs {
    /// Name substring or SAP-style pattern.
    pub(crate) query: String,
    /// Restrict results to a repository kind, such as CLAS or PROG.
    #[arg(long)]
    pub(crate) kind: Option<String>,
    /// Restrict results to matching package/name globs. Repeat for multiple patterns.
    #[arg(long = "package-pattern")]
    pub(crate) package_patterns: Vec<String>,
    /// Number of matching results to skip.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum number of results to return.
    #[arg(long, default_value_t = 100)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub(crate) struct SourceArgs {
    /// ADT object URI, without a source suffix.
    pub(crate) uri: String,
    /// Byte offset to start returning from.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum bytes to return. Omit to return the complete source.
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub(crate) struct UriArgs {
    /// ADT object URI, without a source suffix.
    pub(crate) uri: String,
}

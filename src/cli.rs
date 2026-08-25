use clap::{Args, Parser, Subcommand, ValueEnum};

use crate::output::OutputFormat;

#[derive(Debug, Parser)]
#[command(
    name = "fractal",
    version,
    about = "Explore and edit SAP S/4HANA development systems"
)]
pub struct Cli {
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
pub enum Command {
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
    /// Read data from SAP tables and views.
    Table {
        #[command(subcommand)]
        command: TableCommand,
    },
    /// Run a complete `OpenSQL` SELECT statement.
    Query(QueryArgs),
    /// Read and safely edit supported source-based repository objects.
    Edit {
        #[command(subcommand)]
        command: EditCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Save or update a profile and its password in the OS keychain.
    Login(LoginArgs),
    /// List saved profile names.
    List,
    /// Remove a saved profile and its keychain credential.
    Remove(ProfileArgs),
}

#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Name used to select this profile, for example `DE2_903`.
    pub(crate) name: String,
    /// SAP base URL, for example `<https://sap.example:8001>`.
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
pub struct ProfileArgs {
    /// Saved profile name.
    pub(crate) name: String,
}

#[derive(Debug, Subcommand)]
pub enum SystemCommand {
    /// List saved SAP environments.
    List,
    /// Verify connectivity and authentication for the selected profile.
    Test,
}

#[derive(Debug, Subcommand)]
pub enum PackageCommand {
    /// Walk a package and show its package hierarchy.
    Tree(PackageTreeArgs),
    /// List objects contained in a package.
    Items(PackageItemsArgs),
}

#[derive(Debug, Subcommand)]
pub enum ObjectCommand {
    /// Search for repository objects by name.
    Search(SearchArgs),
    /// Read source for an ADT object URI.
    Source(SourceArgs),
    /// Read metadata XML for an ADT object URI.
    Xml(XmlArgs),
    /// Read the authoritative short description for an ADT object URI.
    Info(UriArgs),
    /// Find objects that reference an ADT object URI (where-used).
    Usages(UsagesArgs),
    /// List known repository kinds with plain-text descriptions.
    Kinds,
}

#[derive(Debug, Subcommand)]
pub enum TableCommand {
    /// Preview rows from one table or view.
    Data(TableDataArgs),
    /// Show fields, keys, declared types, and DDIC column metadata for one table.
    Metadata(TableMetadataArgs),
}

#[derive(Debug, Subcommand)]
pub enum EditCommand {
    /// Read complete source and revision metadata for a future edit.
    Read(EditSourceReadArgs),
    /// Replace one exact fragment and save inactive source. Does not activate.
    Patch(EditSourcePatchArgs),
    /// Replace complete source and save it inactive. Does not activate.
    Set(EditSourceSetArgs),
    /// Run SAP's syntax checker against a stored source version.
    Check(EditSourceCheckArgs),
    /// Activate and verify an object's stored inactive source.
    Activate(EditSourceActivateArgs),
    /// Discard inactive changes while preserving the current active source.
    Discard(EditSourceDiscardArgs),
}

#[derive(Debug, Args)]
pub struct EditSourceReadArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, or TABL.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Stored source version to request. If inactive does not exist, SAP returns active source.
    #[arg(long, value_enum, default_value = "active")]
    pub(crate) version: EditSourceVersionArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum EditSourceVersionArg {
    Active,
    Inactive,
}

#[derive(Debug, Args)]
pub struct EditSourceCheckArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, or TABL.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Stored source version to check.
    #[arg(long, value_enum, default_value = "inactive")]
    pub(crate) version: EditSourceVersionArg,
}

#[derive(Debug, Args)]
pub struct EditSourceActivateArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, or TABL.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Parent CTS change request to attach before activation, for example DE3K900575.
    #[arg(long)]
    pub(crate) transport: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditSourceDiscardArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, or TABL.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Parent CTS change request to use while restoring the active source.
    #[arg(long)]
    pub(crate) transport: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditSourcePatchArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, or TABL.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Exact literal source text that must occur once.
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) find: String,
    /// Replacement source text.
    #[arg(long, allow_hyphen_values = true)]
    pub(crate) replace: String,
    /// Only patch source matching this SHA-256 from a previous read or preview.
    #[arg(long)]
    pub(crate) expected_sha256: Option<String>,
    /// Parent CTS change request to associate with the write, for example DE3K900575.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Validate and preview the change without locking or writing the object.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub struct EditSourceSetArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, or TABL.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Read complete UTF-8 replacement source from this file, or - for stdin.
    #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
    pub(crate) source_file: String,
    /// Only replace source matching this SHA-256 from a previous read or preview.
    #[arg(long)]
    pub(crate) expected_sha256: Option<String>,
    /// Parent CTS change request to associate with the write, for example DE3K900575.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Validate and preview the complete replacement without locking or writing.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub struct TableDataArgs {
    /// DDIC table or view name.
    pub(crate) name: String,
    /// Comma-separated fields to select. Omit to select every field.
    #[arg(long)]
    pub(crate) fields: Option<String>,
    /// `OpenSQL` WHERE fragment.
    #[arg(long = "where")]
    pub(crate) where_clause: Option<String>,
    /// Number of matching rows to skip locally.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum number of rows to return.
    #[arg(long, default_value_t = 100)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub struct TableMetadataArgs {
    /// DDIC table name.
    pub(crate) name: String,
    /// Run an accurate, potentially expensive `COUNT(*)` query.
    #[arg(long)]
    pub(crate) count: bool,
}

#[derive(Debug, Args)]
pub struct QueryArgs {
    /// Complete `OpenSQL` SELECT statement. Use `-` to read it from standard input.
    #[arg(allow_hyphen_values = true)]
    pub(crate) query: String,
    /// Number of matching rows to skip locally.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum number of rows to return.
    #[arg(long, default_value_t = 100)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub struct PackageTreeArgs {
    /// ABAP package name.
    pub(crate) name: String,
    /// Inspect only this package instead of recursively walking subpackages.
    #[arg(long)]
    pub(crate) no_recursive: bool,
}

#[derive(Debug, Args)]
pub struct PackageItemsArgs {
    /// ABAP package name.
    pub(crate) name: String,
    /// Include objects from subpackages.
    #[arg(long)]
    pub(crate) recursive: bool,
    /// Restrict results to a repository kind, such as CLAS or PROG.
    #[arg(long)]
    pub(crate) kind: Option<String>,
    /// Restrict results to a raw SAP ADT object type, such as TTYP/DA.
    #[arg(long = "object-type")]
    pub(crate) object_type: Option<String>,
    /// Case-insensitive substring match against object names.
    #[arg(long)]
    pub(crate) name_substring: Option<String>,
    /// Number of matching items to skip.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum number of items to return.
    #[arg(long, default_value_t = 100)]
    pub(crate) limit: usize,
}

#[derive(Debug, Args)]
pub struct NameArgs {
    /// ABAP package name.
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub struct SearchArgs {
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
pub struct SourceArgs {
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
pub struct XmlArgs {
    /// ADT object URI.
    pub(crate) uri: String,
    /// Byte offset to start returning from.
    #[arg(long, default_value_t = 0)]
    pub(crate) offset: usize,
    /// Maximum bytes to return. Omit to return the complete XML.
    #[arg(long)]
    pub(crate) limit: Option<usize>,
}

#[derive(Debug, Args)]
pub struct UsagesArgs {
    /// ADT object URI.
    pub(crate) uri: String,
    /// Only include direct usage hits; omit hierarchy/context rows (containing
    /// object, method, package) that SAP also reports.
    #[arg(long)]
    pub(crate) direct_results: bool,
}

#[derive(Debug, Args)]
pub struct UriArgs {
    /// ADT object URI.
    pub(crate) uri: String,
}

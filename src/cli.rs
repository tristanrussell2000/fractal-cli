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
    /// Inspect DDIC data elements and domains.
    Ddic {
        #[command(subcommand)]
        command: DdicCommand,
    },
    /// Read data from SAP tables and views.
    Table {
        #[command(subcommand)]
        command: TableCommand,
    },
    /// Run a complete `OpenSQL` SELECT statement.
    Query(QueryArgs),
    /// Inspect and manage change requests in the transport system.
    Transport {
        #[command(subcommand)]
        command: TransportCommand,
    },
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

// Command-line flags are independent switches by nature, and collapsing them
// into an enum would change what the user types. Same reasoning as the edit
// output structs.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Args)]
pub struct LoginArgs {
    /// Name used to select this profile, for example `DEV_100`. Prompted for
    /// when omitted and the terminal is interactive.
    pub(crate) name: Option<String>,
    /// SAP base URL, for example `<https://sap.example:8001>`. Prompted for
    /// when omitted.
    #[arg(long)]
    pub(crate) url: Option<String>,
    /// SAP client, for example 903. Prompted for when omitted.
    #[arg(long)]
    pub(crate) client: Option<String>,
    /// SAP username. Prompted for when omitted.
    #[arg(long)]
    pub(crate) username: Option<String>,
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
    /// Command that prints this profile's password, for example
    /// `pass show sap/dev`. Stored in the profile and run on each use, so no
    /// password is kept by Fractal at all.
    #[arg(long)]
    pub(crate) password_command: Option<String>,
    /// Store the password in a plain, unencrypted file readable only by you,
    /// instead of the OS credential store. For machines that have no keychain.
    #[arg(long, default_value_t = false)]
    pub(crate) store_plaintext: bool,
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
pub enum DdicCommand {
    /// Show one data element or domain, resolving a data element to its domain.
    Show(DdicShowArgs),
}

#[derive(Debug, Args)]
pub struct DdicShowArgs {
    /// Data element or domain name.
    pub(crate) name: String,
    /// Skip detection and read this type directly.
    #[arg(long = "type", value_enum)]
    pub(crate) object_type: Option<DdicTypeArg>,
    /// Report the data element alone, without reading its domain.
    #[arg(long, default_value_t = false)]
    pub(crate) no_resolve: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DdicTypeArg {
    /// Data element.
    Dtel,
    /// Domain.
    Doma,
}

#[derive(Debug, Subcommand)]
pub enum TableCommand {
    /// Preview rows from one table or view.
    Data(TableDataArgs),
    /// Show fields, keys, declared types, and DDIC column metadata for one table.
    Metadata(TableMetadataArgs),
}

#[derive(Debug, Subcommand)]
pub enum TransportCommand {
    /// List change requests owned by a user.
    List(TransportListArgs),
    /// Create a workbench change request.
    Create(TransportCreateArgs),
    /// Show one request: its metadata, its tasks, and the objects it holds.
    Show(TransportShowArgs),
}

#[derive(Debug, Args)]
pub struct TransportShowArgs {
    /// Request number. A task number resolves to its parent request, which is
    /// what SAP returns.
    pub(crate) number: String,
}

#[derive(Debug, Args)]
pub struct TransportCreateArgs {
    /// Short description SAP stores as the request text.
    #[arg(long)]
    pub(crate) description: String,
    /// Package whose transport layer decides the target system. Required:
    /// SAP refuses to create a request without one.
    #[arg(long)]
    pub(crate) package: String,
}

#[derive(Debug, Args)]
pub struct TransportListArgs {
    /// Owner to list requests for. Defaults to the profile's user.
    #[arg(long)]
    pub(crate) user: Option<String>,
    /// List released requests instead of modifiable ones.
    #[arg(long, default_value_t = false)]
    pub(crate) released: bool,
}

#[derive(Debug, Subcommand)]
pub enum EditCommand {
    /// Create an empty object shell. Fill it with `edit set`; does not activate.
    Create(EditObjectCreateArgs),
    /// Delete an object. Refuses while anything still references it.
    Delete(EditObjectDeleteArgs),
    /// Read complete source and revision metadata for a future edit.
    Read(EditSourceReadArgs),
    /// Replace one exact fragment and save inactive source. Does not activate.
    Patch(EditSourcePatchArgs),
    /// Replace complete source and save it inactive. Does not activate.
    Set(EditSourceSetArgs),
    /// Replace the complete XML of an object that has no source, such as DTEL
    /// or DOMA. Read it first with `object xml`.
    SetXml(EditXmlSetArgs),
    /// Run SAP's syntax checker against a stored source version.
    Check(EditSourceCheckArgs),
    /// Activate and verify an object's stored inactive source.
    Activate(EditSourceActivateArgs),
    /// Discard inactive changes while preserving the current active source.
    Discard(EditSourceDiscardArgs),
}

#[derive(Debug, Args)]
pub struct EditObjectCreateArgs {
    /// Object type to create: a source type (PROG, CLAS, INTF, DDLS, TABL, STRU, BDEF, SRVD, DDLX, DCLS) or a metadata type (DTEL, DOMA, TTYP, MSAG).
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// Object name, which must be inside a configured customer namespace.
    #[arg(long)]
    pub(crate) name: String,
    /// Package the object is created in. Use $TMP for a local object.
    #[arg(long)]
    pub(crate) package: String,
    /// Short description SAP stores as the object's text.
    #[arg(long)]
    pub(crate) description: String,
    /// Parent transport request, required for a transportable package.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Service definition a SRVB binding exposes. Only for --type SRVB.
    #[arg(long)]
    pub(crate) service_definition: Option<String>,
    /// Protocol a SRVB binding exposes its service over: odata-v2-ui,
    /// odata-v2-web-api, odata-v4-ui, or odata-v4-web-api. Only for --type SRVB.
    #[arg(long)]
    pub(crate) binding_type: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditObjectDeleteArgs {
    /// Object type to delete.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// Object name, which must be inside a configured customer namespace.
    #[arg(long)]
    pub(crate) name: String,
    /// Parent transport request, required for a transportable object.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Delete even though other objects still reference this one.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
    /// Report what would be deleted, and what references it, without deleting.
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub struct EditSourceReadArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, or DCLS.
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
    /// Source object type: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, or DCLS.
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
    /// Source object type: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, or DCLS.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Parent CTS change request to attach before activation, for example AB1K900575.
    #[arg(long)]
    pub(crate) transport: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditSourceDiscardArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, or DCLS.
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
    /// Source object type: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, or DCLS.
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
    /// Parent CTS change request to associate with the write, for example AB1K900575.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Validate and preview the change without locking or writing the object.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub struct EditSourceSetArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, or DCLS.
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
    /// Parent CTS change request to associate with the write, for example AB1K900575.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Validate and preview the complete replacement without locking or writing.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub struct EditXmlSetArgs {
    /// Metadata object type: DTEL or DOMA.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Read the complete replacement XML from this file, or - for stdin.
    #[arg(long, value_name = "PATH", allow_hyphen_values = true)]
    pub(crate) xml_file: String,
    /// Parent CTS change request to associate with the write.
    #[arg(long)]
    pub(crate) transport: Option<String>,
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{Cli, Command, EditCommand, ObjectCommand, SystemCommand, TableCommand};
    use fractal::suggested_command;

    /// Every command an error can offer as a remedy must actually parse as a
    /// `fractal` invocation. Without this, renaming a flag or a subcommand in
    /// this file would silently start handing callers instructions that do not
    /// run, and nothing would fail to compile.
    #[test]
    fn every_suggested_command_parses_as_a_cli_invocation() {
        let commands = [
            suggested_command::system_test(),
            suggested_command::edit_read("CLAS", "ZCL_SAMPLE", "inactive"),
            suggested_command::edit_read("CLAS", "/ACME/EXAMPLE", "active"),
            suggested_command::edit_check("PROG", "ZSAMPLE", "inactive"),
            suggested_command::object_kinds(),
            suggested_command::object_search("INTF", "ZIF_SAMPLE"),
            suggested_command::object_xml("/sap/bc/adt/ddic/domains/zdomain"),
            suggested_command::table_metadata("ZDEMO_EVENT_LOG"),
        ];

        for command in commands {
            assert!(
                Cli::try_parse_from(command.split(' ')).is_ok(),
                "suggested command does not parse: {command}"
            );
        }
    }

    #[test]
    fn suggested_commands_bind_their_arguments_to_the_intended_flags() {
        // Parsing is not enough: a renamed flag could still parse while
        // carrying the value into the wrong field.
        let cli = Cli::try_parse_from(
            suggested_command::edit_read("CLAS", "ZCL_SAMPLE", "inactive").split(' '),
        )
        .unwrap();
        let Command::Edit {
            command: EditCommand::Read(args),
        } = cli.command
        else {
            panic!("expected edit read");
        };
        assert_eq!(args.object_type, "CLAS");
        assert_eq!(args.name, "ZCL_SAMPLE");
        assert_eq!(args.version, super::EditSourceVersionArg::Inactive);

        let cli =
            Cli::try_parse_from(suggested_command::object_search("INTF", "ZIF_SAMPLE").split(' '))
                .unwrap();
        let Command::Object {
            command: ObjectCommand::Search(args),
        } = cli.command
        else {
            panic!("expected object search");
        };
        assert_eq!(args.query, "ZIF_SAMPLE");
        assert_eq!(args.kind.as_deref(), Some("INTF"));

        let cli =
            Cli::try_parse_from(suggested_command::table_metadata("ZDEMO").split(' ')).unwrap();
        let Command::Table {
            command: TableCommand::Metadata(args),
        } = cli.command
        else {
            panic!("expected table metadata");
        };
        assert_eq!(args.name, "ZDEMO");

        let cli = Cli::try_parse_from(suggested_command::system_test().split(' ')).unwrap();
        assert!(matches!(
            cli.command,
            Command::System {
                command: SystemCommand::Test
            }
        ));
    }
}

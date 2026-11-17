use clap::{Args, Parser, Subcommand, ValueEnum};
use fractal::sap::{adt_version::AdtVersion, class_include::ClassInclude};

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
    /// Probes used while building a feature. Not a supported surface.
    #[command(hide = true)]
    Internal {
        #[command(subcommand)]
        command: InternalCommand,
    },
    /// Inspect and manage change requests in the transport system.
    Transport {
        #[command(subcommand)]
        command: TransportCommand,
    },
    /// Inspect what Fractal recorded before it changed an object.
    ///
    /// A short-horizon undo buffer for the active version, not a backup. Only
    /// operations that change the active version are recorded — an activation
    /// and a delete — and entries are pruned by age and by count.
    ///
    /// Inactive edits are not covered. `edit set`, `edit patch` and
    /// `edit set-xml` stage work without publishing it; what gets recorded is
    /// the activation that follows.
    ///
    /// The journal is local to this machine and this user.
    Journal {
        #[command(subcommand)]
        command: JournalCommand,
    },
    /// Write agent-harness permission rules for Fractal's mutating commands.
    Guard {
        #[command(subcommand)]
        command: GuardCommand,
    },
    /// Delete a repository object.
    ///
    /// Kept out of `edit` deliberately. This is the one irreversible verb, and
    /// a permission rule in an agent harness matches `fractal delete` by
    /// prefix, with nothing read-only sharing that prefix.
    Delete(ObjectDeleteArgs),
    /// Reverse an operation Fractal recorded, restoring what was there before.
    ///
    /// Today that means an activation, which is reversed in three steps:
    /// the previous active version is written as inactive, activated, and the
    /// pending work the original activation consumed is put back. More of the
    /// journal is expected to become undoable — a delete is already recorded in
    /// full, and `fractal journal show` prints the steps to restore one by hand
    /// until that is automated.
    ///
    /// It acts only on operations this machine's journal recorded, and only
    /// while the object still holds what was activated — otherwise it refuses
    /// and prints where the content is.
    ///
    /// A top-level verb for the same reason `delete` is one: it mutates, and a
    /// harness rule matches it by prefix.
    Undo(UndoArgs),
    /// Show and publish OData service bindings.
    ///
    /// An activated binding is not yet a service: nothing answers at its URL
    /// until it is published, which registers it with the gateway. Whether a
    /// binding is OData V2 or V4 was fixed when it was created; it publishes
    /// as what it is.
    Binding {
        #[command(subcommand)]
        command: BindingCommand,
    },
    /// Run ABAP Unit tests and say whether they are green.
    ///
    ///   fractal test --type CLAS --name ZRAP_BP_TRAVEL
    ///   fractal test --type CLAS --name ZRAP_BP_TRAVEL --test-class LTC_TRAVEL
    ///   fractal test --type CLAS --name ZRAP_BP_TRAVEL --test-class LTC_TRAVEL --method VALIDATES_DATES
    ///   fractal test --object CLAS:ZRAP_BP_TRAVEL --object DEVC:ZRAP_TRAVEL
    ///
    /// Targets are classes (CLAS), programs (PROG), function groups (FUGR) or
    /// packages (DEVC, with their sub-packages). Several targets use one
    /// --object TYPE:NAME each (type, a colon, the name; no spaces).
    ///
    /// Green means at least one test passed and nothing failed, errored, was
    /// skipped, or blocked a whole test class (a failing class_setup) or
    /// program (syntax errors). Exit code 0 is green; 1 is a run that
    /// completed and is not green, with the failures in the output; 2 is a
    /// run that could not be done.
    ///
    /// Tests run as your SAP user. Harmless and dangerous tests, short and
    /// medium, run by default; dangerous tests may change data. Critical and
    /// long tests run only when named in --risk and --duration.
    #[command(verbatim_doc_comment)]
    Test(TestArgs),
    /// Read and safely edit supported source-based repository objects.
    Edit {
        #[command(subcommand)]
        command: EditCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum BindingCommand {
    /// Show a binding's protocol and, for each service version, whether it is
    /// published, its URL and its entity sets.
    ///
    /// Reports the binding's own `published` flag beside the registration's,
    /// because the two can disagree; the registration is the one that holds.
    Show(BindingShowArgs),
    /// Publish one service version of an active binding, and confirm it.
    ///
    /// Sends nothing when the service is already published.
    Publish(BindingPublishArgs),
    /// Unpublish one service version of a binding, and confirm it.
    ///
    /// Sends nothing when the service is not published. A published binding
    /// cannot be deleted, so this comes first.
    Unpublish(BindingPublishArgs),
    /// Read from a binding's published service: GET only.
    ///
    /// PATH is the part of the URL after the service root, written as in
    /// /IWFND/GW_CLIENT; the root is taken from the service's registration.
    /// Single-quote it so the shell leaves `$` alone. Any binding can be read,
    /// not only one in the customer namespaces.
    ///
    ///   fractal binding get --name ZSAMPLE_UI_O4 '$metadata'
    ///   fractal binding get --name ZSAMPLE_UI_O4 'Items?$top=5&$count=true'
    ///   fractal binding get --name ZSAMPLE_UI_O4 "Items('A1')?\$expand=_Child"
    ///   fractal binding get --name ZSAMPLE_UI_O2 'Items?$top=5&$format=json'
    ///
    /// OData V2 answers Atom XML unless the path asks for `$format=json`; V4
    /// answers JSON. Reads go through the service, so its access control
    /// applies: an empty result may mean the DCLS filtered every row out.
    #[command(verbatim_doc_comment)]
    Get(BindingGetArgs),
}

#[derive(Debug, Args)]
pub struct BindingGetArgs {
    /// Service binding name.
    #[arg(long)]
    pub(crate) name: String,
    /// Service version to read, such as 0001. Needed only when the binding
    /// has more than one.
    #[arg(long)]
    pub(crate) service_version: Option<String>,
    /// The URL after the service root: entity set, key, navigation,
    /// `$metadata`, and query options. Empty reads the service document.
    #[arg(default_value = "")]
    pub(crate) path: String,
}

#[derive(Debug, Args)]
pub struct BindingShowArgs {
    /// Service binding name.
    #[arg(long)]
    pub(crate) name: String,
}

#[derive(Debug, Args)]
pub struct BindingPublishArgs {
    /// Service binding name.
    #[arg(long)]
    pub(crate) name: String,
    /// Service version to act on, such as 0001. Needed only when the binding
    /// has more than one.
    #[arg(long)]
    pub(crate) service_version: Option<String>,
    /// Read the current state and report the request without sending it.
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub struct TestArgs {
    /// What to test: CLAS, PROG, FUGR or DEVC. Not with --object.
    #[arg(
        long = "type",
        required_unless_present = "objects",
        conflicts_with = "objects"
    )]
    pub(crate) object_type: Option<String>,
    /// Object or package name. Not with --object.
    #[arg(long, required_unless_present = "objects", conflicts_with = "objects")]
    pub(crate) name: Option<String>,
    /// One target of several, written TYPE:NAME, such as CLAS:ZRAP_BP_TRAVEL.
    /// Repeat it once per target.
    #[arg(long = "object", value_name = "TYPE:NAME")]
    pub(crate) objects: Vec<String>,
    /// Run only this local test class of the class named by --name.
    #[arg(long, requires = "name")]
    pub(crate) test_class: Option<String>,
    /// Run only this method of --test-class. Repeat it for more.
    #[arg(long = "method", requires = "test_class")]
    pub(crate) methods: Vec<String>,
    /// Risk levels to run, comma-separated: harmless, dangerous, critical.
    #[arg(long, value_delimiter = ',', default_value = "harmless,dangerous")]
    pub(crate) risk: Vec<RiskArg>,
    /// Durations to run, comma-separated: short, medium, long.
    #[arg(long, value_delimiter = ',', default_value = "short,medium")]
    pub(crate) duration: Vec<DurationArg>,
    /// Seconds to wait for SAP to finish every test.
    #[arg(long, default_value_t = 300)]
    pub(crate) timeout: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RiskArg {
    Harmless,
    Dangerous,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum DurationArg {
    Short,
    Medium,
    Long,
}

#[derive(Debug, Subcommand)]
pub enum AuthCommand {
    /// Save or update a profile and its password in the OS keychain.
    Login(LoginArgs),
    /// List saved profile names.
    List,
    /// Change what a profile may edit, or make it the default, without
    /// touching its stored password.
    Set(AuthSetArgs),
    /// Change a profile's stored password, and nothing else.
    Password(AuthPasswordArgs),
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
    /// Packages whose objects this profile may edit. Repeat for multiple
    /// patterns. Without this, every package is editable. `$TMP` stays editable
    /// either way unless `auth set --allow-temporary-package false` says not.
    #[arg(long)]
    pub(crate) package: Vec<String>,
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

#[derive(Debug, Args)]
pub struct AuthPasswordArgs {
    /// Profile whose password to change. Defaults to the selected profile.
    pub(crate) name: Option<String>,
    /// Read the password from standard input instead of prompting.
    #[arg(long, default_value_t = false)]
    pub(crate) password_stdin: bool,
    /// Store the password in the plain, unencrypted file instead of the OS
    /// credential store. Without this it stays where it was kept.
    #[arg(long, default_value_t = false)]
    pub(crate) store_plaintext: bool,
}

#[derive(Debug, Args)]
pub struct AuthSetArgs {
    /// Profile to change. Defaults to the selected profile.
    pub(crate) name: Option<String>,
    /// Replace the packages this profile may edit. Repeat for multiple patterns.
    #[arg(long)]
    pub(crate) package: Vec<String>,
    /// Grant no packages at all, making every object read-only for this
    /// profile. Distinct from having never set a list, which grants everything.
    #[arg(long, default_value_t = false, conflicts_with = "package")]
    pub(crate) no_package: bool,
    /// Remove the package restriction entirely, so every package is editable
    /// again. The way back from --package or --no-package.
    #[arg(
        long,
        default_value_t = false,
        conflicts_with_all = ["package", "no_package"]
    )]
    pub(crate) any_package: bool,
    /// Replace the customer namespace patterns. Repeat for multiple patterns.
    #[arg(long)]
    pub(crate) namespace: Vec<String>,
    /// Whether the `$TMP` scratch package stays editable regardless of --package.
    #[arg(long)]
    pub(crate) allow_temporary_package: Option<bool>,
    /// Make this the default profile, replacing the current default.
    #[arg(long, default_value_t = false)]
    pub(crate) default: bool,
}

#[derive(Debug, Subcommand)]
pub enum JournalCommand {
    /// List recorded operations, newest first.
    List(JournalListArgs),
    /// Show one entry and the paths of the content it holds.
    ///
    /// Prints paths rather than content, so `diff` and an editor can do the
    /// work. For a delete it also prints what recreating the object needs.
    Show(JournalShowArgs),
    /// Apply the retention policy, then delete content nothing references.
    ///
    /// Destroys before-images permanently. The newest entry for each object is
    /// always kept, whatever its age.
    Clear(JournalClearArgs),
}

#[derive(Debug, Args)]
pub struct JournalListArgs {
    /// Only entries for objects of this type.
    #[arg(long = "type")]
    pub(crate) object_type: Option<String>,
    /// Only entries for this object name.
    #[arg(long)]
    pub(crate) name: Option<String>,
    /// Most recent entries to show.
    #[arg(long, default_value_t = 20)]
    pub(crate) limit: usize,
    /// List every system's entries, not only the selected profile's.
    #[arg(long, default_value_t = false)]
    pub(crate) all_systems: bool,
}

#[derive(Debug, Args)]
pub struct JournalShowArgs {
    /// Entry id, as printed by `fractal journal list`.
    pub(crate) id: String,
    /// Search every system, not only the selected profile's.
    #[arg(long, default_value_t = false)]
    pub(crate) all_systems: bool,
}

#[derive(Debug, Args)]
pub struct JournalClearArgs {
    /// Drop entries older than this many days.
    #[arg(long)]
    pub(crate) older_than: Option<u64>,
    /// Entries to keep per object.
    #[arg(long)]
    pub(crate) keep: Option<usize>,
    /// Report what would be removed without removing it.
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub struct UndoArgs {
    /// Entry id, as printed by `fractal journal list`.
    #[arg(long, conflicts_with_all = ["object_type", "name"])]
    pub(crate) entry: Option<String>,
    /// Object type, to undo that object's most recent recorded activation.
    #[arg(long = "type", requires = "name")]
    pub(crate) object_type: Option<String>,
    /// Object name, to undo its most recent recorded activation.
    #[arg(long, requires = "object_type")]
    pub(crate) name: Option<String>,
    /// Change request the restore is recorded in, for a transportable object.
    ///
    /// Never defaulted to the transport in the entry: that request may since
    /// have been released, and SAP would refuse it.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Report what undoing would do, and change nothing.
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
    /// Proceed past a refusal that is a judgement call, and report which.
    #[arg(long, default_value_t = false)]
    pub(crate) force: bool,
}

#[derive(Debug, Subcommand)]
pub enum GuardCommand {
    /// Write the rules into an agent harness's settings file.
    Install(GuardInstallArgs),
    /// Decide one tool call, reading a `PreToolUse` payload from stdin.
    ///
    /// Not meant to be run by hand: `guard install --harness codex` points the
    /// harness at this, because Codex's permission layer is a hook program
    /// rather than a list of rules.
    Hook(GuardHookArgs),
}

#[derive(Debug, Args)]
pub struct GuardHookArgs {
    /// Never answer `ask`, for a harness that parses that decision but cannot
    /// act on it. Those commands get no opinion instead.
    #[arg(long, default_value_t = false)]
    pub(crate) no_ask: bool,
}

#[derive(Debug, Args)]
pub struct GuardInstallArgs {
    /// Agent harness to configure. Defaults to Claude Code.
    #[arg(long, value_enum)]
    pub(crate) harness: Option<GuardHarnessArg>,
    /// Project directory to write into. Defaults to the current directory.
    #[arg(long)]
    pub(crate) dir: Option<std::path::PathBuf>,
    /// Write to the personal, gitignored settings file instead of the shared
    /// project one.
    #[arg(long, default_value_t = false)]
    pub(crate) local: bool,
    /// Prompt for every mutating command instead of refusing the destructive
    /// ones outright.
    #[arg(long, default_value_t = false)]
    pub(crate) ask_only: bool,
    /// Show what would be written without writing it.
    #[arg(long, default_value_t = false)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GuardHarnessArg {
    /// Claude Code, via `.claude/settings.json`.
    Claude,
    /// Codex, via a `PreToolUse` hook in `.codex/hooks.json`.
    Codex,
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
    /// Create a package, and confirm it by reading it back.
    ///
    /// A name starting with $ makes a local package, which is never
    /// transported and takes no transport. Any other needs --transport, and a
    /// software component and transport layer, which default to the parent's
    /// when --parent is given. Delete one with `fractal delete --type DEVC`;
    /// only an empty package is deleted.
    Create(PackageCreateArgs),
}

#[derive(Debug, Args)]
pub struct PackageCreateArgs {
    /// Package name: a customer name such as ZPROJ_UTIL, or $NAME for a local package.
    #[arg(long)]
    pub(crate) name: String,
    /// Short description SAP stores as the package's text.
    #[arg(long)]
    pub(crate) description: String,
    /// Parent (super) package. Its software component and transport layer are
    /// the defaults.
    #[arg(long)]
    pub(crate) parent: Option<String>,
    /// Software component, such as HOME. Required without --parent for a
    /// transportable package.
    #[arg(long)]
    pub(crate) software_component: Option<String>,
    /// Transport layer. Required without --parent for a transportable package.
    #[arg(long)]
    pub(crate) transport_layer: Option<String>,
    /// Transport request the package is recorded in. Required unless the
    /// package is local.
    #[arg(long)]
    pub(crate) transport: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum ObjectCommand {
    /// Search for repository objects by name.
    Search(SearchArgs),
    /// Read source for an ADT object URI.
    ///
    /// Reads the active version by default. A read that names no version is
    /// served the inactive one whenever it exists, which is why this always
    /// names one.
    Source(SourceArgs),
    /// Read metadata XML for an ADT object URI.
    ///
    /// Reads the active version by default and reports which version arrived,
    /// which is not always the one requested: SAP serves the other layer rather
    /// than refusing when the requested one does not exist.
    Xml(XmlArgs),
    /// Read the authoritative short description for an ADT object URI.
    Info(UriArgs),
    /// Show one data element, domain, structure, table, view or CDS entity by
    /// name.
    ///
    /// A data element resolves to its domain. A structure lists its fields,
    /// with anything an include or append contributed already in place — and
    /// so do tables and views, which are labelled as what they are rather than
    /// as structures.
    ///
    /// A CDS entity (a view entity, a projection) lists its fields with their
    /// keys, types and currency or unit references, its associations with
    /// their targets, cardinality and kind (association, composition, to
    /// parent), and its parameters. Its annotations are left out unless
    /// --annotations is passed. A CDS entity is described as it is active:
    /// one never activated has no field list yet.
    ///
    /// Reads the active version by default, and reports which version it got.
    /// Those are two separate answers: SAP serves the other layer rather than
    /// refusing when the requested one does not exist, and an object that has
    /// never been activated declares itself "new".
    Show(ObjectShowArgs),
    /// Find objects that reference an ADT object URI (where-used).
    Usages(UsagesArgs),
    /// List known repository kinds with plain-text descriptions.
    Kinds,
}

#[derive(Debug, Args)]
pub struct ObjectShowArgs {
    /// Data element, domain, structure, table, view or CDS entity name.
    pub(crate) name: String,
    /// Skip detection and read this type directly.
    #[arg(long = "type", value_enum)]
    pub(crate) object_type: Option<ObjectShowTypeArg>,
    /// Report the data element alone, without reading its domain. Means
    /// nothing for a domain or a structure.
    #[arg(long, default_value_t = false)]
    pub(crate) no_resolve: bool,
    /// Also report a CDS entity's annotations, for the entity and for each
    /// field. Left out unless this is passed: a standard view carries dozens
    /// per field. Means nothing for other kinds.
    #[arg(long, default_value_t = false)]
    pub(crate) annotations: bool,
    /// Stored version to read. A resolved domain is read at the same version.
    /// If the requested version does not exist, SAP serves the other one, and
    /// the reported version says which arrived.
    #[arg(long, value_enum, default_value = "active")]
    pub(crate) version: VersionArg,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ObjectShowTypeArg {
    /// Data element.
    Dtel,
    /// Domain.
    Doma,
    /// Structure.
    Stru,
    /// CDS entity: a view entity, projection or other DDL-defined entity.
    Ddls,
}

#[derive(Debug, Subcommand)]
pub enum TableCommand {
    /// Preview rows from one table or view.
    Data(TableDataArgs),
    /// Show fields, keys, declared types, and DDIC column metadata for one table.
    Metadata(TableMetadataArgs),
    /// Change fields of one row, addressed by its complete key.
    Set(TableSetArgs),
    /// Add one row. Every key field is given with --set, like any other field.
    Insert(TableSetArgs),
    /// Delete one row, addressed by its complete key.
    Remove(TableSetArgs),
}

#[derive(Debug, Args)]
pub struct TableSetArgs {
    /// DDIC table name, which must be inside a configured customer namespace.
    pub(crate) name: String,
    /// One key field, as FIELD=VALUE. Repeat until the key is complete; the
    /// client field is the session's and is never given here.
    #[arg(long = "key", value_name = "FIELD=VALUE")]
    pub(crate) keys: Vec<String>,
    /// One field to change, as FIELD=VALUE. Repeat for more.
    #[arg(long = "set", value_name = "FIELD=VALUE")]
    pub(crate) sets: Vec<String>,
    /// Commit the change. Without this the statement runs and is rolled back.
    #[arg(long)]
    pub(crate) execute: bool,
    /// Change request or task to record a customizing change in. Required for a
    /// delivery-class C or G table, ignored for an application table.
    #[arg(long)]
    pub(crate) transport: Option<String>,
}

#[derive(Debug, Subcommand)]
pub enum InternalCommand {
    /// Run an already-activated class through the ADT console endpoint.
    ClassRun(ClassRunArgs),
}

#[derive(Debug, Args)]
pub struct ClassRunArgs {
    /// Name of an activated class implementing `if_oo_adt_classrun`.
    pub(crate) class: String,
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
    /// Read complete source and revision metadata for a future edit.
    ///
    /// Defaults to the **inactive** version, which is the one a later write
    /// replaces and so the one to build an edit on. When nobody has staged
    /// anything, that is the active source, because SAP falls back. Pass
    /// `--version active` to read what is running instead.
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
    /// List what you have staged and not activated, grouped by transport.
    ///
    /// SAP keeps this list per user, so it shows your inactive objects only.
    /// Each is also written TYPE:NAME, the form `edit activate --object` takes.
    Inactive,
    /// Activate and verify an object's stored inactive source.
    ///
    /// One object:
    ///
    ///   fractal edit activate --type DDLS --name ZRAP_R_TRAVEL
    ///
    /// Several objects in one request, with one --object per object, each
    /// written TYPE:NAME (type, a colon, then the name; no spaces):
    ///
    ///   fractal edit activate --object DDLS:ZRAP_R_TRAVEL --object DDLS:ZRAP_R_BOOKING
    ///
    /// Use the group form for objects that need each other to activate, such
    /// as a new root view with a composition and its child view with an
    /// association to parent: neither can be activated first on its own. The
    /// group skips Fractal's own syntax pre-check, which cannot pass for such a
    /// pair; SAP still checks every object as it activates. SAP does not roll
    /// a group back: if one object fails, others may still activate, so each
    /// object's outcome is reported, and running the same command again after
    /// a fix finishes the group. Covers source-based types only.
    #[command(verbatim_doc_comment)]
    Activate(EditActivateArgs),
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
pub struct ObjectDeleteArgs {
    /// Object type to delete. DEVC deletes a package, and only an empty one.
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
    /// Delete without recording what the object held.
    ///
    /// The journal keeps the only copy of what a delete destroys, so this
    /// removes the sole route back.
    #[arg(long, default_value_t = false)]
    pub(crate) no_journal: bool,
}

#[derive(Debug, Args)]
pub struct EditSourceReadArgs {
    /// Source object type: CLAS, INTF, PROG, DDLS, TABL, STRU, BDEF, SRVD, DDLX, or DCLS.
    #[arg(long = "type")]
    pub(crate) object_type: String,
    /// ABAP repository object name.
    #[arg(long)]
    pub(crate) name: String,
    /// Stored source version to request. Defaults to the version a write would
    /// replace: the inactive one, or the active one when nothing is staged.
    #[arg(long, value_enum, default_value = "inactive")]
    pub(crate) version: VersionArg,
    /// Which of a class's source streams to read. Classes only; defaults to
    /// the global class.
    #[arg(long, value_enum)]
    pub(crate) include: Option<ClassIncludeArg>,
}

/// Which stored layer of an object to read. Shared by every command that
/// selects one, because they all become the same `?version=` on the request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum VersionArg {
    Active,
    Inactive,
}

/// One of a class's separately stored source streams, by ADT's own name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ClassIncludeArg {
    /// The global class.
    Main,
    /// Class-relevant local types.
    Definitions,
    /// Local types, including RAP handler and saver classes.
    Implementations,
    /// Macros.
    Macros,
    /// Test classes. A class has none until one is created.
    Testclasses,
}

impl From<ClassIncludeArg> for ClassInclude {
    fn from(include: ClassIncludeArg) -> Self {
        match include {
            ClassIncludeArg::Main => Self::Main,
            ClassIncludeArg::Definitions => Self::Definitions,
            ClassIncludeArg::Implementations => Self::Implementations,
            ClassIncludeArg::Macros => Self::Macros,
            ClassIncludeArg::Testclasses => Self::Testclasses,
        }
    }
}

impl From<VersionArg> for AdtVersion {
    fn from(version: VersionArg) -> Self {
        match version {
            VersionArg::Active => Self::Active,
            VersionArg::Inactive => Self::Inactive,
        }
    }
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
    pub(crate) version: VersionArg,
}

#[derive(Debug, Args)]
pub struct EditActivateArgs {
    /// Object type to activate: a source type (CLAS, INTF, PROG, DDLS, TABL,
    /// STRU, BDEF, SRVD, DDLX, DCLS) or a metadata type (DTEL, DOMA, TTYP,
    /// MSAG, SRVB). Not with --object.
    #[arg(
        long = "type",
        required_unless_present = "objects",
        conflicts_with = "objects"
    )]
    pub(crate) object_type: Option<String>,
    /// ABAP repository object name. Not with --object.
    #[arg(long, required_unless_present = "objects", conflicts_with = "objects")]
    pub(crate) name: Option<String>,
    /// One object of a group to activate together, written TYPE:NAME, such as
    /// DDLS:ZRAP_R_TRAVEL. Repeat it once per object. A namespaced name keeps
    /// its slashes: DDLS:/ACME/R_TRAVEL. Source-based types only.
    #[arg(long = "object", value_name = "TYPE:NAME")]
    pub(crate) objects: Vec<String>,
    /// Parent CTS change request to attach before activation, for example AB1K900575.
    #[arg(long)]
    pub(crate) transport: Option<String>,
    /// Skip the journal for this activation.
    ///
    /// The journal is the only record of what the active version was
    /// beforehand, so this gives up any chance of undoing it. Intended for a
    /// machine where the journal cannot be written at all.
    #[arg(long, default_value_t = false)]
    pub(crate) no_journal: bool,
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
    /// Which of a class's source streams to write. Classes only; defaults to
    /// the global class. Writing `testclasses` creates the include if the
    /// class has none yet.
    #[arg(long, value_enum)]
    pub(crate) include: Option<ClassIncludeArg>,
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
    /// Which of a class's source streams to write. Classes only; defaults to
    /// the global class. Writing `testclasses` creates the include if the
    /// class has none yet.
    #[arg(long, value_enum)]
    pub(crate) include: Option<ClassIncludeArg>,
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
    /// Replace another user's unactivated edit anyway.
    ///
    /// A whole-source write replaces the inactive version outright, so it
    /// destroys work somebody else has staged there. Without this, such a write
    /// is refused and names who staged it.
    #[arg(long)]
    pub(crate) force: bool,
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
    /// Refuse the write unless the stored document still has this SHA-256.
    ///
    /// Take it from `fractal object xml`, which reports the hash of what it
    /// read. SAP does not keep retrievable version content for these objects,
    /// so an overwrite based on a stale read is unrecoverable.
    #[arg(long)]
    pub(crate) expected_sha256: Option<String>,
    /// Replace another user's unactivated edit anyway.
    ///
    /// A whole-document write replaces the inactive version outright, so it
    /// destroys work somebody else has staged there. Without this, such a write
    /// is refused and names who staged it.
    #[arg(long)]
    pub(crate) force: bool,
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
    /// Add each column's data element, DDIC type and key flag to the output.
    /// Costs one extra read, and applies to JSON output only.
    #[arg(long)]
    pub(crate) types: bool,
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
    /// ADT object URI, without a source suffix. A class include's own URI
    /// (`…/includes/<name>`) is also accepted.
    pub(crate) uri: String,
    /// Which of a class's source streams to read. Classes only; defaults to
    /// the global class.
    #[arg(long, value_enum)]
    pub(crate) include: Option<ClassIncludeArg>,
    /// Stored version to read. ABAP source carries no marker saying which
    /// version it is, so ask for the one you mean.
    #[arg(long, value_enum, default_value = "active")]
    pub(crate) version: VersionArg,
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
    /// Stored version to read. Pass `inactive` when reading a document in order
    /// to edit it: that is the version `edit set-xml` compares its
    /// --expected-sha256 against, so the two agree.
    #[arg(long, value_enum, default_value = "active")]
    pub(crate) version: VersionArg,
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
            suggested_command::edit_read_include(
                "CLAS",
                "ZCL_SAMPLE",
                "implementations",
                "inactive",
            ),
            suggested_command::binding_show("ZSAMPLE_UI_O4"),
            suggested_command::package_items("ZSAMPLE_PKG"),
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
        assert_eq!(args.version, super::VersionArg::Inactive);

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

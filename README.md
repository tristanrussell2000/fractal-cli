# Fractal

Fractal is a command-line tool for exploring and safely editing on-premise SAP S/4HANA
development systems through their native ADT HTTP APIs. It is built for both people and
coding agents: every command emits stable JSON when its output is piped, and readable text
when run in a terminal.

## Install

Releases are published on the
[GitHub Releases page](https://github.com/tristanrussell2000/fractal-cli/releases/latest).
You do not need Rust installed to use a release.

### Windows

Download and run `fractal-x86_64-pc-windows-msvc.msi`. It installs `fractal.exe` under
Program Files, adds it to your `PATH`, and registers an uninstaller in Apps & features.
Open a new terminal after installing.

If your machine blocks MSI installs, download `fractal-x86_64-pc-windows-msvc.zip` instead,
extract `fractal.exe` somewhere on your `PATH`, or run this in PowerShell:

```powershell
irm https://github.com/tristanrussell2000/fractal-cli/releases/latest/download/fractal-installer.ps1 | iex
```

### macOS and Linux

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/tristanrussell2000/fractal-cli/releases/latest/download/fractal-installer.sh | sh
```

The installer puts `fractal` in `~/.cargo/bin` and tells you if you need to
add that directory to your `PATH`. Plain `.tar.xz` archives for each platform are also on the
release page, with a `sha256.sum` file for checking downloads.

Linux stores passwords through the Secret Service D-Bus API. A desktop session with
GNOME Keyring or KWallet works. Headless servers, containers and WSL have no such service.

Getting one running there is not worth the effort, and a half-installed keyring is worse than
none: the library loads, so Fractal uses it, and every operation then fails with
`credential_store_error`. Supply the password another way instead — see
[Machines with no keychain](#machines-with-no-keychain).

### Check the install

```sh
fractal --version
fractal --help
```

### Update or uninstall

Run the installer or MSI for the new version over the old one. On Windows, uninstall from
Apps & features. On macOS and Linux, delete the `fractal` binary from the install directory.

Uninstalling does not remove your saved profiles or any stored passwords, including a
plaintext `credentials.toml` if you made one. Use `fractal auth remove <name>` for each
profile first if you want them gone.

## First use

Save a profile for each SAP system. In a terminal, `auth login` asks for anything you leave
out, so the simplest form is:

```sh
fractal auth login
```

It prompts for the profile name, base URL, SAP client, username, and password. The password
is typed without echo and stored in the operating-system credential store.

Every value can also be passed as a flag. Flags always win, and only the missing ones are
asked for:

```sh
fractal auth login DEV_100 --url https://sap.example:8001 --client 100 --username demo_user
```

In a script or pipeline there is no terminal to ask, so pass every value as a flag and feed the
password through `--password-stdin`:

```sh
printf '%s' "$SAP_PASSWORD" | fractal auth login DEV_100 \
  --url https://sap.example:8001 --client 100 --username demo_user --password-stdin
```

A missing value with no terminal is an error that names the flag to pass; the command never
hangs waiting on input. Then confirm the connection:

```sh
fractal system test
```

The first saved profile becomes the default. Later commands pick a profile in this order:
`--profile <name>`, then the `FRACTAL_PROFILE` environment variable, then the default.

Profile metadata is stored in a TOML file:

| Platform | Path |
|---|---|
| Windows | `%APPDATA%\issi\fractal\config\config.toml` |
| macOS | `~/Library/Application Support/com.issi.fractal/config.toml` |
| Linux | `~/.config/fractal/config.toml` |

### Machines with no keychain

WSL, containers and SSH sessions usually have no OS credential store. Fractal looks for a
password in this order, so any one of these works without a keychain:

1. `FRACTAL_PASSWORD_<PROFILE>` — the profile name uppercased, with anything that is not a
   letter or digit replaced by `_`. Profile `dev` reads `FRACTAL_PASSWORD_DEV`.
2. `FRACTAL_PASSWORD` — used for whichever profile is selected. Handy in CI with one profile.
3. The profile's `password_command`, if set.
4. The plaintext file, if you opted into one.
5. The OS credential store.

The best option on a machine without a keychain is a password manager, which keeps Fractal
out of the business of storing secrets entirely:

```sh
fractal auth login dev --url https://sap.example:8001 --client 100 --username demo_user \
  --password-command 'pass show sap/dev'
```

The command runs through your shell on each use and its standard output is the password, so
anything works: `pass`, the 1Password CLI, `gopass`, `vault read`. Nothing is stored by
Fractal. A command that fails is reported rather than quietly skipped.

As a last resort, keep the password in a plain file:

```sh
fractal auth login dev --url https://sap.example:8001 --client 100 --username demo_user \
  --store-plaintext
```

This is **not encrypted**. The file is created readable only by you (mode `0600`) next to
`config.toml` as `credentials.toml`, and every login that uses it says so. It is never chosen
automatically — a machine without a keychain gets an error naming these options instead, so
storing a password in the clear is always a choice you made.

`fractal auth remove <name>` clears both the plaintext file and the keychain.

### Changing a password

```sh
fractal auth password dev                 # prompts; or pipe it in with --password-stdin
```

This changes only the password, and keeps it where it was stored. `fractal auth list` shows
which source each profile actually uses. A `FRACTAL_PASSWORD_*` variable wins over anything
stored, so if one is set, `auth password` warns that the new password will not be used, and a
rejected login names the variable.

## Commands

```text
fractal auth       login | set | password | list | remove
fractal system     list | test
fractal package    tree | items | create
fractal object     search | source | xml | info | show | outline | definition | versions | version | usages | kinds
fractal table      data | metadata | set | insert | remove
fractal query      <complete SELECT, or - for stdin>
fractal transport  list | show | create
fractal edit       create | read | patch | set | set-xml | check | inactive | activate | discard
fractal binding    show | publish | unpublish | get
fractal delete     <one destructive verb, kept out of `edit` on purpose>
fractal journal    list | show | clear
fractal undo       <reverse a recorded activation or table write>
fractal test       <run ABAP Unit tests: exit 0 green, 1 not green, 2 could not run>
fractal dump       list | show
fractal atc        <run the ABAP Test Cockpit: exit 0 passed, 1 findings or incomplete, 2 could not run>
fractal guard      install
```

Every command and subcommand accepts `--help`.

When a `class-run` or a published service fails with a bare HTTP 500, the short dump is the
explanation. `fractal dump show --latest` reads your newest one: the runtime error, the source URI
and line it stopped at, the error analysis and the call stack. `fractal dump list` lists your
dumps from the last 24 hours, each with the command that reads it.

In a large class, `fractal object outline <URI>` lists the types, attributes, methods and local
classes with the lines each is declared and implemented at, and `fractal object source <URI>
--method NAME` reads just that method (`LCL_Y=>NAME` for a local class's), numbered with the
lines of its source. `fractal object definition <URI> --line 38 --symbol sum_of` says where a
name used on that line is declared, and `--implementation` where it is implemented.

`fractal object versions <URI>` lists an object's versions with the request, its description and
the author of each; `fractal object version <URI> 00005 --diff active` shows what changed since.
SAP writes a version when a transport is released or imported, so an object still in development
has only `active` and, when something is staged, `inactive`.

`fractal atc --type CLAS --name <CLASS>` (or repeated `--object TYPE:NAME`, or `--object DEVC:<PACKAGE>`)
runs the ABAP Test Cockpit with the system's own check variant, the one transport release uses,
and lists each finding at the source line it is at. It fails on priority 1 and 2 findings
(`--fail-on 3` for all), and when SAP could not run one of the checks. `--explain` adds each
check's documentation, which usually names the pragma that suppresses it.

### Output

- In a terminal, output is readable text.
- When piped or redirected, output is JSON.
- `--output json` or `--output readable` forces one mode.

Errors are written to stderr with a nonzero exit code. In JSON mode an error has a stable
`code`, an optional HTTP `status`, a `message`, and where possible a `hint` and a read-only
`suggested_command`.

### Editing safety

The `edit` commands only touch objects in your profile's customer namespaces (`Z*` and `Y*`
by default). `edit patch` and `edit set` save inactive source and never activate.
Activation is a separate, explicit `edit activate` step that syntax-checks first and
verifies the result. It works for metadata objects too (`DTEL`, `DOMA`, `TTYP`, `MSAG`,
`SRVB`), where there is no source to check and success is proved by reading the document
back as active. `fractal delete` refuses when other objects still reference the target
unless `--force` is given.

An activated service binding is not yet a service. `fractal binding publish --name <SRVB>`
registers it with the gateway, as OData V2 or V4 according to the binding, and confirms it by
reading the registration back; `binding unpublish` reverses it, and `binding show` reports the
service URL and entity sets. SAP refuses to delete a published binding, so unpublish first.
`fractal binding get --name <SRVB> 'Items?$top=5'` reads from the published service: you write
the part of the URL after the service root, as in `/IWFND/GW_CLIENT`, and Fractal supplies the
root. It is GET-only and works on any binding, not just your namespaces.

Objects that need each other to activate, such as a new root view and its child, are activated
together with one `--object TYPE:NAME` per object:

```bash
fractal edit activate --object DDLS:ZRAP_R_TRAVEL --object DDLS:ZRAP_R_BOOKING
```

`fractal edit inactive` lists what you have staged and not activated, grouped by transport, with
each object already written `TYPE:NAME`. SAP keeps this list per user, so it shows only yours.

`edit set-xml` replaces a whole DDIC document, and SAP keeps no retrievable version history for
those objects, so pass `--expected-sha256` (from `fractal object xml`) to refuse the write if the
document changed since you read it.

A profile can also restrict editing to particular packages, which is checked against the
object's own package on every mutation:

```bash
fractal auth set <profile> --package 'ZPROJ*' --package ZUTIL
fractal auth set <profile> --any-package          # remove the restriction
```

`$TMP` stays editable regardless, since local scratch objects are not shared code; turn that
off with `--allow-temporary-package false`. Deletion lives at `fractal delete` rather than
under `edit` so that a single prefix identifies the one irreversible verb.

### The edit journal and `fractal undo`

Before Fractal changes the **active** version of an object, it records what was there. That
record is what `fractal undo` restores.

```bash
fractal journal list                     # what was recorded, newest first
fractal journal show <entry-id>          # one entry, and where its content is on disk
fractal undo --type PROG --name ZFOO --dry-run
fractal undo --type PROG --name ZFOO
fractal journal clear --older-than 30    # apply the retention policy
```

**This is a short-horizon undo buffer, not a backup.** Be clear about what it does not cover:

- Only operations that reach the **active** version are recorded: `edit activate`,
  `fractal delete`, and `table set | insert | remove --execute`. Inactive edits are not. `edit set`, `edit patch` and `edit set-xml` stage
  work without publishing it, and what gets recorded is the activation that follows.
- Entries are pruned by age and by count, so old ones go. The newest entry for each object is
  always kept.
- The journal is **local to one machine and one user**, under your OS data directory. It is not
  shared, not transported, and not a substitute for a transport request.
- `--no-journal` on `edit activate` and `fractal delete` skips it entirely.

Undoing an activation is three steps, not one: the previous active version is written as
inactive, activated, and the pending work the activation consumed is put back — so the object
ends where it started on **both** layers rather than losing whatever was staged.

It refuses unless the object still holds what the entry recorded activating, so an undo cannot
quietly discard somebody else's later change. A refusal prints the path to the content anyway,
so the answer is "here is what it was" rather than "no". `--force` overrides the refusals that
are judgement calls and says which; it will not undo an object's first activation, because that
would mean deleting it.

Deletes are recorded but **not** restored automatically: recreating an object is create, write
and activate, and it needs a package and a transport. `fractal journal show` prints those steps
for you to run. One trap it warns about is real and unavoidable — SAP's editing lock outlives
the object, so recreating under the same name can be refused with
`403 ... is currently editing` until it clears.

### Running Fractal under a coding agent

Be clear about where the boundary is: **nothing this CLI checks about its own invocation can
stop the agent that invoked it.** The caller supplies the arguments, the environment and the
config file, so any confirmation flag is a flag the caller simply passes. The layer that can
refuse is your agent harness's permission system, which decides before the command runs.

`fractal guard install` writes those rules for you:

```bash
fractal guard install                    # .claude/settings.json in the current project
fractal guard install --harness codex    # .codex/hooks.json instead
fractal guard install --dry-run          # show what would be written
```

It denies the irreversible commands, asks for the ones that write, leaves read-only commands
alone, and merges into an existing settings file without removing anything already there.
`fractal undo` and `fractal journal clear` are on the ask list — the first publishes a version,
the second destroys the only record of what was replaced — and so are `fractal binding publish`
and `unpublish`, the table row writes (`table set`, `insert`, `remove`, dry runs included, since a
rule cannot see `--execute`), and `fractal internal class-run`, which runs ABAP.

One gap is worth stating rather than papering over: Claude Code's rules match a command
**prefix**, so they cannot see a flag. Approving `fractal edit activate` also approves
`fractal edit activate --no-journal`, which turns the record off. The Codex hook is handed the
whole command and says so in its reason; the declarative install prints a note admitting it
cannot.

The two harnesses work differently. Claude Code takes a list of rules. Codex instead runs a
`PreToolUse` hook program, so the installed hook calls `fractal guard hook` — which means
`fractal` has to be on the PATH Codex runs commands with. Codex also parses but does not yet act
on the `ask` decision, so only the refusals are enforced there; `guard install` says so rather
than claiming coverage it cannot deliver.

## Building from source

Requires a current stable Rust toolchain.

```sh
cargo build --release
./target/release/fractal --help
```

Before opening a pull request:

```sh
cargo fmt --all -- --check
cargo test --all-targets
cargo clippy --all-targets --all-features -- -D warnings
cargo pedantic
```

Tests run offline against local mock servers. Nothing in `cargo test` needs an SAP system.

## Releasing

Releases are built by GitHub Actions with [cargo-dist](https://opensource.axo.dev/cargo-dist/).
Bump `version` in `Cargo.toml`, commit, then tag and push:

```sh
git tag v0.2.0
git push origin v0.2.0
```

The workflow builds every supported target, creates the installers and checksums, and
attaches them to a GitHub Release for that tag.

Supported targets:

| Target | Artifacts |
|---|---|
| `x86_64-pc-windows-msvc` | MSI, ZIP, PowerShell installer |
| `aarch64-apple-darwin` | tar.xz, shell installer |
| `x86_64-apple-darwin` | tar.xz, shell installer |
| `x86_64-unknown-linux-gnu` | tar.xz, shell installer |

## License

MIT

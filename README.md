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

## Commands

```text
fractal auth       login | set | list | remove
fractal system     list | test
fractal package    tree | items
fractal object     search | source | xml | info | usages | kinds
fractal ddic       show
fractal table      data | metadata
fractal query      <complete SELECT, or - for stdin>
fractal transport  list | show | create
fractal edit       create | read | patch | set | set-xml | check | activate | discard
fractal delete     <one destructive verb, kept out of `edit` on purpose>
fractal guard      install
```

Every command and subcommand accepts `--help`.

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
verifies the result. `fractal delete` refuses when other objects still reference the target
unless `--force` is given.

A profile can also restrict editing to particular packages, which is checked against the
object's own package on every mutation:

```bash
fractal auth set <profile> --package 'ZPROJ*' --package ZUTIL
fractal auth set <profile> --any-package          # remove the restriction
```

`$TMP` stays editable regardless, since local scratch objects are not shared code; turn that
off with `--allow-temporary-package false`. Deletion lives at `fractal delete` rather than
under `edit` so that a single prefix identifies the one irreversible verb.

### Running Fractal under a coding agent

Be clear about where the boundary is: **nothing this CLI checks about its own invocation can
stop the agent that invoked it.** The caller supplies the arguments, the environment and the
config file, so any confirmation flag is a flag the caller simply passes. The layer that can
refuse is your agent harness's permission system, which decides before the command runs.

`fractal guard install` writes those rules for you:

```bash
fractal guard install            # .claude/settings.json in the current project
fractal guard install --dry-run  # show the rules without writing them
```

It denies the irreversible commands, asks for the ones that write, leaves read-only commands
alone, and merges into an existing settings file without removing anything already there.

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

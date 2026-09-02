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
GNOME Keyring or KWallet works. Headless servers and plain WSL do not have one, so
`auth login` will fail there with `credential_store_error`.

### Check the install

```sh
fractal --version
fractal --help
```

### Update or uninstall

Run the installer or MSI for the new version over the old one. On Windows, uninstall from
Apps & features. On macOS and Linux, delete the `fractal` binary from the install directory.

Uninstalling does not remove your saved profiles or keychain passwords. Use
`fractal auth remove <name>` for each profile first if you want them gone.

## First use

Save a profile for each SAP system. The password is prompted for and stored in the
operating-system credential store, never in a file.

```sh
fractal auth login DEV_100 --url https://sap.example:8001 --client 100 --username demo_user
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

## Commands

```text
fractal auth       login | list | remove
fractal system     list | test
fractal package    tree | items
fractal object     search | source | xml | info | usages | kinds
fractal table      data | metadata
fractal query      <complete SELECT, or - for stdin>
fractal transport  list | show | create
fractal edit       create | delete | read | patch | set | check | activate | discard
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
verifies the result. `edit delete` refuses when other objects still reference the target
unless `--force` is given.

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

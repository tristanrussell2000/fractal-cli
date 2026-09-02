use crate::reportable_error::ReportableError;
use keyring_core::Entry;
use thiserror::Error;

const KEYCHAIN_SERVICE: &str = "fractal";

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("invalid profile name for credential storage: {0}")]
    InvalidProfileName(String),
    /// The store exists but refused the request.
    ///
    /// Distinct from [`Self::StoreUnavailable`], which is the store failing to
    /// initialise at all. Both mean "no keychain you can use here", so both
    /// carry the profile: the guidance names that profile's own environment
    /// variable, and advice you cannot copy is advice nobody follows.
    #[error("could not access the OS credential store: {source}")]
    Store {
        profile: String,
        #[source]
        source: keyring::Error,
    },
    #[error("no credential is stored for profile '{0}'")]
    Missing(String),
    #[error("the OS credential store could not be initialized: {0}")]
    StoreUnavailable(String),
    #[error("could not run the password command for profile '{profile}': {source}")]
    PasswordCommandFailed {
        profile: String,
        #[source]
        source: std::io::Error,
    },
    #[error("the password command for profile '{profile}' failed: {stderr}")]
    PasswordCommandRejected { profile: String, stderr: String },
    #[error("the password command for profile '{profile}' printed nothing")]
    PasswordCommandEmpty { profile: String },
    #[error("could not read stored passwords from {path}: {source}")]
    PlaintextRead {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("could not write stored passwords to {path}: {source}")]
    PlaintextWrite {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("stored passwords in {path} could not be parsed: {source}")]
    PlaintextMalformed {
        path: String,
        /// Boxed because a TOML parse error carries its span and source text,
        /// and this enum rides on every credential `Result`.
        #[source]
        source: Box<toml::de::Error>,
    },
    #[error(transparent)]
    Config(#[from] crate::config::ConfigError),
}

/// Where a password came from, so the caller can say so.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PasswordSource {
    /// An environment variable, named here because two can apply.
    Environment(String),
    /// The profile's `password_command`.
    Command,
    /// The opted-in plaintext file.
    PlaintextFile,
    /// The operating system's credential store.
    Keychain,
}

impl PasswordSource {
    #[must_use]
    pub fn describe(&self) -> String {
        match self {
            Self::Environment(variable) => format!("the {variable} environment variable"),
            Self::Command => "the profile's password command".to_owned(),
            Self::PlaintextFile => "the plaintext credentials file".to_owned(),
            Self::Keychain => "the OS credential store".to_owned(),
        }
    }
}

impl ReportableError for CredentialError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidProfileName(_) => "invalid_profile_name",
            Self::Store { .. } => "credential_store_error",
            Self::Missing(_) => "credential_missing",
            Self::StoreUnavailable(_) => "credential_store_unavailable",
            Self::PasswordCommandFailed { .. } => "password_command_failed",
            Self::PasswordCommandRejected { .. } => "password_command_rejected",
            Self::PasswordCommandEmpty { .. } => "password_command_empty",
            Self::PlaintextRead { .. } => "plaintext_credentials_read_error",
            Self::PlaintextWrite { .. } => "plaintext_credentials_write_error",
            Self::PlaintextMalformed { .. } => "plaintext_credentials_invalid",
            Self::Config(error) => error.code(),
        }
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Missing(profile) => Some(format!(
                "Run `fractal auth login {profile}` to store the credential, or supply it another way. {}",
                supply_options(profile)
            )),
            // The machine has no keychain at all — WSL, a container, a headless
            // box. Naming the alternatives here is the difference between a
            // dead end and a working setup.
            Self::StoreUnavailable(_) => Some(format!(
                "This machine has no usable OS credential store, which is normal on WSL, in containers, and over SSH. {}",
                supply_options("<profile>")
            )),
            Self::PasswordCommandRejected { .. } | Self::PasswordCommandFailed { .. } => Some(
                "Check the command runs on its own and prints only the password on standard output."
                    .to_owned(),
            ),
            Self::PasswordCommandEmpty { .. } => Some(
                "The command succeeded but printed nothing; a password manager usually needs the entry name as an argument."
                    .to_owned(),
            ),
            Self::PlaintextRead { .. } | Self::PlaintextMalformed { .. } => Some(
                "Fix or delete the file; `fractal auth login --store-plaintext` rewrites it."
                    .to_owned(),
            ),
            // A store that exists but will not answer is the same dead end as
            // having none: common on WSL and headless Linux, where the Secret
            // Service library is installed but nothing is running behind it.
            Self::Store { profile, .. } => Some(format!(
                "The credential store refused the request, which usually means there is no working keychain here — normal on WSL, in containers, and over SSH. {}",
                supply_options(profile)
            )),
            Self::Config(error) => error.hint(),
            Self::InvalidProfileName(_) | Self::PlaintextWrite { .. } => None,
        }
    }
}

/// The ways a password can be supplied, for an error that has none.
fn supply_options(profile: &str) -> String {
    format!(
        "Set {} (or FRACTAL_PASSWORD), set `password_command` on the profile to read it from a password manager, or as a last resort run `fractal auth login {profile} --store-plaintext` to keep it in a plain file.",
        password_environment_variable(profile)
    )
}

/// The environment variable that supplies one profile's password.
///
/// Profile names are not restricted to what a shell accepts in a variable
/// name, so anything outside `A-Z0-9` becomes `_`: profile `dev` reads
/// `FRACTAL_PASSWORD_DEV`, and `my-box.dev` reads `FRACTAL_PASSWORD_MY_BOX_DEV`.
/// The mapping is printed in errors, because a rule nobody can see is a rule
/// nobody can use.
#[must_use]
pub fn password_environment_variable(profile_name: &str) -> String {
    let suffix: String = profile_name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_uppercase()
            } else {
                '_'
            }
        })
        .collect();
    format!("FRACTAL_PASSWORD_{suffix}")
}

/// Finds a profile's password, wherever it is kept.
///
/// Tried in order, most explicit first: the profile's own environment
/// variable, the shared `FRACTAL_PASSWORD`, the profile's `password_command`,
/// the opted-in plaintext file, then the OS credential store. The keychain is
/// last because it is the one source that cannot exist everywhere; putting it
/// first would make a machine without one fail before reaching the ways round
/// that.
///
/// # Errors
///
/// Returns [`CredentialError`] when a configured source exists but fails — a
/// password command that errors is reported rather than skipped — and
/// [`CredentialError::Missing`] when no source supplies anything.
pub fn resolve_password(
    profile_name: &str,
    profile: &crate::config::Profile,
) -> Result<(String, PasswordSource), CredentialError> {
    resolve_password_from(profile_name, profile, |variable| {
        std::env::var(variable).ok()
    })
}

/// [`resolve_password`] with the environment supplied by the caller.
///
/// Reading the real environment is not testable: `std::env::set_var` is unsafe
/// in this edition precisely because tests run in parallel threads, and the
/// variable names here are derived from the profile name anyway. Passing the
/// lookup in keeps the ordering — the part worth testing — exercisable.
fn resolve_password_from<E: Fn(&str) -> Option<String>>(
    profile_name: &str,
    profile: &crate::config::Profile,
    environment: E,
) -> Result<(String, PasswordSource), CredentialError> {
    let profile_variable = password_environment_variable(profile_name);
    for variable in [profile_variable.as_str(), "FRACTAL_PASSWORD"] {
        if let Some(password) = environment(variable).filter(|password| !password.is_empty()) {
            return Ok((password, PasswordSource::Environment(variable.to_owned())));
        }
    }

    if let Some(command) = profile.password_command.as_deref() {
        // A configured command that fails is an error, never a silent fall
        // through to another source: the caller asked for this one.
        return Ok((
            run_password_command(profile_name, command)?,
            PasswordSource::Command,
        ));
    }

    if let Some(password) = read_plaintext_password(profile_name)? {
        return Ok((password, PasswordSource::PlaintextFile));
    }

    get_password(profile_name).map(|password| (password, PasswordSource::Keychain))
}

/// Runs a profile's password command and takes its output as the password.
fn run_password_command(profile_name: &str, command: &str) -> Result<String, CredentialError> {
    let (shell, flag) = if cfg!(windows) {
        ("cmd", "/C")
    } else {
        ("sh", "-c")
    };
    let output = std::process::Command::new(shell)
        .arg(flag)
        .arg(command)
        .output()
        .map_err(|source| CredentialError::PasswordCommandFailed {
            profile: profile_name.to_owned(),
            source,
        })?;

    if !output.status.success() {
        return Err(CredentialError::PasswordCommandRejected {
            profile: profile_name.to_owned(),
            stderr: String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(300)
                .collect(),
        });
    }

    // Only the line ending is stripped: a password may legitimately begin or
    // end with a space, and trimming it would silently produce a wrong one.
    let password = String::from_utf8_lossy(&output.stdout)
        .trim_end_matches(['\r', '\n'])
        .to_owned();
    if password.is_empty() {
        return Err(CredentialError::PasswordCommandEmpty {
            profile: profile_name.to_owned(),
        });
    }
    Ok(password)
}

/// Stores or replaces a profile password in the operating-system credential store.
///
/// # Errors
///
/// Returns an error for an invalid profile name or when the OS credential store
/// cannot save the password.
pub fn save_password(profile_name: &str, password: &str) -> Result<(), CredentialError> {
    let entry = entry(profile_name)?;
    entry
        .set_password(password)
        .map_err(|source| CredentialError::Store {
            profile: profile_name.to_owned(),
            source,
        })
}

/// Retrieves a profile password from the operating-system credential store.
///
/// # Errors
///
/// Returns [`CredentialError::Missing`] when no password exists for the profile,
/// or [`CredentialError::Store`] when the credential store cannot be accessed.
pub fn get_password(profile_name: &str) -> Result<String, CredentialError> {
    let entry = entry(profile_name)?;
    match entry.get_password() {
        Ok(password) => Ok(password),
        Err(keyring::Error::NoEntry) => Err(CredentialError::Missing(profile_name.to_owned())),
        Err(source) => Err(CredentialError::Store {
            profile: profile_name.to_owned(),
            source,
        }),
    }
}

/// Deletes a profile password from the operating-system credential store.
///
/// Deleting an already-missing credential is treated as success.
///
/// # Errors
///
/// Returns an error for an invalid profile name or when the credential store
/// cannot perform the deletion.
pub fn delete_password(profile_name: &str) -> Result<(), CredentialError> {
    let entry = entry(profile_name)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(source) => Err(CredentialError::Store {
            profile: profile_name.to_owned(),
            source,
        }),
    }
}

/// Reads one profile's password from the opted-in plaintext file.
///
/// A missing file is not an error: almost nobody has one, and it is only
/// consulted in case they do.
fn read_plaintext_password(profile_name: &str) -> Result<Option<String>, CredentialError> {
    Ok(read_plaintext_file()?.remove(profile_name))
}

fn read_plaintext_file() -> Result<std::collections::BTreeMap<String, String>, CredentialError> {
    read_plaintext_file_at(&crate::config::plaintext_credentials_path()?)
}

/// The file half of the plaintext store, separated from where the file lives.
///
/// Only so it can be tested: the real path comes from the OS config directory,
/// and a test that used it would read and overwrite the developer's own
/// credentials.
fn read_plaintext_file_at(
    path: &std::path::Path,
) -> Result<std::collections::BTreeMap<String, String>, CredentialError> {
    let contents = match std::fs::read_to_string(path) {
        Ok(contents) => contents,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(std::collections::BTreeMap::new());
        }
        Err(source) => {
            return Err(CredentialError::PlaintextRead {
                path: path.display().to_string(),
                source,
            });
        }
    };

    toml::from_str(&contents).map_err(|source| CredentialError::PlaintextMalformed {
        path: path.display().to_string(),
        source: Box::new(source),
    })
}

/// Writes one profile's password to the plaintext file, creating it 0600.
///
/// Nothing here is encrypted, and the file mode is the only protection there
/// is. Callers must have been asked for this explicitly.
///
/// # Errors
///
/// Returns [`CredentialError`] when the file cannot be read, written, or
/// parsed.
pub fn save_plaintext_password(
    profile_name: &str,
    password: &str,
) -> Result<std::path::PathBuf, CredentialError> {
    let mut stored = read_plaintext_file()?;
    stored.insert(profile_name.to_owned(), password.to_owned());
    write_plaintext_file(&stored)
}

/// Removes one profile's password from the plaintext file, if it is there.
///
/// # Errors
///
/// Returns [`CredentialError`] when the file exists but cannot be read,
/// parsed, or rewritten.
pub fn delete_plaintext_password(profile_name: &str) -> Result<bool, CredentialError> {
    let mut stored = read_plaintext_file()?;
    if stored.remove(profile_name).is_none() {
        return Ok(false);
    }
    write_plaintext_file(&stored)?;
    Ok(true)
}

fn write_plaintext_file(
    stored: &std::collections::BTreeMap<String, String>,
) -> Result<std::path::PathBuf, CredentialError> {
    write_plaintext_file_at(&crate::config::plaintext_credentials_path()?, stored)
}

fn write_plaintext_file_at(
    path: &std::path::Path,
    stored: &std::collections::BTreeMap<String, String>,
) -> Result<std::path::PathBuf, CredentialError> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|source| CredentialError::PlaintextWrite {
            path: path.display().to_string(),
            source,
        })?;
    }
    let contents = toml::to_string_pretty(stored).unwrap_or_default();

    // Created 0600 from the outset rather than written and then chmod-ed: the
    // gap between the two is a window where the password is world-readable.
    #[cfg(unix)]
    {
        use std::io::Write as _;
        use std::os::unix::fs::OpenOptionsExt as _;

        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(path)
            .map_err(|source| CredentialError::PlaintextWrite {
                path: path.display().to_string(),
                source,
            })?;
        file.write_all(contents.as_bytes())
            .map_err(|source| CredentialError::PlaintextWrite {
                path: path.display().to_string(),
                source,
            })?;
    }
    #[cfg(not(unix))]
    std::fs::write(path, contents).map_err(|source| CredentialError::PlaintextWrite {
        path: path.display().to_string(),
        source,
    })?;

    Ok(path.to_path_buf())
}

fn entry(profile_name: &str) -> Result<Entry, CredentialError> {
    if profile_name.trim().is_empty() {
        return Err(CredentialError::InvalidProfileName(profile_name.to_owned()));
    }
    install_platform_store_if_needed()?;

    Entry::new(KEYCHAIN_SERVICE, profile_name).map_err(|source| CredentialError::Store {
        profile: profile_name.to_owned(),
        source,
    })
}

/// Installs the OS credential store, unless one is already installed.
///
/// `keyring::Entry::new` installs the platform store itself, on first use, by
/// calling `keyring_core::set_default_store` — which *overwrites* any store
/// already registered. That silently defeated the mock store in this module's
/// tests: they installed a mock, the first `Entry::new` replaced it with the
/// real macOS Keychain, and every assertion then ran against the developer's
/// actual keychain. It was not a correctness problem, but each real keychain
/// call took several seconds and the four tests here cost roughly 40 seconds of
/// every `cargo test` run.
///
/// Checking first is not enough on its own, because `keyring::Entry::new`
/// forces that initialization whether or not a store is already registered.
/// So entries are created through `keyring_core::Entry` and the `keyring`
/// crate is used for one thing only: installing the platform store, here,
/// when nothing else has installed one. Whoever installs first wins, so tests
/// stay hermetic and fast while normal use is unchanged.
fn install_platform_store_if_needed() -> Result<(), CredentialError> {
    if keyring_core::get_default_store().is_some() {
        return Ok(());
    }
    // Triggers the platform store initialization and reports how it went.
    match keyring::Entry::store_status() {
        Ok(()) => Ok(()),
        Err(error) => Err(CredentialError::StoreUnavailable(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use keyring_core::mock::Store;
    use keyring_core::set_default_store;

    use super::{CredentialError, delete_password, get_password, save_password};
    use crate::reportable_error::ReportableError as _;

    static MOCK_STORE_SETUP: Once = Once::new();

    // Tests use unique profile names, do not replace the global mock store, and
    // do not depend on data created by another test, so they can run in parallel.
    fn use_mock_store() {
        MOCK_STORE_SETUP.call_once(|| {
            set_default_store(Store::new().expect("mock credential store creates"));
        });
    }

    #[test]
    fn saving_overwrites_an_existing_profile_password() {
        use_mock_store();
        let profile = "credentials-overwrite";

        save_password(profile, "old-password").unwrap();
        save_password(profile, "new-password").unwrap();

        assert_eq!(get_password(profile).unwrap(), "new-password");
        delete_password(profile).unwrap();
    }

    #[test]
    fn missing_profile_is_an_explicit_error() {
        use_mock_store();
        let profile = "credentials-missing";

        assert!(matches!(
            get_password(profile),
            Err(CredentialError::Missing(name)) if name == profile
        ));
    }

    #[test]
    fn deleting_a_profile_removes_its_password() {
        use_mock_store();
        let profile = "credentials-delete";

        save_password(profile, "password").unwrap();
        delete_password(profile).unwrap();

        assert!(matches!(
            get_password(profile),
            Err(CredentialError::Missing(name)) if name == profile
        ));
    }

    /// A credential operation must not replace a store someone else installed.
    ///
    /// This is the difference between the tests here being hermetic and them
    /// silently running against the developer's real keychain. Failure is not
    /// an assertion error in the other tests — they still pass — it is those
    /// tests taking about ten seconds per operation and reading and writing
    /// real credentials. So it is asserted directly.
    #[test]
    fn a_credential_operation_keeps_the_store_that_is_already_installed() {
        use_mock_store();
        let installed = keyring_core::get_default_store().expect("mock store is installed");
        let vendor_before = installed.vendor();

        save_password("credentials-store-check", "password").unwrap();
        delete_password("credentials-store-check").unwrap();

        let still_installed = keyring_core::get_default_store().expect("a store is installed");
        assert_eq!(
            still_installed.vendor(),
            vendor_before,
            "a credential operation replaced the installed store with the platform one"
        );
    }

    /// A path of our own, so a test never reads or overwrites the developer's
    /// real credentials file.
    fn scratch_path(label: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "fractal-credentials-{label}-{}.toml",
            std::process::id()
        ))
    }

    #[test]
    fn a_profile_name_becomes_a_usable_environment_variable_name() {
        // Profile names allow characters a shell will not accept in a variable
        // name, and the mapping has to be predictable because errors print it.
        assert_eq!(
            super::password_environment_variable("dev"),
            "FRACTAL_PASSWORD_DEV"
        );
        assert_eq!(
            super::password_environment_variable("DEV_100"),
            "FRACTAL_PASSWORD_DEV_100"
        );
        assert_eq!(
            super::password_environment_variable("my-box.dev"),
            "FRACTAL_PASSWORD_MY_BOX_DEV"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_password_command_supplies_the_line_it_prints() {
        let password = super::run_password_command("dev", "printf 'hunter2\\n'").unwrap();

        // The line ending goes; nothing else does, because a password may
        // legitimately start or end with a space.
        assert_eq!(password, "hunter2");
        assert_eq!(
            super::run_password_command("dev", "printf ' padded '").unwrap(),
            " padded "
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_failing_password_command_is_reported_not_skipped() {
        // Falling through to another source here would use the wrong password,
        // or a stale one, without saying so.
        let error =
            super::run_password_command("dev", "echo 'no such entry' >&2; exit 1").unwrap_err();

        assert_eq!(error.code(), "password_command_rejected");
        assert!(error.to_string().contains("no such entry"));
    }

    #[cfg(unix)]
    #[test]
    fn a_password_command_that_prints_nothing_is_an_error() {
        let error = super::run_password_command("dev", "true").unwrap_err();

        assert_eq!(error.code(), "password_command_empty");
        assert!(error.hint().unwrap().contains("printed nothing"));
    }

    #[test]
    fn the_plaintext_file_round_trips_and_forgets_what_is_removed() {
        let path = scratch_path("round-trip");
        let _ = std::fs::remove_file(&path);

        let mut stored = std::collections::BTreeMap::new();
        stored.insert("dev".to_owned(), "hunter2".to_owned());
        super::write_plaintext_file_at(&path, &stored).unwrap();

        let read_back = super::read_plaintext_file_at(&path).unwrap();
        assert_eq!(read_back.get("dev").map(String::as_str), Some("hunter2"));

        super::write_plaintext_file_at(&path, &std::collections::BTreeMap::new()).unwrap();
        assert!(super::read_plaintext_file_at(&path).unwrap().is_empty());
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_missing_plaintext_file_is_empty_rather_than_an_error() {
        // Almost nobody has one; it is only consulted in case they do.
        let path = scratch_path("absent");
        let _ = std::fs::remove_file(&path);

        assert!(super::read_plaintext_file_at(&path).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn the_plaintext_file_is_created_readable_only_by_its_owner() {
        // The file mode is the only protection this store has.
        use std::os::unix::fs::PermissionsExt as _;

        let path = scratch_path("permissions");
        let _ = std::fs::remove_file(&path);
        let mut stored = std::collections::BTreeMap::new();
        stored.insert("dev".to_owned(), "hunter2".to_owned());

        super::write_plaintext_file_at(&path, &stored).unwrap();

        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {:o}", mode & 0o777);
        let _ = std::fs::remove_file(&path);
    }

    fn profile_with_command(command: Option<&str>) -> crate::config::Profile {
        crate::config::Profile {
            base_url: "https://sap.example:8001".to_owned(),
            client: "100".to_owned(),
            username: "developer".to_owned(),
            insecure_tls: false,
            customer_namespaces: vec!["Z*".to_owned()],
            password_command: command.map(str::to_owned),
        }
    }

    #[test]
    fn the_profiles_own_variable_wins_over_everything_else() {
        // The command here would fail loudly if it ran, which is the point:
        // reaching it at all would mean the ordering is wrong.
        let (password, source) = super::resolve_password_from(
            "dev",
            &profile_with_command(Some("exit 1")),
            |variable| (variable == "FRACTAL_PASSWORD_DEV").then(|| "from-profile-var".to_owned()),
        )
        .unwrap();

        assert_eq!(password, "from-profile-var");
        assert_eq!(
            source,
            super::PasswordSource::Environment("FRACTAL_PASSWORD_DEV".to_owned())
        );
    }

    #[test]
    fn the_shared_variable_is_used_when_the_profiles_own_is_unset() {
        let (password, source) = super::resolve_password_from(
            "dev",
            &profile_with_command(Some("exit 1")),
            |variable| (variable == "FRACTAL_PASSWORD").then(|| "from-shared-var".to_owned()),
        )
        .unwrap();

        assert_eq!(password, "from-shared-var");
        assert_eq!(
            source,
            super::PasswordSource::Environment("FRACTAL_PASSWORD".to_owned())
        );
    }

    #[test]
    fn an_empty_variable_is_treated_as_unset() {
        // An exported-but-empty variable is a common accident; taking it would
        // send an empty password to SAP and count as a failed logon.
        let (password, source) = super::resolve_password_from(
            "dev",
            &profile_with_command(Some("printf secret")),
            |_| Some(String::new()),
        )
        .unwrap();

        assert_eq!(password, "secret");
        assert_eq!(source, super::PasswordSource::Command);
    }

    #[cfg(unix)]
    #[test]
    fn the_password_command_is_used_when_no_variable_is_set() {
        let (password, source) = super::resolve_password_from(
            "dev",
            &profile_with_command(Some("printf 'from-command'")),
            |_| None,
        )
        .unwrap();

        assert_eq!(password, "from-command");
        assert_eq!(source, super::PasswordSource::Command);
    }

    #[test]
    fn a_store_that_does_not_exist_says_how_to_supply_a_password_instead() {
        // The whole point of this work: on WSL, in a container, over SSH, the
        // error has to name the ways round rather than just fail.
        let error = CredentialError::StoreUnavailable("no D-Bus session".to_owned());
        let hint = error.hint().unwrap();

        assert!(hint.contains("FRACTAL_PASSWORD"));
        assert!(hint.contains("password_command"));
        assert!(hint.contains("--store-plaintext"));
    }

    /// A keychain that answers with an error is the same dead end as none.
    ///
    /// This is what WSL actually produces: the Secret Service library loads,
    /// so the store installs and `StoreUnavailable` never fires, and then
    /// every operation fails with an SS error. Guidance attached only to
    /// `StoreUnavailable` left this case with no hint at all.
    #[test]
    fn a_store_that_refuses_names_the_ways_round_it() {
        let error = CredentialError::Store {
            profile: "dev".to_owned(),
            source: keyring::Error::NoStorageAccess(Box::new(std::io::Error::other(
                "SS error: result not returned from SS API",
            ))),
        };

        let hint = error.hint().unwrap();
        assert!(hint.contains("FRACTAL_PASSWORD_DEV"), "{hint}");
        assert!(hint.contains("password_command"), "{hint}");
        assert!(hint.contains("--store-plaintext"), "{hint}");
        // Storing a password in the clear must never be a suggested command:
        // suggestions are read-only, and this one is a mutation.
        assert_eq!(error.suggested_command(), None);
    }

    #[test]
    fn a_missing_credential_names_the_profiles_own_variable() {
        let hint = CredentialError::Missing("dev".to_owned()).hint().unwrap();

        assert!(hint.contains("FRACTAL_PASSWORD_DEV"));
        assert!(hint.contains("fractal auth login dev"));
    }

    #[test]
    fn empty_profile_names_are_rejected_before_keyring_access() {
        assert!(matches!(
            save_password("  ", "password"),
            Err(CredentialError::InvalidProfileName(name)) if name == "  "
        ));
    }
}

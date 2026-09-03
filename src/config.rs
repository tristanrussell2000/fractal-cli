use std::{collections::BTreeMap, fs, path::PathBuf};

use crate::reportable_error::ReportableError;
use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use thiserror::Error;

const QUALIFIER: &str = "com";
const ORGANIZATION: &str = "issi";
const APPLICATION: &str = "fractal";

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("could not determine the platform config directory")]
    NoConfigDirectory,
    #[error("could not read config file {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in config file {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("could not write config file {path}: {source}")]
    Write {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("could not serialize config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("profile '{0}' was not found")]
    ProfileNotFound(String),
    #[error(
        "no default SAP profile is configured; pass --profile <name> for this command, or set one with `fractal auth login <name> --default`"
    )]
    NoProfileSelected,
}

impl ReportableError for ConfigError {
    fn code(&self) -> &'static str {
        match self {
            Self::NoConfigDirectory => "config_directory_unavailable",
            Self::Read { .. } => "config_read_error",
            Self::Parse { .. } => "config_invalid",
            Self::Write { .. } => "config_write_error",
            Self::Serialize(_) => "config_serialize_error",
            Self::ProfileNotFound(_) => "profile_not_found",
            Self::NoProfileSelected => "no_default_profile",
        }
    }

    fn hint(&self) -> Option<String> {
        match self {
            // `auth login` writes config and prompts for a password, so it is
            // named in prose and never as an executable suggested_command.
            Self::NoProfileSelected => Some(
                "Pass --profile <name> for this command, or set one with `fractal auth login <name> --default`."
                    .to_owned(),
            ),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Profile {
    pub base_url: String,
    pub client: String,
    pub username: String,
    #[serde(default)]
    pub insecure_tls: bool,
    #[serde(default = "default_customer_namespaces")]
    pub customer_namespaces: Vec<String>,
    /// A command whose standard output is this profile's password.
    ///
    /// The way to use a password manager — `pass`, the 1Password CLI, Vault —
    /// on a machine with no OS keychain, without Fractal storing the secret
    /// itself or inventing its own crypto. Run through the platform shell, so
    /// pipes and redirections work as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub password_command: Option<String>,
    /// Packages whose objects this profile may mutate.
    ///
    /// `None` means unrestricted, so a profile written before this setting
    /// existed keeps working. `Some([])` is the explicit off switch: nothing is
    /// writable. The distinction is the whole point of the `Option` — an empty
    /// list must not be silently equivalent to no list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub edit_packages: Option<Vec<String>>,
    /// Whether `$TMP`, the per-user scratch package, is granted regardless of
    /// [`Profile::edit_packages`]. Local throwaway objects are not shared code,
    /// and an allowlist that blocked them would mostly be in the way.
    #[serde(
        default = "default_allow_temporary_package",
        skip_serializing_if = "is_default_allow_temporary_package"
    )]
    pub allow_temporary_package: bool,
}

/// SAP's per-user local package. Objects in it are never transported.
pub const TEMPORARY_PACKAGE: &str = "$TMP";

const fn default_allow_temporary_package() -> bool {
    true
}

/// Keeps the default out of the written file, so a config that never mentions
/// this setting is not rewritten to mention it.
const fn is_default_allow_temporary_package(value: &bool) -> bool {
    *value
}

/// What one profile permits an edit to touch.
///
/// Carried instead of a bare namespace list so that every mutating operation
/// receives the complete policy: adding a rule here reaches all of them without
/// a signature change, and no path can accidentally authorize against half of
/// it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPolicy {
    /// A floor on the object *name*.
    pub customer_namespaces: Vec<String>,
    /// A grant on the object's *package*. `None` is unrestricted.
    pub edit_packages: Option<Vec<String>>,
    pub allow_temporary_package: bool,
}

impl EditPolicy {
    /// A policy that authorizes names only, granting every package.
    ///
    /// What a profile without `edit_packages` yields, and the starting point
    /// for tests that are about something other than the allowlist.
    #[must_use]
    pub fn namespaces_only(customer_namespaces: &[&str]) -> Self {
        Self {
            customer_namespaces: customer_namespaces
                .iter()
                .map(|namespace| (*namespace).to_owned())
                .collect(),
            edit_packages: None,
            allow_temporary_package: true,
        }
    }

    /// Whether any package restriction is configured at all.
    #[must_use]
    pub const fn restricts_packages(&self) -> bool {
        self.edit_packages.is_some()
    }
}

impl Profile {
    /// The edit policy this profile grants.
    #[must_use]
    pub fn edit_policy(&self) -> EditPolicy {
        EditPolicy {
            customer_namespaces: self.customer_namespaces.clone(),
            edit_packages: self.edit_packages.clone(),
            allow_temporary_package: self.allow_temporary_package,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Config {
    pub default_profile: Option<String>,
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

#[derive(Debug, Clone)]
pub struct LoadedConfig {
    pub path: PathBuf,
    pub config: Config,
}

/// Returns the platform-specific path to Fractal's profile configuration file.
///
/// # Errors
///
/// Returns [`ConfigError::NoConfigDirectory`] when the platform does not expose
/// a suitable per-user configuration directory.
/// Where an opted-in plaintext password file lives.
///
/// Deliberately not `config.toml`: that file holds no secrets and is safe to
/// share or paste into a bug report, and putting a password in it invites
/// exactly that mistake.
///
/// # Errors
///
/// Returns [`ConfigError::NoConfigDirectory`] when the platform has no config
/// directory.
pub fn plaintext_credentials_path() -> Result<PathBuf, ConfigError> {
    Ok(config_path()?.with_file_name("credentials.toml"))
}

/// Where the profile configuration lives.
///
/// # Errors
///
/// Returns [`ConfigError::NoConfigDirectory`] when the platform has no config
/// directory.
pub fn config_path() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .ok_or(ConfigError::NoConfigDirectory)?;
    Ok(dirs.config_dir().join("config.toml"))
}

/// Loads profile configuration, returning an empty configuration when the file does not exist.
///
/// # Errors
///
/// Returns an error when the configuration path cannot be determined, the file
/// cannot be read, or its TOML content is invalid.
pub fn load() -> Result<LoadedConfig, ConfigError> {
    let path = config_path()?;
    if !path.exists() {
        return Ok(LoadedConfig {
            path,
            config: Config::default(),
        });
    }

    let contents = fs::read_to_string(&path).map_err(|source| ConfigError::Read {
        path: path.clone(),
        source,
    })?;
    let config = toml::from_str(&contents).map_err(|source| ConfigError::Parse {
        path: path.clone(),
        source,
    })?;

    Ok(LoadedConfig { path, config })
}

/// Saves profile configuration and returns the path written.
///
/// # Errors
///
/// Returns an error when the configuration directory cannot be created, the
/// configuration cannot be serialized, or the file cannot be written.
pub fn save(config: &Config) -> Result<PathBuf, ConfigError> {
    let path = config_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|source| ConfigError::Write {
            path: path.clone(),
            source,
        })?;
    }

    let contents = toml::to_string_pretty(config)?;
    fs::write(&path, contents).map_err(|source| ConfigError::Write {
        path: path.clone(),
        source,
    })?;

    Ok(path)
}

pub fn update_profile(
    config: &mut Config,
    name: String,
    profile: Profile,
    make_default: bool,
) -> bool {
    let is_first_profile = config.profiles.is_empty();
    config.profiles.insert(name.clone(), profile);

    if make_default || is_first_profile {
        config.default_profile = Some(name);
        true
    } else {
        false
    }
}

/// Resolves a profile using explicit selection, `FRACTAL_PROFILE`, then the configured default.
///
/// # Errors
///
/// Returns [`ConfigError::NoProfileSelected`] when no selection exists, or
/// [`ConfigError::ProfileNotFound`] when the selected name is not configured.
pub fn resolve_profile<'a>(
    config: &'a Config,
    explicit: Option<&str>,
) -> Result<(&'a str, &'a Profile), ConfigError> {
    resolve_profile_with_environment(
        config,
        explicit,
        std::env::var("FRACTAL_PROFILE").ok().as_deref(),
    )
}

pub fn remove_profile(config: &mut Config, name: &str) -> bool {
    let removed = config.profiles.remove(name).is_some();
    if removed && config.default_profile.as_deref() == Some(name) {
        config.default_profile = None;
    }
    removed
}

/// Resolves a profile using explicit selection, a supplied environment value, then the default.
///
/// # Errors
///
/// Returns [`ConfigError::NoProfileSelected`] when no selection exists, or
/// [`ConfigError::ProfileNotFound`] when the selected name is not configured.
pub fn resolve_profile_with_environment<'a>(
    config: &'a Config,
    explicit: Option<&str>,
    environment: Option<&str>,
) -> Result<(&'a str, &'a Profile), ConfigError> {
    let selected = explicit
        .or(environment)
        .or(config.default_profile.as_deref())
        .ok_or(ConfigError::NoProfileSelected)?;

    let (name, profile) = config
        .profiles
        .get_key_value(selected)
        .ok_or_else(|| ConfigError::ProfileNotFound(selected.to_owned()))?;

    Ok((name, profile))
}

fn default_customer_namespaces() -> Vec<String> {
    vec!["Z*".to_owned(), "Y*".to_owned()]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile() -> Profile {
        Profile {
            base_url: "https://sap.example:8001".to_owned(),
            client: "900".to_owned(),
            username: "developer".to_owned(),
            insecure_tls: false,
            password_command: None,
            edit_packages: None,
            allow_temporary_package: true,
            customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
        }
    }

    fn config() -> Config {
        Config {
            default_profile: Some("default".to_owned()),
            profiles: BTreeMap::from([
                ("default".to_owned(), profile()),
                ("environment".to_owned(), profile()),
                ("explicit".to_owned(), profile()),
            ]),
        }
    }

    #[test]
    fn a_config_written_before_the_allowlist_existed_stays_unrestricted() {
        let toml = r#"
[profiles.dev]
base_url = "https://sap.example:8001"
client = "100"
username = "developer"
customer_namespaces = ["Z*"]
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let profile = &config.profiles["dev"];

        // Absent means "every package", so an existing profile keeps working.
        assert_eq!(profile.edit_packages, None);
        assert!(!profile.edit_policy().restricts_packages());
        // Scratch work is permitted unless a profile says otherwise.
        assert!(profile.allow_temporary_package);
    }

    #[test]
    fn an_empty_list_is_not_the_same_as_no_list() {
        let toml = r#"
[profiles.dev]
base_url = "https://sap.example:8001"
client = "100"
username = "developer"
edit_packages = []
"#;
        let config: Config = toml::from_str(toml).unwrap();
        let profile = &config.profiles["dev"];

        assert_eq!(profile.edit_packages, Some(Vec::new()));
        // The explicit off switch has to survive the round trip, or it silently
        // becomes "unrestricted" — the opposite of what was asked for.
        assert!(profile.edit_policy().restricts_packages());
    }

    #[test]
    fn saving_does_not_write_settings_the_profile_never_set() {
        let mut profile = profile();
        profile.edit_packages = None;
        let rendered = toml::to_string_pretty(&profile).unwrap();

        assert!(!rendered.contains("edit_packages"), "{rendered}");
        assert!(!rendered.contains("allow_temporary_package"), "{rendered}");
    }

    #[test]
    fn an_explicitly_disabled_scratch_package_is_written() {
        let mut profile = profile();
        profile.allow_temporary_package = false;
        profile.edit_packages = Some(vec!["ZPROJ*".to_owned()]);
        let rendered = toml::to_string_pretty(&profile).unwrap();

        assert!(
            rendered.contains("allow_temporary_package = false"),
            "{rendered}"
        );
        assert!(rendered.contains("ZPROJ*"), "{rendered}");
        // And it round-trips, rather than reverting to the default on load.
        let parsed: Profile = toml::from_str(&rendered).unwrap();
        assert!(!parsed.allow_temporary_package);
    }

    #[test]
    fn explicit_profile_wins_over_environment_and_default() {
        let config = config();
        let (name, _) =
            resolve_profile_with_environment(&config, Some("explicit"), Some("environment"))
                .unwrap();
        assert_eq!(name, "explicit");
    }

    #[test]
    fn environment_profile_wins_over_default() {
        let config = config();
        let (name, _) =
            resolve_profile_with_environment(&config, None, Some("environment")).unwrap();
        assert_eq!(name, "environment");
    }

    #[test]
    fn default_profile_is_used_as_last_resort() {
        let config = config();
        let (name, _) = resolve_profile_with_environment(&config, None, None).unwrap();
        assert_eq!(name, "default");
    }

    #[test]
    fn first_profile_becomes_default() {
        let mut config = Config::default();

        let became_default = update_profile(&mut config, "first".to_owned(), profile(), false);

        assert!(became_default);
        assert_eq!(config.default_profile.as_deref(), Some("first"));
    }

    #[test]
    fn later_profile_preserves_existing_default() {
        let mut config = config();

        let became_default = update_profile(&mut config, "new".to_owned(), profile(), false);

        assert!(!became_default);
        assert_eq!(config.default_profile.as_deref(), Some("default"));
    }

    #[test]
    fn default_flag_changes_existing_default() {
        let mut config = config();

        let became_default = update_profile(&mut config, "new".to_owned(), profile(), true);

        assert!(became_default);
        assert_eq!(config.default_profile.as_deref(), Some("new"));
    }

    #[test]
    fn updating_existing_profile_does_not_make_it_default_without_flag() {
        let mut config = config();

        let became_default = update_profile(&mut config, "explicit".to_owned(), profile(), false);

        assert!(!became_default);
        assert_eq!(config.default_profile.as_deref(), Some("default"));
    }

    #[test]
    fn removing_non_default_profile_preserves_default() {
        let mut config = config();

        assert!(remove_profile(&mut config, "explicit"));
        assert!(!config.profiles.contains_key("explicit"));
        assert_eq!(config.default_profile.as_deref(), Some("default"));
    }

    #[test]
    fn removing_default_profile_leaves_no_default() {
        let mut config = config();

        assert!(remove_profile(&mut config, "default"));
        assert!(!config.profiles.contains_key("default"));
        assert_eq!(config.default_profile, None);
    }

    #[test]
    fn removing_unknown_profile_reports_no_change() {
        let mut config = config();

        assert!(!remove_profile(&mut config, "missing"));
        assert_eq!(config.default_profile.as_deref(), Some("default"));
    }
}

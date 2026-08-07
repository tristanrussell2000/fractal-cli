use std::{collections::BTreeMap, fs, path::PathBuf};

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
        "no SAP profile was selected; pass --profile, set FRACTAL_PROFILE, or configure default_profile"
    )]
    NoProfileSelected,
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

pub fn config_path() -> Result<PathBuf, ConfigError> {
    let dirs = ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION)
        .ok_or(ConfigError::NoConfigDirectory)?;
    Ok(dirs.config_dir().join("config.toml"))
}

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
}

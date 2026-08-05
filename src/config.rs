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

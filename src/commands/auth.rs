use std::io::Read;

use serde::Serialize;
use thiserror::Error;

use fractal::reportable_error::ReportableError;

use crate::cli::{LoginArgs, ProfileArgs};
use crate::reported::Reported;
use fractal::{config, credentials};

/// A failure in the local profile and credential workflow.
///
/// These are CLI-level failures with no SAP request behind them, and several
/// describe a half-completed change — a credential removed but the config not
/// saved — so each variant owns the recovery advice for that exact state.
#[derive(Debug, Error)]
pub enum AuthCommandError {
    #[error("profile '{0}' was not found")]
    ProfileNotFound(String),
    #[error("credential removed, but profile config could not be saved: {0}")]
    ConfigWriteAfterCredentialRemoval(#[source] config::ConfigError),
    #[error("could not read password from stdin: {0}")]
    PasswordStdin(#[source] std::io::Error),
    #[error("{0}")]
    PasswordPrompt(#[source] std::io::Error),
    #[error("password cannot be empty")]
    EmptyPassword,
    #[error("could not save profile config: {0}")]
    ConfigWrite(#[source] config::ConfigError),
    #[error("profile config saved, but the credential could not be stored: {source}")]
    CredentialStoreAfterConfigSave {
        profile: String,
        #[source]
        source: credentials::CredentialError,
    },
}

impl ReportableError for AuthCommandError {
    fn code(&self) -> &'static str {
        match self {
            Self::ProfileNotFound(_) => "profile_not_found",
            Self::ConfigWriteAfterCredentialRemoval(_) => {
                "config_write_error_after_credential_removal"
            }
            Self::PasswordStdin(_) => "password_stdin_error",
            Self::PasswordPrompt(_) => "password_prompt_error",
            Self::EmptyPassword => "empty_password",
            Self::ConfigWrite(_) => "config_write_error",
            Self::CredentialStoreAfterConfigSave { .. } => {
                "credential_store_error_after_config_save"
            }
        }
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::ProfileNotFound(_) => {
                Some("Run `fractal auth list` to see configured profiles.".to_owned())
            }
            Self::ConfigWriteAfterCredentialRemoval(_) => Some(
                "Retry `fractal auth remove` for this profile; credential deletion is idempotent."
                    .to_owned(),
            ),
            Self::PasswordStdin(_) => Some(
                "Provide the password through stdin or omit --password-stdin to use the secure prompt."
                    .to_owned(),
            ),
            Self::PasswordPrompt(_) => None,
            Self::EmptyPassword => Some(
                "Provide a non-empty password through the secure prompt or --password-stdin."
                    .to_owned(),
            ),
            Self::ConfigWrite(_) => {
                Some("Check that Fractal's application config directory is writable.".to_owned())
            }
            Self::CredentialStoreAfterConfigSave { profile, .. } => Some(format!(
                "Run `fractal auth list` and retry `fractal auth login {profile}`."
            )),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct AuthLoginResult {
    ok: bool,
    profile: String,
    config_path: String,
    became_default: bool,
    message: String,
}

#[derive(Debug, Serialize)]
pub struct AuthListResult {
    ok: bool,
    config_path: String,
    default_profile: Option<String>,
    profiles: Vec<AuthProfileSummary>,
}

#[derive(Debug, Serialize)]
struct AuthProfileSummary {
    name: String,
    base_url: String,
    client: String,
    username: String,
    insecure_tls: bool,
    customer_namespaces: Vec<String>,
    credential: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    credential_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AuthRemoveResult {
    ok: bool,
    profile: String,
    config_path: String,
    removed_default: bool,
    message: String,
}

pub fn auth_list() -> Result<AuthListResult, Reported> {
    let loaded = config::load()?;
    let profiles = loaded
        .config
        .profiles
        .iter()
        .map(|(name, profile)| {
            let (credential, credential_error) = match credentials::get_password(name) {
                Ok(_) => ("stored".to_owned(), None),
                Err(error @ credentials::CredentialError::Missing(_)) => {
                    ("missing".to_owned(), Some(error.to_string()))
                }
                Err(error) => ("unavailable".to_owned(), Some(error.to_string())),
            };

            AuthProfileSummary {
                name: name.clone(),
                base_url: profile.base_url.clone(),
                client: profile.client.clone(),
                username: profile.username.clone(),
                insecure_tls: profile.insecure_tls,
                customer_namespaces: profile.customer_namespaces.clone(),
                credential,
                credential_error,
            }
        })
        .collect();

    Ok(AuthListResult {
        ok: true,
        config_path: loaded.path.display().to_string(),
        default_profile: loaded.config.default_profile,
        profiles,
    })
}

pub fn auth_remove(args: &ProfileArgs) -> Result<AuthRemoveResult, Reported> {
    let mut loaded = config::load()?;
    if !loaded.config.profiles.contains_key(&args.name) {
        return Err(AuthCommandError::ProfileNotFound(args.name.clone()).into());
    }

    let removed_default = loaded.config.default_profile.as_deref() == Some(args.name.as_str());
    credentials::delete_password(&args.name)?;
    config::remove_profile(&mut loaded.config, &args.name);
    let config_path = config::save(&loaded.config)
        .map_err(AuthCommandError::ConfigWriteAfterCredentialRemoval)?;

    let message = if removed_default {
        "Profile removed. No default profile is set; pass --profile <name> for commands or set a new default with `fractal auth login <name> --default`.".to_owned()
    } else {
        "Profile and credential removed.".to_owned()
    };

    Ok(AuthRemoveResult {
        ok: true,
        profile: args.name.clone(),
        config_path: config_path.display().to_string(),
        removed_default,
        message,
    })
}

pub fn auth_login(args: &LoginArgs) -> Result<AuthLoginResult, Reported> {
    let password = if args.password_stdin {
        let mut password = String::new();
        std::io::stdin()
            .read_to_string(&mut password)
            .map_err(AuthCommandError::PasswordStdin)?;
        password.trim_end_matches(['\r', '\n']).to_owned()
    } else {
        rpassword::prompt_password("Password: ").map_err(AuthCommandError::PasswordPrompt)?
    };

    if password.is_empty() {
        return Err(AuthCommandError::EmptyPassword.into());
    }

    let mut loaded = config::load()?;
    let profile = config::Profile {
        base_url: args.url.trim_end_matches('/').to_owned(),
        client: args.client.clone(),
        username: args.username.clone(),
        insecure_tls: args.insecure_tls,
        customer_namespaces: if args.namespace.is_empty() {
            vec!["Z*".to_owned(), "Y*".to_owned()]
        } else {
            args.namespace.clone()
        },
    };
    let became_default =
        config::update_profile(&mut loaded.config, args.name.clone(), profile, args.default);

    let config_path = config::save(&loaded.config).map_err(AuthCommandError::ConfigWrite)?;
    credentials::save_password(&args.name, &password).map_err(|source| {
        AuthCommandError::CredentialStoreAfterConfigSave {
            profile: args.name.clone(),
            source,
        }
    })?;

    Ok(AuthLoginResult {
        ok: true,
        profile: args.name.clone(),
        config_path: config_path.display().to_string(),
        became_default,
        message: if became_default {
            "Profile saved and selected as the default profile.".to_owned()
        } else {
            "Profile saved; the existing default profile was preserved.".to_owned()
        },
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use crate::cli::{AuthCommand, Cli, Command};

    #[test]
    fn parses_auth_login_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "auth",
            "login",
            "DE2_903",
            "--url",
            "https://sap.example:8001",
            "--client",
            "903",
            "--username",
            "mparker",
            "--insecure-tls",
            "--namespace",
            "Z*",
            "--namespace",
            "Y*",
            "--default",
            "--password-stdin",
        ])
        .unwrap();

        let Command::Auth {
            command: AuthCommand::Login(args),
        } = cli.command
        else {
            panic!("expected auth login command");
        };
        assert_eq!(args.name, "DE2_903");
        assert_eq!(args.url, "https://sap.example:8001");
        assert_eq!(args.client, "903");
        assert_eq!(args.username, "mparker");
        assert!(args.insecure_tls);
        assert_eq!(args.namespace, vec!["Z*", "Y*"]);
        assert!(args.default);
        assert!(args.password_stdin);
    }

    #[test]
    fn auth_login_options_default_to_prompted_password_and_no_namespace_override() {
        let cli = Cli::try_parse_from([
            "fractal",
            "auth",
            "login",
            "DE2_903",
            "--url",
            "https://sap.example:8001",
            "--client",
            "903",
            "--username",
            "mparker",
        ])
        .unwrap();

        let Command::Auth {
            command: AuthCommand::Login(args),
        } = cli.command
        else {
            panic!("expected auth login command");
        };
        assert!(!args.insecure_tls);
        assert!(args.namespace.is_empty());
        assert!(!args.default);
        assert!(!args.password_stdin);
    }

    #[test]
    fn parses_auth_list_command_from_cli() {
        let cli = Cli::try_parse_from(["fractal", "auth", "list"]).unwrap();

        assert!(matches!(
            cli.command,
            Command::Auth {
                command: AuthCommand::List
            }
        ));
    }

    #[test]
    fn parses_auth_remove_options_from_cli() {
        let cli = Cli::try_parse_from(["fractal", "auth", "remove", "DE2_903"]).unwrap();

        let Command::Auth {
            command: AuthCommand::Remove(args),
        } = cli.command
        else {
            panic!("expected auth remove command");
        };
        assert_eq!(args.name, "DE2_903");
    }
}

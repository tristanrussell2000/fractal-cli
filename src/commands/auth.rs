use std::io::{BufRead, IsTerminal, Read, Write};

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
    /// A value was neither passed as a flag nor obtainable by asking.
    #[error("{value} is required")]
    MissingValue {
        value: &'static str,
        flag: &'static str,
    },
    #[error("could not read {value} from the terminal: {source}")]
    Prompt {
        value: &'static str,
        #[source]
        source: std::io::Error,
    },
    #[error("{0}")]
    PasswordPrompt(#[source] std::io::Error),
    #[error("password cannot be empty")]
    EmptyPassword,
    #[error("could not save profile config: {0}")]
    ConfigWrite(#[source] config::ConfigError),
    #[error("profile config saved, but the credential could not be stored: {source}")]
    CredentialStoreAfterConfigSave {
        profile: String,
        /// Boxed to keep this error small: it rides on every `Result` in this
        /// module, and `CredentialError` grew when the credential fallbacks
        /// went in.
        #[source]
        source: Box<credentials::CredentialError>,
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
            Self::MissingValue { .. } => "missing_profile_value",
            Self::Prompt { .. } => "prompt_read_error",
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
            Self::MissingValue { flag, .. } => Some(format!(
                "Pass {flag}, or run `fractal auth login` in an interactive terminal to be asked for it."
            )),
            Self::Prompt { .. } => Some(
                "Pass every value as a flag instead; prompting needs a terminal to read from."
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
    /// Where the password now lives: `os_keychain`, `plaintext_file`, or
    /// `password_command` when Fractal keeps nothing at all.
    password_storage: String,
    /// Present when the password was stored somewhere unencrypted.
    #[serde(skip_serializing_if = "Option::is_none")]
    warning: Option<String>,
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
    // Both stores, not whichever one this machine happens to have: leaving a
    // password behind in a file after "remove" reported success would be the
    // worst possible outcome of this command.
    credentials::delete_plaintext_password(&args.name)?;
    // No credential store on this machine means nothing of ours can be in one,
    // so there is nothing to remove. Failing here would make `remove`
    // impossible on exactly the machines that keep passwords elsewhere.
    if let Err(error) = credentials::delete_password(&args.name)
        && !matches!(error, credentials::CredentialError::StoreUnavailable(_))
    {
        return Err(error.into());
    }
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

/// The four values that identify a profile, however the caller supplied them.
#[derive(Debug, Clone, PartialEq, Eq)]
struct LoginDetails {
    name: String,
    url: String,
    client: String,
    username: String,
}

/// Fills in whatever the caller did not pass as a flag.
///
/// Flags win where present, so a scripted call behaves exactly as before and
/// never blocks on a prompt. Only the gaps are asked for, and only when there
/// is a terminal to ask: without one, a missing value is an error naming the
/// flag rather than a process that hangs waiting on a pipe.
fn resolve_login_details<R: BufRead, W: Write>(
    args: &LoginArgs,
    interactive: bool,
    input: &mut R,
    output: &mut W,
) -> Result<LoginDetails, AuthCommandError> {
    Ok(LoginDetails {
        name: resolve_value(
            args.name.as_deref(),
            "a profile name",
            "the profile name as the first argument",
            "Profile name (for example DEV)",
            interactive,
            input,
            output,
        )?,
        url: resolve_value(
            args.url.as_deref(),
            "a base URL",
            "--url",
            "SAP base URL (for example https://sap.example:8001)",
            interactive,
            input,
            output,
        )?
        .trim_end_matches('/')
        .to_owned(),
        client: resolve_value(
            args.client.as_deref(),
            "a SAP client",
            "--client",
            "SAP client (for example 100)",
            interactive,
            input,
            output,
        )?,
        username: resolve_value(
            args.username.as_deref(),
            "a username",
            "--username",
            "SAP username",
            interactive,
            input,
            output,
        )?,
    })
}

fn resolve_value<R: BufRead, W: Write>(
    supplied: Option<&str>,
    value: &'static str,
    flag: &'static str,
    prompt: &str,
    interactive: bool,
    input: &mut R,
    output: &mut W,
) -> Result<String, AuthCommandError> {
    // An empty flag is treated as absent rather than accepted: a blank profile
    // name or URL cannot be used later, so asking beats storing it.
    if let Some(supplied) = supplied
        .map(str::trim)
        .filter(|supplied| !supplied.is_empty())
    {
        return Ok(supplied.to_owned());
    }
    if !interactive {
        return Err(AuthCommandError::MissingValue { value, flag });
    }

    loop {
        write!(output, "{prompt}: ")
            .and_then(|()| output.flush())
            .map_err(|source| AuthCommandError::Prompt { value, source })?;

        let mut line = String::new();
        let read = input
            .read_line(&mut line)
            .map_err(|source| AuthCommandError::Prompt { value, source })?;
        if read == 0 {
            // End of input: nobody is there to answer, so this is the same
            // situation as having no terminal at all.
            return Err(AuthCommandError::MissingValue { value, flag });
        }

        let answer = line.trim();
        if !answer.is_empty() {
            return Ok(answer.to_owned());
        }
        let _ = writeln!(output, "That cannot be empty.");
    }
}

pub fn auth_login(args: &LoginArgs) -> Result<AuthLoginResult, Reported> {
    // Prompts read the terminal, so they are only offered when there is one.
    let interactive = std::io::stdin().is_terminal();
    let details = {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        // Prompts go to stderr so that stdout stays the command's result.
        let mut output = std::io::stderr();
        resolve_login_details(args, interactive, &mut input, &mut output)?
    };

    // A password command supplies the secret on every use, so there is nothing
    // to ask for and nothing to keep. Asking anyway would collect a password
    // that is then never read.
    let password = if args.password_command.is_some() {
        None
    } else if args.password_stdin {
        let mut password = String::new();
        std::io::stdin()
            .read_to_string(&mut password)
            .map_err(AuthCommandError::PasswordStdin)?;
        Some(password.trim_end_matches(['\r', '\n']).to_owned())
    } else if interactive {
        Some(rpassword::prompt_password("Password: ").map_err(AuthCommandError::PasswordPrompt)?)
    } else {
        // The secure prompt needs a terminal. Saying so beats letting it fail
        // with whatever error reading a pipe produces.
        return Err(AuthCommandError::MissingValue {
            value: "a password",
            flag: "--password-stdin",
        }
        .into());
    };

    if password.as_ref().is_some_and(String::is_empty) {
        return Err(AuthCommandError::EmptyPassword.into());
    }

    let mut loaded = config::load()?;
    let profile = config::Profile {
        base_url: details.url.clone(),
        client: details.client.clone(),
        username: details.username.clone(),
        insecure_tls: args.insecure_tls,
        customer_namespaces: if args.namespace.is_empty() {
            vec!["Z*".to_owned(), "Y*".to_owned()]
        } else {
            args.namespace.clone()
        },
        password_command: args.password_command.clone(),
    };
    let became_default = config::update_profile(
        &mut loaded.config,
        details.name.clone(),
        profile,
        args.default,
    );

    let config_path = config::save(&loaded.config).map_err(AuthCommandError::ConfigWrite)?;
    let storage = store_password(&details.name, password.as_deref(), args.store_plaintext)
        .map_err(|source| AuthCommandError::CredentialStoreAfterConfigSave {
            profile: details.name.clone(),
            source: Box::new(source),
        })?;

    Ok(AuthLoginResult {
        ok: true,
        profile: details.name,
        config_path: config_path.display().to_string(),
        became_default,
        password_storage: storage.description.to_owned(),
        warning: storage.warning,
        message: if became_default {
            "Profile saved and selected as the default profile.".to_owned()
        } else {
            "Profile saved; the existing default profile was preserved.".to_owned()
        },
    })
}

/// Where the password went, and whether that is worth warning about.
struct PasswordStorage {
    description: &'static str,
    warning: Option<String>,
}

/// Puts the password wherever the caller chose.
///
/// Three outcomes: no password at all when a command supplies it on each use,
/// a plain file when the caller explicitly asked for one, or the OS credential
/// store. The plaintext choice is never reached by falling back — a machine
/// with no keychain gets an error naming the options instead, so the downgrade
/// is always something the caller asked for.
fn store_password(
    profile_name: &str,
    password: Option<&str>,
    store_plaintext: bool,
) -> Result<PasswordStorage, credentials::CredentialError> {
    let Some(password) = password else {
        return Ok(PasswordStorage {
            description: "password_command",
            warning: None,
        });
    };

    if store_plaintext {
        let path = credentials::save_plaintext_password(profile_name, password)?;
        return Ok(PasswordStorage {
            description: "plaintext_file",
            warning: Some(format!(
                "The password is stored unencrypted in {}, readable only by your user. Anything that can read your files can read it.",
                path.display()
            )),
        });
    }

    credentials::save_password(profile_name, password)?;
    Ok(PasswordStorage {
        description: "os_keychain",
        warning: None,
    })
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::{AuthCommandError, LoginArgs, LoginDetails, resolve_login_details};
    use crate::cli::{AuthCommand, Cli, Command};
    use fractal::reportable_error::ReportableError;

    fn args(
        name: Option<&str>,
        url: Option<&str>,
        client: Option<&str>,
        username: Option<&str>,
    ) -> LoginArgs {
        LoginArgs {
            name: name.map(str::to_owned),
            url: url.map(str::to_owned),
            client: client.map(str::to_owned),
            username: username.map(str::to_owned),
            insecure_tls: false,
            namespace: Vec::new(),
            default: false,
            password_stdin: false,
            password_command: None,
            store_plaintext: false,
        }
    }

    /// Runs the resolution against scripted answers, as a terminal would give.
    fn answered(
        args: &LoginArgs,
        answers: &str,
    ) -> Result<(LoginDetails, String), AuthCommandError> {
        let mut input = answers.as_bytes();
        let mut output = Vec::new();
        let details = resolve_login_details(args, true, &mut input, &mut output)?;
        Ok((details, String::from_utf8(output).unwrap()))
    }

    #[test]
    fn asks_for_every_value_in_order_when_none_were_passed() {
        let (details, prompts) = answered(
            &args(None, None, None, None),
            "DEV\nhttps://sap.example:8001\n100\nmparker\n",
        )
        .unwrap();

        assert_eq!(details.name, "DEV");
        assert_eq!(details.url, "https://sap.example:8001");
        assert_eq!(details.client, "100");
        assert_eq!(details.username, "mparker");
        // Asked in the order a person would expect to answer them.
        let order: Vec<_> = ["Profile name", "SAP base URL", "SAP client", "SAP username"]
            .iter()
            .map(|label| prompts.find(label).expect("every value is prompted for"))
            .collect();
        assert!(order.windows(2).all(|pair| pair[0] < pair[1]), "{prompts}");
    }

    #[test]
    fn asks_only_for_what_the_flags_left_out() {
        // A half-scripted call must not re-ask for what it already said.
        let (details, prompts) = answered(
            &args(Some("DEV"), Some("https://sap.example:8001"), None, None),
            "100\nmparker\n",
        )
        .unwrap();

        assert_eq!(details.client, "100");
        assert_eq!(details.username, "mparker");
        assert!(!prompts.contains("Profile name"), "{prompts}");
        assert!(!prompts.contains("SAP base URL"), "{prompts}");
    }

    #[test]
    fn flags_alone_never_touch_the_terminal() {
        // The scripted path must not block on a prompt, so nothing is read and
        // nothing is written even when a terminal is available.
        let (details, prompts) = answered(
            &args(
                Some("DEV"),
                Some("https://sap.example:8001/"),
                Some("100"),
                Some("mparker"),
            ),
            "",
        )
        .unwrap();

        assert!(prompts.is_empty(), "{prompts}");
        // A trailing slash would double up when paths are appended.
        assert_eq!(details.url, "https://sap.example:8001");
    }

    #[test]
    fn a_missing_value_without_a_terminal_names_the_flag_to_pass() {
        let mut input = std::io::empty();
        let mut output = Vec::new();

        let error = resolve_login_details(
            &args(
                None,
                Some("https://sap.example:8001"),
                Some("100"),
                Some("mparker"),
            ),
            false,
            &mut input,
            &mut output,
        )
        .unwrap_err();

        assert_eq!(error.code(), "missing_profile_value");
        assert!(error.hint().unwrap().contains("first argument"));
        // Nothing was asked, because there is nobody to answer.
        assert!(output.is_empty());
    }

    #[test]
    fn an_empty_flag_is_treated_as_absent_rather_than_stored() {
        // A blank profile name cannot select anything later, so it is asked
        // for rather than written to the config.
        let (details, prompts) = answered(
            &args(
                Some("   "),
                Some("https://sap.example:8001"),
                Some("100"),
                Some("mparker"),
            ),
            "DEV\n",
        )
        .unwrap();

        assert_eq!(details.name, "DEV");
        assert!(prompts.contains("Profile name"));
    }

    #[test]
    fn an_empty_answer_is_asked_again() {
        let (details, prompts) = answered(
            &args(
                None,
                Some("https://sap.example:8001"),
                Some("100"),
                Some("mparker"),
            ),
            "\n\nDEV\n",
        )
        .unwrap();

        assert_eq!(details.name, "DEV");
        assert_eq!(prompts.matches("That cannot be empty.").count(), 2);
    }

    #[test]
    fn input_that_ends_mid_prompt_is_the_same_as_having_no_terminal() {
        // Otherwise the loop would spin forever on a closed pipe.
        let error = answered(&args(None, None, None, None), "DEV\n").unwrap_err();

        assert_eq!(error.code(), "missing_profile_value");
        assert!(error.hint().unwrap().contains("--url"));
    }

    #[test]
    fn parses_auth_login_options_from_cli() {
        let cli = Cli::try_parse_from([
            "fractal",
            "auth",
            "login",
            "DEV_100",
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
        assert_eq!(args.name.as_deref(), Some("DEV_100"));
        assert_eq!(args.url.as_deref(), Some("https://sap.example:8001"));
        assert_eq!(args.client.as_deref(), Some("903"));
        assert_eq!(args.username.as_deref(), Some("mparker"));
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
            "DEV_100",
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
        let cli = Cli::try_parse_from(["fractal", "auth", "remove", "DEV_100"]).unwrap();

        let Command::Auth {
            command: AuthCommand::Remove(args),
        } = cli.command
        else {
            panic!("expected auth remove command");
        };
        assert_eq!(args.name, "DEV_100");
    }
}

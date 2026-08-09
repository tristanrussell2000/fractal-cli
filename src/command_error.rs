use fractal::{
    config, credentials,
    sap::{adt::AdtError, client::SapError},
};

#[derive(Debug)]
pub enum CommandError {
    Config(config::ConfigError),
    Credential(credentials::CredentialError),
    Sap(SapError),
    Adt(AdtError),
    Message {
        code: &'static str,
        message: String,
        hint: Option<String>,
    },
}

impl CommandError {
    pub(crate) fn from_message(code: &'static str, message: impl Into<String>) -> Self {
        Self::Message {
            code,
            message: message.into(),
            hint: None,
        }
    }

    pub(crate) fn with_hint(
        code: &'static str,
        message: impl Into<String>,
        hint: impl Into<String>,
    ) -> Self {
        Self::Message {
            code,
            message: message.into(),
            hint: Some(hint.into()),
        }
    }

    pub(crate) const fn code(&self) -> &'static str {
        match self {
            Self::Config(error) => match error {
                config::ConfigError::NoConfigDirectory => "config_directory_unavailable",
                config::ConfigError::Read { .. } => "config_read_error",
                config::ConfigError::Parse { .. } => "config_invalid",
                config::ConfigError::Write { .. } => "config_write_error",
                config::ConfigError::Serialize(_) => "config_serialize_error",
                config::ConfigError::ProfileNotFound(_) => "profile_not_found",
                config::ConfigError::NoProfileSelected => "no_default_profile",
            },
            Self::Credential(error) => match error {
                credentials::CredentialError::InvalidProfileName(_) => "invalid_profile_name",
                credentials::CredentialError::Store(_) => "credential_store_error",
                credentials::CredentialError::Missing(_) => "credential_missing",
            },
            Self::Sap(error) => error.code(),
            Self::Adt(error) => error.code(),
            Self::Message { code, .. } => code,
        }
    }

    #[must_use]
    pub(crate) const fn status(&self) -> Option<u16> {
        match self {
            Self::Sap(SapError::Http { status, .. }) => Some(status.as_u16()),
            _ => None,
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Config(error) => error.to_string(),
            Self::Credential(error) => error.to_string(),
            Self::Sap(error) => error.to_string(),
            Self::Adt(error) => error.to_string(),
            Self::Message { message, .. } => message.clone(),
        }
    }

    pub(crate) fn hint(&self) -> Option<String> {
        match self {
            Self::Config(config::ConfigError::NoProfileSelected) => Some(
                "Pass --profile <name> for this command, or set one with `fractal auth login <name> --default`."
                    .to_owned(),
            ),
            Self::Credential(credentials::CredentialError::Missing(profile)) => Some(format!(
                "Run `fractal auth login {profile}` to store the missing credential."
            )),
            Self::Config(_) | Self::Credential(_) => None,
            Self::Sap(error) => Some(error.hint().to_owned()),
            Self::Adt(error) => error.hint(),
            Self::Message { hint, .. } => hint.clone(),
        }
    }
}

impl From<config::ConfigError> for CommandError {
    fn from(error: config::ConfigError) -> Self {
        Self::Config(error)
    }
}

impl From<credentials::CredentialError> for CommandError {
    fn from(error: credentials::CredentialError) -> Self {
        Self::Credential(error)
    }
}

impl From<SapError> for CommandError {
    fn from(error: SapError) -> Self {
        Self::Sap(error)
    }
}

impl From<AdtError> for CommandError {
    fn from(error: AdtError) -> Self {
        Self::Adt(error)
    }
}

impl From<String> for CommandError {
    fn from(message: String) -> Self {
        Self::from_message("command_error", message)
    }
}

impl From<std::io::Error> for CommandError {
    fn from(error: std::io::Error) -> Self {
        Self::from_message("io_error", error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::CommandError;
    use fractal::sap::client::{SapError, SapErrorKind};

    #[test]
    fn preserves_structured_sap_error_fields() {
        let error = CommandError::from(SapError::Http {
            kind: SapErrorKind::AuthenticationFailed,
            status: StatusCode::UNAUTHORIZED,
            url: "https://sap.example/sap/bc/adt/core/discovery".to_owned(),
            message: "Invalid credentials".to_owned(),
        });

        assert_eq!(error.code(), "authentication_failed");
        assert_eq!(error.status(), Some(401));
        assert!(error.message().contains("Invalid credentials"));
        assert!(error.hint().is_some());
    }

    #[test]
    fn missing_profile_error_includes_repair_hint() {
        let error = CommandError::from(fractal::credentials::CredentialError::Missing(
            "DE2_903".to_owned(),
        ));

        assert_eq!(error.code(), "credential_missing");
        assert!(error.hint().unwrap().contains("DE2_903"));
    }
}

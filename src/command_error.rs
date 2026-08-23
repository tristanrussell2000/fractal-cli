use fractal::{
    config, credentials,
    sap::{
        adt::AdtError,
        client::SapError,
        edit::{AdtSourcePatchError, AdtSourceReadError},
        package::PackageError,
        source_check::AdtSourceCheckError,
        table::TableError,
    },
};

#[derive(Debug)]
pub enum CommandError {
    Config(config::ConfigError),
    Credential(credentials::CredentialError),
    Sap(SapError),
    Adt(AdtError),
    AdtSourceRead(AdtSourceReadError),
    AdtSourcePatch(AdtSourcePatchError),
    AdtSourceCheck(AdtSourceCheckError),
    Package(PackageError),
    Table(TableError),
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
            Self::AdtSourceRead(error) => error.code(),
            Self::AdtSourcePatch(error) => error.code(),
            Self::AdtSourceCheck(error) => error.code(),
            Self::Package(error) => match error {
                PackageError::Sap(error) => error.code(),
                PackageError::Parse(_) => "package_response_parse_error",
            },
            Self::Table(error) => error.code(),
            Self::Message { code, .. } => code,
        }
    }

    #[must_use]
    pub(crate) fn status(&self) -> Option<u16> {
        match self {
            Self::Sap(SapError::Http { status, .. }) => Some(status.as_u16()),
            Self::Table(error) => match error.sap_error() {
                Some(SapError::Http { status, .. }) => Some(status.as_u16()),
                _ => None,
            },
            Self::AdtSourceRead(error) => match error.sap_error() {
                Some(SapError::Http { status, .. }) => Some(status.as_u16()),
                _ => None,
            },
            Self::AdtSourcePatch(error) => match error.sap_error() {
                Some(SapError::Http { status, .. }) => Some(status.as_u16()),
                _ => None,
            },
            Self::AdtSourceCheck(error) => match error.sap_error() {
                Some(SapError::Http { status, .. }) => Some(status.as_u16()),
                _ => None,
            },
            _ => None,
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Config(error) => error.to_string(),
            Self::Credential(error) => error.to_string(),
            Self::Sap(error) => error.to_string(),
            Self::Adt(error) => error.to_string(),
            Self::AdtSourceRead(error) => error.to_string(),
            Self::AdtSourcePatch(error) => error.to_string(),
            Self::AdtSourceCheck(error) => error.to_string(),
            Self::Package(error) => error.to_string(),
            Self::Table(error) => error.to_string(),
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
            Self::Sap(error) | Self::Package(PackageError::Sap(error)) => {
                Some(error.hint().to_owned())
            }
            Self::Adt(error) => error.hint(),
            Self::AdtSourceRead(error) => Some(error.hint()),
            Self::AdtSourcePatch(error) => Some(error.hint()),
            Self::AdtSourceCheck(error) => Some(error.hint()),
            Self::Package(PackageError::Parse(_)) => Some(
                "The SAP package response did not match the expected nodestructure format."
                    .to_owned(),
            ),
            Self::Table(error) => error.hint(),
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

impl From<AdtSourceReadError> for CommandError {
    fn from(error: AdtSourceReadError) -> Self {
        Self::AdtSourceRead(error)
    }
}

impl From<AdtSourcePatchError> for CommandError {
    fn from(error: AdtSourcePatchError) -> Self {
        Self::AdtSourcePatch(error)
    }
}

impl From<AdtSourceCheckError> for CommandError {
    fn from(error: AdtSourceCheckError) -> Self {
        Self::AdtSourceCheck(error)
    }
}

impl From<PackageError> for CommandError {
    fn from(error: PackageError) -> Self {
        Self::Package(error)
    }
}

impl From<TableError> for CommandError {
    fn from(error: TableError) -> Self {
        Self::Table(error)
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
    use fractal::{
        edit::EditError,
        sap::{
            client::{SapError, SapErrorKind},
            edit::{AdtSourcePatchError, AdtSourceReadError},
            source_check::AdtSourceCheckError,
            table::{TableError, TableQueryError, TableQueryErrorKind},
        },
    };

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

    #[test]
    fn preserves_structured_table_error_fields_and_http_status() {
        let source = SapError::Http {
            kind: SapErrorKind::Other,
            status: StatusCode::BAD_REQUEST,
            url: "https://sap.example/sap/bc/adt/datapreview/freestyle".to_owned(),
            message: "Unknown column name \"EVNT_ID\".".to_owned(),
        };
        let error = CommandError::from(TableError::Query {
            query: TableQueryError {
                kind: TableQueryErrorKind::UnknownColumn,
                identifier: Some("EVNT_ID".to_owned()),
                suggestions: vec!["EVENT_ID".to_owned()],
                message: "Unknown column name \"EVNT_ID\".".to_owned(),
            },
            source: Box::new(source),
        });

        assert_eq!(error.code(), "table_query_unknown_column");
        assert_eq!(error.status(), Some(400));
        assert!(error.message().contains("EVNT_ID"));
        assert!(error.hint().unwrap().contains("EVENT_ID"));
    }

    #[test]
    fn preserves_edit_source_validation_and_sap_errors() {
        let validation =
            CommandError::from(AdtSourceReadError::UnsupportedObjectType("DOMA".to_owned()));
        assert_eq!(validation.code(), "unsupported_edit_object_type");
        assert_eq!(validation.status(), None);
        assert!(validation.hint().unwrap().contains("CLAS"));

        let sap = CommandError::from(AdtSourceReadError::Sap(SapError::Http {
            kind: SapErrorKind::NotFound,
            status: reqwest::StatusCode::NOT_FOUND,
            url: "https://sap.example/sap/bc/adt/oo/classes/zmissing/source/main".to_owned(),
            message: "Object not found".to_owned(),
        }));
        assert_eq!(sap.code(), "not_found");
        assert_eq!(sap.status(), Some(404));
        assert!(sap.message().contains("Object not found"));
    }

    #[test]
    fn preserves_patch_stage_codes_hints_and_http_statuses() {
        let namespace = CommandError::from(AdtSourcePatchError::Namespace(
            EditError::ObjectOutsideCustomerNamespaces {
                name: "SAP_STANDARD".to_owned(),
                namespaces: vec!["Z*".to_owned()],
            },
        ));
        assert_eq!(namespace.code(), "object_outside_customer_namespaces");
        assert_eq!(namespace.status(), None);
        assert!(namespace.hint().unwrap().contains("Z*"));

        let lock = CommandError::from(AdtSourcePatchError::Lock {
            transport: None,
            source: SapError::Http {
                kind: SapErrorKind::Other,
                status: StatusCode::CONFLICT,
                url: "https://sap.example/sap/bc/adt/programs/programs/zsample".to_owned(),
                message: "Object is locked".to_owned(),
            },
        });
        assert_eq!(lock.code(), "edit_lock_failed");
        assert_eq!(lock.status(), Some(409));
        assert!(lock.message().contains("Object is locked"));
    }

    #[test]
    fn preserves_source_check_stage_and_http_status() {
        let error = CommandError::from(AdtSourceCheckError::Sap(SapError::Http {
            kind: SapErrorKind::Forbidden,
            status: StatusCode::FORBIDDEN,
            url: "https://sap.example/sap/bc/adt/checkruns".to_owned(),
            message: "Check authorization missing".to_owned(),
        }));

        assert_eq!(error.code(), "edit_source_check_failed");
        assert_eq!(error.status(), Some(403));
        assert!(error.message().contains("Check authorization missing"));
        assert!(error.hint().is_some());
    }
}

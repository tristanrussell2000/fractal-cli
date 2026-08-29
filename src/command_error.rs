use fractal::{
    config, credentials,
    sap::{
        client::SapClientError,
        editable_source::{AdtSourceReadError, EditableAdtSourceTargetError},
        error_diagnostics::AdtErrorDiagnostics,
        object_info::ObjectInfoError,
        object_search::ObjectSearchError,
        object_source::ObjectSourceError,
        object_usages::ObjectUsagesError,
        package::PackageError,
        source_activation::AdtSourceActivationError,
        source_check::AdtSourceCheckError,
        source_discard::AdtInactiveSourceDiscardError,
        source_patch::AdtSourcePatchError,
        source_replace::AdtSourceReplacementError,
        table::TableError,
    },
};

#[derive(Debug)]
pub enum CommandError {
    Config(config::ConfigError),
    Credential(credentials::CredentialError),
    Sap(SapClientError),
    Object(ObjectCommandError),
    Edit(EditCommandError),
    Package(PackageError),
    Table(TableError),
    Message {
        code: &'static str,
        message: String,
        hint: Option<String>,
    },
}

/// One repository-object operation failure reported through the CLI boundary.
///
/// Each operation owns an error whose variants are its real failure positions;
/// this enum only routes them through the shared diagnostic access.
#[derive(Debug)]
pub enum ObjectCommandError {
    Search(ObjectSearchError),
    Source(ObjectSourceError),
    Info(ObjectInfoError),
    Usages(ObjectUsagesError),
}

impl ObjectCommandError {
    fn diagnostics(&self) -> &dyn AdtErrorDiagnostics {
        match self {
            Self::Search(error) => error,
            Self::Source(error) => error,
            Self::Info(error) => error,
            Self::Usages(error) => error,
        }
    }
}

/// One ADT source-workflow failure reported through the CLI error boundary.
///
/// Each variant keeps its own workflow-specific codes, hints, and recovery
/// guidance. This enum only routes them through the shared diagnostic access,
/// so `CommandError` needs one arm per accessor rather than one per workflow.
#[derive(Debug)]
pub enum EditCommandError {
    Target(EditableAdtSourceTargetError),
    SourceRead(AdtSourceReadError),
    Patch(AdtSourcePatchError),
    Replacement(AdtSourceReplacementError),
    Check(AdtSourceCheckError),
    Activation(AdtSourceActivationError),
    Discard(AdtInactiveSourceDiscardError),
}

impl EditCommandError {
    fn diagnostics(&self) -> &dyn AdtErrorDiagnostics {
        match self {
            Self::Target(error) => error,
            Self::SourceRead(error) => error,
            Self::Patch(error) => error,
            Self::Replacement(error) => error,
            Self::Check(error) => error,
            Self::Activation(error) => error,
            Self::Discard(error) => error,
        }
    }
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

    pub(crate) fn code(&self) -> &'static str {
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
            Self::Object(error) => error.diagnostics().code(),
            Self::Edit(error) => error.diagnostics().code(),
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
            Self::Sap(SapClientError::Http { status, .. }) => Some(status.as_u16()),
            Self::Table(error) => match error.sap_error() {
                Some(SapClientError::Http { status, .. }) => Some(status.as_u16()),
                _ => None,
            },
            Self::Edit(error) => error.diagnostics().http_status(),
            Self::Object(error) => error.diagnostics().http_status(),
            _ => None,
        }
    }

    pub(crate) fn message(&self) -> String {
        match self {
            Self::Config(error) => error.to_string(),
            Self::Credential(error) => error.to_string(),
            Self::Sap(error) => error.to_string(),
            Self::Object(error) => error.diagnostics().to_string(),
            Self::Edit(error) => error.diagnostics().to_string(),
            Self::Package(error) => error.to_string(),
            Self::Table(error) => error.to_string(),
            Self::Message { message, .. } => message.clone(),
        }
    }

    /// A read-only command that diagnoses this failure, if one exists.
    ///
    /// Deliberately absent for credential and profile failures: their remedy is
    /// `fractal auth login`, which mutates local state and prompts for a
    /// password, so it stays in prose where a human decides to run it.
    pub(crate) fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Sap(error) | Self::Package(PackageError::Sap(error)) => error.suggested_command(),
            Self::Edit(error) => error.diagnostics().suggested_command(),
            Self::Object(error) => error.diagnostics().suggested_command(),
            Self::Table(error) => error.suggested_command(),
            _ => None,
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
                Some(error.hint())
            }
            Self::Object(error) => Some(error.diagnostics().hint()),
            Self::Edit(error) => Some(error.diagnostics().hint()),
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

impl From<SapClientError> for CommandError {
    fn from(error: SapClientError) -> Self {
        Self::Sap(error)
    }
}

impl From<ObjectSearchError> for CommandError {
    fn from(error: ObjectSearchError) -> Self {
        Self::Object(ObjectCommandError::Search(error))
    }
}

impl From<ObjectSourceError> for CommandError {
    fn from(error: ObjectSourceError) -> Self {
        Self::Object(ObjectCommandError::Source(error))
    }
}

impl From<ObjectInfoError> for CommandError {
    fn from(error: ObjectInfoError) -> Self {
        Self::Object(ObjectCommandError::Info(error))
    }
}

impl From<ObjectUsagesError> for CommandError {
    fn from(error: ObjectUsagesError) -> Self {
        Self::Object(ObjectCommandError::Usages(error))
    }
}

impl From<AdtSourceReadError> for CommandError {
    fn from(error: AdtSourceReadError) -> Self {
        Self::Edit(EditCommandError::SourceRead(error))
    }
}

impl From<EditableAdtSourceTargetError> for CommandError {
    fn from(error: EditableAdtSourceTargetError) -> Self {
        Self::Edit(EditCommandError::Target(error))
    }
}

impl From<AdtSourcePatchError> for CommandError {
    fn from(error: AdtSourcePatchError) -> Self {
        Self::Edit(EditCommandError::Patch(error))
    }
}

impl From<AdtSourceCheckError> for CommandError {
    fn from(error: AdtSourceCheckError) -> Self {
        Self::Edit(EditCommandError::Check(error))
    }
}

impl From<AdtSourceActivationError> for CommandError {
    fn from(error: AdtSourceActivationError) -> Self {
        Self::Edit(EditCommandError::Activation(error))
    }
}

impl From<AdtInactiveSourceDiscardError> for CommandError {
    fn from(error: AdtInactiveSourceDiscardError) -> Self {
        Self::Edit(EditCommandError::Discard(error))
    }
}

impl From<AdtSourceReplacementError> for CommandError {
    fn from(error: AdtSourceReplacementError) -> Self {
        Self::Edit(EditCommandError::Replacement(error))
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
    use fractal::sap::{
        client::{SapClientError, SapHttpErrorKind},
        edit_session::AdtEditSessionError,
        editable_source::{
            AdtEditTargetValidationError, AdtSourceReadError, AdtSourceVersion,
            CustomerNamespaceError, EditableAdtObjectType, EditableAdtSourceIdentity,
            EditableAdtSourceTargetError,
        },
        source_activation::AdtSourceActivationError,
        source_check::AdtSourceCheckError,
        source_discard::AdtInactiveSourceDiscardError,
        source_patch::AdtSourcePatchError,
        source_replace::AdtSourceReplacementError,
        table::{TableError, TableQueryError, TableQueryErrorKind},
    };

    fn sample_identity() -> Box<EditableAdtSourceIdentity> {
        Box::new(EditableAdtSourceIdentity {
            object_type: EditableAdtObjectType::Class,
            name: "ZCL_SAMPLE".to_owned(),
            object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
            source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
        })
    }

    #[test]
    fn preserves_structured_sap_error_fields() {
        let error = CommandError::from(SapClientError::Http {
            kind: SapHttpErrorKind::AuthenticationFailed,
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
        let source = SapClientError::Http {
            kind: SapHttpErrorKind::Other,
            status: StatusCode::BAD_REQUEST,
            url: "https://sap.example/sap/bc/adt/datapreview/freestyle".to_owned(),
            message: "Unknown column name \"EVNT_ID\".".to_owned(),
        };
        let error = CommandError::from(TableError::Query {
            query: TableQueryError {
                kind: TableQueryErrorKind::UnknownColumn,
                entity: Some("ZDEMO_EVENT_LOG".to_owned()),
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
        let validation = CommandError::from(EditableAdtSourceTargetError::UnsupportedObjectType(
            "DOMA".to_owned(),
        ));
        assert_eq!(validation.code(), "unsupported_edit_object_type");
        assert_eq!(validation.status(), None);
        assert!(validation.hint().unwrap().contains("CLAS"));

        let sap = CommandError::from(AdtSourceReadError::Sap {
            object_type: "CLAS",
            name: "ZMISSING".to_owned(),
            source: SapClientError::Http {
                kind: SapHttpErrorKind::NotFound,
                status: reqwest::StatusCode::NOT_FOUND,
                url: "https://sap.example/sap/bc/adt/oo/classes/zmissing/source/main".to_owned(),
                message: "Object not found".to_owned(),
            },
        });
        assert_eq!(sap.code(), "not_found");
        assert_eq!(sap.status(), Some(404));
        assert!(sap.message().contains("Object not found"));
    }

    #[test]
    fn preserves_patch_stage_codes_hints_and_http_statuses() {
        let namespace = CommandError::from(AdtSourcePatchError::Validation(
            AdtEditTargetValidationError::Namespace(CustomerNamespaceError {
                name: "SAP_STANDARD".to_owned(),
                namespaces: vec!["Z*".to_owned()],
            }),
        ));
        assert_eq!(namespace.code(), "object_outside_customer_namespaces");
        assert_eq!(namespace.status(), None);
        assert!(namespace.hint().unwrap().contains("Z*"));

        let lock = CommandError::from(AdtSourcePatchError::Session(
            AdtEditSessionError::LockFailed {
                transport: None,
                source: SapClientError::Http {
                    kind: SapHttpErrorKind::Other,
                    status: StatusCode::CONFLICT,
                    url: "https://sap.example/sap/bc/adt/programs/programs/zsample".to_owned(),
                    message: "Object is locked".to_owned(),
                },
            },
        ));
        assert_eq!(lock.code(), "edit_lock_failed");
        assert_eq!(lock.status(), Some(409));
        assert!(lock.message().contains("Object is locked"));
    }

    #[test]
    fn preserves_source_check_stage_and_http_status() {
        let error = CommandError::from(AdtSourceCheckError::Sap {
            identity: sample_identity(),
            version: AdtSourceVersion::Inactive,
            source: SapClientError::Http {
                kind: SapHttpErrorKind::Forbidden,
                status: StatusCode::FORBIDDEN,
                url: "https://sap.example/sap/bc/adt/checkruns".to_owned(),
                message: "Check authorization missing".to_owned(),
            },
        });

        assert_eq!(error.code(), "edit_source_check_failed");
        assert_eq!(error.status(), Some(403));
        assert!(error.message().contains("Check authorization missing"));
        assert!(error.hint().is_some());
    }

    #[test]
    fn preserves_source_activation_stage_and_http_status() {
        let error = CommandError::from(AdtSourceActivationError::ActivationRequest {
            identity: sample_identity(),
            source: SapClientError::Http {
                kind: SapHttpErrorKind::Forbidden,
                status: StatusCode::FORBIDDEN,
                url: "https://sap.example/sap/bc/adt/activation".to_owned(),
                message: "Activation authorization missing".to_owned(),
            },
        });

        assert_eq!(error.code(), "edit_activation_request_failed");
        assert_eq!(error.status(), Some(403));
        assert!(error.message().contains("Activation authorization missing"));
        let hint = error.hint().unwrap();
        assert!(hint.contains("may have reached SAP"));
        assert!(
            hint.contains("fractal edit check --type CLAS --name ZCL_SAMPLE --version inactive")
        );
    }

    #[test]
    fn preserves_source_discard_stage_and_http_status() {
        let error = CommandError::from(AdtInactiveSourceDiscardError::RestoredSourceActivation(
            AdtSourceActivationError::ActivationRequest {
                identity: sample_identity(),
                source: SapClientError::Http {
                    kind: SapHttpErrorKind::Other,
                    status: StatusCode::CONFLICT,
                    url: "https://sap.example/sap/bc/adt/activation".to_owned(),
                    message: "Restored source could not be activated".to_owned(),
                },
            },
        ));

        assert_eq!(error.code(), "edit_discard_activation_failed");
        assert_eq!(error.status(), Some(409));
        assert!(error.message().contains("could not be activated"));
        assert!(error.hint().unwrap().contains("now contains"));
    }

    #[test]
    fn preserves_source_replacement_stage_and_http_status() {
        let error = CommandError::from(AdtSourceReplacementError::PreviewSourceRead(
            AdtSourceReadError::Sap {
                object_type: "PROG",
                name: "ZSAMPLE".to_owned(),
                source: SapClientError::Http {
                    kind: SapHttpErrorKind::Forbidden,
                    status: StatusCode::FORBIDDEN,
                    url: "https://sap.example/sap/bc/adt/programs/programs/zsample/source/main"
                        .to_owned(),
                    message: "Source read authorization missing".to_owned(),
                },
            },
        ));

        assert_eq!(error.code(), "edit_source_replacement_preview_read_failed");
        assert_eq!(error.status(), Some(403));
        assert!(error.message().contains("authorization missing"));
        assert!(error.hint().is_some());
    }
}

use std::fmt::Display;

use super::{
    client::SapError,
    editable_source::{AdtSourceReadError, EditableAdtSourceTargetError},
    object_info::ObjectInfoError,
    object_search::ObjectSearchError,
    object_source::ObjectSourceError,
    object_usages::ObjectUsagesError,
    source_activation::AdtSourceActivationError,
    source_check::AdtSourceCheckError,
    source_discard::AdtInactiveSourceDiscardError,
    source_patch::AdtSourcePatchError,
    source_replace::AdtSourceReplacementError,
};

/// Uniform diagnostic access for ADT source-workflow errors.
///
/// Every implementor already exposes a stable machine-readable code, an
/// agent-facing hint, and the underlying [`SapError`] when a request reached
/// SAP. This trait exists so one caller — the CLI error boundary — can read
/// those without repeating an arm per workflow. It deliberately does not
/// unify the errors themselves: each workflow keeps its own variants and
/// recovery guidance.
pub trait AdtErrorDiagnostics: Display {
    /// The stable machine-readable code for this failure.
    #[must_use]
    fn code(&self) -> &'static str;

    /// Actionable recovery advice for this failure.
    #[must_use]
    fn hint(&self) -> String;

    /// The underlying SAP failure, when the workflow reached SAP.
    #[must_use]
    fn sap_error(&self) -> Option<&SapError>;

    /// A command that diagnoses this failure, if one can be derived.
    ///
    /// **Read-only by construction.** A caller may reasonably execute this
    /// value directly, so it must never contain a mutation: a write appearing
    /// here would defeat the save-only, activate-explicitly discipline the edit
    /// design rests on. Retry-the-write advice — including transport retries —
    /// stays in prose `hint`.
    #[must_use]
    fn suggested_command(&self) -> Option<String> {
        None
    }

    /// The HTTP status of the underlying SAP failure, when there is one.
    #[must_use]
    fn http_status(&self) -> Option<u16> {
        match self.sap_error() {
            Some(SapError::Http { status, .. }) => Some(status.as_u16()),
            _ => None,
        }
    }
}

impl AdtErrorDiagnostics for EditableAdtSourceTargetError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    // Object type and name are validated before any request is sent.
    fn sap_error(&self) -> Option<&SapError> {
        None
    }
}

impl AdtErrorDiagnostics for AdtSourceReadError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for AdtSourcePatchError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for AdtSourceReplacementError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for AdtSourceCheckError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for AdtSourceActivationError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for AdtInactiveSourceDiscardError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }
}

impl AdtErrorDiagnostics for ObjectSearchError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for ObjectSourceError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for ObjectInfoError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

impl AdtErrorDiagnostics for ObjectUsagesError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }

    fn suggested_command(&self) -> Option<String> {
        Self::suggested_command(self)
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;
    use crate::{
        sap::{
            client::SapErrorKind,
            edit_session::AdtEditSessionError,
            editable_source::{
                AdtEditTargetValidationError, AdtSourceVersion, EditableAdtObjectType,
                EditableAdtSourceIdentity, TransportRequestError,
            },
            source_activation::AdtSourceActivationError,
            source_discard::AdtInactiveSourceDiscardError,
            source_patch::AdtSourcePatchError,
            source_replace::AdtSourceReplacementError,
        },
        source_change::SourceChangePlanError,
    };

    /// Every verb that writes to SAP. A suggested command starting with one of
    /// these would invite a caller to execute a mutation it never asked for.
    const MUTATING_COMMANDS: [&str; 4] = [
        "fractal edit patch",
        "fractal edit set",
        "fractal edit activate",
        "fractal edit discard",
    ];

    fn identity() -> Box<EditableAdtSourceIdentity> {
        Box::new(EditableAdtSourceIdentity {
            object_type: EditableAdtObjectType::Class,
            name: "ZCL_SAMPLE".to_owned(),
            object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
            source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
        })
    }

    fn lock_conflict() -> AdtEditSessionError {
        AdtEditSessionError::LockFailed {
            transport: Some("DE3K900575".to_owned()),
            source: SapError::Http {
                kind: SapErrorKind::Other,
                status: StatusCode::CONFLICT,
                url: "https://sap.example/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                message: "Object is already locked in request DE3K900575".to_owned(),
            },
        }
    }

    #[test]
    fn no_mutating_workflow_ever_suggests_a_write_command() {
        let errors: Vec<Box<dyn AdtErrorDiagnostics>> = vec![
            Box::new(AdtSourcePatchError::Patch {
                identity: identity(),
                source: SourceChangePlanError::AnchorNotFound,
            }),
            Box::new(AdtSourcePatchError::Session(lock_conflict())),
            Box::new(AdtSourceReplacementError::Replacement {
                identity: identity(),
                source: SourceChangePlanError::SourceReplacementNoChanges,
            }),
            Box::new(AdtSourceReplacementError::Session(lock_conflict())),
            Box::new(AdtSourceActivationError::NoInactiveVersion {
                identity: identity(),
            }),
            Box::new(AdtSourceActivationError::TransportAttachment(
                lock_conflict(),
            )),
            Box::new(AdtInactiveSourceDiscardError::ActiveSourceChanged {
                identity: identity(),
                before_sha256: "a".repeat(64),
                after_sha256: "b".repeat(64),
            }),
            Box::new(AdtInactiveSourceDiscardError::Session(
                AdtEditSessionError::UnlockFailed(SapError::Network {
                    url: "https://sap.example".to_owned(),
                    message: "connection reset".to_owned(),
                }),
            )),
        ];

        for error in errors {
            let Some(command) = error.suggested_command() else {
                continue;
            };
            assert!(
                command.starts_with("fractal "),
                "a suggested command must be runnable as printed: {command}"
            );
            for mutation in MUTATING_COMMANDS {
                assert!(
                    !command.starts_with(mutation),
                    "{} suggested the mutating command {command}",
                    error.code()
                );
            }
        }
    }

    #[test]
    fn transport_retry_advice_stays_in_prose() {
        // The one remedy that is genuinely "re-run your write, differently".
        // It must reach the caller as a hint and never as an executable field.
        let error = AdtSourcePatchError::Session(lock_conflict());

        assert!(error.hint().contains("DE3K900575"));
        assert_eq!(error.suggested_command(), None);
    }

    #[test]
    fn local_validation_failures_have_no_command_to_offer() {
        let error = AdtSourcePatchError::Validation(
            AdtEditTargetValidationError::InvalidTransport(TransportRequestError {
                value: "not a request".to_owned(),
            }),
        );

        assert_eq!(error.suggested_command(), None);
    }

    #[test]
    fn derives_http_status_only_from_a_sap_http_failure() {
        let http: &dyn AdtErrorDiagnostics = &AdtSourceCheckError::Sap {
            identity: identity(),
            version: AdtSourceVersion::Inactive,
            source: SapError::Http {
                kind: SapErrorKind::Forbidden,
                status: StatusCode::FORBIDDEN,
                url: "https://sap.example/sap/bc/adt/checkruns".to_owned(),
                message: "Check authorization missing".to_owned(),
            },
        };
        assert_eq!(http.http_status(), Some(403));

        let network: &dyn AdtErrorDiagnostics = &AdtSourceCheckError::Sap {
            identity: identity(),
            version: AdtSourceVersion::Inactive,
            source: SapError::Network {
                url: "https://sap.example/sap/bc/adt/checkruns".to_owned(),
                message: "connection refused".to_owned(),
            },
        };
        assert_eq!(network.http_status(), None);

        let local: &dyn AdtErrorDiagnostics =
            &EditableAdtSourceTargetError::UnsupportedObjectType("DOMA".to_owned());
        assert_eq!(local.http_status(), None);
        assert_eq!(local.code(), "unsupported_edit_object_type");
    }
}

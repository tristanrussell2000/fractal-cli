use std::fmt::Display;

use super::{
    client::SapError,
    editable_source::{AdtSourceReadError, EditableAdtSourceTargetError},
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
}

impl AdtErrorDiagnostics for AdtInactiveSourceDiscardError {
    fn code(&self) -> &'static str {
        Self::code(self)
    }

    fn hint(&self) -> String {
        Self::hint(self)
    }

    fn sap_error(&self) -> Option<&SapError> {
        Self::sap_error(self)
    }
}

#[cfg(test)]
mod tests {
    use reqwest::StatusCode;

    use super::*;
    use crate::sap::client::SapErrorKind;

    #[test]
    fn derives_http_status_only_from_a_sap_http_failure() {
        let http: &dyn AdtErrorDiagnostics = &AdtSourceCheckError::Sap(SapError::Http {
            kind: SapErrorKind::Forbidden,
            status: StatusCode::FORBIDDEN,
            url: "https://sap.example/sap/bc/adt/checkruns".to_owned(),
            message: "Check authorization missing".to_owned(),
        });
        assert_eq!(http.http_status(), Some(403));

        let network: &dyn AdtErrorDiagnostics = &AdtSourceCheckError::Sap(SapError::Network {
            url: "https://sap.example/sap/bc/adt/checkruns".to_owned(),
            message: "connection refused".to_owned(),
        });
        assert_eq!(network.http_status(), None);

        let local: &dyn AdtErrorDiagnostics =
            &EditableAdtSourceTargetError::UnsupportedObjectType("DOMA".to_owned());
        assert_eq!(local.http_status(), None);
        assert_eq!(local.code(), "unsupported_edit_object_type");
    }
}

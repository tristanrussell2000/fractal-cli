//! The reporting contract every Fractal error implements.
//!
//! This is the whole of what the CLI boundary consumes: a stable machine code,
//! a message, an optional HTTP status, optional advice, and an optional
//! runnable command. Errors are not otherwise inspected — exactly one site in
//! the crate matches an error's structure to change behavior, and it does so
//! on a concrete type — so a trait is a truer description of the boundary than
//! an enum that has to name every error family.
//!
//! It lives in the library because the library legitimately authors remedial
//! advice: the code that knows which operation failed at which stage is the
//! only code that knows the remedy.

use std::fmt::{Debug, Display};

use crate::sap::client::SapClientError;

/// An error that can render itself as Fractal's structured error output.
///
/// `Debug` is a supertrait because tests `.unwrap()` on `Result`s carrying
/// these errors, and `Display` because it is the message.
pub trait ReportableError: Display + Debug {
    /// The stable machine-readable code. This is an agent-facing contract:
    /// changing one is a breaking change.
    fn code(&self) -> &'static str;

    /// The human-readable explanation. `Display` is the message.
    fn message(&self) -> String {
        self.to_string()
    }

    /// The HTTP status, when this failure came from a SAP response.
    fn status(&self) -> Option<u16> {
        None
    }

    /// Actionable recovery advice.
    fn hint(&self) -> Option<String> {
        None
    }

    /// A command that diagnoses this failure, if one can be derived.
    ///
    /// **Read-only by construction.** A caller may execute this value
    /// directly, so it must never contain a mutation: a write appearing here
    /// would defeat the save-only, activate-explicitly discipline the edit
    /// design rests on. Retry-the-write advice stays in `hint`.
    fn suggested_command(&self) -> Option<String> {
        None
    }
}

/// Extracts the HTTP status from an optional underlying transport failure.
///
/// Keeps the `status()` implementation of every SAP-backed error to one line
/// without putting a `sap_error()` accessor into the public contract.
#[must_use]
pub const fn sap_http_status(error: Option<&SapClientError>) -> Option<u16> {
    match error {
        Some(SapClientError::Http { status, .. }) => Some(status.as_u16()),
        _ => None,
    }
}

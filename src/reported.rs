use fractal::reportable_error::ReportableError;

/// One failure on its way to the CLI's structured error output.
///
/// This is an envelope, not a taxonomy. The boundary consumes exactly five
/// things — code, status, message, hint, and suggested command — so it holds
/// any error that can report them, plus the one piece of context an operation
/// cannot know: the command the caller actually typed.
#[derive(Debug)]
pub struct Reported(Box<dyn ReportableError>);

impl<E: ReportableError + 'static> From<E> for Reported {
    fn from(error: E) -> Self {
        Self(Box::new(error))
    }
}

impl Reported {
    pub(crate) fn code(&self) -> &'static str {
        self.0.code()
    }

    pub(crate) fn status(&self) -> Option<u16> {
        self.0.status()
    }

    pub(crate) fn message(&self) -> String {
        self.0.message()
    }

    pub(crate) fn hint(&self) -> Option<String> {
        self.0.hint()
    }

    pub(crate) fn suggested_command(&self) -> Option<String> {
        self.0.suggested_command()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fractal::sap::client::{SapClientError, SapHttpErrorKind};

    #[test]
    fn reports_the_inner_errors_fields() {
        let reported = Reported::from(SapClientError::Http {
            kind: SapHttpErrorKind::AuthenticationFailed,
            status: reqwest::StatusCode::UNAUTHORIZED,
            url: "https://sap.example/sap/bc/adt/core/discovery".to_owned(),
            message: "Invalid credentials".to_owned(),
        });

        assert_eq!(reported.code(), "authentication_failed");
        assert_eq!(reported.status(), Some(401));
        assert!(reported.message().contains("Invalid credentials"));
        assert_eq!(
            reported.suggested_command().as_deref(),
            Some("fractal system test")
        );
    }

    #[test]
    fn any_error_that_can_report_itself_crosses_the_boundary() {
        // `?` converts through the blanket impl with no `map_err`, which is
        // what keeps handlers free of error plumbing.
        fn handler() -> Result<(), Reported> {
            Err(SapClientError::Network {
                url: "https://sap.example".to_owned(),
                message: "connection refused".to_owned(),
            })?
        }

        let reported = handler().unwrap_err();
        assert_eq!(reported.code(), "network_error");
        assert!(reported.hint().unwrap().contains("VPN"));
    }
}

use std::time::{Duration, Instant};

use reqwest::header::{ACCEPT, HeaderMap, HeaderValue};
use thiserror::Error;

use super::client::{SapClient, SapClientError};
use crate::reportable_error::{ReportableError, sap_http_status};

const CLASSRUN_PATH: &str = "/sap/bc/adt/oo/classrun";

/// The maximum length of an ABAP class name.
const MAX_CLASS_NAME: usize = 30;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClassRunResult {
    pub class_name: String,
    /// Everything the class wrote through `out->write( )`, in order.
    pub output: String,
    pub elapsed: Duration,
}

#[derive(Debug, Error)]
pub enum ClassRunError {
    #[error("invalid ABAP class name: {name}")]
    InvalidName { name: String },
    #[error("running class {class_name} failed: {source}")]
    Sap {
        class_name: String,
        #[source]
        source: SapClientError,
    },
}

impl ClassRunError {
    #[must_use]
    pub const fn sap_error(&self) -> Option<&SapClientError> {
        match self {
            Self::Sap { source, .. } => Some(source),
            Self::InvalidName { .. } => None,
        }
    }
}

impl ReportableError for ClassRunError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidName { .. } => "class_run_name_invalid",
            Self::Sap { .. } => "class_run_failed",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(self.sap_error())
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::InvalidName { .. } => {
                "An ABAP class name is at most 30 characters of A-Z, 0-9, underscore or slash."
                    .to_owned()
            }
            Self::Sap { source, .. } => source.hint()?,
        })
    }

    /// A server error during a run is the class failing, and the dump says why.
    fn suggested_command(&self) -> Option<String> {
        match self {
            Self::Sap { source, .. } if is_server_error(source) => {
                Some(crate::suggested_command::dump_show_latest())
            }
            Self::Sap { source, .. } => source.suggested_command(),
            Self::InvalidName { .. } => None,
        }
    }
}

fn is_server_error(error: &SapClientError) -> bool {
    matches!(error, SapClientError::Http { status, .. } if status.is_server_error())
}

/// Runs an activated ABAP class through the ADT console endpoint.
///
/// The class must implement `if_oo_adt_classrun`; SAP answers `404` when it
/// does not. The endpoint reads no request body — `if_oo_adt_classrun~main`
/// takes no importing parameters — so everything the run needs has to be in
/// the class source.
///
/// # Errors
///
/// Returns [`ClassRunError`] when the name is not a legal ABAP class name, or
/// when SAP rejects or fails the run.
pub async fn run_class(
    sap: &mut SapClient,
    class_name: &str,
) -> Result<ClassRunResult, ClassRunError> {
    let name = normalized_class_name(class_name).ok_or_else(|| ClassRunError::InvalidName {
        name: class_name.to_owned(),
    })?;

    let mut headers = HeaderMap::new();
    headers.insert(ACCEPT, HeaderValue::from_static("text/plain"));

    let path = format!("{CLASSRUN_PATH}/{name}");
    let started = Instant::now();
    let output = sap
        .post_text(&path, &[], None, headers)
        .await
        .map_err(|source| ClassRunError::Sap {
            class_name: name.clone(),
            source,
        })?;

    Ok(ClassRunResult {
        class_name: name,
        output,
        elapsed: started.elapsed(),
    })
}

/// Upper-cases a class name, rejecting anything that is not a legal one.
///
/// The name goes into the request path, so a name that is not checked is a
/// path injection.
fn normalized_class_name(name: &str) -> Option<String> {
    let name = name.trim();
    if name.is_empty() || name.len() > MAX_CLASS_NAME {
        return None;
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '/')
    {
        return None;
    }
    Some(name.to_ascii_uppercase())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_that_fails_inside_sap_points_at_the_dump() {
        let error = ClassRunError::Sap {
            class_name: "ZCL_SAMPLE".to_owned(),
            source: SapClientError::Http {
                kind: crate::sap::client::SapHttpErrorKind::ServerError,
                status: reqwest::StatusCode::INTERNAL_SERVER_ERROR,
                url: "https://sap.example/x".to_owned(),
                message: "Internal Server Error".to_owned(),
            },
        };
        assert_eq!(
            error.suggested_command().as_deref(),
            Some("fractal dump show --latest")
        );
    }

    #[test]
    fn upper_cases_a_legal_name() {
        assert_eq!(
            normalized_class_name(" zcl_probe ").as_deref(),
            Some("ZCL_PROBE")
        );
    }

    #[test]
    fn accepts_a_namespaced_name() {
        assert_eq!(
            normalized_class_name("/issi/cl_probe").as_deref(),
            Some("/ISSI/CL_PROBE")
        );
    }

    #[test]
    fn rejects_an_empty_name() {
        assert_eq!(normalized_class_name("   "), None);
    }

    #[test]
    fn rejects_a_name_that_is_too_long() {
        assert_eq!(normalized_class_name(&"Z".repeat(31)), None);
    }

    #[test]
    fn rejects_path_traversal() {
        assert_eq!(normalized_class_name("../../discovery"), None);
        assert_eq!(normalized_class_name("ZCL_A?sap-client=000"), None);
    }
}

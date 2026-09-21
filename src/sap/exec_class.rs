use thiserror::Error;

use super::{
    adt_version::AdtVersion,
    class_run::{ClassRunError, run_class},
    client::{SapClient, SapClientError},
    editable_source::EditableAdtObjectType,
    object_creation::{AdtObjectCreationError, AdtObjectCreationRequest, create_adt_object},
    object_source::{ByteRangeOptions, ObjectSourceError, get_source},
    source_activation::{
        AdtSourceActivationError, AdtSourceActivationRequest, activate_adt_source,
    },
    source_replace::{
        AdtSourceReplacementError, AdtSourceReplacementRequest, replace_adt_source_atomically,
    },
    staged_work::StagedEditPolicy,
};
use crate::config::{EditPolicy, TEMPORARY_PACKAGE};
use crate::reportable_error::{ReportableError, sap_http_status};

const NAME_PREFIX: &str = "ZCL_FRACTAL_EXEC_";
const MAX_CLASS_NAME: usize = 30;
const DESCRIPTION: &str = "Fractal generated executor - rewritten per run";
/// The reported code for an activation with no pending change to activate.
const NOTHING_TO_ACTIVATE: &str = "edit_activation_no_inactive_source";
/// The reported code for a create whose object is already there.
const ALREADY_EXISTS: &str = "edit_create_object_exists";

#[derive(Debug, Error)]
pub enum ExecClassError {
    #[error("could not create the executor class: {0}")]
    Create(#[from] AdtObjectCreationError),
    #[error("could not write the executor class: {0}")]
    Write(#[from] AdtSourceReplacementError),
    #[error("the generated ABAP did not activate: {0}")]
    Activate(#[from] AdtSourceActivationError),
    #[error("{0}")]
    Run(#[from] ClassRunError),
    #[error("could not open a new ADT session: {0}")]
    Session(#[source] SapClientError),
    #[error("could not read back the executor's active source: {0}")]
    ReadBack(Box<ObjectSourceError>),
}

impl ReportableError for ExecClassError {
    fn code(&self) -> &'static str {
        match self {
            Self::Create(error) => error.code(),
            Self::Write(error) => error.code(),
            Self::Activate(error) => error.code(),
            Self::Run(error) => error.code(),
            Self::Session(_) => "exec_class_session_failed",
            Self::ReadBack(error) => error.code(),
        }
    }

    fn status(&self) -> Option<u16> {
        match self {
            Self::Create(error) => error.status(),
            Self::Write(error) => error.status(),
            Self::Activate(error) => error.status(),
            Self::Run(error) => error.status(),
            Self::Session(error) => sap_http_status(Some(error)),
            Self::ReadBack(error) => error.status(),
        }
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Create(error) => error.hint(),
            Self::Write(error) => error.hint(),
            Self::Activate(error) => error.hint(),
            Self::Run(error) => error.hint(),
            Self::Session(error) => error.hint(),
            Self::ReadBack(error) => error.hint(),
        }
    }
}

/// The executor class this user owns.
///
/// Repository names are global — the package does not scope them — so a shared
/// name would be one mutable object for every developer on the system, and the
/// failure is that one user runs another's generated code.
#[must_use]
pub fn executor_class_name(username: &str) -> String {
    let mut name = String::from(NAME_PREFIX);
    for character in username.chars() {
        if name.len() == MAX_CLASS_NAME {
            break;
        }
        if character.is_ascii_alphanumeric() {
            name.push(character.to_ascii_uppercase());
        } else {
            name.push('_');
        }
    }
    name
}

/// Writes the source into this user's executor class, activates it, and runs it.
///
/// The class is left in place afterwards. Creating and deleting it each time
/// costs three times as much, and what is left behind is spent: every generated
/// statement is a compare-and-swap, so running it again does nothing.
///
/// # Errors
///
/// Returns [`ExecClassError`] when the class cannot be written, does not
/// activate — a syntax error in the generated ABAP shows up here, before
/// anything runs — or fails to run.
pub async fn run_generated_source(
    sap: &mut SapClient,
    username: &str,
    source: String,
) -> Result<String, ExecClassError> {
    let name = executor_class_name(username);
    // The executor lives in $TMP: local, non-transportable, and outside any
    // package allowlist the profile sets for real edits.
    let policy = EditPolicy {
        customer_namespaces: vec!["Z*".to_owned()],
        edit_packages: None,
        allow_temporary_package: true,
    };

    let uri = format!(
        "{}/{}",
        EditableAdtObjectType::Class.collection_path(),
        name.to_ascii_lowercase()
    );
    let write = AdtSourceReplacementRequest {
        object_type: EditableAdtObjectType::Class,
        name: name.clone(),
        replacement_source: source.clone(),
        expected_sha256: None,
        transport: None,
        // Fractal owns this class and rewrites it every run; there is no
        // colleague's work to protect here.
        staged_edits: StagedEditPolicy::Overwrite,
    };

    // Create first, and treat "already exists" as the steady state. The
    // opposite order looks cheaper but is not usable: writing to a class that
    // does not exist answers 404, and inside the same ADT session that 404
    // makes SAP reject the read-back of the class the create then makes.
    if let Err(error) = create_adt_object(
        sap,
        &policy,
        &AdtObjectCreationRequest {
            object_type: EditableAdtObjectType::Class,
            name: name.clone(),
            package: TEMPORARY_PACKAGE.to_owned(),
            description: DESCRIPTION.to_owned(),
            transport: None,
        },
    )
    .await
        && error.code() != ALREADY_EXISTS
    {
        return Err(error.into());
    }

    replace_adt_source_atomically(sap, &policy, &write).await?;

    if let Err(error) = activate_adt_source(
        sap,
        &policy,
        &AdtSourceActivationRequest {
            object_type: EditableAdtObjectType::Class,
            name: name.clone(),
            transport: None,
        },
        None,
    )
    .await
    {
        // Writing source identical to what is already active creates no
        // inactive version, so there is nothing to activate. That is the normal
        // case for a repeated command, not a failure — the executor already
        // holds the statement we want to run.
        if error.code() != NOTHING_TO_ACTIVATE || !active_source_matches(sap, &uri, &source).await?
        {
            return Err(error.into());
        }
    }

    Ok(run_class(sap, &name).await?.output)
}

/// Runs the executor again in a new session, without rewriting or reactivating.
///
/// A class created in a session is not runnable through the console endpoint
/// **in that same session** — SAP answers 200 with "does not implement" — while
/// a new session runs it at once. Measured: retrying in the same session never
/// succeeds, and a fresh session succeeds immediately, so this is session
/// state rather than something that settles with time.
///
/// Only the run is repeated. Nothing is written or activated twice.
///
/// # Errors
///
/// Returns [`ExecClassError`] when the session cannot be restarted or the run
/// fails.
pub async fn rerun_executor(sap: &mut SapClient, username: &str) -> Result<String, ExecClassError> {
    sap.restart_session()
        .await
        .map_err(ExecClassError::Session)?;
    Ok(run_class(sap, &executor_class_name(username)).await?.output)
}

/// Whether the executor's active source is already what was written.
///
/// # Errors
///
/// Returns [`ExecClassError`] when the source cannot be read.
async fn active_source_matches(
    sap: &SapClient,
    uri: &str,
    expected: &str,
) -> Result<bool, ExecClassError> {
    let active = get_source(sap, uri, AdtVersion::Active, ByteRangeOptions::default())
        .await
        .map_err(|source| ExecClassError::ReadBack(Box::new(source)))?;
    Ok(normalized(&active.content) == normalized(expected))
}

/// Source as SAP compares it, not as bytes.
///
/// What comes back is not what was sent: SAP stores CRLF line endings and adds
/// a trailing newline. A byte comparison therefore never matches, which is what
/// made the first version of this check useless.
fn normalized(source: &str) -> String {
    source.replace("\r\n", "\n").trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_source_the_way_sap_stores_it() {
        // SAP returns CRLF and a trailing newline, so the source that comes
        // back is never byte-identical to the source that was sent.
        let sent = "CLASS zcl_x DEFINITION.\nENDCLASS.";
        let stored = "CLASS zcl_x DEFINITION.\r\nENDCLASS.\r\n";
        assert_eq!(normalized(sent), normalized(stored));
        assert_ne!(normalized(sent), normalized("CLASS zcl_y DEFINITION."));
    }

    #[test]
    fn names_the_class_after_the_user() {
        assert_eq!(executor_class_name("trussell"), "ZCL_FRACTAL_EXEC_TRUSSELL");
    }

    #[test]
    fn keeps_the_name_inside_the_abap_limit() {
        let name = executor_class_name("A_VERY_LONG_USER_NAME_INDEED");
        assert_eq!(name.len(), MAX_CLASS_NAME);
        assert!(name.starts_with(NAME_PREFIX));
    }

    #[test]
    fn replaces_characters_a_name_cannot_hold() {
        assert_eq!(executor_class_name("a.b-c"), "ZCL_FRACTAL_EXEC_A_B_C");
    }
}

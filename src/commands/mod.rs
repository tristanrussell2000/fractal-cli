pub mod auth;
pub mod binding;
pub mod edit_activate;
pub mod edit_check;
pub mod edit_create;
pub mod edit_discard;
pub mod edit_lock_warning;
pub mod edit_object_identity;
pub mod edit_patch;
pub mod edit_read;
pub mod edit_set;
pub mod edit_set_xml;
pub mod guard;
pub mod guard_hook;
pub mod internal;
pub mod journal;
pub mod object;
pub mod object_delete;
pub mod object_show;
pub mod package;
pub mod package_lifecycle;
pub mod query;
pub mod system;
pub mod table;
pub mod table_write;
pub mod transport;
pub mod undo;
pub mod unit_test;

mod tabular;

use crate::reported::Reported;
use fractal::{config, credentials, sap::client::SapClient, sap::staged_work::StagedEditPolicy};

/// What a write should do about an edit somebody else has staged.
pub fn staged_edit_policy(force: bool, username: &str) -> StagedEditPolicy {
    if force {
        StagedEditPolicy::Overwrite
    } else {
        StagedEditPolicy::Protect(username.to_owned())
    }
}

/// Which layer the caller is actually looking at.
///
/// Asking for a layer is not getting it: SAP serves the other one rather than
/// refusing, and an object that has never been activated declares itself `new`.
pub fn render_version(requested: &'static str, declared: Option<&str>) -> String {
    match declared {
        None => "unknown (the document does not say)".to_owned(),
        Some("new") => "new (never activated)".to_owned(),
        Some(version) if version == requested => version.to_owned(),
        Some(version) => {
            format!("{version} (asked for {requested}; this object has none)")
        }
    }
}

/// Resolves the selected profile, loads its keychain password, and opens a
/// `SapClient`. Shared by every command that needs to reach SAP; the profile
/// is returned as an owned value (rather than borrowed from a local config
/// load) so callers that need it past the connection setup — e.g. to read
/// `customer_namespaces` or pass it into a request — don't fight lifetimes.
pub async fn connect(
    explicit_profile: Option<&str>,
) -> Result<(String, config::Profile, SapClient), Reported> {
    let loaded = config::load()?;
    let (profile_name, profile) = config::resolve_profile(&loaded.config, explicit_profile)?;
    // Not the keychain directly: an environment variable, a password command
    // or an opted-in plain file all supply the password on machines that have
    // no credential store.
    let (password, source) = credentials::resolve_password(profile_name, profile)?;
    let client = SapClient::new(profile, password)?.with_password_source(source);
    Ok((profile_name.to_owned(), profile.clone(), client))
}

/// Splits `TYPE:NAME` at the first colon. A namespaced name keeps its slashes.
pub(crate) fn parse_object_spec(spec: &str) -> Result<(String, String), ObjectSpecError> {
    match spec.split_once(':') {
        Some((object_type, name)) if !object_type.trim().is_empty() && !name.trim().is_empty() => {
            Ok((object_type.trim().to_owned(), name.trim().to_owned()))
        }
        _ => Err(ObjectSpecError(spec.to_owned())),
    }
}

#[derive(Debug, thiserror::Error)]
#[error("'{0}' is not TYPE:NAME")]
pub struct ObjectSpecError(pub(crate) String);

impl fractal::reportable_error::ReportableError for ObjectSpecError {
    fn code(&self) -> &'static str {
        "invalid_object_spec"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Write each --object as the type, a colon, and the name, with no spaces: --object DDLS:ZRAP_R_TRAVEL --object DDLS:ZRAP_R_BOOKING."
                .to_owned(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::parse_object_spec;

    #[test]
    fn a_namespaced_name_keeps_its_slashes() {
        assert_eq!(
            parse_object_spec("DDLS:/ACME/CHILD").unwrap(),
            ("DDLS".to_owned(), "/ACME/CHILD".to_owned())
        );
    }

    #[test]
    fn a_malformed_object_says_how_to_write_it() {
        for bad in ["ZSAMPLE_PARENT", "DDLS:", ":ZSAMPLE", "DDLS ZSAMPLE"] {
            let error = parse_object_spec(bad).unwrap_err();
            assert_eq!(
                fractal::reportable_error::ReportableError::code(&error),
                "invalid_object_spec",
                "{bad}"
            );
            let hint = fractal::reportable_error::ReportableError::hint(&error).unwrap();
            assert!(hint.contains("--object DDLS:ZRAP_R_TRAVEL"), "{hint}");
        }
    }
}

use crate::reportable_error::ReportableError;
use keyring_core::Entry;
use thiserror::Error;

const KEYCHAIN_SERVICE: &str = "fractal";

#[derive(Debug, Error)]
pub enum CredentialError {
    #[error("invalid profile name for credential storage: {0}")]
    InvalidProfileName(String),
    #[error("could not access the OS credential store: {0}")]
    Store(#[source] keyring::Error),
    #[error("no credential is stored for profile '{0}'")]
    Missing(String),
    #[error("the OS credential store could not be initialized: {0}")]
    StoreUnavailable(String),
}

impl ReportableError for CredentialError {
    fn code(&self) -> &'static str {
        match self {
            Self::InvalidProfileName(_) => "invalid_profile_name",
            Self::Store(_) => "credential_store_error",
            Self::Missing(_) => "credential_missing",
            Self::StoreUnavailable(_) => "credential_store_unavailable",
        }
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Missing(profile) => Some(format!(
                "Run `fractal auth login {profile}` to store the missing credential."
            )),
            _ => None,
        }
    }
}

/// Stores or replaces a profile password in the operating-system credential store.
///
/// # Errors
///
/// Returns an error for an invalid profile name or when the OS credential store
/// cannot save the password.
pub fn save_password(profile_name: &str, password: &str) -> Result<(), CredentialError> {
    let entry = entry(profile_name)?;
    entry.set_password(password).map_err(CredentialError::Store)
}

/// Retrieves a profile password from the operating-system credential store.
///
/// # Errors
///
/// Returns [`CredentialError::Missing`] when no password exists for the profile,
/// or [`CredentialError::Store`] when the credential store cannot be accessed.
pub fn get_password(profile_name: &str) -> Result<String, CredentialError> {
    let entry = entry(profile_name)?;
    match entry.get_password() {
        Ok(password) => Ok(password),
        Err(keyring::Error::NoEntry) => Err(CredentialError::Missing(profile_name.to_owned())),
        Err(error) => Err(CredentialError::Store(error)),
    }
}

/// Deletes a profile password from the operating-system credential store.
///
/// Deleting an already-missing credential is treated as success.
///
/// # Errors
///
/// Returns an error for an invalid profile name or when the credential store
/// cannot perform the deletion.
pub fn delete_password(profile_name: &str) -> Result<(), CredentialError> {
    let entry = entry(profile_name)?;
    match entry.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(CredentialError::Store(error)),
    }
}

fn entry(profile_name: &str) -> Result<Entry, CredentialError> {
    if profile_name.trim().is_empty() {
        return Err(CredentialError::InvalidProfileName(profile_name.to_owned()));
    }
    install_platform_store_if_needed()?;

    Entry::new(KEYCHAIN_SERVICE, profile_name).map_err(CredentialError::Store)
}

/// Installs the OS credential store, unless one is already installed.
///
/// `keyring::Entry::new` installs the platform store itself, on first use, by
/// calling `keyring_core::set_default_store` — which *overwrites* any store
/// already registered. That silently defeated the mock store in this module's
/// tests: they installed a mock, the first `Entry::new` replaced it with the
/// real macOS Keychain, and every assertion then ran against the developer's
/// actual keychain. It was not a correctness problem, but each real keychain
/// call took several seconds and the four tests here cost roughly 40 seconds of
/// every `cargo test` run.
///
/// Checking first is not enough on its own, because `keyring::Entry::new`
/// forces that initialization whether or not a store is already registered.
/// So entries are created through `keyring_core::Entry` and the `keyring`
/// crate is used for one thing only: installing the platform store, here,
/// when nothing else has installed one. Whoever installs first wins, so tests
/// stay hermetic and fast while normal use is unchanged.
fn install_platform_store_if_needed() -> Result<(), CredentialError> {
    if keyring_core::get_default_store().is_some() {
        return Ok(());
    }
    // Triggers the platform store initialization and reports how it went.
    match keyring::Entry::store_status() {
        Ok(()) => Ok(()),
        Err(error) => Err(CredentialError::StoreUnavailable(error.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Once;

    use keyring_core::mock::Store;
    use keyring_core::set_default_store;

    use super::{CredentialError, delete_password, get_password, save_password};

    static MOCK_STORE_SETUP: Once = Once::new();

    // Tests use unique profile names, do not replace the global mock store, and
    // do not depend on data created by another test, so they can run in parallel.
    fn use_mock_store() {
        MOCK_STORE_SETUP.call_once(|| {
            set_default_store(Store::new().expect("mock credential store creates"));
        });
    }

    #[test]
    fn saving_overwrites_an_existing_profile_password() {
        use_mock_store();
        let profile = "credentials-overwrite";

        save_password(profile, "old-password").unwrap();
        save_password(profile, "new-password").unwrap();

        assert_eq!(get_password(profile).unwrap(), "new-password");
        delete_password(profile).unwrap();
    }

    #[test]
    fn missing_profile_is_an_explicit_error() {
        use_mock_store();
        let profile = "credentials-missing";

        assert!(matches!(
            get_password(profile),
            Err(CredentialError::Missing(name)) if name == profile
        ));
    }

    #[test]
    fn deleting_a_profile_removes_its_password() {
        use_mock_store();
        let profile = "credentials-delete";

        save_password(profile, "password").unwrap();
        delete_password(profile).unwrap();

        assert!(matches!(
            get_password(profile),
            Err(CredentialError::Missing(name)) if name == profile
        ));
    }

    /// A credential operation must not replace a store someone else installed.
    ///
    /// This is the difference between the tests here being hermetic and them
    /// silently running against the developer's real keychain. Failure is not
    /// an assertion error in the other tests — they still pass — it is those
    /// tests taking about ten seconds per operation and reading and writing
    /// real credentials. So it is asserted directly.
    #[test]
    fn a_credential_operation_keeps_the_store_that_is_already_installed() {
        use_mock_store();
        let installed = keyring_core::get_default_store().expect("mock store is installed");
        let vendor_before = installed.vendor();

        save_password("credentials-store-check", "password").unwrap();
        delete_password("credentials-store-check").unwrap();

        let still_installed = keyring_core::get_default_store().expect("a store is installed");
        assert_eq!(
            still_installed.vendor(),
            vendor_before,
            "a credential operation replaced the installed store with the platform one"
        );
    }

    #[test]
    fn empty_profile_names_are_rejected_before_keyring_access() {
        assert!(matches!(
            save_password("  ", "password"),
            Err(CredentialError::InvalidProfileName(name)) if name == "  "
        ));
    }
}

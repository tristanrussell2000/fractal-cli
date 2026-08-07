use keyring::Entry;
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
}

pub fn save_password(profile_name: &str, password: &str) -> Result<(), CredentialError> {
    let entry = entry(profile_name)?;
    entry.set_password(password).map_err(CredentialError::Store)
}

pub fn get_password(profile_name: &str) -> Result<String, CredentialError> {
    let entry = entry(profile_name)?;
    match entry.get_password() {
        Ok(password) => Ok(password),
        Err(keyring::Error::NoEntry) => Err(CredentialError::Missing(profile_name.to_owned())),
        Err(error) => Err(CredentialError::Store(error)),
    }
}

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

    Entry::new(KEYCHAIN_SERVICE, profile_name).map_err(CredentialError::Store)
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

    #[test]
    fn empty_profile_names_are_rejected_before_keyring_access() {
        assert!(matches!(
            save_password("  ", "password"),
            Err(CredentialError::InvalidProfileName(name)) if name == "  "
        ));
    }
}

pub mod auth;
pub mod ddic;
pub mod edit_activate;
pub mod edit_check;
pub mod edit_create;
pub mod edit_delete;
pub mod edit_discard;
pub mod edit_lock_warning;
pub mod edit_object_identity;
pub mod edit_patch;
pub mod edit_read;
pub mod edit_set;
pub mod edit_set_xml;
pub mod object;
pub mod package;
pub mod query;
pub mod system;
pub mod table;
pub mod transport;

mod tabular;

use crate::reported::Reported;
use fractal::{config, credentials, sap::client::SapClient};

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
    let (password, _source) = credentials::resolve_password(profile_name, profile)?;
    let client = SapClient::new(profile, password)?;
    Ok((profile_name.to_owned(), profile.clone(), client))
}

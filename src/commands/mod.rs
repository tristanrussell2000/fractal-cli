pub mod auth;
pub mod edit_activate;
pub mod edit_check;
pub mod edit_patch;
pub mod edit_read;
pub mod object;
pub mod package;
pub mod query;
pub mod system;
pub mod table;

mod tabular;

use crate::command_error::CommandError;
use fractal::{config, credentials, sap::client::SapClient};

/// Resolves the selected profile, loads its keychain password, and opens a
/// `SapClient`. Shared by every command that needs to reach SAP; the profile
/// is returned as an owned value (rather than borrowed from a local config
/// load) so callers that need it past the connection setup — e.g. to read
/// `customer_namespaces` or pass it into a request — don't fight lifetimes.
pub async fn connect(
    explicit_profile: Option<&str>,
) -> Result<(String, config::Profile, SapClient), CommandError> {
    let loaded = config::load()?;
    let (profile_name, profile) = config::resolve_profile(&loaded.config, explicit_profile)?;
    let password = credentials::get_password(profile_name)?;
    let client = SapClient::new(profile, password)?;
    Ok((profile_name.to_owned(), profile.clone(), client))
}

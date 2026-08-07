use std::collections::BTreeMap;

use fractal::config::{Config, Profile, resolve_profile_with_environment, update_profile};

fn profile() -> Profile {
    Profile {
        base_url: "https://sap.example:8001".to_owned(),
        client: "900".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
    }
}

fn config() -> Config {
    Config {
        default_profile: Some("default".to_owned()),
        profiles: BTreeMap::from([
            ("default".to_owned(), profile()),
            ("environment".to_owned(), profile()),
            ("explicit".to_owned(), profile()),
        ]),
    }
}

#[test]
fn explicit_profile_wins_over_environment_and_default() {
    let config = config();
    let (name, _) =
        resolve_profile_with_environment(&config, Some("explicit"), Some("environment")).unwrap();
    assert_eq!(name, "explicit");
}

#[test]
fn environment_profile_wins_over_default() {
    let config = config();
    let (name, _) = resolve_profile_with_environment(&config, None, Some("environment")).unwrap();
    assert_eq!(name, "environment");
}

#[test]
fn default_profile_is_used_as_last_resort() {
    let config = config();
    let (name, _) = resolve_profile_with_environment(&config, None, None).unwrap();
    assert_eq!(name, "default");
}

#[test]
fn first_profile_becomes_default() {
    let mut config = Config::default();

    let became_default = update_profile(&mut config, "first".to_owned(), profile(), false);

    assert!(became_default);
    assert_eq!(config.default_profile.as_deref(), Some("first"));
}

#[test]
fn later_profile_preserves_existing_default() {
    let mut config = config();

    let became_default = update_profile(&mut config, "new".to_owned(), profile(), false);

    assert!(!became_default);
    assert_eq!(config.default_profile.as_deref(), Some("default"));
}

#[test]
fn default_flag_changes_existing_default() {
    let mut config = config();

    let became_default = update_profile(&mut config, "new".to_owned(), profile(), true);

    assert!(became_default);
    assert_eq!(config.default_profile.as_deref(), Some("new"));
}

#[test]
fn updating_existing_profile_does_not_make_it_default_without_flag() {
    let mut config = config();

    let became_default = update_profile(&mut config, "explicit".to_owned(), profile(), false);

    assert!(!became_default);
    assert_eq!(config.default_profile.as_deref(), Some("default"));
}

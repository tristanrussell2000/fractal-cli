use std::collections::BTreeMap;

use fractal::config::{Config, Profile, resolve_profile_with_environment};

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

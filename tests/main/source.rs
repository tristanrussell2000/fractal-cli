use fractal::{
    config::Profile,
    sap::{
        adt_object_uri::AdtObjectUriError,
        adt_version::AdtVersion,
        class_include::ClassInclude,
        client::SapClient,
        object_source::{
            ByteRangeOptions, ObjectSourceError, SourceTarget, get_source, read_source_layer,
            resolve_source_target,
        },
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

fn target(uri: &str) -> SourceTarget {
    resolve_source_target(uri, None).unwrap()
}

fn profile(base_url: String) -> Profile {
    Profile {
        base_url,
        client: "903".to_owned(),
        username: "developer".to_owned(),
        insecure_tls: false,
        password_command: None,
        edit_packages: None,
        allow_temporary_package: true,
        customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
    }
}

#[tokio::test]
async fn fetches_complete_source_and_pages_utf8_safely() {
    let server = MockServer::start().await;
    let source = "abcédef";
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_test/source/main"))
        .respond_with(ResponseTemplate::new(200).set_body_string(source))
        .expect(2)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let first = get_source(
        &client,
        &target("/sap/bc/adt/oo/classes/zcl_test"),
        AdtVersion::Active,
        ByteRangeOptions {
            offset: 0,
            limit: Some(4),
        },
    )
    .await
    .unwrap();

    assert_eq!(first.content, "abc");
    assert_eq!(first.start_byte, 0);
    assert_eq!(first.end_byte, 3);
    assert_eq!(first.total_bytes, source.len());
    assert!(first.truncated);
    assert_eq!(first.next_offset, Some(3));

    let second = get_source(
        &client,
        &target("/sap/bc/adt/oo/classes/zcl_test"),
        AdtVersion::Active,
        ByteRangeOptions {
            offset: first.next_offset.unwrap(),
            limit: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.content, "édef");
    assert!(!second.truncated);
    server.verify().await;
}

#[tokio::test]
async fn rejects_invalid_source_uris_before_http() {
    let error = resolve_source_target("not-an-adt-uri", None).unwrap_err();
    assert!(matches!(
        error,
        ObjectSourceError::Uri(AdtObjectUriError::NotAnAdtUri(_))
    ));
}

#[tokio::test]
async fn rejects_doubled_source_suffix_and_known_no_source_kinds() {
    let doubled =
        resolve_source_target("/sap/bc/adt/oo/classes/zcl_test/source/main", None).unwrap_err();
    assert!(matches!(
        doubled,
        ObjectSourceError::Uri(AdtObjectUriError::DoubledSourceSuffix(_))
    ));

    let domain = resolve_source_target("/sap/bc/adt/ddic/domains/zdomain", None).unwrap_err();
    assert!(matches!(domain, ObjectSourceError::NoSourceForKind { .. }));
}

#[tokio::test]
async fn a_source_read_names_the_version_it_wants() {
    let server = MockServer::start().await;
    // Each layer answers only when named. A read that omits the selector would
    // be served the inactive source whenever one exists, established live, and
    // ABAP text carries no marker that would let the caller notice.
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample/source/main"))
        .and(query_param("version", "active"))
        .respond_with(ResponseTemplate::new(200).set_body_string("WRITE / 'ACTIVE'."))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample/source/main"))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string("WRITE / 'PENDING'."))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let active = get_source(
        &client,
        &target("/sap/bc/adt/programs/programs/zsample"),
        AdtVersion::Active,
        ByteRangeOptions::default(),
    )
    .await
    .unwrap();
    let inactive = get_source(
        &client,
        &target("/sap/bc/adt/programs/programs/zsample"),
        AdtVersion::Inactive,
        ByteRangeOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(active.content, "WRITE / 'ACTIVE'.");
    assert_eq!(inactive.content, "WRITE / 'PENDING'.");
    server.verify().await;
}

/// The document for one layer of a program, as SAP writes it.
fn program_document(version: &str, changed_by: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="utf-8"?>
<program:abapProgram adtcore:name="ZSAMPLE" adtcore:type="PROG/P" adtcore:version="{version}"
    adtcore:changedBy="{changed_by}"
    xmlns:program="http://www.sap.com/adt/programs/programs" xmlns:adtcore="http://www.sap.com/adt/core"/>"#
    )
}

/// The bug this exists for: ABAP source declares no version and the response
/// headers are identical for both layers, so a read that silently fell back to
/// active was indistinguishable from one that got what it asked for.
#[tokio::test]
async fn a_source_read_reports_the_layer_that_actually_arrived() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample"))
        .and(query_param("version", "inactive"))
        // Asked for inactive; SAP served active, because there is no inactive.
        .respond_with(
            ResponseTemplate::new(200).set_body_string(program_document("active", "DEVELOPER")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let layer = read_source_layer(
        &client,
        &target("/sap/bc/adt/programs/programs/zsample"),
        AdtVersion::Inactive,
    )
    .await
    .unwrap();

    assert_eq!(layer.version.as_deref(), Some("active"));
    assert_eq!(layer.changed_by.as_deref(), Some("DEVELOPER"));
    server.verify().await;
}

/// A staged edit reports as inactive and names whoever staged it, which is what
/// makes a write compatible with it rather than a surprise.
#[tokio::test]
async fn a_staged_layer_names_who_staged_it() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample"))
        .and(query_param("version", "inactive"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(program_document("inactive", "COLLEAGUE")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let layer = read_source_layer(
        &client,
        &target("/sap/bc/adt/programs/programs/zsample"),
        AdtVersion::Inactive,
    )
    .await
    .unwrap();

    assert_eq!(layer.version.as_deref(), Some("inactive"));
    assert_eq!(layer.changed_by.as_deref(), Some("COLLEAGUE"));
    server.verify().await;
}

/// The layer question is asked of the same layer the source was read at, or the
/// answer would describe a different read.
#[tokio::test]
async fn the_layer_question_names_the_layer_the_source_was_read_at() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample"))
        .and(query_param("version", "active"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(program_document("active", "DEVELOPER")),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample"))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(500))
        .expect(0)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let layer = read_source_layer(
        &client,
        &target("/sap/bc/adt/programs/programs/zsample"),
        AdtVersion::Active,
    )
    .await
    .unwrap();

    assert_eq!(layer.version.as_deref(), Some("active"));
    server.verify().await;
}

#[tokio::test]
async fn an_object_that_has_never_been_activated_declares_itself_new() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/programs/programs/zsample"))
        .respond_with(
            ResponseTemplate::new(200).set_body_string(program_document("new", "DEVELOPER")),
        )
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let layer = read_source_layer(
        &client,
        &target("/sap/bc/adt/programs/programs/zsample"),
        AdtVersion::Inactive,
    )
    .await
    .unwrap();

    assert_eq!(layer.version.as_deref(), Some("new"));
    server.verify().await;
}

#[test]
fn accepts_a_class_include_uri_or_the_class_with_an_include() {
    let from_uri = resolve_source_target(
        "/sap/bc/adt/oo/classes/zcl_sample/includes/definitions",
        None,
    )
    .unwrap();
    let from_flag = resolve_source_target(
        "/sap/bc/adt/oo/classes/zcl_sample",
        Some(ClassInclude::Definitions),
    )
    .unwrap();

    assert_eq!(from_uri, from_flag);
    assert_eq!(from_uri.object_uri, "/sap/bc/adt/oo/classes/zcl_sample");
    assert_eq!(
        from_uri.source_uri,
        "/sap/bc/adt/oo/classes/zcl_sample/includes/definitions"
    );
}

#[test]
fn a_class_defaults_to_main_and_anything_else_has_no_include() {
    let class = resolve_source_target("/sap/bc/adt/oo/classes/zcl_sample", None).unwrap();
    let program = resolve_source_target("/sap/bc/adt/programs/programs/zsample", None).unwrap();

    assert_eq!(class.include, Some(ClassInclude::Main));
    assert_eq!(program.include, None);
}

#[test]
fn refuses_includes_that_cannot_be_read() {
    assert!(matches!(
        resolve_source_target(
            "/sap/bc/adt/programs/programs/zsample",
            Some(ClassInclude::Macros)
        ),
        Err(ObjectSourceError::IncludeOnNonClass { .. })
    ));
    assert!(matches!(
        resolve_source_target(
            "/sap/bc/adt/oo/classes/zcl_sample/includes/macros",
            Some(ClassInclude::Definitions)
        ),
        Err(ObjectSourceError::ConflictingInclude { .. })
    ));
    assert!(matches!(
        resolve_source_target("/sap/bc/adt/oo/classes/zcl_sample/includes/locals", None),
        Err(ObjectSourceError::UnknownInclude { .. })
    ));
}

/// The class-level marker goes inactive when any include is staged, so each
/// stream's layer is read off its own entry in the class document.
#[tokio::test]
async fn an_include_reports_its_own_layer_not_the_class_s() {
    let server = MockServer::start().await;
    let document = r#"<?xml version="1.0" encoding="utf-8"?><class:abapClass xmlns:class="http://www.sap.com/adt/oo/classes" xmlns:adtcore="http://www.sap.com/adt/core" xmlns:abapsource="http://www.sap.com/adt/abapsource" adtcore:name="ZCL_SAMPLE" adtcore:version="inactive" adtcore:changedBy="COLLEAGUE"><class:include class:includeType="definitions" abapsource:sourceUri="includes/definitions" adtcore:version="active" adtcore:changedBy="DEVELOPER"/><class:include class:includeType="implementations" abapsource:sourceUri="includes/implementations" adtcore:version="inactive" adtcore:changedBy="COLLEAGUE"/><class:include class:includeType="main" abapsource:sourceUri="source/main" adtcore:version="active" adtcore:changedBy="DEVELOPER"/></class:abapClass>"#;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_sample"))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string(document))
        .expect(2)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let main = read_source_layer(
        &client,
        &target("/sap/bc/adt/oo/classes/zcl_sample"),
        AdtVersion::Inactive,
    )
    .await
    .unwrap();
    let staged = read_source_layer(
        &client,
        &target("/sap/bc/adt/oo/classes/zcl_sample/includes/implementations"),
        AdtVersion::Inactive,
    )
    .await
    .unwrap();

    assert_eq!(main.version.as_deref(), Some("active"));
    assert_eq!(main.changed_by.as_deref(), Some("DEVELOPER"));
    assert_eq!(staged.version.as_deref(), Some("inactive"));
    assert_eq!(staged.changed_by.as_deref(), Some("COLLEAGUE"));
    server.verify().await;
}

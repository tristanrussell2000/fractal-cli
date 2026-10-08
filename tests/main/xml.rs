use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{
        adt_version::AdtVersion,
        client::SapClient,
        object_source::{ByteRangeOptions, get_xml},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path, query_param},
};

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
async fn fetches_raw_object_xml_without_adding_source_suffix() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0"?><class:abapClass xmlns:class="urn:test"><name>ZCL_TEST</name></class:abapClass>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let xml = get_xml(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test",
        AdtVersion::Active,
        ByteRangeOptions::default(),
    )
    .await
    .unwrap();

    assert!(xml.page.content.contains("ZCL_TEST"));
    server.verify().await;
}

#[tokio::test]
async fn pages_multibyte_xml_safely() {
    let server = MockServer::start().await;
    let xml = "<root>abcédef</root>";
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .expect(2)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let first = get_xml(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test",
        AdtVersion::Active,
        ByteRangeOptions {
            offset: 0,
            limit: Some(10),
        },
    )
    .await
    .unwrap();

    assert_eq!(first.page.content, "<root>abc");
    assert!(first.page.truncated);
    assert_eq!(first.page.next_offset, Some(9));

    let second = get_xml(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test",
        AdtVersion::Active,
        ByteRangeOptions {
            offset: first.page.next_offset.unwrap(),
            limit: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.page.content, "édef</root>");
    assert!(!second.page.truncated);
    server.verify().await;
}

#[tokio::test]
async fn rejects_non_adt_xml_uri_before_http() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let error = get_xml(
        &mut client,
        "not-an-adt-uri",
        AdtVersion::Active,
        ByteRangeOptions::default(),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "invalid_adt_uri");
}

#[tokio::test]
async fn names_the_requested_version_and_reports_the_one_that_arrived() {
    let server = MockServer::start().await;
    // Answers only a request that names the layer, so a plain GET matches
    // nothing. The document declares a different layer than was asked for,
    // which is what SAP does when the requested one does not exist.
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample"))
        .and(query_param("version", "inactive"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0"?><blue:wbobj xmlns:blue="urn:test" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE" adtcore:version="active"/>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_xml(
        &mut client,
        "/sap/bc/adt/ddic/dataelements/zsample",
        AdtVersion::Inactive,
        ByteRangeOptions::default(),
    )
    .await
    .unwrap();

    assert_eq!(result.declared_version.as_deref(), Some("active"));
    server.verify().await;
}

#[tokio::test]
async fn the_version_is_read_from_the_whole_document_not_the_returned_page() {
    let server = MockServer::start().await;
    let xml = r#"<?xml version="1.0"?><blue:wbobj xmlns:blue="urn:test" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE" adtcore:version="inactive"/>"#;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample"))
        .respond_with(ResponseTemplate::new(200).set_body_string(xml))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    // A page far too short to contain the attribute. Asking for the first 20
    // bytes must not cost the caller the answer to "which version is this".
    let result = get_xml(
        &mut client,
        "/sap/bc/adt/ddic/dataelements/zsample",
        AdtVersion::Inactive,
        ByteRangeOptions {
            offset: 0,
            limit: Some(20),
        },
    )
    .await
    .unwrap();

    assert!(result.page.truncated);
    assert_eq!(result.declared_version.as_deref(), Some("inactive"));
}

#[tokio::test]
async fn a_response_that_is_not_xml_still_returns_what_sap_sent() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_test"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<not-closed"))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_xml(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test",
        AdtVersion::Active,
        ByteRangeOptions::default(),
    )
    .await
    .unwrap();

    // This command reports what ADT served. An unparseable document has no
    // version to name, which is not a failure of the read.
    assert_eq!(result.page.content, "<not-closed");
    assert_eq!(result.declared_version, None);
}

/// One document with the `atom:link` etags ADT rewrites whenever pending work
/// is staged or discarded.
fn document_with_links(etag: &str) -> String {
    format!(
        r#"<?xml version="1.0"?><blue:wbobj xmlns:blue="urn:test" xmlns:adtcore="http://www.sap.com/adt/core" xmlns:atom="http://www.w3.org/2005/Atom" adtcore:name="ZSAMPLE" adtcore:version="active"><atom:link href="./zsample/source/main" rel="http://www.sap.com/adt/relations/source" etag="{etag}"/><adtcore:packageRef adtcore:name="ZPKG"/></blue:wbobj>"#
    )
}

async fn hash_of(body: String, limit: Option<usize>) -> String {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    get_xml(
        &mut client,
        "/sap/bc/adt/ddic/dataelements/zsample",
        AdtVersion::Active,
        ByteRangeOptions { offset: 0, limit },
    )
    .await
    .unwrap()
    .sha256
}

/// The hash is a token for `set-xml`, so it must not move for a reason the
/// caller did not cause. ADT rewrites link etags whenever somebody stages or
/// discards pending work, and that is not a change to the document's content.
#[tokio::test]
async fn the_hash_ignores_link_etags_that_move_on_their_own() {
    let before = hash_of(document_with_links("20260101120000001"), None).await;
    let after = hash_of(document_with_links("20260508093000002"), None).await;

    assert_eq!(before, after);
}

/// Hashing the returned page would make the token useless the moment anyone
/// paged, because `set-xml` compares against the whole document.
#[tokio::test]
async fn the_hash_covers_the_whole_document_not_the_returned_page() {
    let whole = hash_of(document_with_links("20260101120000001"), None).await;
    let paged = hash_of(document_with_links("20260101120000001"), Some(20)).await;

    assert_eq!(whole, paged);
}

/// It is still a hash of *something* — a real content change has to move it,
/// or the guard would never refuse anything.
#[tokio::test]
async fn a_real_content_change_still_moves_the_hash() {
    let before = hash_of(document_with_links("20260101120000001"), None).await;
    let after = hash_of(
        document_with_links("20260101120000001").replace("ZPKG", "ZOTHER"),
        None,
    )
    .await;

    assert_ne!(before, after);
}

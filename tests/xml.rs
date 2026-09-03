use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        object_source::{ByteRangeOptions, get_xml},
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{method, path},
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
        ByteRangeOptions::default(),
    )
    .await
    .unwrap();

    assert!(xml.content.contains("ZCL_TEST"));
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
        ByteRangeOptions {
            offset: 0,
            limit: Some(10),
        },
    )
    .await
    .unwrap();

    assert_eq!(first.content, "<root>abc");
    assert!(first.truncated);
    assert_eq!(first.next_offset, Some(9));

    let second = get_xml(
        &mut client,
        "/sap/bc/adt/oo/classes/zcl_test",
        ByteRangeOptions {
            offset: first.next_offset.unwrap(),
            limit: None,
        },
    )
    .await
    .unwrap();
    assert_eq!(second.content, "édef</root>");
    assert!(!second.truncated);
    server.verify().await;
}

#[tokio::test]
async fn rejects_non_adt_xml_uri_before_http() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let error = get_xml(&mut client, "not-an-adt-uri", ByteRangeOptions::default())
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_adt_uri");
}

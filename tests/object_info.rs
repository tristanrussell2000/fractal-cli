use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{client::SapClient, object_info::get_object_info},
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
        customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
    }
}

#[tokio::test]
async fn fetches_object_info_and_extracts_the_description() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_test"))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0"?><class:abapClass xmlns:class="urn:test" xmlns:adtcore="urn:adt" adtcore:description="Test class"><name>ZCL_TEST</name></class:abapClass>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_object_info(&mut client, "/sap/bc/adt/oo/classes/zcl_test")
        .await
        .unwrap();

    assert_eq!(info.uri, "/sap/bc/adt/oo/classes/zcl_test");
    assert_eq!(info.description, "Test class");
    server.verify().await;
}

#[tokio::test]
async fn returns_a_hinted_error_when_sap_response_has_no_description() {
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
    let error = get_object_info(&mut client, "/sap/bc/adt/oo/classes/zcl_test")
        .await
        .unwrap_err();

    assert_eq!(error.code(), "no_description");
    assert!(error.hint().is_some());
    server.verify().await;
}

#[tokio::test]
async fn surfaces_a_not_found_error_without_special_casing() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/oo/classes/zcl_missing"))
        .respond_with(ResponseTemplate::new(404).set_body_string("not found"))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_object_info(&mut client, "/sap/bc/adt/oo/classes/zcl_missing")
        .await
        .unwrap_err();

    assert_eq!(error.code(), "not_found");
    server.verify().await;
}

#[tokio::test]
async fn rejects_non_adt_object_info_uri_before_http() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let error = get_object_info(&mut client, "not-an-adt-uri")
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_adt_uri");
}

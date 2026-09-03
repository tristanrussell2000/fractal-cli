use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{
        client::SapClient,
        ddic_type::{DataElementTypeSource, DdicTypeOptions, get_ddic_type},
        metadata_object::MetadataAdtObjectType,
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

fn resolving() -> DdicTypeOptions {
    DdicTypeOptions {
        object_type: None,
        resolve_domain: true,
    }
}

const DATA_ELEMENT_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj adtcore:name="ZSAMPLE_STATUS" adtcore:type="DTEL/DE" adtcore:description="Sample status"
    xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core">
  <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zpkg" adtcore:type="DEVC/K" adtcore:name="ZPKG"/>
  <dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements">
    <dtel:typeKind>domain</dtel:typeKind>
    <dtel:typeName>ZSAMPLE_STATUS_DOM</dtel:typeName>
    <dtel:dataType>NUMC</dtel:dataType>
    <dtel:dataTypeLength>000002</dtel:dataTypeLength>
    <dtel:dataTypeDecimals>000000</dtel:dataTypeDecimals>
    <dtel:shortFieldLabel>Status</dtel:shortFieldLabel>
    <dtel:searchHelp/>
    <dtel:changeDocument>false</dtel:changeDocument>
  </dtel:dataElement>
</blue:wbobj>"#;

const PREDEFINED_DATA_ELEMENT_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj adtcore:name="ZSAMPLE_AMOUNT" adtcore:description="Sample amount"
    xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core">
  <dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements">
    <dtel:typeKind>predefinedAbapType</dtel:typeKind>
    <dtel:typeName/>
    <dtel:dataType>DEC</dtel:dataType>
    <dtel:dataTypeLength>000015</dtel:dataTypeLength>
    <dtel:dataTypeDecimals>000006</dtel:dataTypeDecimals>
  </dtel:dataElement>
</blue:wbobj>"#;

const DOMAIN_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<doma:domain adtcore:name="ZSAMPLE_STATUS_DOM" adtcore:type="DOMA/DD" adtcore:description="Sample status domain"
    xmlns:doma="http://www.sap.com/dictionary/domain" xmlns:adtcore="http://www.sap.com/adt/core">
  <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zcfg" adtcore:type="DEVC/K" adtcore:name="ZCFG"/>
  <doma:content>
    <doma:typeInformation><doma:datatype>NUMC</doma:datatype><doma:length>000002</doma:length><doma:decimals>000000</doma:decimals></doma:typeInformation>
    <doma:outputInformation><doma:length>000002</doma:length><doma:conversionExit/><doma:signExists>false</doma:signExists><doma:lowercase>false</doma:lowercase></doma:outputInformation>
    <doma:valueInformation>
      <doma:valueTableRef/>
      <doma:fixValues>
        <doma:fixValue><doma:position>0001</doma:position><doma:low>01</doma:low><doma:high/><doma:text>Optional</doma:text></doma:fixValue>
        <doma:fixValue><doma:position>0002</doma:position><doma:low>02</doma:low><doma:high/><doma:text>Mandatory</doma:text></doma:fixValue>
      </doma:fixValues>
    </doma:valueInformation>
  </doma:content>
</doma:domain>"#;

fn mock_ok(path_value: &'static str, body: &'static str) -> Mock {
    Mock::given(method("GET"))
        .and(path(path_value))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
}

fn mock_not_found(path_value: &'static str) -> Mock {
    Mock::given(method("GET"))
        .and(path(path_value))
        .respond_with(ResponseTemplate::new(404).set_body_string("Not found"))
        .expect(1)
}

#[tokio::test]
async fn resolves_a_data_element_through_to_its_domain() {
    let server = MockServer::start().await;
    mock_ok(
        "/sap/bc/adt/ddic/dataelements/zsample_status",
        DATA_ELEMENT_XML,
    )
    .mount(&server)
    .await;
    mock_ok("/sap/bc/adt/ddic/domains/zsample_status_dom", DOMAIN_XML)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&mut client, "zsample_status", &resolving())
        .await
        .unwrap();

    assert_eq!(info.name, "ZSAMPLE_STATUS");
    assert_eq!(info.kind, "DTEL");
    assert_eq!(info.package.as_deref(), Some("ZPKG"));
    assert_eq!(info.effective_type.data_type.as_deref(), Some("NUMC"));

    let domain = info.domain.expect("resolved the domain");
    assert_eq!(domain.name, "ZSAMPLE_STATUS_DOM");
    // The value list is the whole reason for following the link.
    assert_eq!(domain.fixed_values.len(), 2);
    assert_eq!(domain.fixed_values[1].text.as_deref(), Some("Mandatory"));
    // The domain carries its own package and description, not the element's.
    assert_eq!(domain.package.as_deref(), Some("ZCFG"));
    assert_eq!(domain.description.as_deref(), Some("Sample status domain"));
    server.verify().await;
}

#[tokio::test]
async fn no_resolve_reads_the_data_element_alone() {
    let server = MockServer::start().await;
    mock_ok(
        "/sap/bc/adt/ddic/dataelements/zsample_status",
        DATA_ELEMENT_XML,
    )
    .mount(&server)
    .await;
    // No domain mock: an unmatched request would fail the test, which is the
    // assertion that the second call is not made.

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(
        &mut client,
        "ZSAMPLE_STATUS",
        &DdicTypeOptions {
            object_type: None,
            resolve_domain: false,
        },
    )
    .await
    .unwrap();

    assert!(info.domain.is_none());
    assert_eq!(
        info.data_element.expect("has detail").type_source,
        DataElementTypeSource::Domain("ZSAMPLE_STATUS_DOM".to_owned())
    );
    server.verify().await;
}

#[tokio::test]
async fn a_predefined_type_needs_no_domain_request() {
    let server = MockServer::start().await;
    mock_ok(
        "/sap/bc/adt/ddic/dataelements/zsample_amount",
        PREDEFINED_DATA_ELEMENT_XML,
    )
    .mount(&server)
    .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&mut client, "ZSAMPLE_AMOUNT", &resolving())
        .await
        .unwrap();

    assert!(info.domain.is_none());
    assert_eq!(info.effective_type.data_type.as_deref(), Some("DEC"));
    assert_eq!(info.effective_type.decimals, Some(6));
    server.verify().await;
}

#[tokio::test]
async fn falls_back_to_a_domain_when_no_data_element_has_the_name() {
    let server = MockServer::start().await;
    mock_not_found("/sap/bc/adt/ddic/dataelements/zsample_status_dom")
        .mount(&server)
        .await;
    mock_ok("/sap/bc/adt/ddic/domains/zsample_status_dom", DOMAIN_XML)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&mut client, "ZSAMPLE_STATUS_DOM", &resolving())
        .await
        .unwrap();

    assert_eq!(info.kind, "DOMA");
    assert!(info.data_element.is_none());
    // A domain read directly reports its own type as the effective one.
    assert_eq!(info.effective_type.data_type.as_deref(), Some("NUMC"));
    assert_eq!(info.effective_type.length, Some(2));
    server.verify().await;
}

#[tokio::test]
async fn an_explicit_type_skips_detection() {
    let server = MockServer::start().await;
    // Only the domain endpoint is mocked: asking for a domain must not try the
    // data-element collection first.
    mock_ok("/sap/bc/adt/ddic/domains/zsample_status_dom", DOMAIN_XML)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(
        &mut client,
        "ZSAMPLE_STATUS_DOM",
        &DdicTypeOptions {
            object_type: Some(MetadataAdtObjectType::Domain),
            resolve_domain: true,
        },
    )
    .await
    .unwrap();

    assert_eq!(info.kind, "DOMA");
    server.verify().await;
}

#[tokio::test]
async fn a_name_that_is_neither_reports_one_error_rather_than_a_bare_404() {
    let server = MockServer::start().await;
    mock_not_found("/sap/bc/adt/ddic/dataelements/zmissing")
        .mount(&server)
        .await;
    mock_not_found("/sap/bc/adt/ddic/domains/zmissing")
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&mut client, "ZMISSING", &resolving())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "ddic_type_not_found");
    assert_eq!(
        error.suggested_command().as_deref(),
        Some("fractal object search ZMISSING --kind DTEL")
    );
    server.verify().await;
}

#[tokio::test]
async fn detection_stops_at_a_failure_that_is_not_a_missing_object() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample_status"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .expect(1)
        .mount(&server)
        .await;
    // A 401 is the real answer. Reporting "neither a data element nor a
    // domain" would be actively wrong, and the domain must not be tried.

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&mut client, "ZSAMPLE_STATUS", &resolving())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "authentication_failed");
    assert_eq!(error.status(), Some(401));
    server.verify().await;
}

#[tokio::test]
async fn a_referenced_domain_that_cannot_be_read_names_both_objects() {
    let server = MockServer::start().await;
    mock_ok(
        "/sap/bc/adt/ddic/dataelements/zsample_status",
        DATA_ELEMENT_XML,
    )
    .mount(&server)
    .await;
    mock_not_found("/sap/bc/adt/ddic/domains/zsample_status_dom")
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&mut client, "ZSAMPLE_STATUS", &resolving())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "ddic_domain_missing");
    let message = error.message();
    assert!(message.contains("ZSAMPLE_STATUS"), "{message}");
    assert!(message.contains("ZSAMPLE_STATUS_DOM"), "{message}");
    server.verify().await;
}

#[tokio::test]
async fn a_malformed_name_is_refused_before_any_request() {
    let server = MockServer::start().await;
    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&mut client, "ZBAD NAME", &resolving())
        .await
        .unwrap_err();

    assert_eq!(error.code(), "invalid_object_name");
    // Nothing was mounted, so any request would have failed the test.
    server.verify().await;
}

#[tokio::test]
async fn a_standard_domain_outside_the_customer_namespaces_is_readable() {
    let server = MockServer::start().await;
    // The point of the read path having no namespace guard: customer data
    // elements almost always delegate to SAP-standard domains.
    mock_not_found("/sap/bc/adt/ddic/dataelements/std_sample_dom")
        .mount(&server)
        .await;
    mock_ok("/sap/bc/adt/ddic/domains/std_sample_dom", DOMAIN_XML)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&mut client, "STD_SAMPLE_DOM", &resolving())
        .await
        .unwrap();

    assert_eq!(info.kind, "DOMA");
    server.verify().await;
}

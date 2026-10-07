use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{
        adt_version::AdtVersion,
        client::SapClient,
        ddic_fields::{DdicFieldsError, DdicTableClass},
        ddic_structure::{DdicStructureError, get_ddic_structure},
        ddic_type::{DataElementTypeSource, DdicTypeOptions, get_ddic_type},
        metadata_object::MetadataAdtObjectType,
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, method, path, query_param},
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
        version: AdtVersion::Active,
    }
}

const DATA_ELEMENT_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj adtcore:name="ZSAMPLE_STATUS" adtcore:type="DTEL/DE" adtcore:description="Sample status" adtcore:version="active"
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
<doma:domain adtcore:name="ZSAMPLE_STATUS_DOM" adtcore:type="DOMA/DD" adtcore:description="Sample status domain" adtcore:version="active"
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

/// The same document staged as a pending edit, with a label nothing else has.
const INACTIVE_DATA_ELEMENT_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj adtcore:name="ZSAMPLE_STATUS" adtcore:type="DTEL/DE" adtcore:description="Sample status" adtcore:version="inactive"
    xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core">
  <dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements">
    <dtel:typeKind>predefinedAbapType</dtel:typeKind>
    <dtel:dataType>CHAR</dtel:dataType>
    <dtel:dataTypeLength>000010</dtel:dataTypeLength>
    <dtel:shortFieldLabel>Pending</dtel:shortFieldLabel>
  </dtel:dataElement>
</blue:wbobj>"#;

/// Answers only when the request names this layer, so a read that omits the
/// selector matches nothing and fails.
fn mock_version(path_value: &'static str, version: &'static str, body: &'static str) -> Mock {
    Mock::given(method("GET"))
        .and(path(path_value))
        .and(query_param("version", version))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
}

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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&client, "zsample_status", &resolving())
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(
        &client,
        "ZSAMPLE_STATUS",
        &DdicTypeOptions {
            object_type: None,
            resolve_domain: false,
            version: AdtVersion::Active,
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&client, "ZSAMPLE_AMOUNT", &resolving())
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&client, "ZSAMPLE_STATUS_DOM", &resolving())
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(
        &client,
        "ZSAMPLE_STATUS_DOM",
        &DdicTypeOptions {
            object_type: Some(MetadataAdtObjectType::Domain),
            resolve_domain: true,
            version: AdtVersion::Active,
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&client, "ZMISSING", &resolving())
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&client, "ZSAMPLE_STATUS", &resolving())
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&client, "ZSAMPLE_STATUS", &resolving())
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_type(&client, "ZBAD NAME", &resolving())
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
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&client, "STD_SAMPLE_DOM", &resolving())
        .await
        .unwrap();

    assert_eq!(info.kind, "DOMA");
    server.verify().await;
}

#[tokio::test]
async fn every_read_names_the_layer_it_wants() {
    let server = MockServer::start().await;
    mock_version(
        "/sap/bc/adt/ddic/dataelements/zsample_status",
        "active",
        DATA_ELEMENT_XML,
    )
    .mount(&server)
    .await;
    mock_version(
        "/sap/bc/adt/ddic/domains/zsample_status_dom",
        "active",
        DOMAIN_XML,
    )
    .mount(&server)
    .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(&client, "ZSAMPLE_STATUS", &resolving())
        .await
        .unwrap();

    assert_eq!(info.requested_version, "active");
    assert_eq!(info.version.as_deref(), Some("active"));
    // The resolved domain is read at the same layer, so one report describes
    // one point in time rather than two.
    assert_eq!(
        info.domain.expect("resolved the domain").version.as_deref(),
        Some("active")
    );
}

#[tokio::test]
async fn the_inactive_layer_is_reported_as_the_inactive_layer() {
    let server = MockServer::start().await;
    // Both layers are on offer. Without a selector a plain GET would be served
    // the pending edit, which is the bug this test exists for.
    mock_version(
        "/sap/bc/adt/ddic/dataelements/zsample_status",
        "inactive",
        INACTIVE_DATA_ELEMENT_XML,
    )
    .mount(&server)
    .await;
    mock_ok(
        "/sap/bc/adt/ddic/dataelements/zsample_status",
        DATA_ELEMENT_XML,
    )
    .expect(0)
    .mount(&server)
    .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(
        &client,
        "ZSAMPLE_STATUS",
        &DdicTypeOptions {
            object_type: Some(MetadataAdtObjectType::DataElement),
            resolve_domain: true,
            version: AdtVersion::Inactive,
        },
    )
    .await
    .unwrap();

    assert_eq!(info.requested_version, "inactive");
    assert_eq!(info.version.as_deref(), Some("inactive"));
    assert_eq!(info.effective_type.data_type.as_deref(), Some("CHAR"));
}

#[tokio::test]
async fn a_layer_that_does_not_exist_is_reported_as_the_one_that_arrived() {
    let server = MockServer::start().await;
    // SAP falls back rather than refusing: ask for a layer an object does not
    // have and it serves the other one, saying so only in the document.
    mock_version(
        "/sap/bc/adt/ddic/dataelements/zsample_status",
        "inactive",
        DATA_ELEMENT_XML,
    )
    .mount(&server)
    .await;
    mock_version(
        "/sap/bc/adt/ddic/domains/zsample_status_dom",
        "inactive",
        DOMAIN_XML,
    )
    .mount(&server)
    .await;

    let profile = profile(server.uri());
    let client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_type(
        &client,
        "ZSAMPLE_STATUS",
        &DdicTypeOptions {
            object_type: Some(MetadataAdtObjectType::DataElement),
            resolve_domain: true,
            version: AdtVersion::Inactive,
        },
    )
    .await
    .unwrap();

    assert_eq!(info.requested_version, "inactive");
    assert_eq!(info.version.as_deref(), Some("active"));
}

// ---------------------------------------------------------------------------
// Structures
//
// A structure's fields come from DD03L rather than its DDL source, because the
// source hides whatever an include or an append contributed. These live here
// rather than in their own file so the suite links one binary fewer.

const STRUCTURE_XML: &str = r#"<?xml version="1.0" encoding="utf-8"?>
<blue:blueSource adtcore:name="ZSAMPLE_RECORD_S" adtcore:type="TABL/DS" adtcore:description="Sample record structure" adtcore:version="active"
    xmlns:blue="http://www.sap.com/wbobj/blue" xmlns:adtcore="http://www.sap.com/adt/core">
  <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zpkg" adtcore:type="DEVC/K" adtcore:name="ZPKG"/>
</blue:blueSource>"#;

/// One marker row and two real fields, column-major as the preview returns it.
/// `MANDT` and `STATUS` are what the include contributed; the source would
/// show neither.
fn field_rows(table_class: &str) -> String {
    preview(&[
        ("POSITION", vec!["0001", "0002", "0003"]),
        ("FIELDNAME", vec![".INCLUDE", "MANDT", "STATUS"]),
        ("KEYFLAG", vec!["", "X", ""]),
        ("ROLLNAME", vec!["", "MANDT", "ZSAMPLE_STATUS"]),
        ("DOMNAME", vec!["", "MANDT", "ZSAMPLE_DOM"]),
        ("DATATYPE", vec!["", "CLNT", "CHAR"]),
        ("LENG", vec!["000000", "000003", "000012"]),
        ("DECIMALS", vec!["000000", "000000", "000000"]),
        ("INTTYPE", vec!["", "C", "C"]),
        ("NOTNULL", vec!["", "X", ""]),
        ("CHECKTABLE", vec!["", "*", "ZSAMPLE_VALUES"]),
        ("DDTEXT", vec!["", "Client", "Status"]),
        ("TABCLASS", vec![table_class, table_class, table_class]),
    ])
}

/// What SAP returns for a SELECT that matched nothing: every selected column
/// still described, with no values under it.
fn no_field_rows() -> String {
    preview(
        &[
            "POSITION",
            "FIELDNAME",
            "KEYFLAG",
            "ROLLNAME",
            "DOMNAME",
            "DATATYPE",
            "LENG",
            "DECIMALS",
            "INTTYPE",
            "NOTNULL",
            "CHECKTABLE",
            "DDTEXT",
            "TABCLASS",
        ]
        .map(|name| (name, Vec::new())),
    )
}

fn preview(columns: &[(&str, Vec<&str>)]) -> String {
    let mut xml = String::from(
        r#"<dataPreview:tableData xmlns:dataPreview="http://www.sap.com/adt/dataPreview">"#,
    );
    for (name, values) in columns {
        xml.push_str(&column_xml(name, values));
    }
    xml.push_str("</dataPreview:tableData>");
    xml
}

fn column_xml(name: &str, values: &[&str]) -> String {
    let data: String = values
        .iter()
        .map(|value| format!("<dataPreview:data>{value}</dataPreview:data>"))
        .collect();
    format!(
        r#"<dataPreview:columns><dataPreview:metadata dataPreview:name="{name}"/><dataPreview:dataSet>{data}</dataPreview:dataSet></dataPreview:columns>"#
    )
}

async fn mount_structure(server: &MockServer, table_class: &str) {
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "structure-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=structure-test; Path=/"),
        )
        .mount(server)
        .await;
    mock_version(
        "/sap/bc/adt/ddic/structures/zsample_record_s",
        "active",
        STRUCTURE_XML,
    )
    .mount(server)
    .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string_contains("dd03l"))
        .and(body_string_contains("tabname = 'ZSAMPLE_RECORD_S'"))
        .respond_with(ResponseTemplate::new(200).set_body_string(field_rows(table_class)))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn reads_the_fields_an_include_contributed_and_drops_the_marker() {
    let server = MockServer::start().await;
    mount_structure(&server, "INTTAB").await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_structure(&mut client, "zsample_record_s", AdtVersion::Active)
        .await
        .unwrap();

    assert_eq!(info.name, "ZSAMPLE_RECORD_S");
    assert_eq!(info.kind, DdicTableClass::Structure);
    assert_eq!(info.description.as_deref(), Some("Sample record structure"));
    assert_eq!(info.package.as_deref(), Some("ZPKG"));
    assert_eq!(info.uri, "/sap/bc/adt/ddic/structures/zsample_record_s");
    assert_eq!(info.requested_version, "active");
    assert_eq!(info.version.as_deref(), Some("active"));

    // Three DD03L rows, two fields: the `.INCLUDE` marker is not one.
    assert_eq!(info.field_count, 2);
    assert_eq!(info.key_field_count, 1);
    assert_eq!(info.fields[0].name, "mandt");
    assert!(info.fields[0].is_key);
    assert!(info.fields[0].not_null);
    assert_eq!(info.fields[0].data_element.as_deref(), Some("mandt"));
    assert_eq!(info.fields[0].col_type.as_deref(), Some("CLNT"));
    assert_eq!(info.fields[0].sap_type.as_deref(), Some("C"));
    assert_eq!(info.fields[0].length, Some(3));
    // `*` means "any table" and names nothing.
    assert_eq!(info.fields[0].check_table, None);
    // DD03L carries no text; this can only have come from the DD04T join.
    assert_eq!(info.fields[0].description.as_deref(), Some("Client"));
    assert_eq!(info.fields[1].name, "status");
    assert_eq!(info.fields[1].domain.as_deref(), Some("zsample_dom"));
    assert_eq!(
        info.fields[1].check_table.as_deref(),
        Some("zsample_values")
    );
    server.verify().await;
}

/// SAP's structures collection serves tables too, so the kind has to come from
/// DD02L rather than from the collection the document arrived through.
#[tokio::test]
async fn reports_a_table_read_through_the_structures_collection_as_a_table() {
    let server = MockServer::start().await;
    mount_structure(&server, "TRANSP").await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_structure(&mut client, "ZSAMPLE_RECORD_S", AdtVersion::Active)
        .await
        .unwrap();

    assert_eq!(info.kind, DdicTableClass::Table);
    server.verify().await;
}

#[tokio::test]
async fn a_name_with_no_recorded_fields_is_an_error_rather_than_an_empty_structure() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "structure-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=structure-test; Path=/"),
        )
        .mount(&server)
        .await;
    mock_version(
        "/sap/bc/adt/ddic/structures/zsample_record_s",
        "active",
        STRUCTURE_XML,
    )
    .mount(&server)
    .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .respond_with(ResponseTemplate::new(200).set_body_string(no_field_rows()))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let error = get_ddic_structure(&mut client, "zsample_record_s", AdtVersion::Active)
        .await
        .unwrap_err();

    assert!(matches!(
        error,
        DdicStructureError::Fields(DdicFieldsError::NoFields { .. })
    ));
    assert_eq!(error.code(), "ddic_structure_no_fields");
}

/// DD03L names its layers `A` and `N`, so an inactive read has to translate.
#[tokio::test]
async fn an_inactive_read_asks_dd03l_for_the_layer_it_calls_n() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "structure-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=structure-test; Path=/"),
        )
        .mount(&server)
        .await;
    mock_version(
        "/sap/bc/adt/ddic/structures/zsample_record_s",
        "inactive",
        STRUCTURE_XML,
    )
    .mount(&server)
    .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/datapreview/freestyle"))
        .and(body_string_contains("as4local = 'N'"))
        .respond_with(ResponseTemplate::new(200).set_body_string(field_rows("INTTAB")))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let info = get_ddic_structure(&mut client, "zsample_record_s", AdtVersion::Inactive)
        .await
        .unwrap();

    assert_eq!(info.requested_version, "inactive");
    server.verify().await;
}

use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{client::SapClient, object_usages::get_object_usages},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path, query_param},
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
async fn fetches_usages_and_strips_the_self_reference() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "usages-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=usages-test; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/sap/bc/adt/repository/informationsystem/usageReferences",
        ))
        .and(query_param("uri", "/sap/bc/adt/ddic/tables/zexample_table"))
        .and(query_param("sap-client", "903"))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.repository.usagereferences.request.v1+xml",
        ))
        .and(header("x-csrf-token", "usages-csrf"))
        .and(header("cookie", "SAP_SESSIONID=usages-test"))
        .and(body_string_contains(
            r#"adtcore:uri="/sap/bc/adt/ddic/tables/zexample_table""#,
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<usageReferences:usageReferenceResult xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences" numberOfResults="1" resultDescription="[AB1] Where-Used List: ZEXAMPLE_TABLE (Database Table)" referencedObjectIdentifier="">
                <usageReferences:referencedObjects>
                    <usageReferences:referencedObject uri="/sap/bc/adt/ddic/structures/zexample_table_s" parentUri="/sap/bc/adt/packages/zexample" isResult="true" canHaveChildren="false">
                        <usageReferences:adtObject xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZEXAMPLE_TABLE_S" adtcore:type="TABL/DS">
                            <adtcore:packageRef adtcore:uri="/sap/bc/adt/packages/zexample" adtcore:type="DEVC/K" adtcore:name="ZEXAMPLE"/>
                        </usageReferences:adtObject>
                    </usageReferences:referencedObject>
                    <usageReferences:referencedObject uri="/sap/bc/adt/ddic/tables/zexample_table" isResult="false" canHaveChildren="false"/>
                </usageReferences:referencedObjects>
            </usageReferences:usageReferenceResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let refs = get_object_usages(&mut client, "/sap/bc/adt/ddic/tables/zexample_table")
        .await
        .unwrap();

    // The self-reference row is stripped, leaving only the genuine hit.
    assert_eq!(refs.len(), 1);
    assert!(refs[0].direct_result);
    assert_eq!(refs[0].name.as_deref(), Some("ZEXAMPLE_TABLE_S"));
    assert_eq!(refs[0].package.as_deref(), Some("ZEXAMPLE"));
    server.verify().await;
}

#[tokio::test]
async fn returns_an_empty_list_when_sap_reports_no_usages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-csrf-token", "csrf"))
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path(
            "/sap/bc/adt/repository/informationsystem/usageReferences",
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<usageReferences:usageReferenceResult numberOfResults="0" resultDescription="none" referencedObjectIdentifier="" xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences"><usageReferences:referencedObjects/></usageReferences:usageReferenceResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let refs = get_object_usages(&mut client, "/sap/bc/adt/ddic/dataelements/zunused")
        .await
        .unwrap();

    assert!(refs.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn rejects_non_adt_usages_uri_before_http() {
    let profile = profile("http://127.0.0.1:1".to_owned());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();

    let error = get_object_usages(&mut client, "not-an-adt-uri")
        .await
        .unwrap_err();
    assert_eq!(error.code(), "invalid_adt_uri");
}

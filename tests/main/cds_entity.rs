use fractal::reportable_error::ReportableError;
use fractal::{
    config::Profile,
    sap::{cds_entity::get_cds_entity, client::SapClient},
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

const ELEMENT_INFO: &str = "/sap/bc/adt/ddic/ddl/elementinfo";
const EMPTY: &str = r#"<?xml version="1.0" encoding="utf-8"?><abapsource:elementInfo xmlns:abapsource="http://www.sap.com/adt/abapsource"><abapsource:properties/></abapsource:elementInfo>"#;
const VIEW: &str = r#"<?xml version="1.0" encoding="utf-8"?><abapsource:elementInfo adtcore:uri="/sap/bc/adt/ddic/ddl/sources/zsample_r_head" adtcore:type="DDLS/STOB" adtcore:name="ZSAMPLE_R_HEAD" xmlns:abapsource="http://www.sap.com/adt/abapsource" xmlns:adtcore="http://www.sap.com/adt/core"><abapsource:properties><abapsource:entry abapsource:key="isRootNode">true</abapsource:entry><abapsource:entry abapsource:key="entityType">view</abapsource:entry></abapsource:properties><abapsource:elementInfo adtcore:type="STOB/DOF" adtcore:name="HeadId"><abapsource:properties><abapsource:entry abapsource:key="ddicIsKey">true</abapsource:entry><abapsource:entry abapsource:key="ddicDataType">char</abapsource:entry><abapsource:entry abapsource:key="ddicLength">000010</abapsource:entry></abapsource:properties></abapsource:elementInfo></abapsource:elementInfo>"#;

async fn element_info(server: &MockServer, name: &str, body: &str) {
    Mock::given(method("GET"))
        .and(path(ELEMENT_INFO))
        .and(query_param("path", name))
        .respond_with(ResponseTemplate::new(200).set_body_string(body))
        .expect(1)
        .mount(server)
        .await;
}

#[tokio::test]
async fn reads_an_active_entity_in_one_request() {
    let server = MockServer::start().await;
    element_info(&server, "ZSAMPLE_R_HEAD", VIEW).await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let info = get_cds_entity(&client, "zsample_r_head", false)
        .await
        .unwrap();

    assert_eq!(info.name, "ZSAMPLE_R_HEAD");
    assert!(info.root);
    assert_eq!(info.key_field_count, 1);
    server.verify().await;
}

#[tokio::test]
async fn an_entity_never_activated_is_told_apart_from_a_missing_one() {
    let server = MockServer::start().await;
    element_info(&server, "ZSAMPLE_NEW", EMPTY).await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/ddl/sources/zsample_new"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<ddl:ddlSource/>"))
        .expect(1)
        .mount(&server)
        .await;
    element_info(&server, "ZSAMPLE_NONE", EMPTY).await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/ddl/sources/zsample_none"))
        .respond_with(ResponseTemplate::new(404))
        .expect(1)
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let never_activated = get_cds_entity(&client, "ZSAMPLE_NEW", false)
        .await
        .unwrap_err();
    assert_eq!(never_activated.code(), "cds_entity_not_active");
    assert!(
        never_activated
            .hint()
            .unwrap()
            .contains("fractal edit activate --type DDLS --name ZSAMPLE_NEW")
    );

    let missing = get_cds_entity(&client, "ZSAMPLE_NONE", false)
        .await
        .unwrap_err();
    assert!(missing.is_not_found());
    assert_eq!(missing.code(), "cds_entity_not_found");
    server.verify().await;
}

#[tokio::test]
async fn an_unreadable_source_is_an_error_not_an_absence() {
    let server = MockServer::start().await;
    element_info(&server, "ZSAMPLE_LOCKED", EMPTY).await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/ddl/sources/zsample_locked"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;

    let client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = get_cds_entity(&client, "ZSAMPLE_LOCKED", false)
        .await
        .unwrap_err();
    assert!(!error.is_not_found());
    assert_eq!(error.code(), "forbidden");
}

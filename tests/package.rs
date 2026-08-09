use fractal::{
    config::Profile,
    sap::{client::SapClient, package::get_package_contents},
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{basic_auth, body_string, header, method, path, query_param},
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
async fn fetches_and_parses_one_package_contents_response() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .and(header("x-csrf-token", "Fetch"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("x-csrf-token", "package-csrf")
                .insert_header("set-cookie", "SAP_SESSIONID=package-test; Path=/"),
        )
        .expect(1)
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZAPP"))
        .and(query_param("parent_tech_name", "ZAPP"))
        .and(query_param("parent_type", "DEVC/K"))
        .and(query_param("withShortDescriptions", "true"))
        .and(query_param("sap-client", "903"))
        .and(header("accept", "application/vnd.sap.as+xml;charset=UTF-8;dataname=com.sap.adt.RepositoryObjTree.ObjectTree"))
        .and(header("x-csrf-token", "package-csrf"))
        .and(header("cookie", "SAP_SESSIONID=package-test"))
        .and(basic_auth("developer", "password"))
        .and(body_string(""))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<r:response xmlns:r="urn:test">
                <r:TREE_CONTENT>
                    <r:SEU_ADT_REPOSITORY_OBJ_NODE OBJECT_TYPE="DEVC/K" OBJECT_NAME="ZSUB" DESCRIPTION="Subpackage"/>
                    <r:SEU_ADT_REPOSITORY_OBJ_NODE OBJECT_TYPE="CLAS/OC" OBJECT_NAME="ZCL_TEST" OBJECT_URI="/sap/bc/adt/oo/classes/zcl_test" DESCRIPTION="ignored"/>
                    <r:SEU_ADT_REPOSITORY_OBJ_NODE OBJECT_TYPE="TABL/DT" OBJECT_NAME="ZTABLE" OBJECT_URI="/sap/bc/adt/ddic/tables/ztable" DESCRIPTION="Table"/>
                </r:TREE_CONTENT>
            </r:response>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let contents = get_package_contents(&mut client, " zapp ").await.unwrap();

    assert_eq!(contents.package, "ZAPP");
    assert_eq!(contents.subpackages[0].name, "ZSUB");
    assert_eq!(contents.items.len(), 2);
    assert_eq!(contents.items[0].name, "ZCL_TEST");
    assert_eq!(contents.items[0].description, None);
    assert_eq!(contents.items[1].object_type.kind().as_str(), "TABL");
    server.verify().await;
}

use fractal::{
    config::Profile,
    sap::{
        adt::RepositoryKind,
        client::SapClient,
        package::{PackageItemsOptions, get_package_items},
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
        customer_namespaces: vec!["Z*".to_owned(), "Y*".to_owned()],
    }
}

fn response(package: &str, child: Option<&str>) -> String {
    let child_xml = child
        .map(|name| format!(r#"<NODE OBJECT_TYPE="DEVC/K" OBJECT_NAME="{name}"/>"#))
        .unwrap_or_default();
    format!(
        r#"<response><TREE_CONTENT>{child_xml}<NODE OBJECT_TYPE="CLAS/OC" OBJECT_NAME="ZCL_{package}" OBJECT_URI="/class/{package}"/><NODE OBJECT_TYPE="TABL/DT" OBJECT_NAME="ZTABLE_{package}" OBJECT_URI="/table/{package}"/></TREE_CONTENT></response>"#
    )
}

#[tokio::test]
async fn filters_sorts_and_pages_flat_package_items() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-csrf-token", "csrf"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZAPP"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response("ZAPP", None)))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_package_items(
        &mut client,
        "ZAPP",
        PackageItemsOptions {
            kind: Some(RepositoryKind::Clas),
            name_substring: Some("zcl".to_owned()),
            offset: 0,
            limit: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.total, 1);
    assert_eq!(result.items[0].name, "ZCL_ZAPP");
    assert_eq!(result.items[0].object_type.kind(), RepositoryKind::Clas);
    assert!(result.packages_failed.is_empty());
    server.verify().await;
}

#[tokio::test]
async fn recursively_collects_items_from_subpackages() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/core/discovery"))
        .respond_with(ResponseTemplate::new(200).insert_header("x-csrf-token", "csrf"))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZAPP"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response("ZAPP", Some("ZSUB"))))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/repository/nodestructure"))
        .and(query_param("parent_name", "ZSUB"))
        .respond_with(ResponseTemplate::new(200).set_body_string(response("ZSUB", None)))
        .expect(1)
        .mount(&server)
        .await;

    let profile = profile(server.uri());
    let mut client = SapClient::new(&profile, "password".to_owned()).unwrap();
    let result = get_package_items(
        &mut client,
        "ZAPP",
        PackageItemsOptions {
            recursive: true,
            limit: 10,
            ..Default::default()
        },
    )
    .await
    .unwrap();

    assert_eq!(result.total, 4);
    assert_eq!(result.items.len(), 4);
    assert!(result.items.iter().any(|item| item.package == "ZSUB"));
    server.verify().await;
}

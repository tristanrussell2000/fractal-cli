//! Creating and deleting DDIC objects that have no source.
//!
//! These share the POST-then-read-back and lock-then-prove-it-is-gone paths
//! with every other family, so what is pinned here is what differs: the
//! collection, the media type, the XML envelope, and the absence of a source
//! URI. The envelopes are not guessable — a data element is a generic
//! `blue:wbobj` wrapper while a domain has its own `doma:domain` root, with
//! unrelated namespaces — so each was read off a live object and each is
//! asserted here.

mod adt_edit_mock;

use adt_edit_mock::AdtEditSession;
use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        metadata_object::{
            MetadataAdtObjectType, MetadataObjectCreationRequest, create_metadata_object,
        },
    },
};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_string_contains, header, method, path},
};

const DATA_ELEMENT_COLLECTION: &str = "/sap/bc/adt/ddic/dataelements";
const DOMAIN_COLLECTION: &str = "/sap/bc/adt/ddic/domains";
const DATA_ELEMENT_MEDIA_TYPE: &str = "application/vnd.sap.adt.dataelements.v2+xml";

fn session() -> AdtEditSession {
    AdtEditSession {
        sap_client: "100",
        csrf_token: "metadata-create-token",
        session_cookie: "SAP_SESSIONID=metadata",
        object_path: "/sap/bc/adt/ddic/dataelements/zsample_de",
        source_path: "",
        lock_handle: "metadata-lock",
    }
}

fn profile(base_url: String) -> Profile {
    session().profile(base_url, &["Z*"])
}

fn request(object_type: MetadataAdtObjectType, name: &str) -> MetadataObjectCreationRequest {
    MetadataObjectCreationRequest {
        object_type,
        name: name.to_owned(),
        package: "$TMP".to_owned(),
        description: "Sample element".to_owned(),
        transport: None,
    }
}

/// The read-back SAP answers with: the full skeleton, blanks and all.
fn skeleton() -> &'static str {
    r#"<?xml version="1.0" encoding="utf-8"?>
<blue:wbobj xmlns:blue="http://www.sap.com/wbobj/dictionary/dtel" xmlns:adtcore="http://www.sap.com/adt/core" adtcore:name="ZSAMPLE_DE" adtcore:type="DTEL/DE" adtcore:version="new">
  <dtel:dataElement xmlns:dtel="http://www.sap.com/adt/dictionary/dataelements">
    <dtel:typeKind>domain</dtel:typeKind>
    <dtel:typeName/>
  </dtel:dataElement>
</blue:wbobj>"#
}

#[tokio::test]
async fn creates_a_data_element_with_the_envelope_a_live_system_accepts() {
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(DATA_ELEMENT_COLLECTION))
        .and(header("content-type", DATA_ELEMENT_MEDIA_TYPE))
        .and(body_string_contains("<blue:wbobj"))
        .and(body_string_contains(
            "xmlns:blue=\"http://www.sap.com/wbobj/dictionary/dtel\"",
        ))
        .and(body_string_contains("adtcore:type=\"DTEL/DE\""))
        .and(body_string_contains("adtcore:name=\"ZSAMPLE_DE\""))
        .and(body_string_contains(
            "<adtcore:packageRef adtcore:name=\"$TMP\"/>",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_string(skeleton()))
        .expect(1)
        .mount(&server)
        .await;
    // HTTP success is not proof; the object is read back before success is
    // reported, exactly as for a source-based object.
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample_de"))
        .respond_with(ResponseTemplate::new(200).set_body_string(skeleton()))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = create_metadata_object(
        &mut client,
        &["Z*".to_owned()],
        &request(MetadataAdtObjectType::DataElement, "zsample_de"),
    )
    .await
    .unwrap();

    assert_eq!(result.identity.object_type, "DTEL");
    assert_eq!(result.identity.name, "ZSAMPLE_DE");
    assert_eq!(
        result.identity.object_uri,
        "/sap/bc/adt/ddic/dataelements/zsample_de"
    );
    // The distinguishing property of this family.
    assert_eq!(result.identity.source_uri, None);
    server.verify().await;
}

#[tokio::test]
async fn a_domain_uses_its_own_collection_envelope_and_media_type() {
    // Nothing is shared with the data element but the wrapper attributes:
    // different collection, different root element, different namespace,
    // different media type.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(DOMAIN_COLLECTION))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.domains.v2+xml",
        ))
        .and(body_string_contains("<doma:domain"))
        .and(body_string_contains(
            "xmlns:doma=\"http://www.sap.com/dictionary/domain\"",
        ))
        .and(body_string_contains("adtcore:type=\"DOMA/DD\""))
        .respond_with(ResponseTemplate::new(201).set_body_string("<doma:domain/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/domains/zsample_do"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<doma:domain/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = create_metadata_object(
        &mut client,
        &["Z*".to_owned()],
        &request(MetadataAdtObjectType::Domain, "zsample_do"),
    )
    .await
    .unwrap();

    assert_eq!(result.identity.object_type, "DOMA");
    assert_eq!(result.identity.source_uri, None);
    server.verify().await;
}

#[tokio::test]
async fn a_creation_that_cannot_be_read_back_is_not_reported_as_success() {
    // The read-back is the only proof the object exists; SAP has answered 200
    // to destructive requests that did nothing before.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(DATA_ELEMENT_COLLECTION))
        .respond_with(ResponseTemplate::new(201).set_body_string(skeleton()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample_de"))
        .respond_with(ResponseTemplate::new(404).set_body_string("<error/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = create_metadata_object(
        &mut client,
        &["Z*".to_owned()],
        &request(MetadataAdtObjectType::DataElement, "zsample_de"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_create_verification_failed");
    server.verify().await;
}

#[tokio::test]
async fn a_taken_name_is_classified_for_this_family_too() {
    // The classification lives in the shared creation path, so it applies to
    // every family without being reimplemented per type.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(DATA_ELEMENT_COLLECTION))
        .respond_with(ResponseTemplate::new(400).set_body_string(
            "<error><message>Resource Data Element ZSAMPLE_DE does already exist.</message></error>",
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = create_metadata_object(
        &mut client,
        &["Z*".to_owned()],
        &request(MetadataAdtObjectType::DataElement, "zsample_de"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_create_object_exists");
    assert!(!error.hint().unwrap().contains("retry"));
    server.verify().await;
}

#[tokio::test]
async fn a_name_outside_the_customer_namespaces_never_reaches_sap() {
    let server = MockServer::start().await;

    let error = create_metadata_object(
        &mut SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap(),
        &["Z*".to_owned()],
        &request(MetadataAdtObjectType::DataElement, "SFLIGHT_DE"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_customer_namespaces");
    assert!(server.received_requests().await.unwrap().is_empty());
}

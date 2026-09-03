//! Creating and deleting DDIC objects that have no source.
//!
//! Deletion reaches the same guarded path every other family uses, so what is
//! pinned for it here is that a metadata object actually gets there: the
//! where-used refusal and the read-back proof apply to a data element exactly
//! as they do to a program.
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
use fractal::config::EditPolicy;
use fractal::{
    config::Profile,
    reportable_error::ReportableError,
    sap::{
        client::SapClient,
        metadata_object::{
            MetadataAdtObjectType, MetadataObjectCreationRequest, ServiceBindingSpec,
            ServiceBindingType, create_metadata_object, delete_metadata_object,
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
const TABLE_TYPE_COLLECTION: &str = "/sap/bc/adt/ddic/tabletypes";
const USAGES_PATH: &str = "/sap/bc/adt/repository/informationsystem/usageReferences";

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
        binding: None,
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
        &EditPolicy::namespaces_only(&["Z*"]),
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(MetadataAdtObjectType::Domain, "zsample_do"),
    )
    .await
    .unwrap();

    assert_eq!(result.identity.object_type, "DOMA");
    assert_eq!(result.identity.source_uri, None);
    server.verify().await;
}

#[tokio::test]
async fn a_table_type_uses_its_own_collection_envelope_and_media_type() {
    // The third metadata family, and the third unrelated envelope: nothing is
    // shared with the other two but the wrapper attributes.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(TABLE_TYPE_COLLECTION))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.tabletype.v1+xml",
        ))
        .and(body_string_contains("<ttyp:tableType"))
        .and(body_string_contains(
            "xmlns:ttyp=\"http://www.sap.com/dictionary/tabletype\"",
        ))
        .and(body_string_contains("adtcore:type=\"TTYP/DA\""))
        .respond_with(ResponseTemplate::new(201).set_body_string("<ttyp:tableType/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/tabletypes/zsample_tt"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<ttyp:tableType/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = create_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(MetadataAdtObjectType::TableType, "zsample_tt"),
    )
    .await
    .unwrap();

    assert_eq!(result.identity.object_type, "TTYP");
    assert_eq!(result.identity.source_uri, None);
    server.verify().await;
}

#[tokio::test]
async fn a_message_class_is_created_outside_the_ddic_collections() {
    // Every other metadata family lives under /ddic/; this one does not, and
    // its namespace is capitalised where the others are not. Both come from a
    // live object rather than from the pattern the others suggest.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/messageclass"))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.messageclass.v1+xml",
        ))
        .and(body_string_contains("<mc:messageClass"))
        .and(body_string_contains(
            "xmlns:mc=\"http://www.sap.com/adt/MessageClass\"",
        ))
        .and(body_string_contains("adtcore:type=\"MSAG/N\""))
        .respond_with(ResponseTemplate::new(201).set_body_string("<mc:messageClass/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/messageclass/zsample_msg"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<mc:messageClass/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = create_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(MetadataAdtObjectType::MessageClass, "zsample_msg"),
    )
    .await
    .unwrap();

    assert_eq!(result.identity.object_type, "MSAG");
    assert_eq!(
        result.identity.object_uri,
        "/sap/bc/adt/messageclass/zsample_msg"
    );
    assert_eq!(result.identity.source_uri, None);
    server.verify().await;
}

#[tokio::test]
async fn a_service_binding_sends_its_definition_and_protocol() {
    // The one member of this family that is not a shell. SAP answers a bare
    // binding with an HTTP 500 carrying no message, so the details cannot be
    // filled in afterwards — they have to be in the create.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path("/sap/bc/adt/businessservices/bindings"))
        .and(header(
            "content-type",
            "application/vnd.sap.adt.businessservices.servicebinding.v2+xml",
        ))
        .and(body_string_contains("<srvb:serviceBinding"))
        .and(body_string_contains("adtcore:type=\"SRVB/SVB\""))
        .and(body_string_contains(
            "/sap/bc/adt/ddic/srvd/sources/zsample_sd",
        ))
        .and(body_string_contains(
            "<srvb:binding srvb:type=\"ODATA\" srvb:version=\"V4\" srvb:category=\"1\"",
        ))
        .respond_with(ResponseTemplate::new(201).set_body_string("<srvb:serviceBinding/>"))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/businessservices/bindings/zsample_sb"))
        .respond_with(ResponseTemplate::new(200).set_body_string("<srvb:serviceBinding/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = create_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectCreationRequest {
            binding: Some(ServiceBindingSpec {
                service_definition: "ZSAMPLE_SD".to_owned(),
                binding_type: ServiceBindingType::ODataV4WebApi,
            }),
            ..request(MetadataAdtObjectType::ServiceBinding, "zsample_sb")
        },
    )
    .await
    .unwrap();

    assert_eq!(result.identity.object_type, "SRVB");
    server.verify().await;
}

#[tokio::test]
async fn a_binding_without_its_details_never_reaches_sap() {
    // SAP's refusal for this is an unexplained 500, so it is caught here
    // instead.
    let server = MockServer::start().await;

    let error = create_metadata_object(
        &mut SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap(),
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(MetadataAdtObjectType::ServiceBinding, "zsample_sb"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "missing_binding_details");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn binding_details_on_another_type_are_refused_rather_than_ignored() {
    let server = MockServer::start().await;

    let error = create_metadata_object(
        &mut SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap(),
        &EditPolicy::namespaces_only(&["Z*"]),
        &MetadataObjectCreationRequest {
            binding: Some(ServiceBindingSpec {
                service_definition: "ZSAMPLE_SD".to_owned(),
                binding_type: ServiceBindingType::ODataV4Ui,
            }),
            ..request(MetadataAdtObjectType::DataElement, "zsample_de")
        },
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "unexpected_binding_details");
    assert!(server.received_requests().await.unwrap().is_empty());
}

#[tokio::test]
async fn deleting_a_metadata_object_is_guarded_by_where_used_like_any_other() {
    // A data element is referenced by every field built on it, so this guard
    // matters more here, not less. Nothing in the metadata module implements
    // it — reaching the shared path is the whole point.
    let server = MockServer::start().await;
    session().mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(USAGES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<?xml version="1.0" encoding="utf-8"?>
<usageReferences:usageReferenceResult xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences" xmlns:adtcore="http://www.sap.com/adt/core">
  <usageReferences:referencedObjects>
    <usageReferences:referencedObject uri="/sap/bc/adt/ddic/structures/zuses_it" isResult="true" parentUri="">
      <usageReferences:adtObject adtcore:name="ZUSES_IT" adtcore:type="TABL/DS"/>
    </usageReferences:referencedObject>
  </usageReferences:referencedObjects>
</usageReferences:usageReferenceResult>"#,
        ))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let error = delete_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        MetadataAdtObjectType::DataElement,
        "zsample_de",
        None,
        false,
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "edit_delete_object_in_use");
    assert!(error.hint().unwrap().contains("ZUSES_IT"));
    // Refused before anything was locked or deleted.
    let requests = server.received_requests().await.unwrap();
    assert!(
        requests
            .iter()
            .all(|request| request.method != wiremock::http::Method::DELETE),
        "a guarded delete must not reach SAP"
    );
    server.verify().await;
}

#[tokio::test]
async fn a_deleted_metadata_object_must_read_back_as_gone() {
    let server = MockServer::start().await;
    let session = session();
    session.mount_csrf_session(&server).await;
    Mock::given(method("POST"))
        .and(path(USAGES_PATH))
        .respond_with(ResponseTemplate::new(200).set_body_string(
            r#"<usageReferences:usageReferenceResult xmlns:usageReferences="http://www.sap.com/adt/ris/usageReferences"><usageReferences:referencedObjects/></usageReferences:usageReferenceResult>"#,
        ))
        .mount(&server)
        .await;
    session.mount_lock(&server, None).await;
    Mock::given(method("DELETE"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample_de"))
        .respond_with(ResponseTemplate::new(200))
        .expect(1)
        .mount(&server)
        .await;
    // SAP has answered 200 to a destructive request that did nothing before, so
    // success is the read-back saying not-found, not the status code.
    Mock::given(method("GET"))
        .and(path("/sap/bc/adt/ddic/dataelements/zsample_de"))
        .respond_with(ResponseTemplate::new(404).set_body_string("<error/>"))
        .expect(1)
        .mount(&server)
        .await;

    let mut client = SapClient::new(&profile(server.uri()), "password".to_owned()).unwrap();
    let result = delete_metadata_object(
        &mut client,
        &EditPolicy::namespaces_only(&["Z*"]),
        MetadataAdtObjectType::DataElement,
        "zsample_de",
        None,
        false,
    )
    .await
    .unwrap();

    assert_eq!(result.identity.name, "ZSAMPLE_DE");
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
        &EditPolicy::namespaces_only(&["Z*"]),
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
        &EditPolicy::namespaces_only(&["Z*"]),
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
        &EditPolicy::namespaces_only(&["Z*"]),
        &request(MetadataAdtObjectType::DataElement, "SFLIGHT_DE"),
    )
    .await
    .unwrap_err();

    assert_eq!(error.code(), "object_outside_customer_namespaces");
    assert!(server.received_requests().await.unwrap().is_empty());
}

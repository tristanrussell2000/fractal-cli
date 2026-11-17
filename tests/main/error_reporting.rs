//! The single review point for Fractal's structured error contract.
//!
//! Every error family renders through `ReportableError`, so this walks one
//! representative failure per operation and asserts all five reported fields
//! side by side. It replaces the per-family assertions that used to live in
//! `command_error.rs`, which was the only place the whole contract could be
//! read at once.

use reqwest::StatusCode;

use fractal::{
    config::ConfigError,
    credentials::CredentialError,
    reportable_error::ReportableError,
    sap::{
        abap_unit::UnitTestError,
        adt_object_uri::AdtObjectUriError,
        adt_version::AdtVersion,
        class_include::ClassInclude,
        client::{SapClientError, SapHttpErrorKind},
        edit_session::AdtEditSessionError,
        editable_source::{
            AdtEditTargetValidationError, AdtSourceReadError, EditableAdtObjectType,
            EditableAdtSourceIdentity, EditableAdtSourceTargetError,
        },
        group_activation::{GroupActivationError, GroupOutcome, MemberSummary},
        object_info::ObjectInfoError,
        object_source::ObjectSourceError,
        package::PackageError,
        package_lifecycle::PackageLifecycleError,
        service_binding::{PublishOperation, SapPublishMessage, ServiceBindingError},
        source_activation::AdtSourceActivationError,
        source_check::AdtSourceCheckError,
        source_discard::AdtInactiveSourceDiscardError,
        source_patch::AdtSourcePatchError,
        source_replace::AdtSourceReplacementError,
        staged_work::StagedWorkError,
        table_write::{NotWritten, TableWriteRunError},
    },
    source_change::SourceChangePlanError,
};

fn identity() -> Box<EditableAdtSourceIdentity> {
    Box::new(EditableAdtSourceIdentity {
        object_type: EditableAdtObjectType::Class,
        name: "ZCL_SAMPLE".to_owned(),
        object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
        source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
        include: Some(fractal::sap::class_include::ClassInclude::Main),
    })
}

fn http(kind: SapHttpErrorKind, status: StatusCode, message: &str) -> SapClientError {
    SapClientError::Http {
        kind,
        status,
        url: "https://sap.example/sap/bc/adt/thing".to_owned(),
        message: message.to_owned(),
    }
}

/// One row of the contract: the error, and everything the CLI will print.
struct Expectation {
    error: Box<dyn ReportableError>,
    code: &'static str,
    status: Option<u16>,
    message_contains: &'static str,
    has_hint: bool,
    suggested_command: Option<&'static str>,
}

fn contract() -> Vec<Expectation> {
    vec![
        Expectation {
            error: Box::new(StagedWorkError::StagedByAnother {
                object_type: "PROG",
                name: "ZSAMPLE".to_owned(),
                include: None,
                author: "COLLEAGUE".to_owned(),
            }),
            code: "edit_staged_by_another_user",
            status: None,
            message_contains: "COLLEAGUE",
            has_hint: true,
            // Reading the layer that holds their work, which is read-only.
            suggested_command: Some(
                "fractal edit read --type PROG --name ZSAMPLE --version inactive",
            ),
        },
        Expectation {
            error: Box::new(StagedWorkError::StagedByAnother {
                object_type: "CLAS",
                name: "ZCL_SAMPLE".to_owned(),
                include: Some(ClassInclude::Implementations),
                author: "COLLEAGUE".to_owned(),
            }),
            code: "edit_staged_by_another_user",
            status: None,
            message_contains: "ZCL_SAMPLE's implementations include",
            has_hint: true,
            // The include holding their work, read-only.
            suggested_command: Some(
                "fractal edit read --type CLAS --name ZCL_SAMPLE --include implementations --version inactive",
            ),
        },
        Expectation {
            error: Box::new(ConfigError::NoProfileSelected),
            code: "no_default_profile",
            status: None,
            message_contains: "no default SAP profile",
            has_hint: true,
            // `auth login` mutates local state and prompts, so it stays prose.
            suggested_command: None,
        },
        Expectation {
            error: Box::new(CredentialError::Missing("DEV_100".to_owned())),
            code: "credential_missing",
            status: None,
            message_contains: "DEV_100",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(http(
                SapHttpErrorKind::AuthenticationFailed,
                StatusCode::UNAUTHORIZED,
                "Invalid credentials",
            )),
            code: "authentication_failed",
            status: Some(401),
            message_contains: "Invalid credentials",
            has_hint: true,
            suggested_command: Some("fractal system test"),
        },
        Expectation {
            error: Box::new(PackageError::Sap(http(
                SapHttpErrorKind::Forbidden,
                StatusCode::FORBIDDEN,
                "Package read refused",
            ))),
            code: "forbidden",
            status: Some(403),
            message_contains: "Package read refused",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(EditableAdtSourceTargetError::IncludeOnNonClass {
                object_type: "PROG",
                include: ClassInclude::Definitions,
            }),
            code: "edit_include_not_a_class",
            status: None,
            message_contains: "PROG objects have no definitions include",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(AdtSourceReadError::IncludeNotFound {
                name: "ZCL_SAMPLE".to_owned(),
                object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                include: ClassInclude::Testclasses,
                source: http(
                    SapHttpErrorKind::NotFound,
                    StatusCode::NOT_FOUND,
                    "does not have any inactive version",
                ),
            }),
            code: "edit_include_not_found",
            status: Some(404),
            message_contains: "has no testclasses include",
            has_hint: true,
            // The class document lists the includes the class has.
            suggested_command: Some("fractal object xml /sap/bc/adt/oo/classes/zcl_sample"),
        },
        Expectation {
            error: Box::new(ObjectSourceError::IncludeOnNonClass {
                uri: "/sap/bc/adt/programs/programs/zsample".to_owned(),
            }),
            code: "include_not_a_class",
            status: None,
            message_contains: "is not a class",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(ObjectSourceError::ConflictingInclude {
                uri: "/sap/bc/adt/oo/classes/zcl_sample/includes/macros".to_owned(),
                in_uri: "macros",
                requested: "definitions",
            }),
            code: "include_conflict",
            status: None,
            message_contains: "names the macros include",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(ObjectSourceError::UnknownInclude {
                uri: "/sap/bc/adt/oo/classes/zcl_sample/includes/locals".to_owned(),
            }),
            code: "unknown_class_include",
            status: None,
            message_contains: "not a class include",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(AdtSourceActivationError::BeforeImageUnreadable {
                name: "ZCL_SAMPLE".to_owned(),
                include: Some(ClassInclude::Implementations),
                source: AdtSourceReadError::Sap {
                    object_type: "CLAS",
                    name: "ZCL_SAMPLE".to_owned(),
                    source: http(
                        SapHttpErrorKind::ServerError,
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "Internal error",
                    ),
                },
            }),
            code: "edit_activation_before_image_unreadable",
            status: Some(500),
            message_contains: "implementations include",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(AdtInactiveSourceDiscardError::RestoredStreamNotPublished {
                identity: Box::new(identity().for_include(ClassInclude::Implementations)),
            }),
            code: "edit_discard_include_not_published",
            status: None,
            message_contains: "implementations include was restored",
            has_hint: true,
            suggested_command: Some("fractal object xml /sap/bc/adt/oo/classes/zcl_sample"),
        },
        Expectation {
            error: Box::new(ObjectSourceError::NoSourceForKind {
                kind: "DOMA".to_owned(),
                uri: "/sap/bc/adt/ddic/domains/zdomain".to_owned(),
            }),
            code: "no_source_for_kind",
            status: None,
            message_contains: "do not have an ABAP source view",
            has_hint: true,
            suggested_command: Some("fractal object xml /sap/bc/adt/ddic/domains/zdomain"),
        },
        Expectation {
            error: Box::new(ObjectSourceError::Uri(AdtObjectUriError::NotAnAdtUri(
                "/sap/bc/rest/thing".to_owned(),
            ))),
            code: "invalid_adt_uri",
            status: None,
            message_contains: "invalid ADT object URI",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(ObjectInfoError::NoDescription(
                "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
            )),
            code: "no_description",
            status: None,
            message_contains: "no description found",
            has_hint: true,
            suggested_command: Some("fractal object xml /sap/bc/adt/oo/classes/zcl_sample"),
        },
        Expectation {
            error: Box::new(AdtSourceReadError::Sap {
                object_type: "INTF",
                name: "ZIF_MISSING".to_owned(),
                source: http(
                    SapHttpErrorKind::NotFound,
                    StatusCode::NOT_FOUND,
                    "Object not found",
                ),
            }),
            code: "not_found",
            status: Some(404),
            message_contains: "Object not found",
            has_hint: true,
            suggested_command: Some("fractal object search ZIF_MISSING --kind INTF"),
        },
        Expectation {
            error: Box::new(EditableAdtSourceTargetError::UnsupportedObjectType(
                "DOMA".to_owned(),
            )),
            code: "unsupported_edit_object_type",
            status: None,
            message_contains: "unsupported edit source object type",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(AdtSourcePatchError::Patch {
                identity: identity(),
                source: SourceChangePlanError::AnchorNotFound,
            }),
            code: "patch_anchor_not_found",
            status: None,
            message_contains: "not found in the source",
            has_hint: true,
            suggested_command: Some(
                "fractal edit read --type CLAS --name ZCL_SAMPLE --version inactive",
            ),
        },
        Expectation {
            error: Box::new(AdtSourcePatchError::Session(
                AdtEditSessionError::LockFailed {
                    transport: None,
                    source: http(
                        SapHttpErrorKind::Other,
                        StatusCode::CONFLICT,
                        "Object is locked",
                    ),
                },
            )),
            code: "edit_lock_failed",
            status: Some(409),
            message_contains: "Object is locked",
            has_hint: true,
            // Retrying a write is never offered as an executable command.
            suggested_command: None,
        },
        Expectation {
            error: Box::new(AdtSourceReplacementError::Replacement {
                identity: identity(),
                source: SourceChangePlanError::SourceReplacementNoChanges,
            }),
            code: "source_replacement_no_change",
            status: None,
            message_contains: "identical to the current source",
            has_hint: true,
            suggested_command: Some(
                "fractal edit read --type CLAS --name ZCL_SAMPLE --version inactive",
            ),
        },
        Expectation {
            error: Box::new(AdtSourceCheckError::Sap {
                identity: identity(),
                version: AdtVersion::Inactive,
                source: http(
                    SapHttpErrorKind::Forbidden,
                    StatusCode::FORBIDDEN,
                    "Check authorization missing",
                ),
            }),
            code: "edit_source_check_failed",
            status: Some(403),
            message_contains: "Check authorization missing",
            has_hint: true,
            suggested_command: Some(
                "fractal edit read --type CLAS --name ZCL_SAMPLE --version inactive",
            ),
        },
        Expectation {
            error: Box::new(AdtSourceActivationError::NoInactiveVersion {
                identity: identity(),
            }),
            code: "edit_activation_no_inactive_source",
            status: None,
            message_contains: "no inactive source to activate",
            has_hint: true,
            suggested_command: Some(
                "fractal edit read --type CLAS --name ZCL_SAMPLE --version active",
            ),
        },
        Expectation {
            error: Box::new(AdtInactiveSourceDiscardError::ActiveSourceChanged {
                identity: identity(),
                before_sha256: "a".repeat(64),
                after_sha256: "b".repeat(64),
            }),
            code: "edit_discard_active_source_changed",
            status: None,
            message_contains: "changed active source",
            has_hint: true,
            suggested_command: Some(
                "fractal edit read --type CLAS --name ZCL_SAMPLE --version active",
            ),
        },
        Expectation {
            error: Box::new(AdtEditTargetValidationError::InvalidObject(
                EditableAdtSourceTargetError::InvalidObjectName("ZCL-BAD".to_owned()),
            )),
            code: "invalid_edit_object_name",
            status: None,
            message_contains: "invalid edit source object name",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(ServiceBindingError::NotActive {
                name: "ZSAMPLE_UI_O4".to_owned(),
            }),
            code: "binding_not_active",
            status: None,
            message_contains: "no active version",
            has_hint: true,
            // Activating is the remedy, and it mutates, so it stays in the hint.
            suggested_command: None,
        },
        Expectation {
            error: Box::new(ServiceBindingError::ServiceVersionAmbiguous {
                name: "ZSAMPLE_UI_O4".to_owned(),
                versions: vec!["0001".to_owned(), "0002".to_owned()],
            }),
            code: "binding_service_version_ambiguous",
            status: None,
            message_contains: "0001, 0002",
            has_hint: true,
            suggested_command: Some("fractal binding show --name ZSAMPLE_UI_O4"),
        },
        Expectation {
            error: Box::new(ServiceBindingError::Refused {
                operation: PublishOperation::Unpublish,
                name: "ZSAMPLE_UI_O4".to_owned(),
                message: SapPublishMessage {
                    severity: "ERROR".to_owned(),
                    short_text: "Un-publish failed".to_owned(),
                    long_text: None,
                },
            }),
            code: "binding_unpublish_failed",
            status: None,
            message_contains: "Un-publish failed",
            has_hint: true,
            suggested_command: Some("fractal binding show --name ZSAMPLE_UI_O4"),
        },
        Expectation {
            error: Box::new(ServiceBindingError::Unverified {
                operation: PublishOperation::Publish,
                name: "ZSAMPLE_UI_O4".to_owned(),
                message: None,
            }),
            code: "binding_publish_unverified",
            status: None,
            message_contains: "still unpublished",
            has_hint: true,
            suggested_command: Some("fractal binding show --name ZSAMPLE_UI_O4"),
        },
        Expectation {
            error: Box::new(ServiceBindingError::NotPublished {
                name: "ZSAMPLE_UI_O4".to_owned(),
            }),
            code: "binding_not_published",
            status: None,
            message_contains: "not published",
            has_hint: true,
            // Publishing mutates, so it is named in the hint only.
            suggested_command: Some("fractal binding show --name ZSAMPLE_UI_O4"),
        },
        Expectation {
            error: Box::new(ServiceBindingError::PathInvalid {
                path: "/sap/bc/adt/discovery".to_owned(),
                reason: "is absolute; give only the part after the service root",
            }),
            code: "binding_path_invalid",
            status: None,
            message_contains: "is absolute",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(ServiceBindingError::GetFailed {
                name: "ZSAMPLE_UI_O4".to_owned(),
                url: "/sap/opu/odata4/sap/zsample_ui_o4/srvd/sap/zsample_ui_o4/0001/Nope"
                    .to_owned(),
                source: http(
                    SapHttpErrorKind::NotFound,
                    StatusCode::NOT_FOUND,
                    "Not Found",
                ),
            }),
            code: "binding_get_failed",
            status: Some(404),
            message_contains: "/0001/Nope",
            has_hint: true,
            suggested_command: Some("fractal binding show --name ZSAMPLE_UI_O4"),
        },
        Expectation {
            error: Box::new(PackageLifecycleError::NotEmpty {
                name: "ZSAMPLE_PKG".to_owned(),
                items: vec!["DDLS/DF ZSAMPLE_VIEW".to_owned()],
                subpackages: Vec::new(),
            }),
            code: "package_not_empty",
            status: None,
            message_contains: "ZSAMPLE_VIEW",
            has_hint: true,
            suggested_command: Some("fractal package items ZSAMPLE_PKG"),
        },
        Expectation {
            error: Box::new(PackageLifecycleError::TransportRequired {
                name: "ZSAMPLE_PKG".to_owned(),
            }),
            code: "package_transport_required",
            status: None,
            message_contains: "needs a transport request",
            has_hint: true,
            suggested_command: Some("fractal transport list"),
        },
        Expectation {
            error: Box::new(PackageLifecycleError::AttributesRequired {
                name: "ZSAMPLE_PKG".to_owned(),
            }),
            code: "package_attributes_required",
            status: None,
            message_contains: "software component",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(TableWriteRunError::NotWritten(Box::new(NotWritten {
                table: "ZSAMPLE_RECORD".to_owned(),
                key: "id=R1".to_owned(),
                status: "conflict".to_owned(),
                detail: String::new(),
                dry_run: false,
            }))),
            code: "table_write_conflict",
            status: None,
            message_contains: "ZSAMPLE_RECORD row id=R1 was not written",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(TableWriteRunError::NotWritten(Box::new(NotWritten {
                table: "ZSAMPLE_CONFIG".to_owned(),
                key: "code=C1".to_owned(),
                status: "transport_refused".to_owned(),
                detail: "TK127".to_owned(),
                dry_run: false,
            }))),
            code: "table_write_transport_refused",
            status: None,
            message_contains: "TK127",
            has_hint: true,
            suggested_command: Some("fractal transport list"),
        },
        Expectation {
            error: Box::new(GroupActivationError::Incomplete {
                members: vec![MemberSummary {
                    object_type: EditableAdtObjectType::DdlSource,
                    name: "ZSAMPLE_CHILD".to_owned(),
                    outcome: GroupOutcome::NotActivated,
                    first_error: Some("The column sptxtx is unknown".to_owned()),
                }],
            }),
            code: "edit_activation_incomplete",
            status: None,
            message_contains: "ZSAMPLE_CHILD not_activated",
            has_hint: true,
            suggested_command: Some(
                "fractal edit check --type DDLS --name ZSAMPLE_CHILD --version inactive",
            ),
        },
        Expectation {
            error: Box::new(UnitTestError::TargetNotFound {
                target: "CLAS ZSAMPLE_AU".to_owned(),
            }),
            code: "unit_test_target_not_found",
            status: None,
            message_contains: "CLAS ZSAMPLE_AU does not exist",
            has_hint: true,
            suggested_command: None,
        },
        Expectation {
            error: Box::new(UnitTestError::SelectionUnknown {
                target: "CLAS ZSAMPLE_AU LTC_ADD".to_owned(),
                what: "NOPE".to_owned(),
                kind: "method",
                available: vec!["PASSES".to_owned()],
            }),
            code: "unit_test_selection_unknown",
            status: None,
            message_contains: "it has: PASSES",
            has_hint: true,
            suggested_command: None,
        },
    ]
}

#[test]
fn every_operation_reports_its_complete_contract() {
    for expected in contract() {
        let error = &expected.error;
        assert_eq!(error.code(), expected.code, "code for {error:?}");
        assert_eq!(
            error.status(),
            expected.status,
            "status for {}",
            error.code()
        );
        assert!(
            error.message().contains(expected.message_contains),
            "message for {} was {:?}",
            error.code(),
            error.message()
        );
        assert_eq!(
            error.hint().is_some(),
            expected.has_hint,
            "hint presence for {}",
            error.code()
        );
        assert_eq!(
            error.suggested_command().as_deref(),
            expected.suggested_command,
            "suggested command for {}",
            error.code()
        );
    }
}

#[test]
fn no_suggested_command_is_ever_a_mutation() {
    const MUTATING: [&str; 4] = [
        "fractal edit patch",
        "fractal edit set",
        "fractal edit activate",
        "fractal edit discard",
    ];

    for expected in contract() {
        let Some(command) = expected.error.suggested_command() else {
            continue;
        };
        assert!(
            command.starts_with("fractal "),
            "a suggested command must be runnable as printed: {command}"
        );
        for mutation in MUTATING {
            assert!(
                !command.starts_with(mutation),
                "{} suggested the mutating command {command}",
                expected.error.code()
            );
        }
    }
}

#[test]
fn transport_retry_advice_stays_in_prose() {
    // The one remedy that is genuinely "re-run your write, differently". It
    // must reach the caller as a hint and never as an executable field.
    let error = AdtSourcePatchError::Session(AdtEditSessionError::LockFailed {
        transport: Some("AB1K900575".to_owned()),
        source: http(
            SapHttpErrorKind::Other,
            StatusCode::CONFLICT,
            "Object is already locked in request AB1K900575",
        ),
    });

    assert!(error.hint().unwrap().contains("AB1K900575"));
    assert_eq!(error.suggested_command(), None);
}

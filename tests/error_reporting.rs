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
        adt_object_uri::AdtObjectUriError,
        client::{SapClientError, SapHttpErrorKind},
        edit_session::AdtEditSessionError,
        editable_source::{
            AdtEditTargetValidationError, AdtSourceReadError, AdtSourceVersion,
            EditableAdtObjectType, EditableAdtSourceIdentity, EditableAdtSourceTargetError,
        },
        object_info::ObjectInfoError,
        object_source::ObjectSourceError,
        package::PackageError,
        source_activation::AdtSourceActivationError,
        source_check::AdtSourceCheckError,
        source_discard::AdtInactiveSourceDiscardError,
        source_patch::AdtSourcePatchError,
        source_replace::AdtSourceReplacementError,
    },
    source_change::SourceChangePlanError,
};

fn identity() -> Box<EditableAdtSourceIdentity> {
    Box::new(EditableAdtSourceIdentity {
        object_type: EditableAdtObjectType::Class,
        name: "ZCL_SAMPLE".to_owned(),
        object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
        source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
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
            error: Box::new(ConfigError::NoProfileSelected),
            code: "no_default_profile",
            status: None,
            message_contains: "no default SAP profile",
            has_hint: true,
            // `auth login` mutates local state and prompts, so it stays prose.
            suggested_command: None,
        },
        Expectation {
            error: Box::new(CredentialError::Missing("DE2_903".to_owned())),
            code: "credential_missing",
            status: None,
            message_contains: "DE2_903",
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
                version: AdtSourceVersion::Inactive,
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
        transport: Some("DE3K900575".to_owned()),
        source: http(
            SapHttpErrorKind::Other,
            StatusCode::CONFLICT,
            "Object is already locked in request DE3K900575",
        ),
    });

    assert!(error.hint().unwrap().contains("DE3K900575"));
    assert_eq!(error.suggested_command(), None);
}

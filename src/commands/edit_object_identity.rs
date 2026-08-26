use serde::Serialize;

use fractal::sap::editable_source::EditableAdtSourceIdentity;

/// The object identity every `edit` command reports.
///
/// Each command output holds this with `#[serde(flatten)]`, so the emitted JSON
/// keeps the same top-level `object_type`, `name`, `object_uri`, and
/// `source_uri` fields callers have always received.
#[derive(Debug, Serialize)]
pub struct EditObjectIdentityOutput {
    pub object_type: String,
    pub name: String,
    pub object_uri: String,
    pub source_uri: String,
}

impl From<EditableAdtSourceIdentity> for EditObjectIdentityOutput {
    fn from(identity: EditableAdtSourceIdentity) -> Self {
        Self {
            object_type: identity.object_type.as_str().to_owned(),
            name: identity.name,
            object_uri: identity.object_uri,
            source_uri: identity.source_uri,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fractal::sap::editable_source::EditableAdtObjectType;

    #[test]
    fn flattens_into_the_same_top_level_json_fields() {
        #[derive(Debug, Serialize)]
        struct Output {
            ok: bool,
            #[serde(flatten)]
            object: EditObjectIdentityOutput,
            trailing: bool,
        }

        let json = serde_json::to_value(Output {
            ok: true,
            object: EditableAdtSourceIdentity {
                object_type: EditableAdtObjectType::Class,
                name: "ZCL_SAMPLE".to_owned(),
                object_uri: "/sap/bc/adt/oo/classes/zcl_sample".to_owned(),
                source_uri: "/sap/bc/adt/oo/classes/zcl_sample/source/main".to_owned(),
            }
            .into(),
            trailing: true,
        })
        .unwrap();

        assert_eq!(json["object_type"], "CLAS");
        assert_eq!(json["name"], "ZCL_SAMPLE");
        assert_eq!(json["object_uri"], "/sap/bc/adt/oo/classes/zcl_sample");
        assert_eq!(
            json["source_uri"],
            "/sap/bc/adt/oo/classes/zcl_sample/source/main"
        );
        assert!(json.get("object").is_none(), "identity must not nest");
    }
}

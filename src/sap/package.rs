use roxmltree::{Document, Node};
use thiserror::Error;

use super::adt::RepositoryKind;

#[derive(Debug, Error)]
pub enum PackageError {
    #[error("could not parse package node structure: {0}")]
    Parse(String),
}

#[derive(Debug, Clone)]
pub struct Subpackage {
    pub name: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PackageItem {
    pub name: String,
    pub object_type: String,
    pub kind: RepositoryKind,
    pub package: String,
    pub description: Option<String>,
    pub uri: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PackageContents {
    pub package: String,
    pub items: Vec<PackageItem>,
    pub subpackages: Vec<Subpackage>,
}

/// Parses one SAP `nodestructure` response into direct items and subpackages.
///
/// # Errors
///
/// Returns [`PackageError::Parse`] when the response is not well-formed XML.
pub fn parse_package_contents(xml: &str, package: &str) -> Result<PackageContents, PackageError> {
    let document = Document::parse(xml).map_err(|error| PackageError::Parse(error.to_string()))?;
    let package = package.to_ascii_uppercase();
    let mut items = Vec::new();
    let mut subpackages = Vec::new();

    for node in document.descendants().filter(is_repository_node) {
        let Some(name) = attribute(node, &["OBJECT_NAME", "OBJ_NAME"]) else {
            continue;
        };
        let Some(object_type) = attribute(node, &["OBJECT_TYPE", "OBJ_TYPE"]) else {
            continue;
        };

        if object_type == "DEVC/K" {
            subpackages.push(Subpackage {
                name: name.to_ascii_uppercase(),
                description: non_empty(attribute(node, &["DESCRIPTION"])),
            });
            continue;
        }

        let kind = RepositoryKind::from_object_type(object_type);
        let description = if matches!(
            kind,
            RepositoryKind::Clas | RepositoryKind::Prog | RepositoryKind::Msag
        ) {
            None
        } else {
            non_empty(attribute(node, &["DESCRIPTION"]))
        };

        items.push(PackageItem {
            name: name.to_owned(),
            object_type: object_type.to_owned(),
            kind,
            package: package.clone(),
            description,
            uri: non_empty(attribute(node, &["OBJECT_URI", "OBJ_URI"])),
        });
    }

    Ok(PackageContents {
        package,
        items,
        subpackages,
    })
}

fn is_repository_node(node: &Node<'_, '_>) -> bool {
    matches!(
        node.tag_name().name(),
        "SEU_ADT_REPOSITORY_OBJ_NODE" | "OBJECT" | "NODE"
    )
}

fn attribute<'a>(node: Node<'a, 'a>, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| node.attribute(*name))
}

fn non_empty(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_namespaced_package_and_object_nodes() {
        let xml = r#"<r:response xmlns:r="urn:test">
            <r:TREE_CONTENT>
                <r:SEU_ADT_REPOSITORY_OBJ_NODE OBJECT_TYPE="DEVC/K" OBJECT_NAME="zsub" DESCRIPTION="Sub"/>
                <r:SEU_ADT_REPOSITORY_OBJ_NODE OBJECT_TYPE="CLAS/OC" OBJECT_NAME="ZCL_TEST" DESCRIPTION="unreliable" OBJECT_URI="/sap/bc/adt/oo/classes/zcl_test"/>
                <r:SEU_ADT_REPOSITORY_OBJ_NODE OBJECT_TYPE="TABL/DT" OBJECT_NAME="ZTABLE" DESCRIPTION="Table" OBJECT_URI="/sap/bc/adt/ddic/tables/ztable"/>
            </r:TREE_CONTENT>
        </r:response>"#;

        let result = parse_package_contents(xml, "zroot").unwrap();
        assert_eq!(result.package, "ZROOT");
        assert_eq!(result.subpackages[0].name, "ZSUB");
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].kind, RepositoryKind::Clas);
        assert_eq!(result.items[0].description, None);
        assert_eq!(result.items[1].description.as_deref(), Some("Table"));
    }

    #[test]
    fn accepts_alternative_node_names_and_unknown_types() {
        let xml = r#"<response>
            <OBJECT OBJECT_TYPE="NROB/NRO" OBJECT_NAME="ZNUMBER" OBJ_URI="/sap/bc/adt/number/znumber"/>
            <NODE OBJ_TYPE="PROG/P" OBJ_NAME="ZPROG" DESCRIPTION="ignored"/>
        </response>"#;

        let result = parse_package_contents(xml, "ZROOT").unwrap();
        assert_eq!(result.items.len(), 2);
        assert_eq!(result.items[0].kind, RepositoryKind::Other);
        assert_eq!(
            result.items[0].uri.as_deref(),
            Some("/sap/bc/adt/number/znumber")
        );
        assert_eq!(result.items[1].description, None);
    }

    #[test]
    fn ignores_incomplete_nodes_and_preserves_empty_success() {
        let xml =
            r#"<response><NODE OBJECT_TYPE="CLAS/OC"/><NODE OBJECT_NAME="ZNO_TYPE"/></response>"#;
        let result = parse_package_contents(xml, "ZROOT").unwrap();
        assert!(result.items.is_empty());
        assert!(result.subpackages.is_empty());
    }

    #[test]
    fn malformed_xml_is_a_package_error() {
        let error = parse_package_contents("<response>", "ZROOT").unwrap_err();
        assert!(matches!(error, PackageError::Parse(_)));
    }
}

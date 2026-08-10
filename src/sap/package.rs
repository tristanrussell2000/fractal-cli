use reqwest::header::{HeaderMap, HeaderValue};
use roxmltree::{Document, Node};
use thiserror::Error;

use super::{
    adt::{AdtObjectType, RepositoryKind},
    client::SapClient,
};

#[derive(Debug, Error)]
pub enum PackageError {
    #[error(transparent)]
    Sap(#[from] super::client::SapError),
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
    pub object_type: AdtObjectType,
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

#[derive(Debug, Clone)]
pub struct PackageTreeNode {
    pub name: String,
    pub parent: Option<String>,
    pub description: Option<String>,
    pub item_count: usize,
}

#[derive(Debug, Clone)]
pub struct PackageFailure {
    pub package: String,
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct PackageTree {
    pub root: String,
    pub packages: Vec<PackageTreeNode>,
    pub kinds: std::collections::BTreeMap<String, usize>,
    pub packages_failed: Vec<PackageFailure>,
}

const NODE_STRUCTURE_PATH: &str = "/sap/bc/adt/repository/nodestructure";
const NODE_STRUCTURE_ACCEPT: &str =
    "application/vnd.sap.as+xml;charset=UTF-8;dataname=com.sap.adt.RepositoryObjTree.ObjectTree";

/// Fetches and parses the direct contents of one ABAP package.
///
/// # Errors
///
/// Returns [`PackageError`] when SAP rejects the request or its XML response
/// cannot be parsed.
pub async fn get_package_contents(
    sap: &mut SapClient,
    package: &str,
) -> Result<PackageContents, PackageError> {
    let package = package.trim().to_ascii_uppercase();
    if package.is_empty() {
        return Err(PackageError::Parse("package name is required".to_owned()));
    }

    let mut headers = HeaderMap::new();
    headers.insert("Accept", HeaderValue::from_static(NODE_STRUCTURE_ACCEPT));
    let query = [
        ("parent_name", package.as_str()),
        ("parent_tech_name", package.as_str()),
        ("parent_type", "DEVC/K"),
        ("withShortDescriptions", "true"),
    ];
    let xml = sap
        .post_text(NODE_STRUCTURE_PATH, &query, None, headers)
        .await?;
    parse_package_contents(&xml, &package)
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

        let object_type = AdtObjectType::parse(object_type);
        let description = if matches!(
            object_type.kind(),
            RepositoryKind::Clas | RepositoryKind::Prog | RepositoryKind::Msag
        ) {
            None
        } else {
            non_empty(attribute(node, &["DESCRIPTION"]))
        };

        items.push(PackageItem {
            name: name.to_owned(),
            object_type,
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

/// Traverses package contents breadth-first and returns package summaries.
///
/// When `recursive` is false, only the requested package is fetched. Root
/// failures are returned; child-package failures are retained in the result.
///
/// # Errors
///
/// Returns [`PackageError`] when the root package request fails or cannot be parsed.
pub async fn get_package_tree(
    sap: &mut SapClient,
    package: &str,
    recursive: bool,
) -> Result<PackageTree, PackageError> {
    let root = package.trim().to_ascii_uppercase();
    let root_contents = get_package_contents(sap, &root).await?;
    let mut packages = vec![PackageTreeNode {
        name: root.clone(),
        parent: None,
        description: None,
        item_count: root_contents.items.len(),
    }];
    let mut kinds = std::collections::BTreeMap::new();
    for item in &root_contents.items {
        *kinds
            .entry(item.object_type.kind().as_str().to_owned())
            .or_insert(0) += 1;
    }
    let mut packages_failed = Vec::new();

    if recursive {
        let mut queue: Vec<(String, String, Option<String>)> = root_contents
            .subpackages
            .into_iter()
            .map(|subpackage| (subpackage.name, root.clone(), subpackage.description))
            .collect();
        let mut index = 0;
        while index < queue.len() {
            let (name, parent, description) = queue[index].clone();
            index += 1;
            match get_package_contents(sap, &name).await {
                Ok(contents) => {
                    packages.push(PackageTreeNode {
                        name: name.clone(),
                        parent: Some(parent),
                        description,
                        item_count: contents.items.len(),
                    });
                    for item in &contents.items {
                        *kinds
                            .entry(item.object_type.kind().as_str().to_owned())
                            .or_insert(0) += 1;
                    }
                    queue.extend(
                        contents.subpackages.into_iter().map(|subpackage| {
                            (subpackage.name, name.clone(), subpackage.description)
                        }),
                    );
                }
                Err(error) => packages_failed.push(PackageFailure {
                    package: name,
                    message: error.to_string(),
                }),
            }
        }
    }

    Ok(PackageTree {
        root,
        packages,
        kinds,
        packages_failed,
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
        assert_eq!(result.items[0].object_type.kind(), RepositoryKind::Clas);
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
        assert_eq!(result.items[0].object_type.kind(), RepositoryKind::Other);
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

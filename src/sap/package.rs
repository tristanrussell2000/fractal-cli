use futures::{StreamExt, stream};
use reqwest::header::{HeaderMap, HeaderValue};
use roxmltree::{Document, Node};
use thiserror::Error;

use super::{
    client::SapClient,
    non_empty_attribute,
    repository_kind::{AdtObjectType, RepositoryKind},
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

#[derive(Debug, Clone, Default)]
pub struct PackageItemsOptions {
    pub recursive: bool,
    pub kind: Option<RepositoryKind>,
    pub object_type: Option<String>,
    pub name_substring: Option<String>,
    pub offset: usize,
    pub limit: usize,
}

#[derive(Debug, Clone)]
pub struct PackageItemsResult {
    pub root: String,
    pub recursive: bool,
    pub total: usize,
    pub items: Vec<PackageItem>,
    pub kinds: std::collections::BTreeMap<String, usize>,
    pub packages_failed: Vec<PackageFailure>,
}

impl PackageError {
    #[must_use]
    fn is_csrf_failure(&self) -> bool {
        matches!(self, Self::Sap(error) if error.is_csrf_failure())
    }
}

const NODE_STRUCTURE_PATH: &str = "/sap/bc/adt/repository/nodestructure";
const PACKAGE_FETCH_CONCURRENCY: usize = 4;
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
        let Some(name) = child_text(node, &["OBJECT_NAME", "OBJ_NAME"]) else {
            continue;
        };
        let Some(object_type) = child_text(node, &["OBJECT_TYPE", "OBJ_TYPE"]) else {
            continue;
        };

        if object_type == "DEVC/K" {
            subpackages.push(Subpackage {
                name: name.to_ascii_uppercase(),
                description: non_empty_attribute(child_text(node, &["DESCRIPTION"])),
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
            non_empty_attribute(child_text(node, &["DESCRIPTION"]))
        };

        items.push(PackageItem {
            name: name.to_owned(),
            object_type,
            package: package.clone(),
            description,
            uri: non_empty_attribute(child_text(node, &["OBJECT_URI", "OBJ_URI"])),
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
    let walked = walk_package_contents(sap, root.clone(), root_contents, recursive).await;
    let mut packages = vec![PackageTreeNode {
        name: root.clone(),
        parent: None,
        description: None,
        item_count: walked.root_items.len(),
    }];
    let mut kinds = std::collections::BTreeMap::new();
    for item in walked.all_items {
        *kinds
            .entry(item.object_type.kind().as_str().to_owned())
            .or_insert(0) += 1;
    }
    packages.extend(walked.packages);

    Ok(PackageTree {
        root,
        packages,
        kinds,
        packages_failed: walked.packages_failed,
    })
}

/// Fetches, filters, sorts, and paginates package objects as one flat list.
///
/// Direct mode fetches only the requested package. Recursive mode walks
/// subpackages serially; child failures are retained in the result.
///
/// # Errors
///
/// Returns [`PackageError`] when the root package cannot be fetched or parsed.
pub async fn get_package_items(
    sap: &mut SapClient,
    package: &str,
    options: PackageItemsOptions,
) -> Result<PackageItemsResult, PackageError> {
    let root = package.trim().to_ascii_uppercase();
    let root_contents = get_package_contents(sap, &root).await?;
    let walked = walk_package_contents(sap, root.clone(), root_contents, options.recursive).await;
    let mut all_items = walked.all_items;
    let packages_failed = walked.packages_failed;

    let name_filter = options
        .name_substring
        .map(|value| value.to_ascii_uppercase());
    let object_type_filter = options.object_type.map(|value| value.to_ascii_uppercase());
    all_items.retain(|item| {
        options
            .kind
            .is_none_or(|kind| item.object_type.kind() == kind)
            && object_type_filter
                .as_deref()
                .is_none_or(|object_type| item.object_type.as_str() == object_type)
            && name_filter
                .as_deref()
                .is_none_or(|needle| item.name.to_ascii_uppercase().contains(needle))
    });
    all_items.sort_by_key(|item| item.name.to_ascii_uppercase());

    let total = all_items.len();
    let limit = options.limit.max(1);
    let items: Vec<PackageItem> = all_items
        .into_iter()
        .skip(options.offset)
        .take(limit)
        .collect();
    let mut kinds = std::collections::BTreeMap::new();
    for item in &items {
        *kinds
            .entry(item.object_type.kind().as_str().to_owned())
            .or_insert(0) += 1;
    }

    Ok(PackageItemsResult {
        root,
        recursive: options.recursive,
        total,
        items,
        kinds,
        packages_failed,
    })
}

#[derive(Debug)]
struct WalkedPackageContents {
    root_items: Vec<PackageItem>,
    all_items: Vec<PackageItem>,
    packages: Vec<PackageTreeNode>,
    packages_failed: Vec<PackageFailure>,
}

async fn walk_package_contents(
    sap: &mut SapClient,
    root: String,
    root_contents: PackageContents,
    recursive: bool,
) -> WalkedPackageContents {
    let root_items = root_contents.items.clone();
    if !recursive {
        return WalkedPackageContents {
            root_items,
            all_items: root_contents.items,
            packages: Vec::new(),
            packages_failed: Vec::new(),
        };
    }

    let mut all_items = root_contents.items;
    let mut packages = Vec::new();
    let mut failures = Vec::new();
    let mut frontier: Vec<(String, String, Option<String>)> = root_contents
        .subpackages
        .into_iter()
        .map(|subpackage| (subpackage.name, root.clone(), subpackage.description))
        .collect();

    while !frontier.is_empty() {
        let mut results = {
            let read_only: &SapClient = &*sap;
            fetch_frontier(read_only, frontier).await
        };
        let retry_indices: Vec<usize> = results
            .iter()
            .enumerate()
            .filter_map(|(index, (_, _, _, _, result))| {
                result
                    .as_ref()
                    .err()
                    .filter(|error| error.is_csrf_failure())
                    .map(|_| index)
            })
            .collect();

        if !retry_indices.is_empty() && sap.refresh_csrf().await.is_ok() {
            let retry_frontier = retry_indices
                .iter()
                .map(|index| {
                    (
                        results[*index].1.clone(),
                        results[*index].2.clone(),
                        results[*index].3.clone(),
                    )
                })
                .collect::<Vec<_>>();
            let retried = fetch_frontier(sap, retry_frontier).await;
            for (index, retried_result) in retry_indices.into_iter().zip(retried) {
                results[index].4 = retried_result.4;
            }
        }

        results.sort_by_key(|(index, ..)| *index);
        frontier = Vec::new();

        for (_, name, parent, description, result) in results {
            match result {
                Ok(contents) => {
                    packages.push(PackageTreeNode {
                        name: name.clone(),
                        parent: Some(parent),
                        description,
                        item_count: contents.items.len(),
                    });
                    all_items.extend(contents.items);
                    frontier.extend(
                        contents.subpackages.into_iter().map(|subpackage| {
                            (subpackage.name, name.clone(), subpackage.description)
                        }),
                    );
                }
                Err(error) => failures.push(PackageFailure {
                    package: name,
                    message: error.to_string(),
                }),
            }
        }
    }

    WalkedPackageContents {
        root_items,
        all_items,
        packages,
        packages_failed: failures,
    }
}

async fn fetch_frontier(
    sap: &SapClient,
    frontier: Vec<(String, String, Option<String>)>,
) -> Vec<(
    usize,
    String,
    String,
    Option<String>,
    Result<PackageContents, PackageError>,
)> {
    stream::iter(frontier.into_iter().enumerate())
        .map(|(index, (name, parent, description))| async move {
            let result = get_package_contents_read_only(sap, &name).await;
            (index, name, parent, description, result)
        })
        .buffer_unordered(PACKAGE_FETCH_CONCURRENCY)
        .collect()
        .await
}

async fn get_package_contents_read_only(
    sap: &SapClient,
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
        .post_text_read_only(NODE_STRUCTURE_PATH, &query, None, headers)
        .await?;
    parse_package_contents(&xml, &package)
}

fn is_repository_node(node: &Node<'_, '_>) -> bool {
    matches!(
        node.tag_name().name(),
        "SEU_ADT_REPOSITORY_OBJ_NODE" | "OBJECT" | "NODE"
    )
}

/// `nodestructure` encodes each field as a child element's text, not an XML
/// attribute (e.g. `<OBJECT_TYPE>DEVC/K</OBJECT_TYPE>`, never
/// `OBJECT_TYPE="DEVC/K"`). Empty/self-closed fields have no text child, so
/// this already returns `None` for them without extra checks.
fn child_text<'a>(node: Node<'a, 'a>, names: &[&str]) -> Option<&'a str> {
    names.iter().find_map(|name| {
        node.children()
            .find(|child| child.is_element() && child.tag_name().name() == *name)
            .and_then(|child| child.text())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // Field shapes below mirror a real captured `nodestructure` response (a live
    // ZDTLS package on a DE3 system): every field is a child element's text,
    // never an XML attribute, and the response is wrapped in the standard ABAP
    // asx:abap/asx:values/DATA envelope. The parser previously assumed XML
    // attributes, which matched no real SAP response and silently produced
    // zero items against a live system.
    #[test]
    fn parses_namespaced_package_and_object_nodes() {
        let xml = r#"<asx:abap xmlns:asx="http://www.sap.com/abapxml"><asx:values><DATA><TREE_CONTENT>
            <SEU_ADT_REPOSITORY_OBJ_NODE><OBJECT_TYPE>DEVC/K</OBJECT_TYPE><OBJECT_NAME>ZSUB</OBJECT_NAME><DESCRIPTION>Sub</DESCRIPTION></SEU_ADT_REPOSITORY_OBJ_NODE>
            <SEU_ADT_REPOSITORY_OBJ_NODE><OBJECT_TYPE>CLAS/OC</OBJECT_TYPE><OBJECT_NAME>ZCL_TEST</OBJECT_NAME><DESCRIPTION>unreliable</DESCRIPTION><OBJECT_URI>/sap/bc/adt/oo/classes/zcl_test</OBJECT_URI></SEU_ADT_REPOSITORY_OBJ_NODE>
            <SEU_ADT_REPOSITORY_OBJ_NODE><OBJECT_TYPE>TABL/DT</OBJECT_TYPE><OBJECT_NAME>ZTABLE</OBJECT_NAME><DESCRIPTION>Table</DESCRIPTION><OBJECT_URI>/sap/bc/adt/ddic/tables/ztable</OBJECT_URI></SEU_ADT_REPOSITORY_OBJ_NODE>
        </TREE_CONTENT></DATA></asx:values></asx:abap>"#;

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
            <OBJECT><OBJECT_TYPE>NROB/NRO</OBJECT_TYPE><OBJECT_NAME>ZNUMBER</OBJECT_NAME><OBJ_URI>/sap/bc/adt/number/znumber</OBJ_URI></OBJECT>
            <NODE><OBJ_TYPE>PROG/P</OBJ_TYPE><OBJ_NAME>ZPROG</OBJ_NAME><DESCRIPTION>ignored</DESCRIPTION></NODE>
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
        let xml = r#"<response>
            <NODE><OBJECT_TYPE>CLAS/OC</OBJECT_TYPE></NODE>
            <NODE><OBJECT_NAME>ZNO_TYPE</OBJECT_NAME></NODE>
        </response>"#;
        let result = parse_package_contents(xml, "ZROOT").unwrap();
        assert!(result.items.is_empty());
        assert!(result.subpackages.is_empty());
    }

    // Regression test for the live-system bug: SAP includes a package-root
    // pseudo-node (its own DEVC/K facet entry) with self-closed, empty fields
    // alongside `TECH_NAME`. A self-closed element has no text child, so
    // `child_text` must return `None` for it rather than an empty string,
    // or this node would be mistaken for a nameless real subpackage.
    #[test]
    fn skips_self_closed_empty_fields_instead_of_treating_them_as_present() {
        let xml = r#"<response>
            <SEU_ADT_REPOSITORY_OBJ_NODE><OBJECT_TYPE>DEVC/K</OBJECT_TYPE><OBJECT_NAME/><TECH_NAME>ZDTLS</TECH_NAME><DESCRIPTION/></SEU_ADT_REPOSITORY_OBJ_NODE>
        </response>"#;
        let result = parse_package_contents(xml, "ZDTLS").unwrap();
        assert!(result.subpackages.is_empty());
    }

    #[test]
    fn malformed_xml_is_a_package_error() {
        let error = parse_package_contents("<response>", "ZROOT").unwrap_err();
        assert!(matches!(error, PackageError::Parse(_)));
    }
}

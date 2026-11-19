//! A class's or interface's outline, and go to definition.
//!
//! The outline is the object structure ADT serves for the editor's outline
//! view: every type, constant, attribute, method and local class, each with
//! the lines of its declaration and, for methods and classes, its
//! implementation, in whichever include holds it. Programs answer with a
//! structure that has no lines at all, so only classes and interfaces are read.
//!
//! Go to definition posts a position and **the source text itself**: SAP
//! navigates within the text it is sent, not the stored source, and answers
//! nothing without it. Columns count from 0, lines from 1.

use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue};
use serde::Serialize;
use thiserror::Error;

use super::{
    adt_version::AdtVersion,
    client::{SapClient, SapClientError},
    find_attribute_value, find_children, find_non_empty_attribute,
};
use crate::reportable_error::{ReportableError, sap_http_status};

const NAVIGATION_PATH: &str = "/sap/bc/adt/navigation/target";
const DECLARATION_BLOCK: &str = "/source/definitionBlock";
const IMPLEMENTATION_BLOCK: &str = "/source/implementationBlock";
const DECLARATION_NAME: &str = "/source/definitionIdentifier";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Outline {
    pub name: String,
    /// `CLAS/OC` or `INTF/OI`.
    pub adt_type: String,
    pub declaration: Option<LineRange>,
    pub implementation: Option<LineRange>,
    pub elements: Vec<OutlineElement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct OutlineElement {
    /// `method`, `attribute`, `constant`, `type`, `interface`, `alias`,
    /// `event`, `local class`, `local interface`, or SAP's type where unknown.
    pub kind: String,
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub visibility: Option<String>,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub r#static: bool,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
    pub declaration: Option<LineRange>,
    pub implementation: Option<LineRange>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub elements: Vec<Self>,
}

/// Lines of one source stream, counted from 1, both ends included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LineRange {
    /// `main`, or a class include such as `implementations`.
    pub include: String,
    pub start: u32,
    pub end: u32,
}

/// Where a name is declared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Definition {
    /// The source holding it, without the position.
    pub uri: String,
    pub line: u32,
    /// Counted from 1.
    pub column: u32,
}

#[derive(Debug, Error)]
pub enum OutlineError {
    #[error(transparent)]
    Sap(#[from] SapClientError),
    #[error("SAP returned malformed outline XML: {0}")]
    Parse(#[from] roxmltree::Error),
    #[error("{0} has no outline with lines; only classes and interfaces do")]
    Unsupported(String),
    #[error("{object} has no method {method}")]
    NoSuchMethod { object: String, method: String },
}

impl ReportableError for OutlineError {
    fn code(&self) -> &'static str {
        match self {
            Self::Sap(error) => error.code(),
            Self::Parse(_) => "outline_response_invalid",
            Self::Unsupported(_) => "outline_unsupported",
            Self::NoSuchMethod { .. } => "outline_no_such_method",
        }
    }

    fn status(&self) -> Option<u16> {
        sap_http_status(match self {
            Self::Sap(error) => Some(error),
            _ => None,
        })
    }

    fn hint(&self) -> Option<String> {
        match self {
            Self::Sap(error) => error.hint(),
            Self::Parse(_) => Some("SAP's object structure was not valid ADT XML.".to_owned()),
            Self::Unsupported(_) => Some(
                "Read the source with `fractal object source`; a CDS view's fields come from `fractal object show`."
                    .to_owned(),
            ),
            Self::NoSuchMethod { .. } => Some(
                "Run `fractal object outline` for the methods it has. A local class's method is written LCL_NAME=>METHOD."
                    .to_owned(),
            ),
        }
    }
}

/// The outline of a class or interface.
///
/// # Errors
///
/// Returns [`OutlineError::Unsupported`] for anything that is not a class or
/// an interface.
pub async fn get_outline(
    sap: &SapClient,
    object_uri: &str,
    version: AdtVersion,
) -> Result<Outline, OutlineError> {
    if !(object_uri.contains("/oo/classes/") || object_uri.contains("/oo/interfaces/")) {
        return Err(OutlineError::Unsupported(object_uri.to_owned()));
    }
    let xml = sap
        .get_text_with_query(
            &format!("{}/objectstructure", object_uri.trim_end_matches('/')),
            &[("version", version.as_str())],
        )
        .await?;
    parse_outline(&xml)
}

fn parse_outline(xml: &str) -> Result<Outline, OutlineError> {
    let document = roxmltree::Document::parse(xml)?;
    let root = document.root_element();
    Ok(Outline {
        name: find_non_empty_attribute(root, "name").unwrap_or_default(),
        adt_type: find_non_empty_attribute(root, "type").unwrap_or_default(),
        declaration: range(root, DECLARATION_BLOCK),
        implementation: range(root, IMPLEMENTATION_BLOCK),
        elements: elements_of(root),
    })
}

fn elements_of(node: roxmltree::Node<'_, '_>) -> Vec<OutlineElement> {
    find_children(node, "objectStructureElement")
        .filter(|element| find_attribute_value(*element, "isExternalRef") != Some("true"))
        .map(|element| {
            let adt_type = find_non_empty_attribute(element, "type").unwrap_or_default();
            let constant = find_attribute_value(element, "constant") == Some("true");
            OutlineElement {
                kind: kind_of(&adt_type, constant),
                name: find_non_empty_attribute(element, "name").unwrap_or_default(),
                visibility: find_non_empty_attribute(element, "visibility"),
                r#static: find_attribute_value(element, "level") == Some("static"),
                read_only: !constant && find_attribute_value(element, "readOnly") == Some("true"),
                declaration: range(element, DECLARATION_BLOCK)
                    .or_else(|| range(element, DECLARATION_NAME)),
                implementation: range(element, IMPLEMENTATION_BLOCK),
                elements: elements_of(element),
            }
        })
        .collect()
}

fn kind_of(adt_type: &str, constant: bool) -> String {
    match adt_type.split_once('/').map_or(adt_type, |(_, kind)| kind) {
        "OA" | "IA" if constant => "constant",
        "OA" | "IA" => "attribute",
        "OM" | "IO" | "OLD" => "method",
        "OT" | "IT" => "type",
        "OR" => "interface",
        "ON" | "IN" => "alias",
        "OE" | "IE" => "event",
        "OCL" => "local class",
        "OCI" | "OIL" => "local interface",
        _ => return adt_type.to_owned(),
    }
    .to_owned()
}

/// The range a link of this relation points at: `./source/main#start=30,2;end=35,11`.
fn range(element: roxmltree::Node<'_, '_>, relation: &str) -> Option<LineRange> {
    let href = find_children(element, "link")
        .find(|link| find_attribute_value(*link, "rel").is_some_and(|rel| rel.ends_with(relation)))
        .and_then(|link| find_non_empty_attribute(link, "href"))?;
    let (path, fragment) = href.split_once('#')?;
    let include = if path.ends_with("/source/main") {
        "main".to_owned()
    } else {
        path.rsplit_once("/includes/")?.1.to_owned()
    };
    let line_of = |key: &str| {
        fragment
            .split(';')
            .find_map(|part| part.strip_prefix(key))
            .and_then(|position| position.split(',').next())
            .and_then(|line| line.parse::<u32>().ok())
    };
    let start = line_of("start=")?;
    Some(LineRange {
        include,
        start,
        end: line_of("end=").unwrap_or(start),
    })
}

/// A method's implementation lines. `NAME` is a method of the object itself,
/// `IF_X~NAME` one of its interfaces', `LCL_Y=>NAME` a local class's.
///
/// # Errors
///
/// Returns [`OutlineError::NoSuchMethod`] when the outline has no such method
/// with an implementation.
pub fn method_lines(outline: &Outline, method: &str) -> Result<LineRange, OutlineError> {
    let not_found = || OutlineError::NoSuchMethod {
        object: outline.name.clone(),
        method: method.to_owned(),
    };
    let (container, method_name) = match method.split_once("=>") {
        Some((class, name)) => (Some(class), name),
        None => (None, method),
    };
    let elements = match container {
        Some(class) => {
            &outline
                .elements
                .iter()
                .find(|element| {
                    element.kind.starts_with("local") && element.name.eq_ignore_ascii_case(class)
                })
                .ok_or_else(not_found)?
                .elements
        }
        None => &outline.elements,
    };
    elements
        .iter()
        .find(|element| element.kind == "method" && element.name.eq_ignore_ascii_case(method_name))
        .and_then(|element| element.implementation.clone())
        .ok_or_else(not_found)
}

/// Where the name at `line`, `column` (counted from 0) of `source_uri` is
/// declared, or with `implementation`, implemented. `None` when there is no
/// name there.
///
/// # Errors
///
/// Returns [`OutlineError`] when the request fails or the answer is malformed.
pub async fn find_definition(
    sap: &mut SapClient,
    source_uri: &str,
    source: &str,
    line: u32,
    column: u32,
    implementation: bool,
) -> Result<Option<Definition>, OutlineError> {
    let position = format!("{source_uri}#start={line},{column}");
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("text/plain"));
    headers.insert(ACCEPT, HeaderValue::from_static("application/xml"));
    let answer = sap
        .post_text(
            NAVIGATION_PATH,
            &[
                ("uri", position.as_str()),
                (
                    "filter",
                    if implementation {
                        "implementation"
                    } else {
                        "definition"
                    },
                ),
            ],
            Some(source),
            headers,
        )
        .await?;
    parse_definition(&answer)
}

fn parse_definition(answer: &str) -> Result<Option<Definition>, OutlineError> {
    if answer.trim().is_empty() {
        return Ok(None);
    }
    let document = roxmltree::Document::parse(answer)?;
    let Some(uri) = find_non_empty_attribute(document.root_element(), "uri") else {
        return Ok(None);
    };
    let (path, fragment) = uri.split_once('#').unwrap_or((&uri, ""));
    let position = fragment
        .split(';')
        .find_map(|part| part.strip_prefix("start="))
        .unwrap_or_default();
    let mut numbers = position.split(',').map(|number| number.parse::<u32>().ok());
    let line = numbers.next().flatten().unwrap_or(1);
    let column = numbers.next().flatten().unwrap_or(0);
    Ok(Some(Definition {
        uri: path.to_owned(),
        line,
        column: column + 1,
    }))
}

/// The column, counted from 0, of the `occurrence`-th whole-word match of
/// `symbol` on `line_text`, ignoring case.
#[must_use]
pub fn symbol_column(line_text: &str, symbol: &str, occurrence: usize) -> Option<u32> {
    let haystack = line_text.to_ascii_lowercase();
    let needle = symbol.trim().to_ascii_lowercase();
    if needle.is_empty() {
        return None;
    }
    let is_word = |c: char| c.is_ascii_alphanumeric() || matches!(c, '_' | '/' | '~');
    haystack
        .match_indices(&needle)
        .filter(|(at, _)| {
            let before = haystack[..*at].chars().next_back();
            let after = haystack[at + needle.len()..].chars().next();
            !before.is_some_and(is_word) && !after.is_some_and(is_word)
        })
        .nth(occurrence.saturating_sub(1))
        .and_then(|(at, _)| u32::try_from(at).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    const STRUCTURE: &str = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/testdata/outline/structure.xml"
    ));

    fn outline() -> Outline {
        parse_outline(STRUCTURE).unwrap()
    }

    fn lines(include: &str, start: u32, end: u32) -> Option<LineRange> {
        Some(LineRange {
            include: include.to_owned(),
            start,
            end,
        })
    }

    #[test]
    fn the_class_and_each_element_carry_their_lines() {
        let outline = outline();
        assert_eq!(outline.name, "ZSAMPLE_OUTLINE");
        assert_eq!(outline.declaration, lines("main", 1, 21));
        assert_eq!(outline.implementation, lines("main", 23, 45));

        let kinds: Vec<(&str, &str)> = outline
            .elements
            .iter()
            .map(|element| (element.kind.as_str(), element.name.as_str()))
            .collect();
        assert_eq!(
            kinds,
            [
                ("interface", "IF_OO_ADT_CLASSRUN"),
                ("type", "TY_PAIR"),
                ("type", "TT_PAIRS"),
                ("constant", "C_LIMIT"),
                ("attribute", "TOTAL"),
                ("method", "ADD"),
                ("method", "SUM"),
                ("method", "DESCRIBE"),
                ("attribute", "PAIRS"),
                ("attribute", "INSTANCES"),
                ("method", "IF_OO_ADT_CLASSRUN~MAIN"),
                ("local class", "LCL_HELPER"),
            ],
            "the text elements reference is left out"
        );

        let describe = &outline.elements[7];
        assert_eq!(describe.visibility.as_deref(), Some("protected"));
        assert_eq!(describe.declaration, lines("main", 16, 17));
        assert_eq!(describe.implementation, lines("main", 41, 44));
        let total = &outline.elements[4];
        assert!(total.read_only);
        let limit = &outline.elements[3];
        assert!(
            limit.r#static && !limit.read_only,
            "a constant is not also read-only"
        );
        let instances = &outline.elements[9];
        assert!(instances.r#static);
    }

    #[test]
    fn a_local_class_lists_its_methods_in_their_include() {
        let outline = outline();
        let helper = outline.elements.last().unwrap();
        assert_eq!(helper.declaration, lines("implementations", 1, 6));
        assert_eq!(helper.elements[0].name, "SUM_OF");
        assert_eq!(
            helper.elements[0].implementation,
            lines("implementations", 9, 13)
        );
    }

    #[test]
    fn a_method_is_found_by_its_own_local_or_interface_name() {
        let outline = outline();
        assert_eq!(
            method_lines(&outline, "describe").unwrap(),
            lines("main", 41, 44).unwrap()
        );
        assert_eq!(
            method_lines(&outline, "if_oo_adt_classrun~main").unwrap(),
            lines("main", 24, 28).unwrap()
        );
        assert_eq!(
            method_lines(&outline, "LCL_HELPER=>SUM_OF").unwrap(),
            lines("implementations", 9, 13).unwrap()
        );
        assert_eq!(
            method_lines(&outline, "NOPE").unwrap_err().code(),
            "outline_no_such_method"
        );
        assert!(
            method_lines(&outline, "total").is_err(),
            "an attribute is not a method"
        );
    }

    #[test]
    fn a_definition_names_the_source_line_and_column() {
        let answer = r#"<?xml version="1.0" encoding="utf-8"?><adtcore:objectReference adtcore:uri="/sap/bc/adt/oo/classes/zsample_outline/includes/implementations#start=3,18" xmlns:adtcore="http://www.sap.com/adt/core"/>"#;
        assert_eq!(
            parse_definition(answer).unwrap(),
            Some(Definition {
                uri: "/sap/bc/adt/oo/classes/zsample_outline/includes/implementations".to_owned(),
                line: 3,
                column: 19,
            })
        );
        assert_eq!(
            parse_definition("").unwrap(),
            None,
            "nothing at that position"
        );
    }

    #[test]
    fn a_symbol_is_found_as_a_whole_word() {
        let line = "    result = lcl_helper=>sum_of( pairs ).";
        assert_eq!(symbol_column(line, "sum_of", 1), Some(25));
        assert_eq!(symbol_column(line, "LCL_HELPER", 1), Some(13));
        assert_eq!(symbol_column(line, "sum", 1), None, "only whole words");
        let twice = "    IF lines( pairs ) < lines( pairs ).";
        assert_eq!(symbol_column(twice, "pairs", 2), Some(31));
        assert_eq!(symbol_column(twice, "pairs", 3), None);
    }
}

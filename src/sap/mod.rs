pub mod adt;
pub mod client;
pub mod package;
pub mod table;

/// Trims an XML attribute value and turns blank into `None`. Shared by every
/// `sap` submodule's parser — SAP consistently uses empty-but-present
/// attributes (`colType=""`, self-closed fields) to mean "no value" rather
/// than omitting the attribute.
pub(crate) fn non_empty_attribute(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    (!value.is_empty()).then(|| value.to_owned())
}

/// Finds the first child element with the given local (namespace-stripped)
/// tag name. Shared by every `sap` submodule's parser.
pub(crate) fn find_child<'a, 'i>(
    node: roxmltree::Node<'a, 'i>,
    name: &str,
) -> Option<roxmltree::Node<'a, 'i>> {
    node.children()
        .find(|child| child.is_element() && child.tag_name().name() == name)
}

//! The separately stored source streams of a class.

/// One of a class's source streams.
///
/// `Main` is the global class. The other four are local includes, each read
/// and written at its own URI under the class, but locked through the class
/// and activated with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum ClassInclude {
    Main,
    /// Class-relevant local types (`CCDEF`).
    Definitions,
    /// Local types (`CCIMP`), where RAP handler classes live.
    Implementations,
    /// `CCMAC`.
    Macros,
    /// `CCAU`. Absent until created, unlike the other four.
    Testclasses,
}

const INCLUDES_SEGMENT: &str = "/includes/";
const MAIN_SOURCE_SUFFIX: &str = "/source/main";

impl ClassInclude {
    pub const ALL: [Self; 5] = [
        Self::Main,
        Self::Definitions,
        Self::Implementations,
        Self::Macros,
        Self::Testclasses,
    ];

    /// ADT's own name, as in `class:includeType` and the include URI.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Definitions => "definitions",
            Self::Implementations => "implementations",
            Self::Macros => "macros",
            Self::Testclasses => "testclasses",
        }
    }

    #[must_use]
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        Self::ALL
            .into_iter()
            .find(|include| include.as_str().eq_ignore_ascii_case(value))
    }

    /// The include's source URI under a class URI.
    ///
    /// `main` is not `includes/main`: the class document lists it with
    /// `abapsource:sourceUri="source/main"`.
    #[must_use]
    pub fn source_uri(self, class_uri: &str) -> String {
        let class_uri = class_uri.trim_end_matches('/');
        match self {
            Self::Main => format!("{class_uri}{MAIN_SOURCE_SUFFIX}"),
            include => format!("{class_uri}{INCLUDES_SEGMENT}{}", include.as_str()),
        }
    }

    /// Splits a class include URI into the class URI and the include.
    ///
    /// `None` for anything that is not a class source URI, including a bare
    /// class URI. An unknown include name is `None` too: guessing would read
    /// a different stream from the one named.
    #[must_use]
    pub fn split_source_uri(uri: &str) -> Option<(&str, Self)> {
        let path = uri.split(['#', '?']).next().unwrap_or(uri);
        if !is_class_uri(path) {
            return None;
        }
        if let Some(class_uri) = path.strip_suffix(MAIN_SOURCE_SUFFIX) {
            return Some((class_uri, Self::Main));
        }
        let (class_uri, name) = path.rsplit_once(INCLUDES_SEGMENT)?;
        if name.contains('/') {
            return None;
        }
        Some((class_uri, Self::parse(name)?))
    }
}

/// Whether a URI is under the class collection.
#[must_use]
pub fn is_class_uri(uri: &str) -> bool {
    uri.to_ascii_lowercase()
        .starts_with("/sap/bc/adt/oo/classes/")
}

#[cfg(test)]
mod tests {
    use super::*;

    const CLASS: &str = "/sap/bc/adt/oo/classes/zcl_sample";

    #[test]
    fn main_is_the_source_main_stream_and_the_rest_are_includes() {
        assert_eq!(
            ClassInclude::Main.source_uri(CLASS),
            "/sap/bc/adt/oo/classes/zcl_sample/source/main"
        );
        assert_eq!(
            ClassInclude::Implementations.source_uri(CLASS),
            "/sap/bc/adt/oo/classes/zcl_sample/includes/implementations"
        );
    }

    #[test]
    fn every_include_round_trips_through_its_uri() {
        for include in ClassInclude::ALL {
            let uri = include.source_uri(CLASS);
            assert_eq!(ClassInclude::split_source_uri(&uri), Some((CLASS, include)));
        }
    }

    #[test]
    fn a_check_message_location_still_splits() {
        assert_eq!(
            ClassInclude::split_source_uri(
                "/sap/bc/adt/oo/classes/zcl_sample/includes/implementations#start=9,9"
            ),
            Some((CLASS, ClassInclude::Implementations))
        );
    }

    #[test]
    fn refuses_what_is_not_a_known_class_include() {
        assert_eq!(ClassInclude::split_source_uri(CLASS), None);
        assert_eq!(
            ClassInclude::split_source_uri("/sap/bc/adt/oo/classes/zcl_sample/includes/locals"),
            None
        );
        assert_eq!(
            ClassInclude::split_source_uri("/sap/bc/adt/programs/programs/zsample/source/main"),
            None
        );
    }

    #[test]
    fn parses_names_case_insensitively() {
        assert_eq!(
            ClassInclude::parse(" Definitions "),
            Some(ClassInclude::Definitions)
        );
        assert_eq!(ClassInclude::parse("locals"), None);
    }
}

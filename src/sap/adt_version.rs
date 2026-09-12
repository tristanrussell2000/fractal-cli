//! Which stored version of an object to ask ADT for.

/// The `?version=` selector, shared by every read that names a layer.
///
/// Omitting it is not a third option: a read with no selector is served the
/// inactive version whenever one exists, and the active version otherwise.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum AdtVersion {
    #[default]
    Active,
    Inactive,
}

impl AdtVersion {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Inactive => "inactive",
        }
    }
}

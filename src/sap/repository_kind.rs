//! Repository object-type models shared across ADT operations.
//!
//! These are models, not operation code: `package.rs` and `editable_source.rs`
//! both depend on them, so they must not live inside an operation module.

use thiserror::Error;

#[derive(Debug, Error)]
#[error("unknown repository kind '{0}'")]
pub struct RepositoryKindParseError(pub String);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdtObjectType {
    ClasOc,
    IntfOi,
    TablDt,
    TablDs,
    TtypTt,
    ViewDv,
    DtelDe,
    DomaDd,
    DdlsDf,
    BdefBdo,
    SrvdSrv,
    SrvbSvb,
    MsagN,
    FugrF,
    ProgP,
    EnhoXhh,
    EnhsXsb,
    EnhsXsd,
    EnhsXb,
    Unknown(String),
}

impl AdtObjectType {
    #[must_use]
    pub fn parse(value: &str) -> Self {
        match value {
            "CLAS/OC" => Self::ClasOc,
            "INTF/OI" => Self::IntfOi,
            "TABL/DT" => Self::TablDt,
            "TABL/DS" => Self::TablDs,
            "TTYP/TT" => Self::TtypTt,
            "VIEW/DV" => Self::ViewDv,
            "DTEL/DE" => Self::DtelDe,
            "DOMA/DD" => Self::DomaDd,
            "DDLS/DF" => Self::DdlsDf,
            "BDEF/BDO" => Self::BdefBdo,
            "SRVD/SRV" => Self::SrvdSrv,
            "SRVB/SVB" => Self::SrvbSvb,
            "MSAG/N" => Self::MsagN,
            "FUGR/F" => Self::FugrF,
            "PROG/P" => Self::ProgP,
            "ENHO/XHH" => Self::EnhoXhh,
            "ENHS/XSB" => Self::EnhsXsb,
            "ENHS/XSD" => Self::EnhsXsd,
            "ENHS/XB" => Self::EnhsXb,
            _ => Self::Unknown(value.to_owned()),
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::ClasOc => "CLAS/OC",
            Self::IntfOi => "INTF/OI",
            Self::TablDt => "TABL/DT",
            Self::TablDs => "TABL/DS",
            Self::TtypTt => "TTYP/TT",
            Self::ViewDv => "VIEW/DV",
            Self::DtelDe => "DTEL/DE",
            Self::DomaDd => "DOMA/DD",
            Self::DdlsDf => "DDLS/DF",
            Self::BdefBdo => "BDEF/BDO",
            Self::SrvdSrv => "SRVD/SRV",
            Self::SrvbSvb => "SRVB/SVB",
            Self::MsagN => "MSAG/N",
            Self::FugrF => "FUGR/F",
            Self::ProgP => "PROG/P",
            Self::EnhoXhh => "ENHO/XHH",
            Self::EnhsXsb => "ENHS/XSB",
            Self::EnhsXsd => "ENHS/XSD",
            Self::EnhsXb => "ENHS/XB",
            Self::Unknown(value) => value,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> RepositoryKind {
        match self {
            Self::ClasOc => RepositoryKind::Clas,
            Self::IntfOi => RepositoryKind::Intf,
            Self::TablDt => RepositoryKind::Tabl,
            Self::TablDs => RepositoryKind::Stru,
            Self::TtypTt => RepositoryKind::Ttyp,
            Self::ViewDv => RepositoryKind::View,
            Self::DtelDe => RepositoryKind::Dtel,
            Self::DomaDd => RepositoryKind::Doma,
            Self::DdlsDf => RepositoryKind::Ddls,
            Self::BdefBdo => RepositoryKind::Bdef,
            Self::SrvdSrv => RepositoryKind::Srvd,
            Self::SrvbSvb => RepositoryKind::Srvb,
            Self::MsagN => RepositoryKind::Msag,
            Self::FugrF => RepositoryKind::Fugr,
            Self::ProgP => RepositoryKind::Prog,
            Self::EnhoXhh => RepositoryKind::Enho,
            Self::EnhsXsb | Self::EnhsXsd | Self::EnhsXb => RepositoryKind::Enhs,
            Self::Unknown(_) => RepositoryKind::Other,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepositoryKind {
    Clas,
    Intf,
    Tabl,
    Stru,
    Ttyp,
    View,
    Dtel,
    Doma,
    Ddls,
    Bdef,
    Srvd,
    Srvb,
    Msag,
    Fugr,
    Prog,
    Enho,
    Enhs,
    Other,
}

impl RepositoryKind {
    /// Parses a logical repository kind such as `CLAS` or `PROG`.
    ///
    /// # Errors
    ///
    /// Returns [`RepositoryKindParseError`] when the value is not a supported kind.
    pub fn parse(value: &str) -> Result<Self, RepositoryKindParseError> {
        match value.to_ascii_uppercase().as_str() {
            "CLAS" => Ok(Self::Clas),
            "INTF" => Ok(Self::Intf),
            "TABL" => Ok(Self::Tabl),
            "STRU" => Ok(Self::Stru),
            "TTYP" => Ok(Self::Ttyp),
            "VIEW" => Ok(Self::View),
            "DTEL" => Ok(Self::Dtel),
            "DOMA" => Ok(Self::Doma),
            "DDLS" => Ok(Self::Ddls),
            "BDEF" => Ok(Self::Bdef),
            "SRVD" => Ok(Self::Srvd),
            "SRVB" => Ok(Self::Srvb),
            "MSAG" => Ok(Self::Msag),
            "FUGR" => Ok(Self::Fugr),
            "PROG" => Ok(Self::Prog),
            "ENHO" => Ok(Self::Enho),
            "ENHS" => Ok(Self::Enhs),
            "OTHER" => Ok(Self::Other),
            _ => Err(RepositoryKindParseError(value.to_owned())),
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Clas => "CLAS",
            Self::Intf => "INTF",
            Self::Tabl => "TABL",
            Self::Stru => "STRU",
            Self::Ttyp => "TTYP",
            Self::View => "VIEW",
            Self::Dtel => "DTEL",
            Self::Doma => "DOMA",
            Self::Ddls => "DDLS",
            Self::Bdef => "BDEF",
            Self::Srvd => "SRVD",
            Self::Srvb => "SRVB",
            Self::Msag => "MSAG",
            Self::Fugr => "FUGR",
            Self::Prog => "PROG",
            Self::Enho => "ENHO",
            Self::Enhs => "ENHS",
            Self::Other => "OTHER",
        }
    }

    /// Every known kind, in the same order as [`Self::as_str`].
    pub const ALL: [Self; 18] = [
        Self::Clas,
        Self::Intf,
        Self::Tabl,
        Self::Stru,
        Self::Ttyp,
        Self::View,
        Self::Dtel,
        Self::Doma,
        Self::Ddls,
        Self::Bdef,
        Self::Srvd,
        Self::Srvb,
        Self::Msag,
        Self::Fugr,
        Self::Prog,
        Self::Enho,
        Self::Enhs,
        Self::Other,
    ];

    /// A short, human-readable description of the kind for reference/lookup use.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Clas => "Class — an ABAP object-oriented class",
            Self::Intf => "Interface — an ABAP object-oriented interface",
            Self::Tabl => "Database table — a DDIC transparent table",
            Self::Stru => "Structure — a DDIC structure with no database table behind it",
            Self::Ttyp => "Table type — a DDIC type for internal tables",
            Self::View => "View — a classic DDIC database view",
            Self::Dtel => "Data element — a DDIC field type carrying semantic meaning and labels",
            Self::Doma => "Domain — a DDIC value range and technical type for data elements",
            Self::Ddls => "CDS view — a Core Data Services view definition (DDL source)",
            Self::Bdef => {
                "Behavior definition — a RAP (RESTful ABAP Programming) behavior definition"
            }
            Self::Srvd => "Service definition — a RAP service definition exposing CDS views",
            Self::Srvb => {
                "Service binding — a RAP service binding (e.g. OData) for a service definition"
            }
            Self::Msag => "Message class — a container of ABAP messages",
            Self::Fugr => "Function group — a container of function modules",
            Self::Prog => "Program — a classic ABAP report or executable program",
            Self::Enho => {
                "Enhancement implementation — an implementation of an enhancement spot or BAdI"
            }
            Self::Enhs => {
                "Enhancement spot — a defined extension point (BAdI definition, source plug-in)"
            }
            Self::Other => {
                "Any object type not covered by the kinds above — check the raw object_type field for the exact SAP ADT type code"
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_repository_kinds() {
        assert_eq!(RepositoryKind::parse("clas").unwrap(), RepositoryKind::Clas);
        assert_eq!(
            RepositoryKind::parse("OTHER").unwrap(),
            RepositoryKind::Other
        );
        assert!(RepositoryKind::parse("invalid").is_err());
    }

    #[test]
    fn parses_known_and_unknown_object_types() {
        assert_eq!(AdtObjectType::parse("CLAS/OC").as_str(), "CLAS/OC");
        assert_eq!(AdtObjectType::parse("TTYP/DA").as_str(), "TTYP/DA");
        assert_eq!(
            AdtObjectType::parse("TTYP/DA").kind(),
            RepositoryKind::Other
        );
    }

    #[test]
    fn maps_known_and_unknown_object_types() {
        let cases = [
            ("CLAS/OC", RepositoryKind::Clas),
            ("INTF/OI", RepositoryKind::Intf),
            ("TABL/DT", RepositoryKind::Tabl),
            ("TABL/DS", RepositoryKind::Stru),
            ("DDLS/DF", RepositoryKind::Ddls),
            ("ENHS/XSD", RepositoryKind::Enhs),
            ("UNKNOWN/X", RepositoryKind::Other),
        ];
        for (object_type, expected) in cases {
            assert_eq!(AdtObjectType::parse(object_type).kind(), expected);
        }
    }
}

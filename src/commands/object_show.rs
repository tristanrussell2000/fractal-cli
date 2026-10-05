use std::fmt::Write as _;

use serde::Serialize;

use crate::{
    cli::{ObjectShowArgs, ObjectShowTypeArg},
    commands::{connect, render_version, tabular},
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::reportable_error::ReportableError;
use fractal::sap::{
    ddic_structure::{DdicStructureInfo, get_ddic_structure},
    ddic_type::{
        DataElementTypeSource, DdicTypeError, DdicTypeInfo, DdicTypeOptions, get_ddic_type,
    },
    metadata_object::MetadataAdtObjectType,
};

#[derive(Debug, Serialize)]
pub struct ObjectShowOutput {
    ok: bool,
    profile: String,
    #[serde(flatten)]
    info: ShowInfo,
}

/// Serialization only: the two shapes differ enough that one flattened struct
/// would carry a block of nulls for whichever kind was not read.
#[derive(Debug, Serialize)]
#[serde(untagged)]
pub enum ShowInfo {
    Type(Box<DdicTypeInfo>),
    Structure(Box<DdicStructureInfo>),
}

/// A name that is none of the three kinds this command reads.
#[derive(Debug, thiserror::Error)]
#[error("'{name}' is not a data element, domain, structure or table")]
pub struct UnknownDdicObject {
    name: String,
}

impl ReportableError for UnknownDdicObject {
    fn code(&self) -> &'static str {
        "ddic_object_not_found"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Search for the name to see what it actually is, or pass --type to skip detection."
                .to_owned(),
        )
    }
}

pub async fn object_show(
    explicit_profile: Option<&str>,
    args: &ObjectShowArgs,
) -> Result<ObjectShowOutput, Reported> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let version = args.version.into();

    let info = match args.object_type {
        Some(ObjectShowTypeArg::Stru) => ShowInfo::Structure(Box::new(
            get_ddic_structure(&mut client, &args.name, version).await?,
        )),
        Some(object_type) => {
            let options = DdicTypeOptions {
                object_type: Some(match object_type {
                    ObjectShowTypeArg::Dtel => MetadataAdtObjectType::DataElement,
                    ObjectShowTypeArg::Doma => MetadataAdtObjectType::Domain,
                    ObjectShowTypeArg::Stru => unreachable!("handled above"),
                }),
                resolve_domain: !args.no_resolve,
                version,
            };
            ShowInfo::Type(Box::new(
                get_ddic_type(&client, &args.name, &options).await?,
            ))
        }
        None => detect(&mut client, args, version).await?,
    };

    Ok(ObjectShowOutput {
        ok: true,
        profile: profile_name,
        info,
    })
}

/// Tries a data element, then a domain, then a field list.
///
/// The field-list read is last because it is the expensive one — a document
/// read plus a query — and because the first two settle most names.
/// Only a "none of those" answer falls through to it; any other failure is
/// the caller's real problem and is reported as it stands.
async fn detect(
    client: &mut fractal::sap::client::SapClient,
    args: &ObjectShowArgs,
    version: fractal::sap::adt_version::AdtVersion,
) -> Result<ShowInfo, Reported> {
    let options = DdicTypeOptions {
        object_type: None,
        resolve_domain: !args.no_resolve,
        version,
    };
    match get_ddic_type(client, &args.name, &options).await {
        Ok(info) => return Ok(ShowInfo::Type(Box::new(info))),
        Err(DdicTypeError::NotFound(_)) => {}
        Err(error) => return Err(error.into()),
    }

    match get_ddic_structure(client, &args.name, version).await {
        Ok(info) => Ok(ShowInfo::Structure(Box::new(info))),
        // The name was none of the kinds tried. Say that, rather than
        // reporting the last attempt's 404 as though a table had been meant.
        Err(error) if error.is_not_found() => Err(UnknownDdicObject {
            name: args.name.clone(),
        }
        .into()),
        Err(error) => Err(error.into()),
    }
}

pub fn print_object_show(result: &ObjectShowOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    match &result.info {
        ShowInfo::Type(info) => print!("{}", render_object_show_readable(info)),
        ShowInfo::Structure(info) => print!("{}", render_structure_readable(info)),
    }
}

fn render_structure_readable(info: &DdicStructureInfo) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "{} ({})", info.name, info.kind);
    if let Some(description) = &info.description {
        let _ = writeln!(output, "description: {description}");
    }
    if let Some(package) = &info.package {
        let _ = writeln!(output, "package: {package}");
    }
    let _ = writeln!(output, "uri: {}", info.uri);
    let _ = writeln!(
        output,
        "version: {}",
        render_version(info.requested_version, info.version.as_deref())
    );
    let _ = writeln!(
        output,
        "fields: {} (key fields: {})",
        info.field_count, info.key_field_count
    );

    let columns = [
        tabular::plain_column("KEY"),
        tabular::plain_column("FIELD"),
        tabular::plain_column("DATA ELEMENT"),
        tabular::plain_column("DOMAIN"),
        tabular::plain_column("COLUMN TYPE"),
        tabular::plain_column("SAP TYPE"),
        tabular::plain_column("LENGTH"),
        tabular::plain_column("CHECK TABLE"),
        tabular::plain_column("DESCRIPTION"),
    ];
    let rows: Vec<_> = info
        .fields
        .iter()
        .map(|field| {
            vec![
                if field.is_key { "yes" } else { "" }.to_owned(),
                field.name.clone(),
                field.data_element.clone().unwrap_or_default(),
                field.domain.clone().unwrap_or_default(),
                field.col_type.clone().unwrap_or_default(),
                field.sap_type.clone().unwrap_or_default(),
                render_length(field),
                field.check_table.clone().unwrap_or_default(),
                field.description.clone().unwrap_or_default(),
            ]
        })
        .collect();
    output.push_str(&tabular::render_grid(&columns, &rows));
    output
}

/// Decimals only matter when there are any, and a zero length means the type
/// has no fixed one.
fn render_length(field: &fractal::sap::ddic_fields::DdicField) -> String {
    let Some(length) = field.length.filter(|value| *value > 0) else {
        return String::new();
    };
    match field.decimals.filter(|value| *value > 0) {
        Some(decimals) => format!("{length},{decimals}"),
        None => length.to_string(),
    }
}

fn render_object_show_readable(info: &DdicTypeInfo) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "{} ({})", info.name, info.kind);
    if let Some(description) = &info.description {
        let _ = writeln!(output, "description: {description}");
    }
    if let Some(package) = &info.package {
        let _ = writeln!(output, "package: {package}");
    }
    let _ = writeln!(output, "uri: {}", info.uri);
    let _ = writeln!(
        output,
        "version: {}",
        render_version(info.requested_version, info.version.as_deref())
    );
    let _ = writeln!(output, "type: {}", render_effective_type(info));

    if let Some(element) = &info.data_element {
        let labels = [
            ("short", &element.short_label),
            ("medium", &element.medium_label),
            ("long", &element.long_label),
            ("heading", &element.heading_label),
        ];
        for (name, label) in labels {
            if let Some(label) = label {
                let _ = writeln!(output, "label {name}: {label}");
            }
        }
        if let Some(search_help) = &element.search_help {
            let _ = writeln!(output, "search help: {search_help}");
        }
        if let Some(parameter) = &element.set_get_parameter {
            let _ = writeln!(output, "set/get parameter: {parameter}");
        }
        if element.change_document {
            let _ = writeln!(output, "change document: yes");
        }
        // Say so explicitly: an absent domain block otherwise reads as a
        // failed lookup rather than a data element that has no domain.
        if info.domain.is_none() {
            let _ = writeln!(output, "domain: {}", render_missing_domain(element));
        }
    }

    if let Some(domain) = &info.domain {
        let _ = writeln!(output, "\ndomain {}", domain.name);
        if let Some(description) = &domain.description {
            let _ = writeln!(output, "  description: {description}");
        }
        let _ = writeln!(output, "  uri: {}", domain.uri);
        let _ = writeln!(
            output,
            "  version: {}",
            render_version(info.requested_version, domain.version.as_deref())
        );
        if let Some(length) = domain.output_length.filter(|value| *value > 0) {
            let _ = writeln!(output, "  output length: {length}");
        }
        if let Some(conversion_exit) = &domain.conversion_exit {
            let _ = writeln!(output, "  conversion exit: {conversion_exit}");
        }
        if domain.lowercase {
            let _ = writeln!(output, "  lowercase: yes");
        }
        if domain.sign_exists {
            let _ = writeln!(output, "  sign: yes");
        }
        if let Some(value_table) = &domain.value_table {
            let _ = writeln!(output, "  value table: {}", value_table.name);
        }
        if !domain.fixed_values.is_empty() {
            let _ = writeln!(output, "  fixed values: {}", domain.fixed_values.len());
            let columns = [
                tabular::plain_column("VALUE"),
                tabular::plain_column("TO"),
                tabular::plain_column("TEXT"),
            ];
            let rows: Vec<_> = domain
                .fixed_values
                .iter()
                .map(|value| {
                    vec![
                        value.low.clone(),
                        value.high.clone().unwrap_or_default(),
                        value.text.clone().unwrap_or_default(),
                    ]
                })
                .collect();
            output.push_str(&tabular::render_grid(&columns, &rows));
        }
    }

    output
}

fn render_effective_type(info: &DdicTypeInfo) -> String {
    let mut rendered = info
        .effective_type
        .data_type
        .clone()
        .unwrap_or_else(|| "unknown".to_owned());
    // Zero length means "no fixed length" — a STRING or RAWSTRING — and
    // printing `STRING 0` reads as a declared length rather than an unlimited
    // one. Same for decimals, which SAP sends as zero for every non-decimal
    // type, so a bare `,0` on a CHAR would be noise.
    if let Some(length) = info.effective_type.length.filter(|value| *value > 0) {
        let _ = write!(rendered, " {length}");
        if let Some(decimals) = info.effective_type.decimals.filter(|value| *value > 0) {
            let _ = write!(rendered, ",{decimals}");
        }
    }
    match info
        .data_element
        .as_ref()
        .map(|element| &element.type_source)
    {
        Some(DataElementTypeSource::Domain(name)) => {
            let _ = write!(rendered, " (via domain {name})");
        }
        Some(DataElementTypeSource::PredefinedAbapType) => rendered.push_str(" (predefined)"),
        Some(DataElementTypeSource::Other(kind)) if !kind.is_empty() => {
            let _ = write!(rendered, " ({kind})");
        }
        _ => {}
    }
    rendered
}

const fn render_missing_domain(element: &fractal::sap::ddic_type::DataElementInfo) -> &'static str {
    match element.type_source {
        DataElementTypeSource::Domain(_) => "not read (--no-resolve)",
        DataElementTypeSource::PredefinedAbapType | DataElementTypeSource::Other(_) => "none",
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, ObjectCommand, VersionArg};
    use fractal::sap::ddic_type::{
        DataElementInfo, DdicObjectRef, DomainFixedValue, DomainInfo, EffectiveType,
    };

    fn show_args(cli: Cli) -> ObjectShowArgs {
        let Command::Object {
            command: ObjectCommand::Show(args),
        } = cli.command
        else {
            panic!("expected object show command");
        };
        args
    }

    fn data_element_info() -> DdicTypeInfo {
        DdicTypeInfo {
            name: "ZSAMPLE_STATUS".to_owned(),
            kind: "DTEL",
            uri: "/sap/bc/adt/ddic/dataelements/zsample_status".to_owned(),
            requested_version: "active",
            version: Some("active".to_owned()),
            description: Some("Sample status".to_owned()),
            package: Some("ZPKG".to_owned()),
            effective_type: EffectiveType {
                data_type: Some("NUMC".to_owned()),
                length: Some(2),
                decimals: Some(0),
            },
            data_element: Some(DataElementInfo {
                type_source: DataElementTypeSource::Domain("ZSAMPLE_STATUS_DOM".to_owned()),
                short_label: Some("Status".to_owned()),
                medium_label: None,
                long_label: Some("Sample status label".to_owned()),
                heading_label: None,
                search_help: None,
                search_help_parameter: None,
                set_get_parameter: None,
                change_document: false,
            }),
            domain: None,
        }
    }

    fn domain_info() -> DomainInfo {
        DomainInfo {
            name: "ZSAMPLE_STATUS_DOM".to_owned(),
            uri: "/sap/bc/adt/ddic/domains/zsample_status_dom".to_owned(),
            version: Some("active".to_owned()),
            description: Some("Sample status domain".to_owned()),
            package: Some("ZCFG".to_owned()),
            data_type: Some("NUMC".to_owned()),
            length: Some(2),
            decimals: Some(0),
            output_length: Some(2),
            conversion_exit: None,
            lowercase: false,
            sign_exists: false,
            value_table: Some(DdicObjectRef {
                name: "ZSAMPLE_VALUES".to_owned(),
                uri: None,
            }),
            fixed_values: vec![DomainFixedValue {
                position: Some(1),
                low: "01".to_owned(),
                high: None,
                text: Some("Optional".to_owned()),
            }],
        }
    }

    #[test]
    fn resolves_the_domain_unless_told_not_to() {
        let args = show_args(Cli::try_parse_from(["fractal", "object", "show", "ZFIELD"]).unwrap());
        assert_eq!(args.name, "ZFIELD");
        assert_eq!(args.object_type, None);
        assert!(!args.no_resolve);

        let args = show_args(
            Cli::try_parse_from([
                "fractal",
                "object",
                "show",
                "ZFIELD",
                "--type",
                "doma",
                "--no-resolve",
            ])
            .unwrap(),
        );
        assert_eq!(args.object_type, Some(ObjectShowTypeArg::Doma));
        assert!(args.no_resolve);
    }

    #[test]
    fn reads_the_active_version_unless_told_otherwise() {
        let args = show_args(Cli::try_parse_from(["fractal", "object", "show", "ZFIELD"]).unwrap());
        assert_eq!(args.version, VersionArg::Active);

        let args = show_args(
            Cli::try_parse_from([
                "fractal",
                "object",
                "show",
                "ZFIELD",
                "--version",
                "inactive",
            ])
            .unwrap(),
        );
        assert_eq!(args.version, VersionArg::Inactive);
    }

    #[test]
    fn the_reported_version_is_the_one_that_arrived_not_the_one_requested() {
        // Asking for a layer an object does not have gets the other one, so
        // echoing the request back would state the very thing this command
        // used to get wrong.
        assert_eq!(render_version("active", Some("active")), "active");
        assert_eq!(
            render_version("inactive", Some("active")),
            "active (asked for inactive; this object has none)"
        );
        assert_eq!(
            render_version("active", Some("new")),
            "new (never activated)"
        );
        assert_eq!(
            render_version("active", None),
            "unknown (the document does not say)"
        );
    }

    #[test]
    fn readable_output_states_the_version_of_both_documents() {
        let mut info = data_element_info();
        info.requested_version = "inactive";
        info.version = Some("inactive".to_owned());
        let mut domain = domain_info();
        // The domain had no pending edit, so the same request fell back.
        domain.version = Some("active".to_owned());
        info.domain = Some(domain);
        let rendered = render_object_show_readable(&info);

        assert!(rendered.contains("version: inactive\n"), "{rendered}");
        assert!(
            rendered.contains("  version: active (asked for inactive; this object has none)"),
            "{rendered}"
        );
    }

    #[test]
    fn readable_output_names_the_domain_a_data_element_delegates_to() {
        let mut info = data_element_info();
        info.domain = Some(domain_info());
        let rendered = render_object_show_readable(&info);

        assert!(rendered.contains("ZSAMPLE_STATUS (DTEL)"), "{rendered}");
        assert!(
            rendered.contains("type: NUMC 2 (via domain ZSAMPLE_STATUS_DOM)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("label long: Sample status label"),
            "{rendered}"
        );
        assert!(rendered.contains("domain ZSAMPLE_STATUS_DOM"), "{rendered}");
        assert!(
            rendered.contains("value table: ZSAMPLE_VALUES"),
            "{rendered}"
        );
        assert!(rendered.contains("Optional"), "{rendered}");
        // Zero decimals are noise on a non-decimal type.
        assert!(!rendered.contains("NUMC 2,0"), "{rendered}");
    }

    #[test]
    fn an_unread_domain_is_distinguished_from_a_data_element_that_has_none() {
        let skipped = render_object_show_readable(&data_element_info());
        assert!(
            skipped.contains("domain: not read (--no-resolve)"),
            "{skipped}"
        );

        let mut predefined = data_element_info();
        predefined.data_element.as_mut().unwrap().type_source =
            DataElementTypeSource::PredefinedAbapType;
        let rendered = render_object_show_readable(&predefined);
        assert!(rendered.contains("domain: none"), "{rendered}");
        assert!(rendered.contains("type: NUMC 2 (predefined)"), "{rendered}");
    }

    #[test]
    fn an_unlimited_type_reports_no_length() {
        let mut info = data_element_info();
        info.effective_type = EffectiveType {
            data_type: Some("STRING".to_owned()),
            length: Some(0),
            decimals: Some(0),
        };
        let rendered = render_object_show_readable(&info);

        // `STRING 0` would read as a declared length rather than an unlimited one.
        assert!(rendered.contains("type: STRING (via domain"), "{rendered}");
        assert!(!rendered.contains("STRING 0"), "{rendered}");
    }

    #[test]
    fn a_domain_without_a_fixed_output_length_omits_it() {
        let mut domain = domain_info();
        domain.output_length = Some(0);
        let mut info = data_element_info();
        info.domain = Some(domain);

        assert!(!render_object_show_readable(&info).contains("output length"));
    }

    #[test]
    fn a_decimal_type_keeps_its_decimals() {
        let mut info = data_element_info();
        info.effective_type = EffectiveType {
            data_type: Some("DEC".to_owned()),
            length: Some(15),
            decimals: Some(6),
        };
        assert!(render_object_show_readable(&info).contains("type: DEC 15,6"));
    }

    #[test]
    fn a_domain_read_directly_renders_without_data_element_lines() {
        let domain = domain_info();
        let info = DdicTypeInfo {
            name: domain.name.clone(),
            kind: "DOMA",
            uri: domain.uri.clone(),
            requested_version: "active",
            version: domain.version.clone(),
            description: domain.description.clone(),
            package: domain.package.clone(),
            effective_type: EffectiveType {
                data_type: domain.data_type.clone(),
                length: domain.length,
                decimals: domain.decimals,
            },
            data_element: None,
            domain: Some(domain),
        };
        let rendered = render_object_show_readable(&info);

        assert!(rendered.contains("ZSAMPLE_STATUS_DOM (DOMA)"), "{rendered}");
        assert!(!rendered.contains("label "), "{rendered}");
        assert!(!rendered.contains("domain: "), "{rendered}");
        assert!(rendered.contains("fixed values: 1"), "{rendered}");
    }
}

use std::fmt::Write as _;

use serde::Serialize;

use crate::{
    cli::{DdicShowArgs, DdicTypeArg},
    commands::{connect, tabular},
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::sap::{
    ddic_type::{DataElementTypeSource, DdicTypeInfo, DdicTypeOptions, get_ddic_type},
    metadata_object::MetadataAdtObjectType,
};

#[derive(Debug, Serialize)]
pub struct DdicShowOutput {
    ok: bool,
    profile: String,
    #[serde(flatten)]
    info: DdicTypeInfo,
}

pub async fn ddic_show(
    explicit_profile: Option<&str>,
    args: &DdicShowArgs,
) -> Result<DdicShowOutput, Reported> {
    let options = DdicTypeOptions {
        object_type: args.object_type.map(|object_type| match object_type {
            DdicTypeArg::Dtel => MetadataAdtObjectType::DataElement,
            DdicTypeArg::Doma => MetadataAdtObjectType::Domain,
        }),
        resolve_domain: !args.no_resolve,
    };
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let info = get_ddic_type(&mut client, &args.name, &options).await?;

    Ok(DdicShowOutput {
        ok: true,
        profile: profile_name,
        info,
    })
}

pub fn print_ddic_show(result: &DdicShowOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_ddic_show_readable(&result.info));
}

fn render_ddic_show_readable(info: &DdicTypeInfo) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "{} ({})", info.name, info.kind);
    if let Some(description) = &info.description {
        let _ = writeln!(output, "description: {description}");
    }
    if let Some(package) = &info.package {
        let _ = writeln!(output, "package: {package}");
    }
    let _ = writeln!(output, "uri: {}", info.uri);
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

fn render_missing_domain(element: &fractal::sap::ddic_type::DataElementInfo) -> &'static str {
    match element.type_source {
        DataElementTypeSource::Domain(_) => "not read (--no-resolve)",
        DataElementTypeSource::PredefinedAbapType | DataElementTypeSource::Other(_) => "none",
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, DdicCommand};
    use fractal::sap::ddic_type::{
        DataElementInfo, DdicObjectRef, DomainFixedValue, DomainInfo, EffectiveType,
    };

    fn show_args(cli: Cli) -> DdicShowArgs {
        let Command::Ddic {
            command: DdicCommand::Show(args),
        } = cli.command
        else {
            panic!("expected ddic show command");
        };
        args
    }

    fn data_element_info() -> DdicTypeInfo {
        DdicTypeInfo {
            name: "ZDTLS_RULE_FIELD".to_owned(),
            kind: "DTEL",
            uri: "/sap/bc/adt/ddic/dataelements/zdtls_rule_field".to_owned(),
            description: Some("Rule field".to_owned()),
            package: Some("ZPKG".to_owned()),
            effective_type: EffectiveType {
                data_type: Some("NUMC".to_owned()),
                length: Some(2),
                decimals: Some(0),
            },
            data_element: Some(DataElementInfo {
                type_source: DataElementTypeSource::Domain("ZDTLS_RULE".to_owned()),
                short_label: Some("Rule".to_owned()),
                medium_label: None,
                long_label: Some("Template rule type".to_owned()),
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
            name: "ZDTLS_RULE".to_owned(),
            uri: "/sap/bc/adt/ddic/domains/zdtls_rule".to_owned(),
            description: Some("Rule domain".to_owned()),
            package: Some("ZCFG".to_owned()),
            data_type: Some("NUMC".to_owned()),
            length: Some(2),
            decimals: Some(0),
            output_length: Some(2),
            conversion_exit: None,
            lowercase: false,
            sign_exists: false,
            value_table: Some(DdicObjectRef {
                name: "T134".to_owned(),
                uri: None,
            }),
            fixed_values: vec![DomainFixedValue {
                position: Some(1),
                low: "01".to_owned(),
                high: None,
                text: Some("May be template managed".to_owned()),
            }],
        }
    }

    #[test]
    fn resolves_the_domain_unless_told_not_to() {
        let args = show_args(Cli::try_parse_from(["fractal", "ddic", "show", "ZFIELD"]).unwrap());
        assert_eq!(args.name, "ZFIELD");
        assert_eq!(args.object_type, None);
        assert!(!args.no_resolve);

        let args = show_args(
            Cli::try_parse_from([
                "fractal",
                "ddic",
                "show",
                "ZFIELD",
                "--type",
                "doma",
                "--no-resolve",
            ])
            .unwrap(),
        );
        assert_eq!(args.object_type, Some(DdicTypeArg::Doma));
        assert!(args.no_resolve);
    }

    #[test]
    fn readable_output_names_the_domain_a_data_element_delegates_to() {
        let mut info = data_element_info();
        info.domain = Some(domain_info());
        let rendered = render_ddic_show_readable(&info);

        assert!(rendered.contains("ZDTLS_RULE_FIELD (DTEL)"), "{rendered}");
        assert!(
            rendered.contains("type: NUMC 2 (via domain ZDTLS_RULE)"),
            "{rendered}"
        );
        assert!(
            rendered.contains("label long: Template rule type"),
            "{rendered}"
        );
        assert!(rendered.contains("domain ZDTLS_RULE"), "{rendered}");
        assert!(rendered.contains("value table: T134"), "{rendered}");
        assert!(rendered.contains("May be template managed"), "{rendered}");
        // Zero decimals are noise on a non-decimal type.
        assert!(!rendered.contains("NUMC 2,0"), "{rendered}");
    }

    #[test]
    fn an_unread_domain_is_distinguished_from_a_data_element_that_has_none() {
        let skipped = render_ddic_show_readable(&data_element_info());
        assert!(
            skipped.contains("domain: not read (--no-resolve)"),
            "{skipped}"
        );

        let mut predefined = data_element_info();
        predefined.data_element.as_mut().unwrap().type_source =
            DataElementTypeSource::PredefinedAbapType;
        let rendered = render_ddic_show_readable(&predefined);
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
        let rendered = render_ddic_show_readable(&info);

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

        assert!(!render_ddic_show_readable(&info).contains("output length"));
    }

    #[test]
    fn a_decimal_type_keeps_its_decimals() {
        let mut info = data_element_info();
        info.effective_type = EffectiveType {
            data_type: Some("DEC".to_owned()),
            length: Some(15),
            decimals: Some(6),
        };
        assert!(render_ddic_show_readable(&info).contains("type: DEC 15,6"));
    }

    #[test]
    fn a_domain_read_directly_renders_without_data_element_lines() {
        let domain = domain_info();
        let info = DdicTypeInfo {
            name: domain.name.clone(),
            kind: "DOMA",
            uri: domain.uri.clone(),
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
        let rendered = render_ddic_show_readable(&info);

        assert!(rendered.contains("ZDTLS_RULE (DOMA)"), "{rendered}");
        assert!(!rendered.contains("label "), "{rendered}");
        assert!(!rendered.contains("domain: "), "{rendered}");
        assert!(rendered.contains("fixed values: 1"), "{rendered}");
    }
}

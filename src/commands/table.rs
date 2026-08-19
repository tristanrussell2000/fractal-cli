use std::fmt::Write as _;

use serde::Serialize;

use crate::{
    cli::{TableDataArgs, TableMetadataArgs},
    command_error::CommandError,
    commands::{connect, tabular},
    output::{OutputFormat, print_result},
};
use fractal::sap::table::{
    TableDataOptions, TableDataResult, TableFieldMetadata, TableMetadata, get_table_data,
    get_table_metadata,
};

#[derive(Debug, Serialize)]
pub struct TableDataResultOutput {
    ok: bool,
    profile: String,
    entity: String,
    total_rows: Option<u64>,
    returned: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    executed_query: Option<String>,
    columns: Vec<tabular::ColumnOutput>,
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct TableMetadataResultOutput {
    ok: bool,
    profile: String,
    entity: String,
    field_count: usize,
    key_field_count: usize,
    fields: Vec<TableFieldMetadataOutput>,
}

#[derive(Debug, Serialize)]
struct TableFieldMetadataOutput {
    name: String,
    declared_type: String,
    is_key: bool,
    sap_type: Option<String>,
    col_type: Option<String>,
    length: Option<u32>,
    description: Option<String>,
}

pub async fn table_data(
    explicit_profile: Option<&str>,
    args: &TableDataArgs,
) -> Result<TableDataResultOutput, CommandError> {
    let options = table_options_from_args(args);
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = get_table_data(&mut client, &args.name, &options).await?;

    Ok(map_table_data_result(
        profile_name,
        &args.name,
        args.offset,
        args.limit,
        result,
    ))
}

pub async fn table_metadata(
    explicit_profile: Option<&str>,
    args: &TableMetadataArgs,
) -> Result<TableMetadataResultOutput, CommandError> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = get_table_metadata(&mut client, &args.name).await?;
    Ok(map_table_metadata_result(profile_name, result))
}

fn table_options_from_args(args: &TableDataArgs) -> TableDataOptions {
    TableDataOptions {
        fields: args
            .fields
            .as_deref()
            .into_iter()
            .flat_map(|fields| fields.split(','))
            .map(str::trim)
            .map(str::to_owned)
            .collect(),
        where_clause: args.where_clause.clone(),
        offset: args.offset,
        limit: args.limit,
    }
}

fn map_table_data_result(
    profile: String,
    requested_entity: &str,
    offset: usize,
    limit: usize,
    result: TableDataResult,
) -> TableDataResultOutput {
    let returned = result.rows.len();
    let page_end = offset.saturating_add(returned);
    let next_offset = result
        .total_rows
        .is_some_and(|total| u64::try_from(page_end).is_ok_and(|end| end < total))
        .then_some(page_end);

    TableDataResultOutput {
        ok: true,
        profile,
        entity: result
            .entity
            .unwrap_or_else(|| requested_entity.trim().to_ascii_uppercase()),
        total_rows: result.total_rows,
        returned,
        offset,
        limit,
        next_offset,
        executed_query: result.executed_query,
        columns: tabular::map_columns(result.columns),
        rows: result.rows,
    }
}

fn map_table_metadata_result(profile: String, result: TableMetadata) -> TableMetadataResultOutput {
    let fields: Vec<_> = result
        .fields
        .into_iter()
        .map(map_table_field_metadata)
        .collect();
    let key_field_count = fields.iter().filter(|field| field.is_key).count();

    TableMetadataResultOutput {
        ok: true,
        profile,
        entity: result.entity,
        field_count: fields.len(),
        key_field_count,
        fields,
    }
}

fn map_table_field_metadata(field: TableFieldMetadata) -> TableFieldMetadataOutput {
    TableFieldMetadataOutput {
        name: field.name,
        declared_type: field.declared_type,
        is_key: field.is_key,
        sap_type: field.sap_type,
        col_type: field.col_type,
        length: field.length,
        description: field.description,
    }
}

pub fn print_table_data(result: &TableDataResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_table_data_readable(result));
}

pub fn print_table_metadata(result: &TableMetadataResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_table_metadata_readable(result));
}

fn render_table_data_readable(result: &TableDataResultOutput) -> String {
    let mut output = String::new();
    let total = result
        .total_rows
        .map_or_else(|| "unknown".to_owned(), |total| total.to_string());
    let _ = writeln!(output, "entity: {}", result.entity);
    let _ = writeln!(
        output,
        "rows: {} of {} (offset {}, limit {})",
        result.returned, total, result.offset, result.limit
    );
    if let Some(query) = &result.executed_query {
        let _ = writeln!(output, "query: {query}");
    }
    output.push_str(&tabular::render_grid(&result.columns, &result.rows));
    output
}

fn render_table_metadata_readable(result: &TableMetadataResultOutput) -> String {
    let mut output = String::new();
    let _ = writeln!(output, "entity: {}", result.entity);
    let _ = writeln!(
        output,
        "fields: {} (key fields: {})",
        result.field_count, result.key_field_count
    );

    let columns = [
        metadata_column("KEY"),
        metadata_column("FIELD"),
        metadata_column("DECLARED TYPE"),
        metadata_column("SAP TYPE"),
        metadata_column("COLUMN TYPE"),
        metadata_column("LENGTH"),
        metadata_column("DESCRIPTION"),
    ];
    let rows: Vec<_> = result
        .fields
        .iter()
        .map(|field| {
            vec![
                if field.is_key { "yes" } else { "" }.to_owned(),
                field.name.clone(),
                field.declared_type.clone(),
                field.sap_type.clone().unwrap_or_default(),
                field.col_type.clone().unwrap_or_default(),
                field
                    .length
                    .map(|length| length.to_string())
                    .unwrap_or_default(),
                field.description.clone().unwrap_or_default(),
            ]
        })
        .collect();
    output.push_str(&tabular::render_grid(&columns, &rows));
    output
}

fn metadata_column(name: &str) -> tabular::ColumnOutput {
    tabular::ColumnOutput {
        name: name.to_owned(),
        sap_type: None,
        col_type: None,
        length: None,
        description: None,
    }
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, TableCommand};
    use fractal::sap::table::{TableColumn, TableFieldMetadata, TableMetadata};

    fn table_args(cli: Cli) -> TableDataArgs {
        let Command::Table {
            command: TableCommand::Data(args),
        } = cli.command
        else {
            panic!("expected table data command");
        };
        args
    }

    fn metadata_args(cli: Cli) -> TableMetadataArgs {
        let Command::Table {
            command: TableCommand::Metadata(args),
        } = cli.command
        else {
            panic!("expected table metadata command");
        };
        args
    }

    #[test]
    fn parses_table_data_options_without_a_full_query_escape_hatch() {
        let args = table_args(
            Cli::try_parse_from([
                "fractal",
                "table",
                "data",
                "ZDEMO_EVENT_LOG",
                "--fields",
                "EVENT_ID,STATUS",
                "--where",
                "STATUS = 'OPEN'",
                "--offset",
                "2",
                "--limit",
                "5",
            ])
            .unwrap(),
        );

        assert_eq!(args.name, "ZDEMO_EVENT_LOG");
        assert_eq!(args.fields.as_deref(), Some("EVENT_ID,STATUS"));
        assert_eq!(args.where_clause.as_deref(), Some("STATUS = 'OPEN'"));
        assert_eq!(args.offset, 2);
        assert_eq!(args.limit, 5);
        assert!(
            Cli::try_parse_from([
                "fractal",
                "table",
                "data",
                "ZDEMO_EVENT_LOG",
                "--query",
                "SELECT EVENT_ID FROM ZDEMO_EVENT_LOG",
            ])
            .is_err()
        );
    }

    #[test]
    fn splits_the_comma_separated_field_list() {
        let args = table_args(
            Cli::try_parse_from([
                "fractal",
                "table",
                "data",
                "ZDEMO_EVENT_LOG",
                "--fields",
                " EVENT_ID, STATUS ",
            ])
            .unwrap(),
        );
        let options = table_options_from_args(&args);

        assert_eq!(
            options.fields,
            vec!["EVENT_ID".to_owned(), "STATUS".to_owned()]
        );
        assert_eq!(options.where_clause, None);
    }

    #[test]
    fn parses_table_metadata_name_without_counting_options() {
        let args = metadata_args(
            Cli::try_parse_from(["fractal", "table", "metadata", "ZSAMPLE_RECORD"]).unwrap(),
        );

        assert_eq!(args.name, "ZSAMPLE_RECORD");
        assert!(
            Cli::try_parse_from(["fractal", "table", "metadata", "ZSAMPLE_RECORD", "--count",])
                .is_err()
        );
    }

    #[test]
    fn maps_table_results_and_paging_metadata() {
        let result = map_table_data_result(
            "development".to_owned(),
            "zdemo_event_log",
            2,
            2,
            TableDataResult {
                entity: Some("ZDEMO_EVENT_LOG".to_owned()),
                executed_query: None,
                total_rows: Some(10),
                columns: vec![TableColumn {
                    name: "EVENT_ID".to_owned(),
                    sap_type: Some("N".to_owned()),
                    col_type: Some("NUMC".to_owned()),
                    length: Some(10),
                    description: Some("Event ID".to_owned()),
                }],
                rows: vec![vec!["0000000003".to_owned()], vec!["0000000004".to_owned()]],
            },
        );

        assert!(result.ok);
        assert_eq!(result.entity, "ZDEMO_EVENT_LOG");
        assert_eq!(result.returned, 2);
        assert_eq!(result.next_offset, Some(4));
        assert_eq!(result.columns[0].col_type.as_deref(), Some("NUMC"));
    }

    #[test]
    fn renders_readable_table_data() {
        let result = map_table_data_result(
            "development".to_owned(),
            "ZDEMO_EVENT_LOG",
            0,
            1,
            TableDataResult {
                entity: Some("ZDEMO_EVENT_LOG".to_owned()),
                executed_query: None,
                total_rows: Some(1),
                columns: vec![TableColumn {
                    name: "EVENT_ID".to_owned(),
                    sap_type: None,
                    col_type: None,
                    length: None,
                    description: None,
                }],
                rows: vec![vec!["0000000001".to_owned()]],
            },
        );

        let rendered = render_table_data_readable(&result);
        assert!(rendered.contains("entity: ZDEMO_EVENT_LOG"));
        assert!(rendered.contains("rows: 1 of 1"));
        assert!(rendered.contains("EVENT_ID"));
        assert!(rendered.contains("0000000001"));
    }

    #[test]
    fn maps_and_renders_table_metadata() {
        let result = map_table_metadata_result(
            "development".to_owned(),
            TableMetadata {
                entity: "zsample_record".to_owned(),
                fields: vec![
                    TableFieldMetadata {
                        name: "client".to_owned(),
                        declared_type: "abap.clnt".to_owned(),
                        is_key: true,
                        sap_type: Some("C".to_owned()),
                        col_type: Some("CLNT".to_owned()),
                        length: Some(3),
                        description: Some("Client".to_owned()),
                    },
                    TableFieldMetadata {
                        name: "status".to_owned(),
                        declared_type: "zsample_status".to_owned(),
                        is_key: false,
                        sap_type: Some("C".to_owned()),
                        col_type: Some("CHAR".to_owned()),
                        length: Some(12),
                        description: Some("Status".to_owned()),
                    },
                ],
            },
        );

        assert!(result.ok);
        assert_eq!(result.entity, "zsample_record");
        assert_eq!(result.field_count, 2);
        assert_eq!(result.key_field_count, 1);
        assert_eq!(result.fields[0].declared_type, "abap.clnt");

        let rendered = render_table_metadata_readable(&result);
        assert!(rendered.contains("entity: zsample_record"));
        assert!(rendered.contains("fields: 2 (key fields: 1)"));
        assert!(rendered.contains("DECLARED TYPE"));
        assert!(rendered.contains("abap.clnt"));
        assert!(rendered.contains("zsample_status"));
        assert!(rendered.contains("Status"));

        let json = serde_json::to_value(&result).unwrap();
        assert_eq!(json["field_count"], 2);
        assert_eq!(json["key_field_count"], 1);
        assert_eq!(json["fields"][0]["is_key"], true);
        assert_eq!(json["fields"][1]["description"], "Status");
    }
}

use std::{fmt::Write as _, io::Read};

use serde::Serialize;

use crate::{
    cli::TableDataArgs,
    command_error::CommandError,
    commands::connect,
    output::{OutputFormat, print_result},
};
use fractal::sap::table::{
    TableColumn, TableDataOptions, TableDataQuery, TableDataResult, get_table_data,
};

const MAX_READABLE_CELL_WIDTH: usize = 40;

#[derive(Debug, Serialize)]
pub struct TableDataResultOutput {
    ok: bool,
    profile: String,
    entity: String,
    query_mode: String,
    total_rows: Option<u64>,
    returned: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    executed_query: Option<String>,
    columns: Vec<TableColumnOutput>,
    rows: Vec<Vec<String>>,
}

#[derive(Debug, Serialize)]
struct TableColumnOutput {
    name: String,
    sap_type: Option<String>,
    col_type: Option<String>,
    length: Option<u32>,
    description: Option<String>,
}

pub async fn table_data(
    explicit_profile: Option<&str>,
    args: &TableDataArgs,
) -> Result<TableDataResultOutput, CommandError> {
    let options = {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        table_options_from_args(args, &mut stdin)?
    };
    let query_mode = match options.query {
        TableDataQuery::Simple { .. } => "simple",
        TableDataQuery::Full(_) => "full",
    };
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = get_table_data(&mut client, &args.name, &options).await?;

    Ok(map_table_data_result(
        profile_name,
        &args.name,
        query_mode,
        args.offset,
        args.limit,
        result,
    ))
}

fn table_options_from_args<R: Read>(
    args: &TableDataArgs,
    reader: &mut R,
) -> Result<TableDataOptions, CommandError> {
    let query = match args.query.as_deref() {
        Some("-") => {
            let mut query = String::new();
            reader.read_to_string(&mut query)?;
            TableDataQuery::Full(query)
        }
        Some(query) => TableDataQuery::Full(query.to_owned()),
        None => TableDataQuery::Simple {
            fields: args
                .fields
                .as_deref()
                .into_iter()
                .flat_map(|fields| fields.split(','))
                .map(str::trim)
                .map(str::to_owned)
                .collect(),
            where_clause: args.where_clause.clone(),
        },
    };

    Ok(TableDataOptions {
        query,
        offset: args.offset,
        limit: args.limit,
    })
}

fn map_table_data_result(
    profile: String,
    requested_entity: &str,
    query_mode: &str,
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
        query_mode: query_mode.to_owned(),
        total_rows: result.total_rows,
        returned,
        offset,
        limit,
        next_offset,
        executed_query: result.executed_query,
        columns: result.columns.into_iter().map(map_column).collect(),
        rows: result.rows,
    }
}

fn map_column(column: TableColumn) -> TableColumnOutput {
    TableColumnOutput {
        name: column.name,
        sap_type: column.sap_type,
        col_type: column.col_type,
        length: column.length,
        description: column.description,
    }
}

pub fn print_table_data(result: &TableDataResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_table_data_readable(result));
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

    if result.columns.is_empty() {
        output.push_str("(no columns)\n");
        return output;
    }

    let rows: Vec<Vec<String>> = result
        .rows
        .iter()
        .map(|row| row.iter().map(|cell| normalize_cell(cell)).collect())
        .collect();
    let widths: Vec<usize> = result
        .columns
        .iter()
        .enumerate()
        .map(|(index, column)| {
            rows.iter()
                .filter_map(|row| row.get(index))
                .map(|cell| cell.chars().count())
                .chain(std::iter::once(column.name.chars().count()))
                .max()
                .unwrap_or(1)
                .clamp(1, MAX_READABLE_CELL_WIDTH)
        })
        .collect();
    let headers: Vec<String> = result
        .columns
        .iter()
        .map(|column| column.name.clone())
        .collect();
    write_readable_row(&mut output, &headers, &widths);
    let separators: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    write_readable_row(&mut output, &separators, &widths);
    for row in &rows {
        write_readable_row(&mut output, row, &widths);
    }

    output
}

fn normalize_cell(cell: &str) -> String {
    cell.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn write_readable_row(output: &mut String, cells: &[String], widths: &[usize]) {
    for (index, width) in widths.iter().copied().enumerate() {
        if index > 0 {
            output.push_str("  ");
        }
        let value = cells.get(index).map_or("", String::as_str);
        let value = truncate_cell(value, width);
        output.push_str(&value);
        output.push_str(&" ".repeat(width.saturating_sub(value.chars().count())));
    }
    output.push('\n');
}

fn truncate_cell(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width == 1 {
        return "…".to_owned();
    }

    value.chars().take(width - 1).chain(['…']).collect()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command, TableCommand};

    fn table_args(cli: Cli) -> TableDataArgs {
        let Command::Table {
            command: TableCommand::Data(args),
        } = cli.command
        else {
            panic!("expected table data command");
        };
        args
    }

    #[test]
    fn parses_simple_table_data_options() {
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
    }

    #[test]
    fn full_query_conflicts_with_simple_query_options() {
        let result = Cli::try_parse_from([
            "fractal",
            "table",
            "data",
            "ZDEMO_EVENT_LOG",
            "--fields",
            "EVENT_ID",
            "--query",
            "SELECT EVENT_ID FROM ZDEMO_EVENT_LOG",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn splits_fields_and_reads_a_full_query_from_stdin() {
        let simple_args = table_args(
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
        let simple = table_options_from_args(&simple_args, &mut Cursor::new("")).unwrap();
        assert_eq!(
            simple.query,
            TableDataQuery::Simple {
                fields: vec!["EVENT_ID".to_owned(), "STATUS".to_owned()],
                where_clause: None,
            }
        );

        let full_args = table_args(
            Cli::try_parse_from([
                "fractal",
                "table",
                "data",
                "ZDEMO_EVENT_LOG",
                "--query",
                "-",
            ])
            .unwrap(),
        );
        let full = table_options_from_args(
            &full_args,
            &mut Cursor::new("SELECT EVENT_ID FROM ZDEMO_EVENT_LOG\n"),
        )
        .unwrap();
        assert_eq!(
            full.query,
            TableDataQuery::Full("SELECT EVENT_ID FROM ZDEMO_EVENT_LOG\n".to_owned())
        );
    }

    #[test]
    fn maps_table_results_and_paging_metadata() {
        let result = map_table_data_result(
            "development".to_owned(),
            "zdemo_event_log",
            "simple",
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
        assert_eq!(result.query_mode, "simple");
        assert_eq!(result.returned, 2);
        assert_eq!(result.next_offset, Some(4));
        assert_eq!(result.columns[0].col_type.as_deref(), Some("NUMC"));
    }

    #[test]
    fn renders_readable_rows_and_normalizes_multiline_cells() {
        let result = map_table_data_result(
            "development".to_owned(),
            "ZDEMO_EVENT_LOG",
            "full",
            0,
            1,
            TableDataResult {
                entity: None,
                executed_query: Some("SELECT EVENT_ID, NOTE FROM ZDEMO_EVENT_LOG".to_owned()),
                total_rows: Some(1),
                columns: vec![
                    TableColumn {
                        name: "EVENT_ID".to_owned(),
                        sap_type: None,
                        col_type: None,
                        length: None,
                        description: None,
                    },
                    TableColumn {
                        name: "NOTE".to_owned(),
                        sap_type: None,
                        col_type: None,
                        length: None,
                        description: None,
                    },
                ],
                rows: vec![vec!["0000000001".to_owned(), "first\nsecond".to_owned()]],
            },
        );

        let rendered = render_table_data_readable(&result);
        assert!(rendered.contains("rows: 1 of 1"));
        assert!(rendered.contains("EVENT_ID"));
        assert!(rendered.contains("0000000001"));
        assert!(rendered.contains("first second"));
    }
}

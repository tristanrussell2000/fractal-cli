use std::{fmt::Write as _, io::Read};

use serde::Serialize;
use thiserror::Error;

use fractal::reportable_error::ReportableError;

use crate::{
    cli::QueryArgs,
    commands::{connect, tabular},
    output::{OutputFormat, print_result},
    reported::Reported,
};
use fractal::sap::table::{QueryOptions, TableDataResult, run_query};

#[derive(Debug, Serialize)]
pub struct QueryResultOutput {
    ok: bool,
    profile: String,
    query: String,
    executed_query: Option<String>,
    total_rows: Option<u64>,
    returned: usize,
    offset: usize,
    limit: usize,
    next_offset: Option<usize>,
    columns: Vec<tabular::ColumnOutput>,
    rows: Vec<Vec<String>>,
}

pub async fn query(
    explicit_profile: Option<&str>,
    args: &QueryArgs,
) -> Result<QueryResultOutput, Reported> {
    let query = {
        let stdin = std::io::stdin();
        let mut stdin = stdin.lock();
        resolve_query(&args.query, &mut stdin)?
    };
    let options = QueryOptions {
        offset: args.offset,
        limit: args.limit,
    };
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = run_query(&mut client, &query, &options).await?;

    Ok(map_query_result(
        profile_name,
        query,
        args.offset,
        args.limit,
        result,
    ))
}

/// A failure reading the statement the caller piped in.
#[derive(Debug, Error)]
#[error("could not read the query from stdin: {0}")]
pub struct QueryInputError(#[source] std::io::Error);

impl ReportableError for QueryInputError {
    fn code(&self) -> &'static str {
        "query_stdin_read_error"
    }

    fn hint(&self) -> Option<String> {
        Some(
            "Pipe one complete SELECT statement into the command, or pass it as the argument."
                .to_owned(),
        )
    }
}

fn resolve_query<R: Read>(value: &str, reader: &mut R) -> Result<String, QueryInputError> {
    if value != "-" {
        return Ok(value.to_owned());
    }

    let mut query = String::new();
    reader.read_to_string(&mut query).map_err(QueryInputError)?;
    Ok(query)
}

fn map_query_result(
    profile: String,
    query: String,
    offset: usize,
    limit: usize,
    result: TableDataResult,
) -> QueryResultOutput {
    let returned = result.rows.len();
    let page_end = offset.saturating_add(returned);
    let next_offset = result
        .total_rows
        .is_some_and(|total| u64::try_from(page_end).is_ok_and(|end| end < total))
        .then_some(page_end);

    QueryResultOutput {
        ok: true,
        profile,
        query,
        executed_query: result.executed_query,
        total_rows: result.total_rows,
        returned,
        offset,
        limit,
        next_offset,
        columns: tabular::map_columns(result.columns),
        rows: result.rows,
    }
}

pub fn print_query(result: &QueryResultOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_result(result, output);
        return;
    }

    print!("{}", render_query_readable(result));
}

fn render_query_readable(result: &QueryResultOutput) -> String {
    let mut output = String::new();
    let total = result
        .total_rows
        .map_or_else(|| "unknown".to_owned(), |total| total.to_string());
    let query = result.executed_query.as_ref().unwrap_or(&result.query);
    let _ = writeln!(output, "query: {query}");
    let _ = writeln!(
        output,
        "rows: {} of {} (offset {}, limit {})",
        result.returned, total, result.offset, result.limit
    );
    output.push_str(&tabular::render_grid(&result.columns, &result.rows));
    output
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use clap::Parser;

    use super::*;
    use crate::cli::{Cli, Command};
    use fractal::sap::table::TableColumn;

    fn query_args(cli: Cli) -> QueryArgs {
        let Command::Query(args) = cli.command else {
            panic!("expected query command");
        };
        args
    }

    #[test]
    fn parses_a_standalone_query_without_an_entity() {
        let args = query_args(
            Cli::try_parse_from([
                "fractal",
                "query",
                "SELECT EVENT_ID FROM ZDEMO_EVENT_LOG",
                "--offset",
                "2",
                "--limit",
                "5",
            ])
            .unwrap(),
        );

        assert_eq!(args.query, "SELECT EVENT_ID FROM ZDEMO_EVENT_LOG");
        assert_eq!(args.offset, 2);
        assert_eq!(args.limit, 5);
    }

    #[test]
    fn reads_the_query_from_stdin_for_a_dash_argument() {
        let args = query_args(Cli::try_parse_from(["fractal", "query", "-"]).unwrap());
        let query = resolve_query(
            &args.query,
            &mut Cursor::new("SELECT EVENT_ID FROM ZDEMO_EVENT_LOG\n"),
        )
        .unwrap();

        assert_eq!(query, "SELECT EVENT_ID FROM ZDEMO_EVENT_LOG\n");
    }

    #[test]
    fn maps_and_renders_a_query_result_without_an_entity() {
        let result = map_query_result(
            "development".to_owned(),
            "SELECT EVENT_ID FROM ZDEMO_EVENT_LOG".to_owned(),
            0,
            1,
            TableDataResult {
                entity: None,
                executed_query: Some("SELECT EVENT_ID FROM ZDEMO_EVENT_LOG".to_owned()),
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

        assert!(result.ok);
        assert_eq!(result.returned, 1);
        assert_eq!(result.next_offset, None);
        let rendered = render_query_readable(&result);
        assert!(rendered.contains("query: SELECT EVENT_ID"));
        assert!(rendered.contains("EVENT_ID"));
        assert!(rendered.contains("0000000001"));
    }
}

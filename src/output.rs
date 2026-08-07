use std::{fmt::Display, future::Future};

use clap::ValueEnum;
use serde::Serialize;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub(crate) enum OutputFormat {
    Json,
    Readable,
}

pub(crate) fn default_output_format() -> OutputFormat {
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        OutputFormat::Readable
    } else {
        OutputFormat::Json
    }
}

pub(crate) fn run_and_print<T, E, F>(operation: F, output: OutputFormat) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Result<T, E>,
{
    run_and_print_with(operation, print_result, output)
}

pub(crate) async fn run_and_print_async<T, E, F, Fut>(operation: F, output: OutputFormat) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
{
    run_and_print_with_async(operation, print_result, output).await
}

pub(crate) fn run_and_print_with<T, E, F, P>(operation: F, print: P, output: OutputFormat) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Result<T, E>,
    P: FnOnce(&T, OutputFormat),
{
    match operation() {
        Ok(result) => {
            print(&result, output);
            0
        }
        Err(error) => {
            print_error_message(&error.to_string(), output);
            2
        }
    }
}

async fn run_and_print_with_async<T, E, F, Fut, P>(
    operation: F,
    print: P,
    output: OutputFormat,
) -> i32
where
    T: Serialize,
    E: Display,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, E>>,
    P: FnOnce(&T, OutputFormat),
{
    match operation().await {
        Ok(result) => {
            print(&result, output);
            0
        }
        Err(error) => {
            print_error_message(&error.to_string(), output);
            2
        }
    }
}

pub(crate) fn print_result<T: Serialize>(result: &T, output: OutputFormat) {
    match output {
        OutputFormat::Json => print_json(result),
        OutputFormat::Readable => {
            // Commands without a dedicated readable renderer use structured JSON for now.
            print_json(result);
        }
    }
}

fn print_json<T: Serialize>(result: &T) {
    println!(
        "{}",
        serde_json::to_string_pretty(result).expect("result serializes")
    );
}

pub(crate) fn print_error_message(error: &str, output: OutputFormat) {
    let result = ErrorResult {
        ok: false,
        error: error.to_owned(),
    };
    match output {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Readable => eprintln!("error: {error}"),
    }
}

#[derive(Debug, Serialize)]
struct ErrorResult {
    ok: bool,
    error: String,
}

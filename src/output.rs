use std::future::Future;

use clap::ValueEnum;
use serde::Serialize;

use crate::command_error::CommandError;

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum OutputFormat {
    Json,
    Readable,
}

pub fn default_output_format() -> OutputFormat {
    if std::io::IsTerminal::is_terminal(&std::io::stdout()) {
        OutputFormat::Readable
    } else {
        OutputFormat::Json
    }
}

pub fn run_and_print<T, F>(operation: F, output: OutputFormat) -> i32
where
    T: Serialize,
    F: FnOnce() -> Result<T, CommandError>,
{
    run_and_print_with(operation, print_result, output)
}

pub async fn run_and_print_async<T, F, Fut>(operation: F, output: OutputFormat) -> i32
where
    T: Serialize,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, CommandError>>,
{
    run_and_print_with_async(operation, print_result, output).await
}

pub fn run_and_print_with<T, F, P>(operation: F, print: P, output: OutputFormat) -> i32
where
    T: Serialize,
    F: FnOnce() -> Result<T, CommandError>,
    P: FnOnce(&T, OutputFormat),
{
    match operation() {
        Ok(result) => {
            print(&result, output);
            0
        }
        Err(error) => {
            print_error(&error, output);
            2
        }
    }
}

pub async fn run_and_print_with_async<T, F, Fut, P>(
    operation: F,
    print: P,
    output: OutputFormat,
) -> i32
where
    T: Serialize,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, CommandError>>,
    P: FnOnce(&T, OutputFormat),
{
    match operation().await {
        Ok(result) => {
            print(&result, output);
            0
        }
        Err(error) => {
            print_error(&error, output);
            2
        }
    }
}

pub fn print_result<T: Serialize>(result: &T, output: OutputFormat) {
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

fn print_error(error: &CommandError, output: OutputFormat) {
    let result = ErrorResult {
        ok: false,
        code: error.code(),
        status: error.status(),
        message: error.message(),
        hint: error.hint(),
    };
    match output {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Readable => {
            eprintln!("error [{}]: {}", result.code, result.message);
            if let Some(hint) = result.hint {
                eprintln!("hint: {hint}");
            }
        }
    }
}

#[derive(Debug, Serialize)]
struct ErrorResult {
    ok: bool,
    code: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<u16>,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

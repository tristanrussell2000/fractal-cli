use std::future::Future;

use clap::ValueEnum;
use serde::Serialize;

use crate::reported::Reported;

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
    F: FnOnce() -> Result<T, Reported>,
{
    run_and_print_with(operation, print_result, output)
}

pub async fn run_and_print_async<T, F, Fut>(operation: F, output: OutputFormat) -> i32
where
    T: Serialize,
    F: FnOnce() -> Fut,
    Fut: Future<Output = Result<T, Reported>>,
{
    run_and_print_with_async(operation, print_result, output).await
}

pub fn run_and_print_with<T, F, P>(operation: F, print: P, output: OutputFormat) -> i32
where
    T: Serialize,
    F: FnOnce() -> Result<T, Reported>,
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
    Fut: Future<Output = Result<T, Reported>>,
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

fn print_error(error: &Reported, output: OutputFormat) {
    let result = ErrorResult {
        ok: false,
        code: error.code(),
        status: error.status(),
        message: error.message(),
        hint: error.hint(),
        suggested_command: error.suggested_command(),
    };
    match output {
        OutputFormat::Json => print_json(&result),
        OutputFormat::Readable => {
            eprintln!("error [{}]: {}", result.code, result.message);
            if let Some(hint) = result.hint {
                eprintln!("hint: {hint}");
            }
            if let Some(command) = result.suggested_command {
                eprintln!("try: {command}");
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
    #[serde(skip_serializing_if = "Option::is_none")]
    suggested_command: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::ErrorResult;

    #[test]
    fn omits_the_suggested_command_when_no_remedy_is_derivable() {
        // The field is additive: an existing consumer must not start seeing a
        // new key on error paths that have no runnable remedy.
        let json = serde_json::to_value(ErrorResult {
            ok: false,
            code: "patch_no_change",
            status: None,
            message: "the patch would not change the source".to_owned(),
            hint: Some("Use replacement text that differs from the anchor.".to_owned()),
            suggested_command: None,
        })
        .unwrap();

        assert_eq!(json["code"], "patch_no_change");
        assert!(json.get("suggested_command").is_none());
        assert!(json.get("status").is_none());
    }

    #[test]
    fn serializes_a_derivable_remedy_alongside_the_prose_hint() {
        let json = serde_json::to_value(ErrorResult {
            ok: false,
            code: "patch_anchor_not_found",
            status: None,
            message: "patch find text was not found in the source".to_owned(),
            hint: Some("Copy the exact anchor.".to_owned()),
            suggested_command: Some(
                "fractal edit read --type CLAS --name ZCL_SAMPLE --version inactive".to_owned(),
            ),
        })
        .unwrap();

        assert_eq!(
            json["suggested_command"],
            "fractal edit read --type CLAS --name ZCL_SAMPLE --version inactive"
        );
        assert_eq!(json["hint"], "Copy the exact anchor.");
    }
}

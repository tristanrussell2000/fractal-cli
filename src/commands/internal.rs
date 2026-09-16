use serde::Serialize;

use crate::{
    cli::ClassRunArgs,
    commands::connect,
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::sap::class_run::run_class;

#[derive(Debug, Serialize)]
pub struct ClassRunOutput {
    ok: bool,
    profile: String,
    class_name: String,
    elapsed_ms: u128,
    output_bytes: usize,
    output: String,
}

pub async fn class_run(
    explicit_profile: Option<&str>,
    args: &ClassRunArgs,
) -> Result<ClassRunOutput, Reported> {
    let (profile_name, _profile, mut client) = connect(explicit_profile).await?;
    let result = run_class(&mut client, &args.class).await?;

    Ok(ClassRunOutput {
        ok: true,
        profile: profile_name,
        class_name: result.class_name,
        elapsed_ms: result.elapsed.as_millis(),
        output_bytes: result.output.len(),
        output: result.output,
    })
}

pub fn print_class_run(result: &ClassRunOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }

    print!(
        "{} ran in {} ms, {} bytes of output\n\n{}",
        result.class_name, result.elapsed_ms, result.output_bytes, result.output
    );
}

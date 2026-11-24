use std::fmt::Write as _;

use axoupdater::{
    AxoUpdater, AxoupdateError, ReleaseSource, ReleaseSourceType, UpdateRequest, Version,
};
use serde::Serialize;
use thiserror::Error;

use crate::{
    cli::SelfUpdateArgs,
    output::{OutputFormat, print_json},
    reported::Reported,
};
use fractal::{reportable_error::ReportableError, suggested_command};

const APP_NAME: &str = "fractal";
const INSTALLER_SH: &str = "curl --proto '=https' --tlsv1.2 -LsSf https://github.com/{repo}/releases/latest/download/fractal-installer.sh | sh";
const INSTALLER_PS1: &str = "powershell -ExecutionPolicy Bypass -c \"irm https://github.com/{repo}/releases/latest/download/fractal-installer.ps1 | iex\"";

#[derive(Debug, Error)]
pub enum SelfUpdateError {
    #[error(
        "this copy of fractal ({exe}) was not installed by Fractal's installer, so it cannot update itself"
    )]
    Unmanaged {
        exe: String,
        /// Where the installer's copy lives, when there is one.
        installed_at: Option<String>,
    },
    #[error("there is no release {version}")]
    VersionNotFound { version: String },
    #[error("the installer failed{}: {output}", status.map_or_else(String::new, |status| format!(" with exit code {status}")))]
    InstallFailed { status: Option<i32>, output: String },
    #[error("could not reach GitHub to look up releases: {0}")]
    Network(String),
    #[error("could not update: {0}")]
    Other(String),
}

impl ReportableError for SelfUpdateError {
    fn code(&self) -> &'static str {
        match self {
            Self::Unmanaged { .. } => "self_update_unmanaged",
            Self::VersionNotFound { .. } => "self_update_version_not_found",
            Self::InstallFailed { .. } => "self_update_install_failed",
            Self::Network(_) => "network_error",
            Self::Other(_) => "self_update_failed",
        }
    }

    fn hint(&self) -> Option<String> {
        Some(match self {
            Self::Unmanaged { installed_at, .. } => {
                let install = installer_command();
                match installed_at {
                    Some(dir) => format!(
                        "The installer's copy is under {dir}; run that one, or reinstall this one with: {install}"
                    ),
                    None if cfg!(windows) => format!(
                        "An MSI install updates by running the latest MSI from the releases page. Or switch to the self-updating install: {install}"
                    ),
                    None => format!("Install with the installer to make it self-updating: {install}"),
                }
            }
            Self::VersionNotFound { .. } => {
                "`fractal self check` names the latest release; the releases page lists every one."
                    .to_owned()
            }
            Self::InstallFailed { .. } => {
                "Nothing was replaced if the installer stopped early; the copy you ran is still the one installed."
                    .to_owned()
            }
            Self::Network(_) => "Check the network connection to github.com.".to_owned(),
            Self::Other(_) => return None,
        })
    }

    fn suggested_command(&self) -> Option<String> {
        matches!(self, Self::VersionNotFound { .. }).then(suggested_command::self_check)
    }
}

impl From<AxoupdateError> for SelfUpdateError {
    fn from(error: AxoupdateError) -> Self {
        match error {
            AxoupdateError::VersionNotFound { version, .. } => Self::VersionNotFound { version },
            AxoupdateError::InstallFailed {
                status,
                stdout,
                stderr,
            } => Self::InstallFailed {
                status,
                output: [stdout, stderr]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join("\n")
                    .trim()
                    .to_owned(),
            },
            AxoupdateError::Reqwest(error) => Self::Network(error.to_string()),
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Serialize)]
pub struct SelfCheckOutput {
    ok: bool,
    current: String,
    latest: String,
    update_available: bool,
    /// Whether this copy came from the installer and so can update itself.
    self_updating: bool,
}

#[derive(Debug, Serialize)]
pub struct SelfUpdateOutput {
    ok: bool,
    updated: bool,
    from: String,
    to: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    tag: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    install_dir: Option<String>,
}

fn repository() -> &'static str {
    env!("CARGO_PKG_REPOSITORY").trim_start_matches("https://github.com/")
}

fn installer_command() -> String {
    let template = if cfg!(windows) {
        INSTALLER_PS1
    } else {
        INSTALLER_SH
    };
    template.replace("{repo}", repository())
}

fn current_version() -> Version {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("the crate version is valid semver")
}

/// An updater for this release, reading the receipt when this copy has one.
fn updater() -> Result<(AxoUpdater, bool), SelfUpdateError> {
    let mut updater = AxoUpdater::new_for(APP_NAME);
    let self_updating = updater.load_receipt().is_ok()
        && updater
            .check_receipt_is_for_this_executable()
            .unwrap_or(false);
    let (owner, name) = repository()
        .split_once('/')
        .expect("the repository is owner/name");
    updater.set_release_source(ReleaseSource {
        release_type: ReleaseSourceType::GitHub,
        owner: owner.to_owned(),
        name: name.to_owned(),
        app_name: APP_NAME.to_owned(),
    });
    // The receipt's version goes stale when a copy is rebuilt; the binary's own is the truth.
    updater
        .set_current_version(current_version())
        .map_err(SelfUpdateError::from)?;
    updater.disable_installer_output();
    Ok((updater, self_updating))
}

fn unmanaged(updater: &AxoUpdater) -> SelfUpdateError {
    SelfUpdateError::Unmanaged {
        exe: std::env::current_exe()
            .map(|path| path.display().to_string())
            .unwrap_or_default(),
        installed_at: updater
            .install_prefix_root()
            .ok()
            .map(|dir| dir.to_string()),
    }
}

pub async fn self_check() -> Result<SelfCheckOutput, Reported> {
    let (mut updater, self_updating) = updater()?;
    let latest = updater
        .query_new_version()
        .await
        .map_err(SelfUpdateError::from)?
        .cloned()
        .ok_or_else(|| SelfUpdateError::Other("GitHub listed no releases".to_owned()))?;
    let current = current_version();
    Ok(SelfCheckOutput {
        ok: true,
        update_available: latest > current,
        current: current.to_string(),
        latest: latest.to_string(),
        self_updating,
    })
}

pub async fn self_update(args: &SelfUpdateArgs) -> Result<SelfUpdateOutput, Reported> {
    let (mut updater, self_updating) = updater()?;
    if !self_updating {
        return Err(unmanaged(&updater).into());
    }
    if let Some(version) = &args.to {
        updater.configure_version_specifier(UpdateRequest::SpecificVersion(
            version.trim_start_matches('v').to_owned(),
        ));
    }
    let current = current_version().to_string();
    let result = updater.run().await.map_err(SelfUpdateError::from)?;
    Ok(match result {
        Some(result) => SelfUpdateOutput {
            ok: true,
            updated: true,
            from: current,
            to: result.new_version.to_string(),
            tag: Some(result.new_version_tag),
            install_dir: Some(result.install_prefix.to_string()),
        },
        None => SelfUpdateOutput {
            ok: true,
            updated: false,
            to: current.clone(),
            from: current,
            tag: None,
            install_dir: None,
        },
    })
}

pub fn print_self_check(result: &SelfCheckOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    let mut text = format!("fractal {}, latest {}", result.current, result.latest);
    if result.update_available {
        text.push_str(if result.self_updating {
            ": run `fractal self update`"
        } else {
            ": this copy was not installed by the installer, so it cannot update itself"
        });
    } else {
        text.push_str(": up to date");
    }
    println!("{text}");
}

pub fn print_self_update(result: &SelfUpdateOutput, output: OutputFormat) {
    if matches!(output, OutputFormat::Json) {
        print_json(result);
        return;
    }
    let mut text = String::new();
    if result.updated {
        let _ = write!(text, "updated fractal {} -> {}", result.from, result.to);
        if let Some(dir) = &result.install_dir {
            let _ = write!(text, " in {dir}");
        }
    } else {
        let _ = write!(text, "fractal {} is up to date", result.from);
    }
    println!("{text}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_repository_is_owner_and_name() {
        assert_eq!(repository().split('/').count(), 2);
        assert!(installer_command().contains(repository()));
    }

    #[test]
    fn library_errors_map_to_their_codes() {
        let not_found = SelfUpdateError::from(AxoupdateError::VersionNotFound {
            name: "fractal-cli".to_owned(),
            app_name: "fractal".to_owned(),
            version: "9.9.9".to_owned(),
        });
        assert_eq!(not_found.code(), "self_update_version_not_found");
        assert_eq!(
            not_found.suggested_command().as_deref(),
            Some("fractal self check")
        );
        let failed = SelfUpdateError::from(AxoupdateError::InstallFailed {
            status: Some(1),
            stdout: Some("downloading".to_owned()),
            stderr: Some("checksum mismatch\n".to_owned()),
        });
        assert_eq!(failed.code(), "self_update_install_failed");
        assert!(
            failed
                .to_string()
                .contains("exit code 1: downloading\nchecksum mismatch")
        );
    }

    #[test]
    fn an_unmanaged_copy_is_pointed_at_the_installed_one() {
        let error = SelfUpdateError::Unmanaged {
            exe: "/src/fractal/target/release/fractal".to_owned(),
            installed_at: Some("/home/dev/.cargo".to_owned()),
        };
        assert_eq!(error.code(), "self_update_unmanaged");
        assert!(
            error
                .hint()
                .unwrap()
                .starts_with("The installer's copy is under /home/dev/.cargo")
        );
    }
}

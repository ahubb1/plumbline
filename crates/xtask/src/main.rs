//! Workspace automation for Plumbline. Run as `cargo run -p xtask -- <command>`.
//!
//! Nothing here opens a window or starts the app; every command is a headless build step.

mod checkdll;
mod dist;
mod fetch;
mod licenses;
mod manifest;
mod pe;
mod release;
mod util;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "xtask", about = "Plumbline workspace tasks")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Download the pinned PDFium build, verify its SHA-256 and unpack it to vendor/pdfium/8125.
    FetchPdfium {
        /// Unpack again even when a valid copy is already there.
        #[arg(long)]
        force: bool,
    },
    /// Assert that a DLL imports only Windows system DLLs (default: the vendored pdfium.dll).
    CheckDll {
        /// PE file to check.
        file: Option<PathBuf>,
        /// Refuse only Visual C++ redistributable and dynamic CRT imports (the rule for our
        /// own static-CRT exes).
        #[arg(long)]
        no_vc_redist_only: bool,
        /// Do not run dumpbin even if the MSVC Build Tools are installed.
        #[arg(long)]
        no_dumpbin: bool,
    },
    /// Build the release exes and assemble the portable folder target/dist/plumbline.
    Dist {
        /// Package what is already in target/release instead of building.
        #[arg(long)]
        no_build: bool,
        /// Also write target/dist/plumbline.zip.
        #[arg(long)]
        zip: bool,
    },
    /// Run dist's release build, then build the single-file launcher in pack/ around it and
    /// copy it to target/dist/Plumbline.exe.
    Pack,
    /// List the declared licence of every dependency (from cargo metadata).
    Licenses {
        /// Write the listing to this file instead of printing it.
        #[arg(long)]
        out: Option<PathBuf>,
    },
}

fn main() -> ExitCode {
    let result = match Cli::parse().command {
        Cmd::FetchPdfium { force } => fetch::run(force),
        Cmd::CheckDll {
            file,
            no_vc_redist_only,
            no_dumpbin,
        } => {
            let file = file.unwrap_or_else(|| fetch::Layout::in_workspace().dll());
            let policy = if no_vc_redist_only {
                checkdll::Policy::NoVcRedist
            } else {
                checkdll::Policy::SystemOnly
            };
            checkdll::run(&file, policy, !no_dumpbin)
        }
        Cmd::Dist { no_build, zip } => dist::run(&dist::Options { no_build, zip }).map(|_| ()),
        Cmd::Pack => release::pack(),
        Cmd::Licenses { out } => licenses::run(out.as_deref()),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("error: {err:#}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn subcommand_names_match_the_documented_ones() {
        let cmd = Cli::command();
        let names: Vec<&str> = cmd.get_subcommands().map(|c| c.get_name()).collect();
        assert_eq!(
            names,
            ["fetch-pdfium", "check-dll", "dist", "pack", "licenses"]
        );
    }

    #[test]
    fn parses_flags() {
        assert!(matches!(
            Cli::parse_from(["xtask", "fetch-pdfium", "--force"]).command,
            Cmd::FetchPdfium { force: true }
        ));
        assert!(matches!(
            Cli::parse_from(["xtask", "dist", "--no-build"]).command,
            Cmd::Dist {
                no_build: true,
                zip: false
            }
        ));
        assert!(Cli::try_parse_from(["xtask", "nonsense"]).is_err());
    }
}

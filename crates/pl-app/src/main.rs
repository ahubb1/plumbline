//! `plumbline.exe`: the window. Any arguments are PDF files to open.

// No console window behind the app in release builds.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::path::PathBuf;

fn main() -> std::process::ExitCode {
    let files: Vec<PathBuf> = std::env::args_os().skip(1).map(PathBuf::from).collect();
    match pl_app::shell::run(files) {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(message) => {
            eprintln!("plumbline: {message}");
            std::process::ExitCode::FAILURE
        }
    }
}

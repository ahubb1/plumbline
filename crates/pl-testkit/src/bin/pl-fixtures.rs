//! Writes every pl-testkit fixture to a directory.
//!
//! ```text
//! pl-fixtures [OUT_DIR] [--dense N] [--streams K] [--raster WxH] [--small]
//! ```
//! `OUT_DIR` defaults to `<workspace>/target/fixtures`.

use pl_testkit::{default_output_dir, write_fixtures, Options};
use std::path::PathBuf;
use std::process::ExitCode;

fn parse_args() -> Result<(PathBuf, Options), String> {
    let mut dir = None;
    let mut options = Options::default();
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        let mut value = |flag: &str| args.next().ok_or_else(|| format!("{flag} needs a value"));
        match arg.as_str() {
            "--small" => options = Options::small(),
            "--dense" => {
                options.dense_segments = value("--dense")?.parse().map_err(|e| format!("--dense: {e}"))?;
            }
            "--streams" => {
                options.dense_streams = value("--streams")?.parse().map_err(|e| format!("--streams: {e}"))?;
            }
            "--raster" => {
                let v = value("--raster")?;
                let (w, h) = v.split_once('x').ok_or("--raster expects WxH, e.g. 2550x3300")?;
                options.raster_px = [
                    w.parse().map_err(|e| format!("--raster width: {e}"))?,
                    h.parse().map_err(|e| format!("--raster height: {e}"))?,
                ];
            }
            "-h" | "--help" => {
                return Err("usage: pl-fixtures [OUT_DIR] [--dense N] [--streams K] [--raster WxH] [--small]".into());
            }
            other if other.starts_with('-') => return Err(format!("unknown option {other}")),
            other => {
                if dir.replace(PathBuf::from(other)).is_some() {
                    return Err("only one output directory may be given".into());
                }
            }
        }
    }
    if options.raster_px[0] < 20 || options.raster_px[1] < 20 {
        return Err("--raster must be at least 20x20".into());
    }
    Ok((dir.unwrap_or_else(default_output_dir), options))
}

fn main() -> ExitCode {
    let (dir, options) = match parse_args() {
        Ok(v) => v,
        Err(message) => {
            eprintln!("{message}");
            return ExitCode::from(2);
        }
    };
    let fixtures = pl_testkit::all_fixtures_with(&options);
    match write_fixtures(&dir, &fixtures) {
        Ok(paths) => {
            for (path, fx) in paths.iter().zip(&fixtures) {
                println!(
                    "{:>10} bytes  {:>3} page(s)  {:>7} segment(s)  {}",
                    fx.bytes.len(),
                    fx.manifest.page_count(),
                    fx.manifest.total_segments(),
                    path.display()
                );
            }
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("cannot write fixtures to {}: {error}", dir.display());
            ExitCode::FAILURE
        }
    }
}

//! Release builds that do not carry the builder's folders, the check that proves it, and the
//! `pack` task that builds the single-file `Plumbline.exe`.
//!
//! rustc writes source paths into a binary (panic locations, `file!()`). For registry crates
//! those lie under `%USERPROFILE%\.cargo\registry\src`, which names the local account. The
//! release builds run by `dist` and `pack` therefore remap the home folder, CARGO_HOME,
//! RUSTUP_HOME and the workspace root with `--remap-path-prefix`, and every exe they produce is
//! scanned for the builder's home folder (its `Users\<name>` tail), and for CARGO_HOME and
//! RUSTUP_HOME when they lie elsewhere, as UTF-8 and UTF-16LE, with either slash or doubled
//! backslashes, in any ASCII case.
//! `pdfium.dll` is a third-party binary and is not scanned.
//!
//! The flags go in `CARGO_ENCODED_RUSTFLAGS`, which replaces `target.<triple>.rustflags` from
//! `.cargo/config.toml`, so the static-CRT flag set there is passed again here.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::checkdll::{self, Policy};
use crate::dist;
use crate::util;

/// The flag `.cargo/config.toml` (and `pack/.cargo/config.toml`) set for the target.
pub const CRT_STATIC: [&str; 2] = ["-C", "target-feature=+crt-static"];

/// One `--remap-path-prefix FROM=TO`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Remap {
    pub from: PathBuf,
    pub to: &'static str,
}

/// The builder's home folder: `USERPROFILE`, else `HOME`.
pub fn home_dir() -> Result<PathBuf> {
    ["USERPROFILE", "HOME"]
        .iter()
        .filter_map(std::env::var_os)
        .map(PathBuf::from)
        .find(|p| !p.as_os_str().is_empty())
        .context("neither USERPROFILE nor HOME is set; cannot tell which folder to keep out of the build")
}

/// The remaps for a build of `workspace`, given the home folder and the `CARGO_HOME` and
/// `RUSTUP_HOME` values (defaults `<home>\.cargo` and `<home>\.rustup`). rustc applies the
/// last remap that matches, so the home folder comes first and the folders inside it after.
pub fn remaps_for(
    home: &Path,
    cargo_home: Option<PathBuf>,
    rustup_home: Option<PathBuf>,
    workspace: &Path,
) -> Vec<Remap> {
    vec![
        Remap {
            from: home.to_path_buf(),
            to: "/home",
        },
        Remap {
            from: cargo_home.unwrap_or_else(|| home.join(".cargo")),
            to: "/cargo",
        },
        Remap {
            from: rustup_home.unwrap_or_else(|| home.join(".rustup")),
            to: "/rustup",
        },
        Remap {
            from: workspace.to_path_buf(),
            to: "/plumbline",
        },
    ]
}

/// A folder named by an environment variable, if it is set and not empty.
fn env_dir(name: &str) -> Option<PathBuf> {
    std::env::var_os(name)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// [`remaps_for`] with the values of this process's environment.
pub fn remaps(workspace: &Path) -> Result<Vec<Remap>> {
    Ok(remaps_for(
        &home_dir()?,
        env_dir("CARGO_HOME"),
        env_dir("RUSTUP_HOME"),
        workspace,
    ))
}

/// `CARGO_ENCODED_RUSTFLAGS` (arguments separated by 0x1F): the static-CRT flag, then one
/// `--remap-path-prefix` per remap.
pub fn encoded_rustflags(remaps: &[Remap]) -> String {
    let mut args: Vec<String> = CRT_STATIC.iter().map(|s| s.to_string()).collect();
    for remap in remaps {
        args.push("--remap-path-prefix".into());
        args.push(format!("{}={}", remap.from.display(), remap.to));
    }
    args.join("\x1f")
}

/// Sets up `cmd` (a `cargo build --release`) to build without the builder's folders in the
/// output. `RUSTFLAGS` is removed so it cannot be mistaken for what is used.
pub fn remap_build(cmd: &mut Command, workspace: &Path) -> Result<()> {
    cmd.env(
        "CARGO_ENCODED_RUSTFLAGS",
        encoded_rustflags(&remaps(workspace)?),
    )
    .env_remove("RUSTFLAGS");
    Ok(())
}

/// Adds `text` to `out` with backslashes, doubled backslashes (as escaped in source text) and
/// forward slashes, each as UTF-8 and as UTF-16LE, without repeats.
fn push_forms(out: &mut Vec<Vec<u8>>, text: &str) {
    let back = text.replace('/', "\\");
    for form in [back.replace('\\', "\\\\"), back, text.replace('\\', "/")] {
        let utf16: Vec<u8> = form.encode_utf16().flat_map(u16::to_le_bytes).collect();
        for bytes in [form.as_bytes().to_vec(), utf16] {
            if !out.contains(&bytes) {
                out.push(bytes);
            }
        }
    }
}

/// The part of `path` that names the account: its last two components (`Users\<name>`), so
/// that the name is also found in a path that reaches it from another root; the whole path
/// when it has fewer components. A trailing separator is dropped.
fn account_tail(path: &Path) -> String {
    let names: Vec<String> = path
        .components()
        .filter_map(|c| match c {
            std::path::Component::Normal(n) => Some(n.to_string_lossy().into_owned()),
            _ => None,
        })
        .collect();
    if names.len() >= 2 {
        format!("{}\\{}", names[names.len() - 2], names[names.len() - 1])
    } else {
        path.to_string_lossy()
            .trim_end_matches(['\\', '/'])
            .to_string()
    }
}

/// Byte patterns that betray `home` in a binary: its account tail (see [`account_tail`]), and
/// the full path of each folder in `extra` (CARGO_HOME, RUSTUP_HOME) that lies outside `home`,
/// each with backslashes and with forward slashes, as UTF-8 and as UTF-16LE.
pub fn needles(home: &Path, extra: &[PathBuf]) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    push_forms(&mut out, &account_tail(home));
    for dir in extra.iter().filter(|d| !d.starts_with(home)) {
        push_forms(
            &mut out,
            dir.to_string_lossy().trim_end_matches(['\\', '/']),
        );
    }
    out
}

/// Number of places in `hay` where `needle` occurs, ignoring ASCII case.
fn count_ci(hay: &[u8], needle: &[u8]) -> usize {
    let Some(&first) = needle.first() else {
        return 0;
    };
    let first = first.to_ascii_lowercase();
    let mut count = 0;
    let mut i = 0;
    while i + needle.len() <= hay.len() {
        if hay[i].to_ascii_lowercase() == first
            && hay[i..i + needle.len()].eq_ignore_ascii_case(needle)
        {
            count += 1;
            i += needle.len();
        } else {
            i += 1;
        }
    }
    count
}

/// How often `home` (or a folder of `extra` outside it) occurs in `bytes`, in any of the
/// forms of [`needles`].
pub fn home_hits(bytes: &[u8], home: &Path, extra: &[PathBuf]) -> usize {
    needles(home, extra)
        .iter()
        .map(|n| count_ci(bytes, n))
        .sum()
}

/// A file's name alone, for messages: its full path may lie inside the home folder.
fn file_name(file: &Path) -> String {
    file.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "(unnamed file)".to_string())
}

/// Fails, naming each file and its count, when any of `files` contains `home` or a folder of
/// `extra` outside it. Neither the folders nor the files' full paths are printed: the message
/// may end up in a log, and the files may lie inside the home folder.
pub fn check_no_home(files: &[PathBuf], home: &Path, extra: &[PathBuf]) -> Result<()> {
    if account_tail(home).len() < 4 {
        bail!("the home folder is too short to scan for");
    }
    let mut bad = Vec::new();
    for file in files {
        let bytes =
            std::fs::read(file).with_context(|| format!("cannot read {}", file_name(file)))?;
        let hits = home_hits(&bytes, home, extra);
        if hits > 0 {
            bad.push(format!("{}: {hits}", file_name(file)));
        }
    }
    if !bad.is_empty() {
        bail!(
            "the builder's home folder path is embedded in the release build (copies per file):\n  {}\n\
             Build with `cargo run -p xtask -- dist` or `pack`, which remap it.",
            bad.join("\n  ")
        );
    }
    Ok(())
}

/// [`check_no_home`] with the builder's home folder and the CARGO_HOME and RUSTUP_HOME of the
/// build; prints what was checked, by file name.
pub fn scan(files: &[PathBuf]) -> Result<()> {
    let home = home_dir()?;
    let extra: Vec<PathBuf> =
        remaps_for(&home, env_dir("CARGO_HOME"), env_dir("RUSTUP_HOME"), &home)
            .into_iter()
            .filter(|r| r.to == "/cargo" || r.to == "/rustup")
            .map(|r| r.from)
            .collect();
    check_no_home(files, &home, &extra)?;
    for file in files {
        println!("path scan: {} holds no home-folder path", file_name(file));
    }
    Ok(())
}

/// The files `pack/build.rs` embeds, all taken from the portable folder.
pub const PAYLOAD: [&str; 3] = ["plumbline.exe", "pdfium.dll", "THIRD-PARTY-LICENSES.txt"];

/// Copies the payload files from the portable folder `dist_dir` into `payload_dir`, replacing
/// what was there.
pub fn stage_payload(dist_dir: &Path, payload_dir: &Path) -> Result<()> {
    let missing: Vec<&str> = PAYLOAD
        .iter()
        .copied()
        .filter(|name| !dist_dir.join(name).is_file())
        .collect();
    if !missing.is_empty() {
        bail!("{} lacks {}", dist_dir.display(), missing.join(", "));
    }
    if payload_dir.exists() {
        std::fs::remove_dir_all(payload_dir)
            .with_context(|| format!("cannot clear {}", payload_dir.display()))?;
    }
    std::fs::create_dir_all(payload_dir)
        .with_context(|| format!("cannot create {}", payload_dir.display()))?;
    for name in PAYLOAD {
        std::fs::copy(dist_dir.join(name), payload_dir.join(name))
            .with_context(|| format!("cannot copy {name} into {}", payload_dir.display()))?;
    }
    Ok(())
}

/// The `pack` subcommand: `dist`'s release build, then the single-file launcher in `pack/`
/// built around it, checked, and copied to `target/dist/Plumbline.exe`.
pub fn pack() -> Result<()> {
    let root = util::workspace_root();
    let dist_dir = dist::run(&dist::Options {
        no_build: false,
        zip: false,
    })?;
    let pack_dir = root.join("pack");
    stage_payload(&dist_dir, &pack_dir.join("payload"))?;

    let target_dir = pack_dir.join("target");
    let mut build = util::cargo();
    build
        .args(["build", "--release", "--target-dir"])
        .arg(&target_dir)
        .current_dir(&pack_dir);
    // pack/ is inside the workspace root, so that remap covers its sources too.
    remap_build(&mut build, &root)?;
    util::run(&mut build).context("release build of pack/ failed")?;

    let exe = target_dir.join("release").join("Plumbline.exe");
    if !exe.is_file() {
        bail!("{} was not built", exe.display());
    }
    checkdll::check_file(&exe, Policy::NoVcRedist, true)
        .context("Plumbline.exe is not a static-CRT build")?;
    crate::manifest::check_file(&exe)?;
    scan(std::slice::from_ref(&exe))?;

    let dist_root = dist_dir
        .parent()
        .context("the portable folder has no parent")?;
    let out = dist_root.join("Plumbline.exe");
    std::fs::copy(&exe, &out).with_context(|| format!("cannot copy to {}", out.display()))?;
    let size = std::fs::metadata(&out)?.len();
    println!("single exe: {}", out.display());
    println!(
        "  size    {size} bytes ({:.1} MiB)",
        size as f64 / (1024.0 * 1024.0)
    );
    println!("  SHA-256 {}", util::sha256_file(&out)?);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TestDir;

    /// A home folder made up for the tests (not a real account).
    fn fake_home() -> PathBuf {
        PathBuf::from(r"D:\Homes\builder")
    }

    #[test]
    fn remaps_default_to_folders_in_home_and_put_home_first() {
        let home = fake_home();
        let ws = PathBuf::from(r"E:\src\plumbline");
        let remaps = remaps_for(&home, None, None, &ws);
        let pairs: Vec<(PathBuf, &str)> = remaps.iter().map(|r| (r.from.clone(), r.to)).collect();
        assert_eq!(
            pairs,
            [
                (home.clone(), "/home"),
                (home.join(".cargo"), "/cargo"),
                (home.join(".rustup"), "/rustup"),
                (ws.clone(), "/plumbline"),
            ]
        );
        let custom = remaps_for(
            &home,
            Some("F:\\cargo".into()),
            Some("F:\\rustup".into()),
            &ws,
        );
        assert_eq!(custom[1].from, PathBuf::from("F:\\cargo"));
        assert_eq!(custom[2].from, PathBuf::from("F:\\rustup"));
    }

    #[test]
    fn encoded_rustflags_keep_crt_static_and_carry_each_remap() {
        let ws = PathBuf::from(r"E:\my src\plumbline");
        let flags = encoded_rustflags(&remaps_for(&fake_home(), None, None, &ws));
        let args: Vec<&str> = flags.split('\x1f').collect();
        assert_eq!(&args[..2], CRT_STATIC);
        assert_eq!(args.len(), 2 + 2 * 4);
        assert_eq!(args[2], "--remap-path-prefix");
        assert_eq!(args[3], r"D:\Homes\builder=/home");
        assert_eq!(args[5], r"D:\Homes\builder\.cargo=/cargo");
        // A path with a space stays one argument.
        assert_eq!(args[9], r"E:\my src\plumbline=/plumbline");
    }

    #[test]
    fn home_is_found_in_utf8_utf16_either_slash_and_any_case() {
        let home = fake_home();
        let mut bin = b"\0\0panic at ".to_vec();
        bin.extend_from_slice(br"d:\homes\BUILDER\.cargo\registry\src\x.rs");
        bin.extend_from_slice(b"\0junk\0");
        bin.extend_from_slice(b"D:/Homes/builder/.cargo/x.rs\0");
        let wide: Vec<u8> = r"D:\Homes\builder\AppData"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        bin.extend_from_slice(&wide);
        assert_eq!(home_hits(&bin, &home, &[]), 3);
        assert_eq!(
            home_hits(b"/cargo/registry/src/x.rs D:\\Homes\\build", &home, &[]),
            0
        );
        // A trailing separator on the home value changes nothing.
        assert_eq!(home_hits(&bin, Path::new(r"D:\Homes\builder\"), &[]), 3);
        // The account is also found under another root and with doubled backslashes.
        assert_eq!(home_hits(br"\\?\D:\HOMES\builder\x", &home, &[]), 1);
        assert_eq!(home_hits(br#""D:\\Homes\\builder\\x""#, &home, &[]), 1);
    }

    #[test]
    fn cargo_and_rustup_homes_outside_home_are_scanned_too() {
        let home = fake_home();
        let extra = [PathBuf::from(r"F:\Tools\cargo"), home.join(".rustup")];
        let bin = b"x F:/tools/cargo/registry/src/a.rs y";
        assert_eq!(home_hits(bin, &home, &extra), 1);
        assert_eq!(home_hits(bin, &home, &[]), 0);
        // A folder inside home adds no needle of its own: the home tail covers it.
        assert_eq!(needles(&home, &extra[1..]), needles(&home, &[]));
    }

    #[test]
    fn check_no_home_fails_naming_the_file_and_passes_clean_files() {
        let dir = TestDir::new("scan");
        let clean = dir.path().join("clean.exe");
        let dirty = dir.path().join("dirty.exe");
        std::fs::write(&clean, b"MZ /cargo/registry/src/a.rs").unwrap();
        std::fs::write(&dirty, b"MZ D:\\Homes\\builder\\.cargo\\a.rs").unwrap();
        check_no_home(std::slice::from_ref(&clean), &fake_home(), &[]).unwrap();
        let err = check_no_home(&[clean, dirty], &fake_home(), &[])
            .unwrap_err()
            .to_string();
        assert!(err.contains("dirty.exe: 1"), "{err}");
        assert!(!err.contains("clean.exe"), "{err}");
        assert!(
            !err.contains("Homes"),
            "the message prints the home folder: {err}"
        );
        // Files are named without their folder, which may lie inside the home folder.
        let folder = dir.path().to_string_lossy().into_owned();
        assert!(
            !err.contains(&folder),
            "the message prints the file's folder: {err}"
        );
    }

    #[test]
    fn stage_payload_copies_the_three_files_and_clears_old_ones() {
        let dir = TestDir::new("payload");
        let dist_dir = dir.path().join("dist");
        let payload = dir.path().join("payload");
        std::fs::create_dir_all(&dist_dir).unwrap();
        std::fs::create_dir_all(&payload).unwrap();
        std::fs::write(payload.join("stale.bin"), b"old").unwrap();
        for name in PAYLOAD {
            std::fs::write(dist_dir.join(name), name.as_bytes()).unwrap();
        }
        std::fs::write(dist_dir.join("plumb.exe"), b"cli").unwrap();
        stage_payload(&dist_dir, &payload).unwrap();
        let mut names: Vec<String> = std::fs::read_dir(&payload)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(
            names,
            ["THIRD-PARTY-LICENSES.txt", "pdfium.dll", "plumbline.exe"]
        );
        assert_eq!(
            std::fs::read(payload.join("pdfium.dll")).unwrap(),
            b"pdfium.dll"
        );

        std::fs::remove_file(dist_dir.join("pdfium.dll")).unwrap();
        let err = stage_payload(&dist_dir, &payload).unwrap_err().to_string();
        assert!(err.contains("pdfium.dll"), "{err}");
    }
}

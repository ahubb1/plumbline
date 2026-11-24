//! `dist`: builds the release exes and assembles the portable folder.
//!
//! Layout of `target/dist/plumbline/`:
//!
//! ```text
//! plumbline.exe  plumb.exe  pdfium.dll      (the DLL sits beside the exes; plan section 7)
//! THIRD-PARTY-LICENSES.txt                  (every third-party licence text, in one file)
//! fonts/LiberationSans-*.ttf
//! licenses/fonts/LICENSE                    (SIL OFL 1.1, unmodified)
//! licenses/pdfium/LICENSE                   (pdfium-binaries packaging)
//! licenses/pdfium/third_party/*             (PDFium and its bundled libraries)
//! licenses/plumbline/*                      (the project's own licence files, when present)
//! ```
//!
//! The release build passes `--remap-path-prefix` for the builder's folders and the exes are
//! scanned for the home folder afterwards (see `release.rs`).

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::checkdll::{self, Policy};
use crate::fetch::{self, Layout, PDFIUM_8125};
use crate::licenses::{self, RULE};
use crate::release;
use crate::util::{self, system_tool};

pub const EXES: &[&str] = &["plumbline.exe", "plumb.exe"];
pub const DIST_NAME: &str = "plumbline";

/// Everything `assemble` copies from.
#[derive(Debug, Clone)]
pub struct Inputs {
    /// Directory holding the release exes (`target/release`).
    pub release_dir: PathBuf,
    /// Unpacked PDFium release (`vendor/pdfium/8125`).
    pub pdfium_dir: PathBuf,
    /// `vendor/fonts`: the `.ttf` files and their `LICENSE`.
    pub fonts_dir: PathBuf,
    /// Workspace root, searched for the project's own `LICENSE*` files.
    pub project_root: PathBuf,
    /// Text of `THIRD-PARTY-LICENSES.txt`.
    pub third_party: String,
}

fn copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("cannot create {}", parent.display()))?;
    }
    std::fs::copy(from, to)
        .with_context(|| format!("cannot copy {} to {}", from.display(), to.display()))?;
    Ok(())
}

/// Files directly inside `dir` that satisfy `keep`, sorted by name.
fn files_in(dir: &Path, keep: impl Fn(&str) -> bool) -> Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for entry in std::fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))? {
        let path = entry?.path();
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or_default();
        if path.is_file() && keep(name) {
            out.push(path);
        }
    }
    out.sort();
    Ok(out)
}

/// Writes the portable folder into `out_dir`, replacing whatever was there. Returns the paths
/// written, relative to `out_dir`, with forward slashes, sorted.
pub fn assemble(inputs: &Inputs, out_dir: &Path) -> Result<Vec<String>> {
    // Check every source first so a missing input does not leave a half-built folder.
    let mut plan: Vec<(PathBuf, String)> = Vec::new();
    for exe in EXES {
        plan.push((inputs.release_dir.join(exe), exe.to_string()));
    }
    plan.push((
        inputs.pdfium_dir.join("bin").join("pdfium.dll"),
        "pdfium.dll".into(),
    ));
    plan.push((
        inputs.pdfium_dir.join("LICENSE"),
        "licenses/pdfium/LICENSE".into(),
    ));
    let pdfium_notices = files_in(&inputs.pdfium_dir.join("licenses"), |_| true)?;
    if pdfium_notices.is_empty() {
        bail!(
            "{} holds no third-party notices",
            inputs.pdfium_dir.join("licenses").display()
        );
    }
    for path in pdfium_notices {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        plan.push((path, format!("licenses/pdfium/third_party/{name}")));
    }
    let fonts = files_in(&inputs.fonts_dir, |n| n.to_ascii_lowercase().ends_with(".ttf"))?;
    if fonts.is_empty() {
        bail!("{} holds no .ttf files", inputs.fonts_dir.display());
    }
    for path in fonts {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        plan.push((path, format!("fonts/{name}")));
    }
    plan.push((
        inputs.fonts_dir.join("LICENSE"),
        "licenses/fonts/LICENSE".into(),
    ));
    for path in files_in(&inputs.project_root, |n| {
        let upper = n.to_ascii_uppercase();
        upper.starts_with("LICENSE") || upper.starts_with("NOTICE")
    })? {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        plan.push((path, format!("licenses/plumbline/{name}")));
    }
    let missing: Vec<String> = plan
        .iter()
        .filter(|(from, _)| !from.is_file())
        .map(|(from, _)| from.display().to_string())
        .collect();
    if !missing.is_empty() {
        bail!("cannot assemble the portable folder; missing:\n  {}", missing.join("\n  "));
    }

    if out_dir.exists() {
        std::fs::remove_dir_all(out_dir)
            .with_context(|| format!("cannot clear {}", out_dir.display()))?;
    }
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("cannot create {}", out_dir.display()))?;
    let mut written = Vec::new();
    for (from, rel) in &plan {
        copy(from, &out_dir.join(rel))?;
        written.push(rel.clone());
    }
    let notice = "THIRD-PARTY-LICENSES.txt";
    std::fs::write(out_dir.join(notice), &inputs.third_party)
        .with_context(|| format!("cannot write {notice}"))?;
    written.push(notice.to_string());
    written.sort();
    Ok(written)
}

const DOUBLE_RULE: &str =
    "==============================================================================\n";

fn section(out: &mut String, title: &str) {
    out.push_str("\n\n");
    out.push_str(DOUBLE_RULE);
    out.push_str(title);
    out.push('\n');
    out.push_str(DOUBLE_RULE);
    out.push('\n');
}

fn titled_text(out: &mut String, title: &str, text: &str) {
    out.push('\n');
    out.push_str(RULE);
    out.push_str(title);
    out.push('\n');
    out.push_str(RULE);
    out.push('\n');
    out.push_str(text.trim_end());
    out.push('\n');
}

/// The complete `THIRD-PARTY-LICENSES.txt`: a header, the crate listing, the crates' licence
/// texts (from [`licenses::crate_texts`]), then the Liberation fonts' licence
/// (`fonts_dir/LICENSE`) and PDFium's (`pdfium_dir/LICENSE` and `pdfium_dir/licenses/*`).
pub fn third_party_text(
    crate_listing: &str,
    crate_texts: &str,
    fonts_dir: &Path,
    pdfium_dir: &Path,
) -> Result<String> {
    let ver = fetch::PDFIUM_VERSION;
    let fonts = licenses::read_text(&fonts_dir.join("LICENSE"))?;
    let pdfium = licenses::read_text(&pdfium_dir.join("LICENSE"))?;
    let notices = files_in(&pdfium_dir.join("licenses"), |_| true)?;
    if notices.is_empty() {
        bail!(
            "{} holds no third-party notices",
            pdfium_dir.join("licenses").display()
        );
    }

    let mut out = format!(
        "Plumbline: third-party software notices\n\n\
         Plumbline bundles the third-party software below. This file carries the complete\n\
         licence texts for all of it:\n\n\
         \x20 1. Rust crates linked into plumbline.exe and plumb.exe: listing\n\
         \x20 2. Rust crates: licence texts\n\
         \x20 3. Liberation Sans fonts: SIL Open Font License 1.1\n\
         \x20 4. PDFium (pdfium.dll, build {ver} from bblanchon/pdfium-binaries) and the\n\
         \x20    libraries it bundles\n"
    );
    section(&mut out, "1. Rust crates: listing");
    out.push_str(crate_listing.trim_end());
    out.push('\n');
    section(&mut out, "2. Rust crates: licence texts");
    out.push_str(crate_texts.trim_end());
    out.push('\n');
    section(&mut out, "3. Liberation Sans fonts: SIL Open Font License 1.1");
    out.push_str(fonts.trim_end());
    out.push('\n');
    section(
        &mut out,
        &format!("4. PDFium (pdfium.dll, build {ver}) and the libraries it bundles"),
    );
    titled_text(&mut out, "pdfium-binaries packaging: LICENSE", &pdfium);
    for path in notices {
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        titled_text(&mut out, &format!("licenses/{name}"), &licenses::read_text(&path)?);
    }
    Ok(out)
}

pub struct Options {
    /// Skip `cargo build --release` and package whatever is in `target/release`.
    pub no_build: bool,
    /// Also write `target/dist/plumbline.zip`.
    pub zip: bool,
}

/// The `dist` subcommand. Returns the portable folder.
pub fn run(options: &Options) -> Result<PathBuf> {
    let root = util::workspace_root();
    let layout = Layout::in_workspace();
    fetch::validate_install(&layout.install_dir(), &PDFIUM_8125)
        .context("PDFium is not in place; run `cargo run -p xtask -- fetch-pdfium` first")?;
    checkdll::check_file(&layout.dll(), Policy::SystemOnly, true)?;

    let meta = licenses::load(&root)?;
    // Gathered before the long build so a crate without a licence text fails early.
    let crate_texts = licenses::crate_texts(&meta.packages)?;
    if !options.no_build {
        let mut build = util::cargo();
        build
            .args(["build", "--release"])
            .args(["-p", "pl-app", "--bin", "plumbline"])
            .args(["-p", "pl-cli", "--bin", "plumb"])
            .current_dir(&root);
        release::remap_build(&mut build, &root)?;
        util::run(&mut build).context("release build failed")?;
    }
    let release_dir = meta.target_directory.join("release");

    // Static CRT: the exes must not need the Visual C++ redistributable (plan section 11).
    let mut exes = Vec::new();
    for exe in EXES {
        let path = release_dir.join(exe);
        if !path.is_file() {
            bail!("{} was not built", path.display());
        }
        checkdll::check_file(&path, Policy::NoVcRedist, true)
            .with_context(|| format!("{exe} is not a static-CRT build"))?;
        exes.push(path);
    }
    // No source path may name the builder's account (pdfium.dll is third-party: not scanned).
    release::scan(&exes)?;

    let pdfium_dir = layout.install_dir();
    let fonts_dir = layout.vendor.join("fonts");
    let third_party = third_party_text(
        &licenses::render_report(&meta.packages),
        &crate_texts,
        &fonts_dir,
        &pdfium_dir,
    )?;
    let inputs = Inputs {
        release_dir,
        pdfium_dir,
        fonts_dir,
        project_root: root,
        third_party,
    };
    let dist_root = meta.target_directory.join("dist");
    let out_dir = dist_root.join(DIST_NAME);
    let written = assemble(&inputs, &out_dir)?;
    println!("portable folder: {} ({} files)", out_dir.display(), written.len());
    for rel in &written {
        println!("  {rel}");
    }

    if options.zip {
        let zip = dist_root.join(format!("{DIST_NAME}.zip"));
        zip_folder(&dist_root, &zip)?;
        println!("portable zip: {}", zip.display());
        // The zip is only good if it works somewhere else: unpack it into an empty folder
        // and have its own plumb.exe read a PDF there.
        let work = std::env::temp_dir().join(format!("plumbline-dist-verify-{}", std::process::id()));
        let checked = verify_zip(&zip, &work);
        let _ = std::fs::remove_dir_all(&work);
        let report = checked.context("the portable zip does not run from a clean folder")?;
        println!("verified: plumb info from a clean folder reports {report}");
    }
    Ok(out_dir)
}

/// Zips `<dist_root>/plumbline` as `zip`, replacing it.
pub fn zip_folder(dist_root: &Path, zip: &Path) -> Result<()> {
    let _ = std::fs::remove_file(zip);
    // The tar that ships with Windows (bsdtar) picks the zip format from the extension.
    util::run(
        Command::new(system_tool("tar.exe"))
            .args(["-a", "-c", "-f"])
            .arg(zip)
            .arg("-C")
            .arg(dist_root)
            .arg(DIST_NAME),
    )
    .context("zipping the portable folder failed")
}

/// A complete one-page PDF (Letter, one line drawn on it) with a correct cross-reference
/// table, for trying the packaged `plumb.exe` on.
pub fn probe_pdf() -> Vec<u8> {
    let content = "0 0 0 RG 2 w 72 72 m 540 720 l S\n";
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_string(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Resources << >> >>"
            .to_string(),
        format!("<< /Length {} >>\nstream\n{content}endstream", content.len()),
    ];
    let mut out = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n{body}\nendobj\n", i + 1).as_bytes());
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    out
}

/// Unpacks `zip` into the new, empty folder `work` and runs the `plumb.exe` inside it on a
/// PDF written there: with an empty environment (no search path beyond Windows' own, so
/// nothing of the build machine can stand in for a file missing from the zip) and `work` as
/// the working directory. Succeeds when `plumb info` exits with 0 and reports the one page;
/// returns the line it reported.
pub fn verify_zip(zip: &Path, work: &Path) -> Result<String> {
    if work.exists() {
        std::fs::remove_dir_all(work).with_context(|| format!("cannot clear {}", work.display()))?;
    }
    std::fs::create_dir_all(work).with_context(|| format!("cannot create {}", work.display()))?;
    util::run(
        Command::new(system_tool("tar.exe"))
            .arg("-x")
            .arg("-f")
            .arg(zip)
            .arg("-C")
            .arg(work),
    )
    .context("unpacking the zip failed")?;
    let folder = work.join(DIST_NAME);
    let exe = folder.join("plumb.exe");
    for needed in ["plumb.exe", "plumbline.exe", "pdfium.dll"] {
        if !folder.join(needed).is_file() {
            bail!("the zip has no {DIST_NAME}/{needed}");
        }
    }
    let pdf = work.join("probe.pdf");
    std::fs::write(&pdf, probe_pdf()).with_context(|| format!("cannot write {}", pdf.display()))?;

    let mut command = Command::new(&exe);
    command.arg("info").arg("probe.pdf").current_dir(work).env_clear();
    // Windows itself needs these two; nothing else is passed on.
    for keep in ["SystemRoot", "windir"] {
        if let Some(value) = std::env::var_os(keep) {
            command.env(keep, value);
        }
    }
    if let Some(root) = std::env::var_os("SystemRoot") {
        command.env("PATH", Path::new(&root).join("System32"));
    }
    let output = command
        .output()
        .with_context(|| format!("could not start {}", exe.display()))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        bail!("plumb info failed with {}:\n{stdout}{stderr}", output.status);
    }
    match stdout.lines().find(|line| line.starts_with("pages:")) {
        Some(line) if line.trim() == "pages: 1" => Ok(line.trim().to_string()),
        Some(line) => bail!("plumb info reported `{line}` for a one-page file"),
        None => bail!("plumb info printed no page count:\n{stdout}{stderr}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::testdir::TestDir;

    fn write(path: PathBuf, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    /// A synthetic source tree with every input present.
    fn inputs(dir: &Path) -> Inputs {
        let release = dir.join("release");
        let pdfium = dir.join("pdfium");
        let fonts = dir.join("fonts");
        let root = dir.join("root");
        write(release.join("plumbline.exe"), b"app");
        write(release.join("plumb.exe"), b"cli");
        write(release.join("plumbline.pdb"), b"symbols");
        write(pdfium.join("bin").join("pdfium.dll"), b"dll");
        write(pdfium.join("LICENSE"), b"pdfium-binaries");
        write(pdfium.join("licenses").join("pdfium.txt"), b"pdfium");
        write(pdfium.join("licenses").join("zlib.txt"), b"zlib");
        write(pdfium.join("include").join("fpdfview.h"), b"header");
        write(fonts.join("LiberationSans-Regular.ttf"), b"regular");
        write(fonts.join("LiberationSans-Bold.ttf"), b"bold");
        write(fonts.join("LICENSE"), b"OFL");
        write(root.join("LICENSE-MIT"), b"mit");
        write(root.join("Cargo.toml"), b"[workspace]");
        let third_party = third_party_text("crate listing\n", "crate texts\n", &fonts, &pdfium).unwrap();
        Inputs {
            release_dir: release,
            pdfium_dir: pdfium,
            fonts_dir: fonts,
            project_root: root,
            third_party,
        }
    }

    #[test]
    fn third_party_text_is_one_complete_file() {
        let dir = TestDir::new("dist-notice");
        let inputs = inputs(dir.path());
        let text = &inputs.third_party;
        let at = |needle: &str| {
            text.find(needle)
                .unwrap_or_else(|| panic!("`{needle}` is not in:\n{text}"))
        };
        // Listing, crate texts, the fonts' OFL, then PDFium's packaging licence and each
        // bundled notice, in that order and in full.
        let order = [
            at("crate listing"),
            at("crate texts"),
            at("\nOFL\n"),
            at("pdfium-binaries packaging: LICENSE"),
            at("\npdfium-binaries\n"),
            at("licenses/pdfium.txt"),
            at("licenses/zlib.txt"),
            at("\nzlib\n"),
        ];
        assert!(order.windows(2).all(|w| w[0] < w[1]), "{text}");
        for stale in ["listing only", "M12", "see licenses/"] {
            assert!(!text.contains(stale), "`{stale}` is in:\n{text}");
        }
        // Without the PDFium notices the file cannot be complete.
        std::fs::remove_dir_all(inputs.pdfium_dir.join("licenses")).unwrap();
        assert!(third_party_text("", "", &inputs.fonts_dir, &inputs.pdfium_dir).is_err());
    }

    #[test]
    fn assembles_the_expected_tree() {
        let dir = TestDir::new("dist-ok");
        let inputs = inputs(dir.path());
        let out = dir.path().join("dist").join("plumbline");
        let written = assemble(&inputs, &out).unwrap();
        assert_eq!(
            written,
            [
                "THIRD-PARTY-LICENSES.txt",
                "fonts/LiberationSans-Bold.ttf",
                "fonts/LiberationSans-Regular.ttf",
                "licenses/fonts/LICENSE",
                "licenses/pdfium/LICENSE",
                "licenses/pdfium/third_party/pdfium.txt",
                "licenses/pdfium/third_party/zlib.txt",
                "licenses/plumbline/LICENSE-MIT",
                "pdfium.dll",
                "plumb.exe",
                "plumbline.exe",
            ]
        );
        for rel in &written {
            assert!(out.join(rel).is_file(), "{rel}");
        }
        assert_eq!(std::fs::read(out.join("plumbline.exe")).unwrap(), b"app");
        assert_eq!(std::fs::read(out.join("pdfium.dll")).unwrap(), b"dll");
        assert_eq!(std::fs::read(out.join("licenses/fonts/LICENSE")).unwrap(), b"OFL");
        let notice = std::fs::read_to_string(out.join("THIRD-PARTY-LICENSES.txt")).unwrap();
        assert!(notice.contains("PDFium") && notice.contains("crate listing"));
        // Debug symbols, headers and the workspace manifest are not shipped.
        assert!(!out.join("plumbline.pdb").exists());
        assert!(!out.join("include").exists());
        assert!(!out.join("licenses/plumbline/Cargo.toml").exists());
    }

    /// M3: "The zip runs `plumb info` from a clean temp folder." The portable folder is
    /// assembled from the `plumb.exe` this workspace has built and the vendored PDFium, zipped
    /// the way `dist --zip` zips it, unpacked somewhere else and run there.
    #[test]
    fn the_zip_runs_plumb_info_from_a_clean_folder() {
        let root = util::workspace_root();
        let layout = Layout::in_workspace();
        let target = root.join("target");
        // The exe of a `dist` run if there is one, otherwise the one `cargo test` builds.
        let built = ["release", "debug"]
            .iter()
            .map(|profile| target.join(profile).join("plumb.exe"))
            .find(|exe| exe.is_file());
        let (Some(built), true) = (built, layout.dll().is_file()) else {
            eprintln!("skipped: plumb.exe has not been built, or PDFium is not fetched");
            return;
        };

        let dir = TestDir::new("dist-zip");
        let mut inputs = inputs(dir.path());
        std::fs::copy(&built, inputs.release_dir.join("plumb.exe")).unwrap();
        inputs.pdfium_dir = layout.install_dir();
        let dist_root = dir.path().join("dist");
        let written = assemble(&inputs, &dist_root.join(DIST_NAME)).unwrap();
        assert!(written.contains(&"pdfium.dll".to_string()));
        let zip = dist_root.join("plumbline.zip");
        zip_folder(&dist_root, &zip).unwrap();
        assert!(zip.is_file());

        // A folder that has nothing to do with the build tree.
        let clean = TestDir::new("dist-clean");
        let work = clean.path().join("unpacked");
        let report = verify_zip(&zip, &work).unwrap();
        assert_eq!(report, "pages: 1");
        assert!(work.join(DIST_NAME).join("licenses/fonts/LICENSE").is_file());

        // A zip without the DLL is caught: plumb.exe cannot be helped out by the search path.
        std::fs::remove_file(dist_root.join(DIST_NAME).join("pdfium.dll")).unwrap();
        zip_folder(&dist_root, &zip).unwrap();
        let err = verify_zip(&zip, &work).unwrap_err().to_string();
        assert!(err.contains("pdfium.dll"), "{err}");
    }

    #[test]
    fn the_probe_pdf_has_a_consistent_cross_reference_table() {
        let pdf = probe_pdf();
        let text = String::from_utf8(pdf.clone()).unwrap();
        let xref: usize = text
            .rsplit("startxref\n")
            .next()
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(text[xref..].starts_with("xref\n0 5\n"));
        for (i, line) in text[xref..].lines().skip(3).take(4).enumerate() {
            let offset: usize = line[..10].parse().unwrap();
            assert!(
                text[offset..].starts_with(&format!("{} 0 obj", i + 1)),
                "object {}",
                i + 1
            );
        }
    }

    #[test]
    fn reassembly_removes_stale_files() {
        let dir = TestDir::new("dist-stale");
        let inputs = inputs(dir.path());
        let out = dir.path().join("dist").join("plumbline");
        assemble(&inputs, &out).unwrap();
        write(out.join("old-build.txt"), b"stale");
        assemble(&inputs, &out).unwrap();
        assert!(!out.join("old-build.txt").exists());
        assert!(out.join("plumb.exe").is_file());
    }

    #[test]
    fn missing_input_fails_before_touching_the_output() {
        let dir = TestDir::new("dist-missing");
        let inputs = inputs(dir.path());
        let out = dir.path().join("dist").join("plumbline");
        assemble(&inputs, &out).unwrap();
        write(out.join("marker.txt"), b"previous good build");

        std::fs::remove_file(inputs.release_dir.join("plumb.exe")).unwrap();
        let err = assemble(&inputs, &out).unwrap_err().to_string();
        assert!(err.contains("plumb.exe"), "{err}");
        assert!(out.join("marker.txt").is_file(), "output was cleared despite the failure");
    }

    #[test]
    fn missing_font_licence_or_fonts_is_refused() {
        let dir = TestDir::new("dist-fonts");
        let inputs = inputs(dir.path());
        let out = dir.path().join("out");
        std::fs::remove_file(inputs.fonts_dir.join("LICENSE")).unwrap();
        assert!(assemble(&inputs, &out).is_err());
        write(inputs.fonts_dir.join("LICENSE"), b"OFL");
        for font in ["LiberationSans-Regular.ttf", "LiberationSans-Bold.ttf"] {
            std::fs::remove_file(inputs.fonts_dir.join(font)).unwrap();
        }
        assert!(assemble(&inputs, &out).is_err());
        assert!(!out.exists());
    }
}

//! `licenses`: lists the licence of every crate that is linked into the shipped programs,
//! read from `cargo metadata`.
//!
//! Only what ships is listed: the crates reachable from `plumbline.exe` (pl-app) and
//! `plumb.exe` (pl-cli) through ordinary dependencies. Dev-dependencies (test harnesses, the
//! fixture generator) and build-dependencies are not linked into those programs, so they are
//! not in the listing, in `THIRD-PARTY-LICENSES.txt` or in the zip. That matters for the one
//! crate in the lock file outside the permissive set: `colored` (MPL-2.0), which only the
//! snapshot-test harness pulls in.
//!
//! Besides the listing, [`crate_texts`] gathers the licence texts themselves for
//! `THIRD-PARTY-LICENSES.txt`: the licence, notice and AUTHORS files each crate's package ships
//! (read from the crate's source folder next to its manifest, and one folder down, which is
//! where the licence of embedded data such as hayro-cmap's `assets/LICENSE.txt` sits), one copy
//! of each distinct text with the crates that share it, and for a crate whose package ships
//! none, the standard SPDX text of its declared licence (kept in `crates/xtask/spdx/`) with its
//! copyright line naming the crate's authors, and its repository noted.
//! A shipped crate that would end up with no text at all is an error. The allow/deny decision
//! is `cargo deny check licenses`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

use crate::util;

/// Target the shipped binaries are built for; dependencies of other platforms are left out.
pub const TARGET: &str = "x86_64-pc-windows-msvc";

/// The workspace crates whose binaries are shipped.
pub const SHIPPED_ROOTS: [&str; 2] = ["pl-app", "pl-cli"];

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Package {
    pub name: String,
    pub version: String,
    /// SPDX expression from the manifest, if the crate declares one.
    pub license: Option<String>,
    /// `license-file` from the manifest, for crates with no SPDX expression.
    pub license_file: Option<String>,
    /// False for the workspace's own crates.
    pub third_party: bool,
    /// True when the crate is linked into a shipped program: it is reachable from one of
    /// [`SHIPPED_ROOTS`] through ordinary (not dev, not build) dependencies. When the metadata
    /// carries no dependency edges at all, every crate counts as shipped.
    pub shipped: bool,
    /// The crate's `Cargo.toml`; its folder holds the licence files the package ships.
    pub manifest_path: Option<PathBuf>,
    /// `authors` from the manifest.
    pub authors: Vec<String>,
    /// `repository` from the manifest.
    pub repository: Option<String>,
}

/// Parsed `cargo metadata` output.
#[derive(Debug)]
pub struct Metadata {
    /// Packages in the resolved graph for [`TARGET`], sorted by name then version.
    pub packages: Vec<Package>,
    pub target_directory: PathBuf,
}

/// Runs `cargo metadata` for the workspace and returns its JSON.
pub fn cargo_metadata_json(root: &Path) -> Result<String> {
    let out = util::cargo()
        .args(["metadata", "--format-version", "1", "--filter-platform", TARGET])
        .current_dir(root)
        .output()
        .context("could not start cargo metadata")?;
    if !out.status.success() {
        bail!(
            "cargo metadata failed with {}:\n{}",
            out.status,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8(out.stdout).context("cargo metadata output is not UTF-8")
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

/// Reads the package list out of `cargo metadata --format-version 1` JSON.
///
/// `packages` in that JSON lists every platform's crates even under `--filter-platform`, so
/// the list is narrowed to the ids that appear in `resolve.nodes`.
pub fn parse_metadata(json: &str) -> Result<Metadata> {
    let root: Value = serde_json::from_str(json).context("cargo metadata output is not JSON")?;
    let resolved: Option<BTreeSet<&str>> = root
        .get("resolve")
        .and_then(|r| r.get("nodes"))
        .and_then(Value::as_array)
        .map(|nodes| {
            nodes
                .iter()
                .filter_map(|n| n.get("id").and_then(Value::as_str))
                .collect()
        });
    let listed = root
        .get("packages")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("cargo metadata output has no `packages` array"))?;

    let shipped = shipped_ids(&root, listed);
    let mut packages = Vec::new();
    for p in listed {
        let id = p.get("id").and_then(Value::as_str).unwrap_or_default();
        if resolved.as_ref().is_some_and(|set| !set.contains(id)) {
            continue;
        }
        packages.push(Package {
            shipped: shipped.as_ref().is_none_or(|set| set.contains(id)),
            name: text(p, "name").ok_or_else(|| anyhow!("package without a name"))?,
            version: text(p, "version").ok_or_else(|| anyhow!("package without a version"))?,
            license: text(p, "license"),
            license_file: text(p, "license_file"),
            // Registry and git crates have a source; path crates (ours) have null.
            third_party: p.get("source").is_some_and(|s| !s.is_null()),
            manifest_path: text(p, "manifest_path").map(PathBuf::from),
            authors: p
                .get("authors")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|a| a.as_str().map(str::to_owned))
                .collect(),
            repository: text(p, "repository"),
        });
    }
    packages.sort();
    let target_directory = text(&root, "target_directory")
        .map(PathBuf::from)
        .ok_or_else(|| anyhow!("cargo metadata output has no `target_directory`"))?;
    Ok(Metadata {
        packages,
        target_directory,
    })
}

/// The ids of the packages linked into the shipped programs, or `None` when the metadata has
/// no dependency edges or none of the shipped crates (then nothing can be ruled out).
///
/// An edge counts when one of its `dep_kinds` has kind `null`, which is how `cargo metadata`
/// spells an ordinary dependency; `"dev"` and `"build"` edges are not followed.
fn shipped_ids<'a>(root: &'a Value, listed: &'a [Value]) -> Option<BTreeSet<&'a str>> {
    let nodes = root.get("resolve")?.get("nodes")?.as_array()?;
    let mut edges: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    let mut any_edge = false;
    for node in nodes {
        let Some(id) = node.get("id").and_then(Value::as_str) else {
            continue;
        };
        let deps = node.get("deps").and_then(Value::as_array);
        any_edge |= deps.is_some();
        let normal = deps.into_iter().flatten().filter_map(|dep| {
            let kinds = dep.get("dep_kinds").and_then(Value::as_array)?;
            let ordinary = kinds
                .iter()
                .any(|k| k.get("kind").is_none_or(Value::is_null));
            ordinary.then(|| dep.get("pkg").and_then(Value::as_str)).flatten()
        });
        edges.entry(id).or_default().extend(normal);
    }
    if !any_edge {
        return None;
    }
    let mut todo: Vec<&str> = listed
        .iter()
        .filter(|p| {
            p.get("source").is_none_or(Value::is_null)
                && p.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|n| SHIPPED_ROOTS.contains(&n))
        })
        .filter_map(|p| p.get("id").and_then(Value::as_str))
        .collect();
    if todo.is_empty() {
        return None;
    }
    let mut seen = BTreeSet::new();
    while let Some(id) = todo.pop() {
        if seen.insert(id) {
            todo.extend(edges.get(id).into_iter().flatten().copied());
        }
    }
    Some(seen)
}

pub fn load(root: &Path) -> Result<Metadata> {
    parse_metadata(&cargo_metadata_json(root)?)
}

fn license_label(p: &Package) -> String {
    match (&p.license, &p.license_file) {
        (Some(expr), _) => expr.clone(),
        (None, Some(file)) => format!("(licence file: {file})"),
        (None, None) => "(NO LICENCE DECLARED)".to_string(),
    }
}

/// Plain-text listing: a per-expression summary, then one line per third-party crate that is
/// linked into a shipped program.
pub fn render_report(packages: &[Package]) -> String {
    let third: Vec<&Package> = packages
        .iter()
        .filter(|p| p.third_party && p.shipped)
        .collect();
    let mut by_license: BTreeMap<String, usize> = BTreeMap::new();
    for p in &third {
        *by_license.entry(license_label(p)).or_default() += 1;
    }
    let width = third.iter().map(|p| p.name.len()).max().unwrap_or(0);

    let mut out = String::new();
    out.push_str("Plumbline: third-party Rust crates and their declared licences\n");
    out.push_str(&format!(
        "Source: cargo metadata, target {TARGET}. {} crates.\n",
        third.len()
    ));
    out.push('\n');
    out.push_str("By licence expression:\n");
    for (license, count) in &by_license {
        out.push_str(&format!("  {count:>4}  {license}\n"));
    }
    out.push_str("\nCrates:\n");
    for p in &third {
        out.push_str(&format!(
            "  {:<width$}  {:<12}  {}\n",
            p.name,
            p.version,
            license_label(p)
        ));
    }
    out
}

/// Shipped third-party crates that declare no SPDX expression.
pub fn undeclared(packages: &[Package]) -> Vec<&Package> {
    packages
        .iter()
        .filter(|p| p.third_party && p.shipped && p.license.is_none())
        .collect()
}

/// Upper-case file-name prefixes of the licence and notice files a crate package ships.
/// `AUTHORS` is there because some copyright notices name their holders only through it.
const LICENCE_PREFIXES: [&str; 7] = [
    "LICENSE",
    "LICENCE",
    "COPYING",
    "NOTICE",
    "UNLICENSE",
    "COPYRIGHT",
    "AUTHORS",
];

/// Sub-folders of a crate package whose licence files are not looked at: they hold tests,
/// benchmarks and examples, which are not compiled into a dependent program.
const SKIPPED_DIRS: [&str; 3] = ["tests", "benches", "examples"];

/// Folder names (upper case) whose every file is a licence text, such as `LICENSES/`.
const LICENCE_DIRS: [&str; 2] = ["LICENSES", "LICENCES"];

/// Standard SPDX texts, used for a crate whose package ships no licence file.
const SPDX_TEXTS: &[(&str, &str)] = &[
    ("Apache-2.0", include_str!("../spdx/Apache-2.0.txt")),
    ("BSD-2-Clause", include_str!("../spdx/BSD-2-Clause.txt")),
    ("BSD-3-Clause", include_str!("../spdx/BSD-3-Clause.txt")),
    ("BSL-1.0", include_str!("../spdx/BSL-1.0.txt")),
    ("ISC", include_str!("../spdx/ISC.txt")),
    ("MIT", include_str!("../spdx/MIT.txt")),
    ("Zlib", include_str!("../spdx/Zlib.txt")),
];

/// The standard text of an SPDX licence identifier, if it is one of [`SPDX_TEXTS`].
pub fn spdx_text(id: &str) -> Option<&'static str> {
    SPDX_TEXTS
        .iter()
        .find(|(known, _)| known.eq_ignore_ascii_case(id))
        .map(|(_, text)| *text)
}

/// The licence identifiers in an SPDX expression (also the old `MIT/Apache-2.0` form), in
/// order of appearance, without repeats. Operators and `WITH` exceptions are left out.
pub fn spdx_ids(expr: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut after_with = false;
    for token in expr
        .split(|c: char| c.is_whitespace() || matches!(c, '(' | ')' | '/'))
        .filter(|t| !t.is_empty())
    {
        if after_with {
            after_with = false;
            continue;
        }
        if token.eq_ignore_ascii_case("WITH") {
            after_with = true;
            continue;
        }
        if token.eq_ignore_ascii_case("OR") || token.eq_ignore_ascii_case("AND") {
            continue;
        }
        if !out.iter().any(|known| known == token) {
            out.push(token.to_string());
        }
    }
    out
}

/// A licence file's text: UTF-8 without a byte-order mark, or Latin-1 when it is not UTF-8.
fn decode(bytes: Vec<u8>) -> String {
    let bytes = match bytes.strip_prefix(b"\xEF\xBB\xBF") {
        Some(rest) => rest.to_vec(),
        None => bytes,
    };
    match String::from_utf8(bytes) {
        Ok(text) => text,
        Err(err) => err.into_bytes().iter().map(|&b| char::from(b)).collect(),
    }
}

/// Reads a text file the way licence files are read (see `decode`).
pub fn read_text(path: &Path) -> Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("cannot read {}", path.display()))?;
    Ok(decode(bytes))
}

/// True for a file name that names a licence or notice file. Rust sources such as
/// `licenses.rs` or `copying.rs` are not.
fn is_licence_name(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    LICENCE_PREFIXES.iter().any(|p| upper.starts_with(p)) && !upper.ends_with(".RS")
}

/// The licence and notice files in `dir` and one folder down, as (path relative to `dir`
/// with `/`, text), sorted by path. One folder down covers a licence kept next to data a crate
/// embeds (`assets/LICENSE.txt`) and every file of a `LICENSES/` folder; `tests/`, `benches/`
/// and `examples/` are left out. `license_file` is the manifest's `license-file`, taken
/// whatever its name; when the path does not exist as written it is looked for by its file
/// name in `dir`, where `cargo package` puts a licence file that lived outside the package.
/// Empty files are skipped.
pub fn licence_files(dir: &Path, license_file: Option<&str>) -> Result<Vec<(String, String)>> {
    let list = |dir: &Path| -> Result<Vec<(String, PathBuf)>> {
        let mut out = Vec::new();
        for entry in
            std::fs::read_dir(dir).with_context(|| format!("cannot list {}", dir.display()))?
        {
            let path = entry?.path();
            if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
                out.push((name.to_string(), path.clone()));
            }
        }
        Ok(out)
    };
    let mut found: BTreeMap<String, PathBuf> = BTreeMap::new();
    for (name, path) in list(dir)? {
        if path.is_file() {
            if is_licence_name(&name) {
                found.insert(name, path);
            }
            continue;
        }
        if !path.is_dir() || SKIPPED_DIRS.iter().any(|s| name.eq_ignore_ascii_case(s)) {
            continue;
        }
        let whole = LICENCE_DIRS.contains(&name.to_ascii_uppercase().as_str());
        for (inner, inner_path) in list(&path)? {
            let wanted = if whole {
                !inner.to_ascii_uppercase().ends_with(".RS")
            } else {
                is_licence_name(&inner)
            };
            if wanted && inner_path.is_file() {
                found.insert(format!("{name}/{inner}"), inner_path);
            }
        }
    }
    if let Some(file) = license_file {
        let given = dir.join(file);
        let by_name = Path::new(file).file_name().map(|n| dir.join(n));
        if let Some(path) = [Some(given), by_name].into_iter().flatten().find(|p| p.is_file()) {
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            found.entry(name).or_insert(path);
        }
    }
    let mut out = Vec::new();
    for (name, path) in found {
        let text = read_text(&path)?;
        if !text.trim().is_empty() {
            out.push((name, text));
        }
    }
    Ok(out)
}

/// Who holds the copyright of a crate whose package ships no licence file, as its manifest
/// says: its authors, or when it names none, "the <crate> authors" with its repository.
fn copyright_holder(p: &Package) -> String {
    if !p.authors.is_empty() {
        return p.authors.join(", ");
    }
    match &p.repository {
        Some(repo) => format!("the {} authors ({repo})", p.name),
        None => format!("the {} authors", p.name),
    }
}

/// A standard licence text with its template copyright line (`Copyright (c) <year> ...`)
/// filled in with `holder`. Texts without such a line (Apache-2.0, BSL-1.0) are unchanged.
pub fn with_copyright(template: &str, holder: &str) -> String {
    template
        .split_inclusive('\n')
        .map(|line| {
            if line.trim_start().starts_with("Copyright (c) <year>") {
                let end = if line.ends_with('\n') { "\n" } else { "" };
                format!("Copyright (c) {holder}{end}")
            } else {
                line.to_string()
            }
        })
        .collect()
}

/// One distinct licence text and the crates that carry it.
struct SharedText {
    text: String,
    users: Vec<String>,
}

pub const RULE: &str =
    "------------------------------------------------------------------------------\n";

/// The licence texts of every shipped third-party crate, each distinct text once with the
/// crates that share it. A crate whose package ships no licence file gets the standard text
/// of each licence its SPDX expression names, noted with its authors and repository. Fails,
/// naming the crates, when any shipped crate would be left with no text.
pub fn crate_texts(packages: &[Package]) -> Result<String> {
    let mut shared: Vec<SharedText> = Vec::new();
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    let mut add = |text: String, user: String| {
        let i = *index.entry(text.clone()).or_insert_with(|| {
            shared.push(SharedText {
                text,
                users: Vec::new(),
            });
            shared.len() - 1
        });
        shared[i].users.push(user);
    };

    let mut without: Vec<String> = Vec::new();
    let mut crates = 0;
    for p in packages.iter().filter(|p| p.third_party && p.shipped) {
        crates += 1;
        let label = format!("{} {}", p.name, p.version);
        let dir = p.manifest_path.as_deref().and_then(Path::parent);
        let files = match dir {
            Some(dir) => licence_files(dir, p.license_file.as_deref())?,
            None => Vec::new(),
        };
        if !files.is_empty() {
            for (name, text) in files {
                add(text, format!("{label} ({name})"));
            }
            continue;
        }
        let ids = p.license.as_deref().map(spdx_ids).unwrap_or_default();
        let unknown: Vec<&String> = ids.iter().filter(|id| spdx_text(id).is_none()).collect();
        if ids.is_empty() || !unknown.is_empty() {
            without.push(format!("{label} ({})", license_label(p)));
            continue;
        }
        let authors = if p.authors.is_empty() {
            "not given".to_string()
        } else {
            p.authors.join(", ")
        };
        let repository = p.repository.as_deref().unwrap_or("not given");
        let holder = copyright_holder(p);
        for id in &ids {
            add(
                with_copyright(spdx_text(id).unwrap(), &holder),
                format!(
                    "{label} (its package ships no licence file: standard {id} text; \
                     authors: {authors}; repository: {repository})"
                ),
            );
        }
    }
    if !without.is_empty() {
        bail!(
            "no licence text can be given for {} shipped crate(s): their packages ship no \
             licence file and their licences have no standard text in crates/xtask/spdx:\n  {}",
            without.len(),
            without.join("\n  ")
        );
    }

    let mut out = format!(
        "{} distinct licence texts for {crates} crates. Each text is headed by the crates \
         whose packages carry it.\n",
        shared.len()
    );
    for (n, entry) in shared.iter().enumerate() {
        out.push('\n');
        out.push_str(RULE);
        out.push_str(&format!("[{}] Used by:\n", n + 1));
        for user in &entry.users {
            out.push_str(&format!("  {user}\n"));
        }
        out.push_str(RULE);
        out.push('\n');
        out.push_str(entry.text.trim_end());
        out.push('\n');
    }
    Ok(out)
}

/// The `licenses` subcommand: prints the listing, or writes it to `out`.
pub fn run(out: Option<&Path>) -> Result<()> {
    let meta = load(&util::workspace_root())?;
    let report = render_report(&meta.packages);
    match out {
        Some(path) => {
            std::fs::write(path, &report)
                .with_context(|| format!("cannot write {}", path.display()))?;
            println!("wrote {}", path.display());
        }
        None => print!("{report}"),
    }
    let missing = undeclared(&meta.packages);
    if !missing.is_empty() {
        eprintln!(
            "note: {} crate(s) declare no SPDX licence expression: {}",
            missing.len(),
            missing
                .iter()
                .map(|p| format!("{} {}", p.name, p.version))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
      "packages": [
        {"name": "zeta", "version": "1.0.0", "id": "registry+https://x#zeta@1.0.0",
         "license": "MIT OR Apache-2.0", "license_file": null, "source": "registry+https://x"},
        {"name": "alpha", "version": "0.2.0", "id": "registry+https://x#alpha@0.2.0",
         "license": "BSD-3-Clause", "license_file": null, "source": "registry+https://x"},
        {"name": "odd", "version": "3.1.4", "id": "registry+https://x#odd@3.1.4",
         "license": null, "license_file": "LICENSE.txt", "source": "registry+https://x"},
        {"name": "bare", "version": "0.0.1", "id": "registry+https://x#bare@0.0.1",
         "license": null, "license_file": null, "source": "registry+https://x"},
        {"name": "linux-only", "version": "9.9.9", "id": "registry+https://x#linux-only@9.9.9",
         "license": "GPL-3.0-only", "license_file": null, "source": "registry+https://x"},
        {"name": "pl-geom", "version": "0.1.0", "id": "path+file:///c/Dev/plumbline/crates/pl-geom#0.1.0",
         "license": "MIT OR Apache-2.0", "license_file": null, "source": null}
      ],
      "resolve": {"nodes": [
        {"id": "registry+https://x#zeta@1.0.0"},
        {"id": "registry+https://x#alpha@0.2.0"},
        {"id": "registry+https://x#odd@3.1.4"},
        {"id": "registry+https://x#bare@0.0.1"},
        {"id": "path+file:///c/Dev/plumbline/crates/pl-geom#0.1.0"}
      ]},
      "target_directory": "C:\\Dev\\plumbline\\target"
    }"#;

    #[test]
    fn parses_sorts_and_drops_unresolved_packages() {
        let meta = parse_metadata(SAMPLE).unwrap();
        let names: Vec<&str> = meta.packages.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["alpha", "bare", "odd", "pl-geom", "zeta"]);
        assert_eq!(meta.target_directory, PathBuf::from(r"C:\Dev\plumbline\target"));
        let geom = meta.packages.iter().find(|p| p.name == "pl-geom").unwrap();
        assert!(!geom.third_party);
        let odd = meta.packages.iter().find(|p| p.name == "odd").unwrap();
        assert!(odd.third_party);
        assert_eq!(odd.license, None);
        assert_eq!(odd.license_file.as_deref(), Some("LICENSE.txt"));
    }

    #[test]
    fn report_lists_third_party_only_with_summary() {
        let meta = parse_metadata(SAMPLE).unwrap();
        let report = render_report(&meta.packages);
        assert!(report.contains("4 crates."), "{report}");
        assert!(report.contains("     1  BSD-3-Clause\n"), "{report}");
        assert!(report.contains("     1  MIT OR Apache-2.0\n"), "{report}");
        assert!(report.contains("(licence file: LICENSE.txt)"), "{report}");
        assert!(report.contains("(NO LICENCE DECLARED)"), "{report}");
        assert!(!report.contains("pl-geom"), "{report}");
        assert!(!report.contains("linux-only"), "{report}");
        let alpha = report.find("  alpha").unwrap();
        let zeta = report.find("  zeta").unwrap();
        assert!(alpha < zeta);
    }

    #[test]
    fn undeclared_finds_crates_without_spdx() {
        let meta = parse_metadata(SAMPLE).unwrap();
        let names: Vec<&str> = undeclared(&meta.packages).iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["bare", "odd"]);
    }

    fn package(name: &str, license: Option<&str>, dir: Option<&Path>) -> Package {
        Package {
            name: name.into(),
            version: "1.0.0".into(),
            license: license.map(str::to_owned),
            license_file: None,
            third_party: true,
            shipped: true,
            manifest_path: dir.map(|d| d.join("Cargo.toml")),
            authors: vec!["A. Author <a@example.org>".into()],
            repository: Some("https://example.org/repo".into()),
        }
    }

    #[test]
    fn spdx_ids_handles_operators_slashes_and_exceptions() {
        assert_eq!(spdx_ids("MIT OR Apache-2.0"), ["MIT", "Apache-2.0"]);
        assert_eq!(spdx_ids("MIT/Apache-2.0"), ["MIT", "Apache-2.0"]);
        assert_eq!(
            spdx_ids("(MIT OR Apache-2.0) AND Unicode-3.0"),
            ["MIT", "Apache-2.0", "Unicode-3.0"]
        );
        assert_eq!(
            spdx_ids("Apache-2.0 WITH LLVM-exception OR MIT"),
            ["Apache-2.0", "MIT"]
        );
        assert_eq!(spdx_ids("MIT OR MIT"), ["MIT"]);
        assert!(spdx_text("mit").is_some_and(|t| t.contains("Permission is hereby granted")));
        assert!(spdx_text("Apache-2.0").is_some_and(|t| t.contains("Version 2.0, January 2004")));
        assert!(spdx_text("Unicode-3.0").is_none());
    }

    #[test]
    fn licence_files_picks_licence_and_notice_files_only() {
        let dir = crate::util::testdir::TestDir::new("lic-files");
        let d = dir.path();
        std::fs::write(d.join("LICENSE-MIT"), "mit text\n").unwrap();
        std::fs::write(d.join("license-apache-2.0"), "apache text\n").unwrap();
        std::fs::write(d.join("COPYRIGHT"), "\u{feff}copyright text\n").unwrap();
        std::fs::write(d.join("NOTICE.md"), b"caf\xe9\n").unwrap();
        std::fs::write(d.join("LICENSE.empty"), "  \n").unwrap();
        std::fs::write(d.join("terms.txt"), "custom terms\n").unwrap();
        std::fs::write(d.join("README.md"), "readme\n").unwrap();
        std::fs::create_dir(d.join("LICENSES")).unwrap();
        let names = |files: Vec<(String, String)>| -> Vec<String> {
            files.into_iter().map(|(n, _)| n).collect()
        };
        let files = licence_files(d, None).unwrap();
        // A byte-order mark is dropped and a Latin-1 file is read as Latin-1.
        assert!(files.contains(&("COPYRIGHT".into(), "copyright text\n".into())));
        assert!(files.contains(&("NOTICE.md".into(), "caf\u{e9}\n".into())));
        assert_eq!(
            names(files),
            ["COPYRIGHT", "LICENSE-MIT", "NOTICE.md", "license-apache-2.0"]
        );
        // `license-file` is taken whatever its name, also when it was moved into the package.
        let files = licence_files(d, Some("../shared/terms.txt")).unwrap();
        assert!(names(files).contains(&"terms.txt".to_string()));
    }

    #[test]
    fn licence_files_look_one_folder_down_but_not_into_tests_or_sources() {
        let dir = crate::util::testdir::TestDir::new("lic-sub");
        let d = dir.path();
        for sub in ["assets", "tests", "examples", "src", "LICENSES"] {
            std::fs::create_dir(d.join(sub)).unwrap();
        }
        std::fs::write(d.join("LICENSE-MIT"), "mit\n").unwrap();
        std::fs::write(d.join("AUTHORS"), "the authors\n").unwrap();
        std::fs::write(d.join("assets").join("LICENSE.txt"), "data licence\n").unwrap();
        std::fs::write(d.join("assets").join("cmaps.bin"), "data\n").unwrap();
        std::fs::write(d.join("tests").join("COPYRIGHT"), "test data\n").unwrap();
        std::fs::write(d.join("examples").join("LICENSE"), "example\n").unwrap();
        std::fs::write(d.join("src").join("licenses.rs"), "fn f() {}\n").unwrap();
        std::fs::write(d.join("LICENSES").join("Apache-2.0.txt"), "apache\n").unwrap();
        std::fs::write(d.join("LICENSES").join("mod.rs"), "fn f() {}\n").unwrap();
        std::fs::create_dir_all(d.join("assets").join("deeper")).unwrap();
        std::fs::write(d.join("assets").join("deeper").join("LICENSE"), "deep\n").unwrap();
        let names: Vec<String> = licence_files(d, None)
            .unwrap()
            .into_iter()
            .map(|(n, _)| n)
            .collect();
        assert_eq!(
            names,
            ["AUTHORS", "LICENSE-MIT", "LICENSES/Apache-2.0.txt", "assets/LICENSE.txt"]
        );

        // In the combined file the embedded data's licence is headed by its path.
        let packages = [package("cmapper", Some("MIT"), Some(d))];
        let text = crate_texts(&packages).unwrap();
        assert!(text.contains("  cmapper 1.0.0 (assets/LICENSE.txt)\n"), "{text}");
        assert!(text.contains("data licence") && !text.contains("test data"), "{text}");
    }

    #[test]
    fn with_copyright_fills_the_template_line_only() {
        let mit = with_copyright(spdx_text("MIT").unwrap(), "Jo Doe");
        assert!(mit.starts_with("MIT License\n\nCopyright (c) Jo Doe\n\nPermission"), "{mit}");
        let bsd = with_copyright(spdx_text("BSD-3-Clause").unwrap(), "Jo Doe");
        assert!(bsd.starts_with("Copyright (c) Jo Doe\n"), "{bsd}");
        let apache = spdx_text("Apache-2.0").unwrap();
        assert_eq!(with_copyright(apache, "Jo Doe"), apache);
        let mut nameless = package("solo", Some("MIT"), None);
        nameless.authors.clear();
        assert_eq!(
            copyright_holder(&nameless),
            "the solo authors (https://example.org/repo)"
        );
    }

    #[test]
    fn crate_texts_shares_identical_texts_and_falls_back_to_spdx() {
        let dir = crate::util::testdir::TestDir::new("lic-texts");
        let (a, b, c) = (dir.path().join("a"), dir.path().join("b"), dir.path().join("c"));
        for d in [&a, &b, &c] {
            std::fs::create_dir_all(d).unwrap();
        }
        std::fs::write(a.join("LICENSE-MIT"), "same mit text\n").unwrap();
        std::fs::write(b.join("LICENSE"), "same mit text\n").unwrap();
        std::fs::write(b.join("NOTICE"), "b notice\n").unwrap();
        let mut ours = package("ours", Some("MIT"), Some(&c));
        ours.third_party = false;
        let mut dev_only = package("dev-only", Some("MPL-2.0"), Some(&c));
        dev_only.shipped = false;
        let packages = [
            package("alpha", Some("MIT"), Some(&a)),
            package("beta", Some("MIT"), Some(&b)),
            package("gamma", Some("MIT OR Apache-2.0"), Some(&c)),
            ours,
            dev_only,
        ];
        let text = crate_texts(&packages).unwrap();
        assert!(text.starts_with("4 distinct licence texts for 3 crates."), "{text}");
        assert_eq!(text.matches("same mit text").count(), 1, "{text}");
        assert!(text.contains("  alpha 1.0.0 (LICENSE-MIT)\n  beta 1.0.0 (LICENSE)\n"), "{text}");
        assert!(text.contains("b notice"), "{text}");
        assert!(text.contains("gamma 1.0.0 (its package ships no licence file: standard MIT text"));
        assert!(text.contains("authors: A. Author <a@example.org>"), "{text}");
        // The standard text's template copyright line names the crate's authors.
        assert!(text.contains("Copyright (c) A. Author <a@example.org>\n"), "{text}");
        assert!(!text.contains("Copyright (c) <year>"), "{text}");
        assert!(text.contains("repository: https://example.org/repo"), "{text}");
        assert!(text.contains("Version 2.0, January 2004"), "{text}");
        assert!(!text.contains("ours") && !text.contains("dev-only"), "{text}");
    }

    #[test]
    fn a_crate_left_without_text_is_an_error_naming_it() {
        let dir = crate::util::testdir::TestDir::new("lic-none");
        let packages = [
            package("bare", None, Some(dir.path())),
            package("exotic", Some("MIT AND Unicode-3.0"), Some(dir.path())),
            package("fine", Some("MIT"), Some(dir.path())),
        ];
        let err = crate_texts(&packages).unwrap_err().to_string();
        assert!(err.contains("2 shipped crate(s)"), "{err}");
        assert!(err.contains("bare 1.0.0") && err.contains("exotic 1.0.0"), "{err}");
        assert!(!err.contains("fine"), "{err}");
    }

    #[test]
    fn bad_input_is_an_error() {
        assert!(parse_metadata("not json").is_err());
        assert!(parse_metadata("{}").is_err());
        assert!(parse_metadata(r#"{"packages": []}"#).is_err());
    }

    const GRAPH: &str = r#"{
      "packages": [
        {"name": "pl-app", "version": "0.1.0", "id": "app", "license": "MIT", "license_file": null, "source": null},
        {"name": "pl-cli", "version": "0.1.0", "id": "cli", "license": "MIT", "license_file": null, "source": null},
        {"name": "pl-testkit", "version": "0.1.0", "id": "kit", "license": "MIT", "license_file": null, "source": null},
        {"name": "gui", "version": "1.0.0", "id": "gui", "license": "MIT", "license_file": null, "source": "registry+https://x"},
        {"name": "deep", "version": "1.0.0", "id": "deep", "license": "Zlib", "license_file": null, "source": "registry+https://x"},
        {"name": "harness", "version": "1.0.0", "id": "harness", "license": "MIT", "license_file": null, "source": "registry+https://x"},
        {"name": "tinted", "version": "2.0.0", "id": "tinted", "license": "MPL-2.0", "license_file": null, "source": "registry+https://x"},
        {"name": "codegen", "version": "1.0.0", "id": "codegen", "license": "ISC", "license_file": null, "source": "registry+https://x"},
        {"name": "both", "version": "1.0.0", "id": "both", "license": "BSD-2-Clause", "license_file": null, "source": "registry+https://x"}
      ],
      "resolve": {"nodes": [
        {"id": "app", "deps": [
          {"pkg": "gui", "dep_kinds": [{"kind": null, "target": null}]},
          {"pkg": "harness", "dep_kinds": [{"kind": "dev", "target": null}]},
          {"pkg": "codegen", "dep_kinds": [{"kind": "build", "target": null}]},
          {"pkg": "both", "dep_kinds": [{"kind": "dev", "target": null}, {"kind": null, "target": null}]}
        ]},
        {"id": "cli", "deps": [{"pkg": "kit", "dep_kinds": [{"kind": "dev", "target": null}]}]},
        {"id": "kit", "deps": []},
        {"id": "gui", "deps": [{"pkg": "deep", "dep_kinds": [{"kind": null, "target": null}]}]},
        {"id": "deep", "deps": []},
        {"id": "harness", "deps": [{"pkg": "tinted", "dep_kinds": [{"kind": null, "target": null}]}]},
        {"id": "tinted", "deps": []},
        {"id": "codegen", "deps": []},
        {"id": "both", "deps": []}
      ]},
      "target_directory": "C:\\Dev\\plumbline\\target"
    }"#;

    #[test]
    fn only_crates_linked_into_the_shipped_programs_are_listed() {
        let meta = parse_metadata(GRAPH).unwrap();
        let shipped: Vec<&str> = meta
            .packages
            .iter()
            .filter(|p| p.shipped)
            .map(|p| p.name.as_str())
            .collect();
        // Ordinary dependencies, transitively; a crate that is both a dev and an ordinary
        // dependency ships. The test harness, what only it pulls in, the build-only crate and
        // the dev-only fixture generator do not.
        assert_eq!(shipped, ["both", "deep", "gui", "pl-app", "pl-cli"]);
        let report = render_report(&meta.packages);
        assert!(report.contains("3 crates."), "{report}");
        for absent in ["tinted", "MPL-2.0", "harness", "codegen", "pl-testkit"] {
            assert!(!report.contains(absent), "{absent} is in the listing:\n{report}");
        }
        assert!(report.contains("  deep"), "{report}");
        // Metadata without dependency edges (the older sample) rules nothing out.
        assert!(parse_metadata(SAMPLE).unwrap().packages.iter().all(|p| p.shipped));
    }

    /// Runs the real `cargo metadata` on this workspace.
    #[test]
    fn real_workspace_metadata_parses() {
        let meta = load(&util::workspace_root()).unwrap();
        assert!(meta.packages.iter().any(|p| p.name == "xtask" && !p.third_party));
        assert!(meta.packages.iter().any(|p| p.name == "anyhow" && p.third_party));
        assert!(meta.target_directory.is_absolute());
        let by_name = |name: &str| {
            meta.packages
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("{name} is not in the lock file"))
        };
        // What the shipped programs link, and what only tests and tools use.
        for shipped in ["pl-app", "pl-cli", "pl-doc", "egui", "lopdf", "pdfium-render"] {
            assert!(by_name(shipped).shipped, "{shipped} should be listed");
        }
        for dev_only in ["pl-testkit", "egui_kittest", "xtask"] {
            assert!(!by_name(dev_only).shipped, "{dev_only} is linked into a shipped program");
        }
        // The one licence in the lock file outside the permissive set is dev-only, so it
        // reaches neither THIRD-PARTY-LICENSES.txt nor the zip.
        let report = render_report(&meta.packages);
        for p in meta.packages.iter().filter(|p| p.third_party) {
            let license = p.license.as_deref().unwrap_or("");
            if license == "MPL-2.0" {
                assert!(!p.shipped, "{} ({license}) is linked into a shipped program", p.name);
                assert!(!report.contains(&format!("  {} ", p.name)));
            }
        }
        assert!(!report.contains("MPL-2.0"), "an MPL-2.0 crate is in the shipped listing");
        assert!(!report.contains("GPL-3.0") && !report.contains("AGPL"));
        // Every shipped crate has a licence text, from its package or the standard one.
        let texts = crate_texts(&meta.packages).unwrap();
        assert!(texts.contains("  egui 0.37.0 ("), "egui has no licence text");
        // Adobe's licence for the CMap data hayro-cmap embeds sits in its assets/ folder.
        if let Some(cmap) = meta.packages.iter().find(|p| p.name == "hayro-cmap" && p.shipped) {
            let heading = format!("  hayro-cmap {} (assets/LICENSE.txt)", cmap.version);
            assert!(texts.contains(&heading), "hayro-cmap's data licence is missing");
        }
        // (Upstream texts may mention `<year>` in their how-to-apply notes; the template
        // copyright line of a standard text must not survive.)
        assert!(
            !texts.contains("Copyright (c) <year>"),
            "a standard text keeps its template copyright line"
        );
        assert!(texts.contains("Permission is hereby granted"));
    }
}

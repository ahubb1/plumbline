//! `fetch-pdfium`: downloads the pinned PDFium build, verifies it and unpacks it.
//!
//! Nothing from the network is unpacked or kept before its SHA-256 matches the pinned value.

use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::util::{self, check_sha256, sha256_file, system_tool, verify_file_sha256};

/// One pinned PDFium release.
#[derive(Debug, Clone, Copy)]
pub struct Release {
    pub url: &'static str,
    /// SHA-256 of the downloaded archive.
    pub archive_sha256: &'static str,
    /// SHA-256 of `bin/pdfium.dll` inside that archive, used to validate an existing install.
    pub dll_sha256: &'static str,
}

/// PDFium 8125 from bblanchon/pdfium-binaries: the build pdfium-render 0.9.6 binds to.
/// The archive digest is the one published by the GitHub release API (plan section 4); the
/// DLL digest was taken from that verified archive.
pub const PDFIUM_8125: Release = Release {
    url: "https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8125/pdfium-win-x64.tgz",
    archive_sha256: "9a964261760cff52570aabf038ab346d3a5034dd8b30d0d5fe8861235d356b8b",
    dll_sha256: "4b0dec92555d69437bef0df37be630181ab3e83a6823ca264ede355a4f2ef994",
};

pub const PDFIUM_VERSION: &str = "8125";

/// Where the archive and the unpacked files live.
#[derive(Debug, Clone)]
pub struct Layout {
    /// The `vendor` directory.
    pub vendor: PathBuf,
}

impl Layout {
    pub fn in_workspace() -> Self {
        Layout {
            vendor: util::workspace_root().join("vendor"),
        }
    }

    /// Download cache: `vendor/dl/pdfium-win-x64.tgz`.
    pub fn archive(&self) -> PathBuf {
        self.vendor.join("dl").join("pdfium-win-x64.tgz")
    }

    /// Unpacked release: `vendor/pdfium/8125`.
    pub fn install_dir(&self) -> PathBuf {
        self.vendor.join("pdfium").join(PDFIUM_VERSION)
    }

    pub fn dll(&self) -> PathBuf {
        self.install_dir().join("bin").join("pdfium.dll")
    }
}

/// Checks an unpacked release directory: the DLL must have the pinned digest and the headers
/// must be there.
pub fn validate_install(dir: &Path, release: &Release) -> Result<()> {
    let dll = dir.join("bin").join("pdfium.dll");
    if !dll.is_file() {
        bail!("{} is missing", dll.display());
    }
    verify_file_sha256(&dll, release.dll_sha256)?;
    let header = dir.join("include").join("fpdfview.h");
    if !header.is_file() {
        bail!("{} is missing", header.display());
    }
    Ok(())
}

/// What `fetch` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// A valid install was already there; nothing was downloaded or unpacked.
    AlreadyPresent,
    /// Unpacked from a cached archive that verified; nothing was downloaded.
    UnpackedFromCache,
    /// Downloaded, verified and unpacked.
    Downloaded,
}

/// Downloads `url` to `dest` with the curl that ships with Windows.
pub fn curl_download(url: &str, dest: &Path) -> Result<()> {
    util::run(
        Command::new(system_tool("curl.exe"))
            .args(["--fail", "--location", "--silent", "--show-error"])
            .args(["--retry", "3", "--proto", "=https", "--output"])
            .arg(dest)
            .arg(url),
    )
}

/// Makes sure a verified archive is in the download cache. Returns true when it had to be
/// downloaded. A download whose digest is wrong is deleted and reported as an error.
fn ensure_archive(
    layout: &Layout,
    release: &Release,
    download: &dyn Fn(&str, &Path) -> Result<()>,
) -> Result<bool> {
    let archive = layout.archive();
    if archive.is_file() {
        if sha256_file(&archive)?.eq_ignore_ascii_case(release.archive_sha256) {
            return Ok(false);
        }
        println!(
            "cached {} does not match the pinned SHA-256; downloading again",
            archive.display()
        );
    }
    let dl_dir = archive.parent().expect("archive path has a parent");
    std::fs::create_dir_all(dl_dir)
        .with_context(|| format!("cannot create {}", dl_dir.display()))?;
    let part = archive.with_extension("tgz.part");
    let _ = std::fs::remove_file(&part);
    println!("downloading {}", release.url);
    let fetched = download(release.url, &part)
        .with_context(|| format!("download of {} failed", release.url))
        .and_then(|()| {
            let actual = sha256_file(&part)?;
            check_sha256(release.url, &actual, release.archive_sha256)
        });
    if let Err(err) = fetched {
        let _ = std::fs::remove_file(&part);
        return Err(err);
    }
    let _ = std::fs::remove_file(&archive);
    std::fs::rename(&part, &archive)
        .with_context(|| format!("cannot move the download to {}", archive.display()))?;
    Ok(true)
}

/// Unpacks a gzip tar archive into an existing directory with the tar that ships with Windows.
pub fn untar(archive: &Path, dest: &Path) -> Result<()> {
    util::run(
        Command::new(system_tool("tar.exe"))
            .arg("-xzf")
            .arg(archive)
            .arg("-C")
            .arg(dest),
    )
}

/// Brings `layout.install_dir()` to a verified copy of `release`.
///
/// Order: skip if already valid (unless `force`); reuse or download the archive and verify its
/// digest; unpack beside the target; verify the unpacked DLL; only then replace the target.
pub fn fetch(
    layout: &Layout,
    release: &Release,
    force: bool,
    download: &dyn Fn(&str, &Path) -> Result<()>,
) -> Result<Outcome> {
    let install = layout.install_dir();
    if !force {
        match validate_install(&install, release) {
            Ok(()) => return Ok(Outcome::AlreadyPresent),
            Err(err) if install.exists() => {
                println!("existing install is not valid ({err:#}); replacing it");
            }
            Err(_) => {}
        }
    }

    let downloaded = ensure_archive(layout, release, download)?;

    let parent = install.parent().expect("install path has a parent");
    std::fs::create_dir_all(parent)
        .with_context(|| format!("cannot create {}", parent.display()))?;
    let staging = parent.join(format!("{PDFIUM_VERSION}.unpacking"));
    if staging.exists() {
        std::fs::remove_dir_all(&staging)
            .with_context(|| format!("cannot clear {}", staging.display()))?;
    }
    std::fs::create_dir_all(&staging)
        .with_context(|| format!("cannot create {}", staging.display()))?;
    let unpacked = untar(&layout.archive(), &staging)
        .context("unpacking the PDFium archive failed")
        .and_then(|()| {
            validate_install(&staging, release).context("the unpacked archive is not as pinned")
        });
    if let Err(err) = unpacked {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(err);
    }
    if install.exists() {
        std::fs::remove_dir_all(&install).with_context(|| {
            format!(
                "cannot remove the old {} (is pdfium.dll in use by a running program?)",
                install.display()
            )
        })?;
    }
    std::fs::rename(&staging, &install)
        .with_context(|| format!("cannot move the unpacked files to {}", install.display()))?;
    Ok(if downloaded {
        Outcome::Downloaded
    } else {
        Outcome::UnpackedFromCache
    })
}

/// The `fetch-pdfium` subcommand.
pub fn run(force: bool) -> Result<()> {
    let layout = Layout::in_workspace();
    let outcome = fetch(&layout, &PDFIUM_8125, force, &curl_download)?;
    let dir = layout.install_dir();
    match outcome {
        Outcome::AlreadyPresent => println!(
            "PDFium {PDFIUM_VERSION} already present and valid in {}",
            dir.display()
        ),
        Outcome::UnpackedFromCache => println!(
            "PDFium {PDFIUM_VERSION} unpacked from the verified cached archive into {}",
            dir.display()
        ),
        Outcome::Downloaded => println!(
            "PDFium {PDFIUM_VERSION} downloaded, verified and unpacked into {}",
            dir.display()
        ),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::util::sha256_hex;
    use crate::util::testdir::TestDir;
    use std::cell::Cell;

    const DLL_BYTES: &[u8] = b"not a real dll, only bytes with a known digest";

    /// Builds a small .tgz shaped like the real release and returns its path.
    fn make_archive(dir: &Path) -> PathBuf {
        let src = dir.join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::create_dir_all(src.join("include")).unwrap();
        std::fs::write(src.join("bin").join("pdfium.dll"), DLL_BYTES).unwrap();
        std::fs::write(src.join("include").join("fpdfview.h"), b"// header\n").unwrap();
        std::fs::write(src.join("LICENSE"), b"licence text\n").unwrap();
        let archive = dir.join("source.tgz");
        util::run(
            Command::new(system_tool("tar.exe"))
                .arg("-czf")
                .arg(&archive)
                .arg("-C")
                .arg(&src)
                .args(["bin", "include", "LICENSE"]),
        )
        .unwrap();
        archive
    }

    /// A `Release` with digests computed at run time (leaked: tests only).
    fn release_for(archive: &Path) -> Release {
        let leak = |s: String| -> &'static str { Box::leak(s.into_boxed_str()) };
        Release {
            url: "https://example.invalid/pdfium-win-x64.tgz",
            archive_sha256: leak(sha256_file(archive).unwrap()),
            dll_sha256: leak(sha256_hex(DLL_BYTES)),
        }
    }

    fn layout(dir: &Path) -> Layout {
        Layout {
            vendor: dir.join("vendor"),
        }
    }

    fn never(_: &str, _: &Path) -> Result<()> {
        panic!("the downloader must not be called here")
    }

    #[test]
    fn pinned_release_is_the_one_in_the_plan() {
        assert_eq!(
            PDFIUM_8125.archive_sha256,
            "9a964261760cff52570aabf038ab346d3a5034dd8b30d0d5fe8861235d356b8b"
        );
        assert!(PDFIUM_8125.url.starts_with("https://github.com/bblanchon/pdfium-binaries/"));
        assert!(PDFIUM_8125.url.ends_with("/chromium/8125/pdfium-win-x64.tgz"));
        assert_eq!(PDFIUM_8125.dll_sha256.len(), 64);
        let l = Layout::in_workspace();
        assert!(l.install_dir().ends_with("vendor/pdfium/8125"));
        assert!(l.dll().ends_with("bin/pdfium.dll"));
    }

    #[test]
    fn downloads_verifies_unpacks_then_skips() {
        let dir = TestDir::new("fetch-ok");
        let source = make_archive(dir.path());
        let release = release_for(&source);
        let layout = layout(dir.path());
        let calls = Cell::new(0);
        let download = |url: &str, dest: &Path| -> Result<()> {
            assert_eq!(url, release.url);
            calls.set(calls.get() + 1);
            std::fs::copy(&source, dest)?;
            Ok(())
        };

        assert_eq!(fetch(&layout, &release, false, &download).unwrap(), Outcome::Downloaded);
        assert_eq!(calls.get(), 1);
        assert_eq!(std::fs::read(layout.dll()).unwrap(), DLL_BYTES);
        assert!(layout.install_dir().join("include").join("fpdfview.h").is_file());
        assert!(layout.install_dir().join("LICENSE").is_file());
        assert!(layout.archive().is_file());
        assert!(!layout.archive().with_extension("tgz.part").exists());
        assert!(!layout.vendor.join("pdfium").join("8125.unpacking").exists());

        // Second run: present and valid, so neither download nor unpack happens.
        assert_eq!(fetch(&layout, &release, false, &never).unwrap(), Outcome::AlreadyPresent);
        assert_eq!(calls.get(), 1);
    }

    #[test]
    fn hash_mismatch_fails_loudly_and_installs_nothing() {
        let dir = TestDir::new("fetch-bad");
        let source = make_archive(dir.path());
        let release = release_for(&source);
        let layout = layout(dir.path());
        let download = |_: &str, dest: &Path| -> Result<()> {
            let mut bytes = std::fs::read(&source)?;
            let last = bytes.len() - 1;
            bytes[last] ^= 0x01; // one flipped bit
            std::fs::write(dest, bytes)?;
            Ok(())
        };

        let err = fetch(&layout, &release, false, &download).unwrap_err();
        let text = format!("{err:#}");
        assert!(text.contains("SHA-256 MISMATCH"), "{text}");
        assert!(text.contains(release.archive_sha256), "{text}");
        assert!(!layout.archive().exists(), "a bad download must not be cached");
        assert!(!layout.archive().with_extension("tgz.part").exists());
        assert!(!layout.install_dir().exists(), "a bad download must not be unpacked");
    }

    #[test]
    fn failed_download_is_an_error_and_leaves_nothing() {
        let dir = TestDir::new("fetch-offline");
        let source = make_archive(dir.path());
        let release = release_for(&source);
        let layout = layout(dir.path());
        let download = |_: &str, dest: &Path| -> Result<()> {
            std::fs::write(dest, b"partial")?;
            bail!("curl exited with 22")
        };
        let text = format!("{:#}", fetch(&layout, &release, false, &download).unwrap_err());
        assert!(text.contains("curl exited with 22"), "{text}");
        assert!(!layout.archive().exists());
        assert!(!layout.archive().with_extension("tgz.part").exists());
        assert!(!layout.install_dir().exists());
    }

    #[test]
    fn tampered_install_is_replaced_from_the_cached_archive() {
        let dir = TestDir::new("fetch-tamper");
        let source = make_archive(dir.path());
        let release = release_for(&source);
        let layout = layout(dir.path());
        let download = |_: &str, dest: &Path| -> Result<()> {
            std::fs::copy(&source, dest)?;
            Ok(())
        };
        fetch(&layout, &release, false, &download).unwrap();

        std::fs::write(layout.dll(), b"tampered").unwrap();
        assert!(validate_install(&layout.install_dir(), &release).is_err());
        assert_eq!(
            fetch(&layout, &release, false, &never).unwrap(),
            Outcome::UnpackedFromCache
        );
        assert_eq!(std::fs::read(layout.dll()).unwrap(), DLL_BYTES);
    }

    #[test]
    fn corrupt_cached_archive_is_downloaded_again() {
        let dir = TestDir::new("fetch-cache");
        let source = make_archive(dir.path());
        let release = release_for(&source);
        let layout = layout(dir.path());
        std::fs::create_dir_all(layout.archive().parent().unwrap()).unwrap();
        std::fs::write(layout.archive(), b"truncated").unwrap();
        let download = |_: &str, dest: &Path| -> Result<()> {
            std::fs::copy(&source, dest)?;
            Ok(())
        };
        assert_eq!(fetch(&layout, &release, false, &download).unwrap(), Outcome::Downloaded);
        verify_file_sha256(&layout.archive(), release.archive_sha256).unwrap();
    }

    #[test]
    fn force_unpacks_again_even_when_valid() {
        let dir = TestDir::new("fetch-force");
        let source = make_archive(dir.path());
        let release = release_for(&source);
        let layout = layout(dir.path());
        let download = |_: &str, dest: &Path| -> Result<()> {
            std::fs::copy(&source, dest)?;
            Ok(())
        };
        fetch(&layout, &release, false, &download).unwrap();
        let marker = layout.install_dir().join("stale.txt");
        std::fs::write(&marker, b"x").unwrap();
        assert_eq!(fetch(&layout, &release, true, &never).unwrap(), Outcome::UnpackedFromCache);
        assert!(!marker.exists());
        validate_install(&layout.install_dir(), &release).unwrap();
    }

    #[test]
    fn archive_with_the_wrong_dll_is_refused_after_unpack() {
        let dir = TestDir::new("fetch-wrongdll");
        let source = make_archive(dir.path());
        let mut release = release_for(&source);
        release.dll_sha256 = "0000000000000000000000000000000000000000000000000000000000000000";
        let layout = layout(dir.path());
        let download = |_: &str, dest: &Path| -> Result<()> {
            std::fs::copy(&source, dest)?;
            Ok(())
        };
        let text = format!("{:#}", fetch(&layout, &release, false, &download).unwrap_err());
        assert!(text.contains("SHA-256 MISMATCH"), "{text}");
        assert!(!layout.install_dir().exists());
        assert!(!layout.vendor.join("pdfium").join("8125.unpacking").exists());
    }

    /// The vendored copy, when present, must be exactly the pinned build.
    #[test]
    fn vendored_install_matches_the_pin() {
        let layout = Layout::in_workspace();
        if !layout.dll().is_file() {
            eprintln!("skipped: {} is absent (run fetch-pdfium)", layout.dll().display());
            return;
        }
        validate_install(&layout.install_dir(), &PDFIUM_8125).unwrap();
        if layout.archive().is_file() {
            verify_file_sha256(&layout.archive(), PDFIUM_8125.archive_sha256).unwrap();
        }
    }
}

//! Small shared helpers: workspace paths, system tools, hashing.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use sha2::{Digest, Sha256};

/// Root of the cargo workspace (two levels above this crate's manifest).
pub fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("xtask lives in <root>/crates/xtask")
        .to_path_buf()
}

/// The cargo that is running this xtask, or plain `cargo` from the search path.
pub fn cargo() -> Command {
    Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
}

/// A tool that ships with Windows (`curl.exe`, `tar.exe`).
///
/// The copy in `%SystemRoot%\System32` is preferred over whatever is first on the search path:
/// a Git Bash `tar` earlier on the path reads `C:\...` as a remote host name.
pub fn system_tool(name: &str) -> PathBuf {
    if let Some(root) = std::env::var_os("SystemRoot") {
        let candidate = Path::new(&root).join("System32").join(name);
        if candidate.is_file() {
            return candidate;
        }
    }
    PathBuf::from(name)
}

/// Runs a command to completion and fails with its name when it cannot start or exits non-zero.
pub fn run(cmd: &mut Command) -> Result<()> {
    let shown = format!("{cmd:?}");
    let status = cmd
        .status()
        .with_context(|| format!("could not start {shown}"))?;
    if !status.success() {
        bail!("{shown} failed with {status}");
    }
    Ok(())
}

pub fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Lower-case hex SHA-256 of a byte slice.
#[cfg(test)]
pub fn sha256_hex(bytes: &[u8]) -> String {
    to_hex(Sha256::digest(bytes).as_slice())
}

/// Lower-case hex SHA-256 of a file, read in chunks.
pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path).with_context(|| format!("cannot open {}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file
            .read(&mut buf)
            .with_context(|| format!("cannot read {}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(to_hex(hasher.finalize().as_slice()))
}

/// Compares a computed digest with the pinned one. `what` names the thing in the error.
pub fn check_sha256(what: &str, actual: &str, expected: &str) -> Result<()> {
    if actual.eq_ignore_ascii_case(expected.trim()) {
        return Ok(());
    }
    bail!(
        "SHA-256 MISMATCH for {what}\n  expected {expected}\n  actual   {actual}\n\
         The file is corrupt, truncated or not the pinned release. It will not be used."
    )
}

/// Hashes a file and compares it with the pinned digest.
pub fn verify_file_sha256(path: &Path, expected: &str) -> Result<()> {
    let actual = sha256_file(path)?;
    check_sha256(&path.display().to_string(), &actual, expected)
}

#[cfg(test)]
pub mod testdir {
    //! Throwaway directories for tests, removed on drop.
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU32, Ordering};

    static NEXT: AtomicU32 = AtomicU32::new(0);

    pub struct TestDir(PathBuf);

    impl TestDir {
        pub fn new(tag: &str) -> Self {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let dir = std::env::temp_dir()
                .join(format!("plumbline-xtask-{tag}-{}-{n}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            TestDir(dir)
        }

        pub fn path(&self) -> &Path {
            &self.0
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // FIPS 180-2 test vectors.
    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";

    #[test]
    fn sha256_known_vectors() {
        assert_eq!(sha256_hex(b""), EMPTY);
        assert_eq!(sha256_hex(b"abc"), ABC);
    }

    #[test]
    fn file_hash_equals_slice_hash_across_chunk_boundary() {
        let dir = testdir::TestDir::new("hash");
        let path = dir.path().join("blob.bin");
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &data).unwrap();
        assert_eq!(sha256_file(&path).unwrap(), sha256_hex(&data));
    }

    #[test]
    fn matching_hash_passes_in_either_case() {
        check_sha256("abc", ABC, ABC).unwrap();
        check_sha256("abc", ABC, &ABC.to_uppercase()).unwrap();
    }

    #[test]
    fn mismatched_hash_fails_and_names_both_digests() {
        let err = check_sha256("thing.tgz", ABC, EMPTY)
            .unwrap_err()
            .to_string();
        assert!(err.contains("SHA-256 MISMATCH"), "{err}");
        assert!(err.contains("thing.tgz"), "{err}");
        assert!(err.contains(ABC) && err.contains(EMPTY), "{err}");
    }

    #[test]
    fn verify_file_detects_one_flipped_byte() {
        let dir = testdir::TestDir::new("verify");
        let path = dir.path().join("a.bin");
        std::fs::write(&path, b"abc").unwrap();
        verify_file_sha256(&path, ABC).unwrap();
        std::fs::write(&path, b"abd").unwrap();
        assert!(verify_file_sha256(&path, ABC).is_err());
    }

    #[test]
    fn missing_file_is_an_error_not_a_pass() {
        let dir = testdir::TestDir::new("missing");
        assert!(verify_file_sha256(&dir.path().join("nope"), EMPTY).is_err());
    }

    #[test]
    fn workspace_root_holds_the_workspace_manifest() {
        assert!(workspace_root().join("Cargo.toml").is_file());
        assert!(workspace_root().join("crates").join("xtask").is_dir());
    }
}

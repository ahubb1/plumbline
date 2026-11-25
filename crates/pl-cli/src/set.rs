//! Sets: a `.plset` file that names a group of PDF files (a drawing set) so they can be listed
//! and searched together.
//!
//! The set is pl-doc's [`DrawingSet`] (the same file the app opens and writes; its format is
//! described in `pl_doc::sets`), and searching it is pl-doc's [`SetSearch`](pl_doc::SetSearch).
//! This module only adapts them to the command line: whether a file was new to the set, the
//! paths as the set file writes them, and the matched text of each hit.

use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use pl_doc::pl_render::SearchOptions;
use pl_doc::{DrawingSet, Session};

/// A path as a key for "is this the same file": absolute, with `\` and case folded on
/// Windows.
pub fn path_key(path: &Path) -> String {
    let abs = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let s = abs.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        s.to_lowercase()
    } else {
        s
    }
}

/// A drawing set as the command line uses it.
#[derive(Debug, Clone)]
pub struct SetFile {
    pub set: DrawingSet,
}

impl SetFile {
    pub fn new(name: &str) -> SetFile {
        SetFile {
            set: DrawingSet::new(name),
        }
    }

    pub fn read(path: &Path) -> Result<SetFile> {
        let set = DrawingSet::open(path).with_context(|| format!("read {}", path.display()))?;
        Ok(SetFile { set })
    }

    /// Writes the set (through a temporary file, replaced atomically).
    pub fn write(&mut self, path: &Path) -> Result<()> {
        self.set
            .save(path)
            .with_context(|| format!("write {}", path.display()))
    }

    pub fn len(&self) -> usize {
        self.set.len()
    }

    /// Every file of the set as a path to open.
    pub fn resolved(&self) -> Vec<PathBuf> {
        self.set.files().iter().map(|f| f.path.clone()).collect()
    }

    /// A file of the set as the set file names it: relative to the set's folder where it can
    /// be, with `/`.
    pub fn shown(&self, index: usize, set_path: &Path) -> String {
        let Some(file) = self.set.file(index) else {
            return String::new();
        };
        let base = std::path::absolute(set_path)
            .ok()
            .and_then(|p| p.parent().map(Path::to_path_buf))
            .unwrap_or_default();
        pl_doc::relative_path(&file.path, &base)
            .unwrap_or_else(|| file.path.clone())
            .to_string_lossy()
            .replace('\\', "/")
    }

    /// Adds `file` (a path as the user gave it); returns false when the set already has it.
    /// The file must exist and be a PDF.
    pub fn add(&mut self, file: &Path) -> Result<bool> {
        let abs =
            std::path::absolute(file).with_context(|| format!("resolve {}", file.display()))?;
        if !abs.is_file() {
            bail!("{} is not a file", file.display());
        }
        if self.set.position(&abs).is_some() {
            return Ok(false);
        }
        self.set
            .add_file(&abs)
            .with_context(|| format!("add {}", file.display()))?;
        Ok(true)
    }
}

/// One search hit in a set.
#[derive(Debug, Clone, PartialEq)]
pub struct SetHit {
    /// Index into the set's files.
    pub file: usize,
    pub path: PathBuf,
    /// Zero-based.
    pub page: u32,
    pub bounds: [f64; 4],
    /// What was matched, on one line.
    pub text: String,
}

/// What a set search found.
#[derive(Debug, Default)]
pub struct SetSearch {
    pub hits: Vec<SetHit>,
    /// Files that could not be searched, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    pub files: usize,
}

/// Searches every file of the set in turn (pl-doc's [`pl_doc::SetSearch`]). A file that is
/// missing or cannot be read is reported and skipped; the others are still searched. The
/// matched text of each hit is read from its page.
pub fn search(
    session: &mut Session,
    set: &SetFile,
    query: &str,
    options: SearchOptions,
    password: Option<&str>,
) -> Result<SetSearch> {
    if query.trim().is_empty() {
        bail!("the search text is empty");
    }
    let mut search = pl_doc::SetSearch::new(session.backend().clone(), &set.set, query, options);
    let found = search.run().to_vec();
    let failed = search
        .skipped()
        .iter()
        .map(|(file, why)| {
            (
                set.set
                    .file(*file)
                    .map(|f| f.path.clone())
                    .unwrap_or_default(),
                why.clone(),
            )
        })
        .collect();
    drop(search);
    let mut result = SetSearch {
        files: set.len(),
        failed,
        ..SetSearch::default()
    };
    // The matched words, a file at a time.
    let mut start = 0;
    while start < found.len() {
        let file = found[start].file;
        let end = start + found[start..].iter().take_while(|h| h.file == file).count();
        let path = found[start].path.clone();
        let texts: Vec<String> = match session.open(&path, password) {
            Ok(id) => {
                let texts = found[start..end]
                    .iter()
                    .map(|hit| {
                        session
                            .document_mut(id)
                            .ok()
                            .and_then(|doc| doc.page_text(hit.page).ok())
                            .and_then(|text| pl_doc::select_hit(&text, &hit.hit))
                            .map(|s| s.text.split_whitespace().collect::<Vec<_>>().join(" "))
                            .unwrap_or_default()
                    })
                    .collect();
                let _ = session.close(id);
                texts
            }
            Err(_) => vec![String::new(); end - start],
        };
        for (hit, text) in found[start..end].iter().zip(texts) {
            result.hits.push(SetHit {
                file: hit.file,
                path: hit.path.clone(),
                page: hit.page,
                bounds: pl_doc::quads_bounds(&hit.hit.quads).unwrap_or([0.0; 4]),
                text,
            });
        }
        start = end;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_set_file_round_trips_and_keeps_relative_paths() {
        let dir = tempfile::tempdir().unwrap();
        let set_path = dir.path().join("plans.plset");
        let sub = dir.path().join("sub");
        std::fs::create_dir(&sub).unwrap();
        let a = dir.path().join("a.pdf");
        let b = sub.join("b.pdf");
        let (fixture, _) = pl_testkit::structure_source();
        for p in [&a, &b] {
            std::fs::write(p, &fixture.bytes).unwrap();
        }
        let mut set = SetFile::new("Plans");
        assert!(set.add(&a).unwrap());
        assert!(set.add(&b).unwrap());
        assert!(!set.add(&a).unwrap(), "the same file is not added twice");
        assert!(set.add(&dir.path().join("none.pdf")).is_err());
        let text_file = dir.path().join("notes.txt");
        std::fs::write(&text_file, b"hello").unwrap();
        assert!(set.add(&text_file).is_err(), "not a PDF");

        set.write(&set_path).unwrap();
        assert_eq!(set.shown(0, &set_path), "a.pdf");
        assert_eq!(set.shown(1, &set_path), "sub/b.pdf");
        let back = SetFile::read(&set_path).unwrap();
        assert_eq!(back.len(), 2);
        assert_eq!(back.set.name, "Plans");
        let resolved = back.resolved();
        assert_eq!(path_key(&resolved[1]), path_key(&b));
        // The file is the one the app reads.
        let text = std::fs::read_to_string(&set_path).unwrap();
        assert!(text.contains("\"sub/b.pdf\""), "{text}");

        std::fs::write(&set_path, r#"{"format": 9, "name": "x", "files": []}"#).unwrap();
        assert!(SetFile::read(&set_path).is_err());
        std::fs::write(&set_path, "not json").unwrap();
        assert!(SetFile::read(&set_path).is_err());
    }
}

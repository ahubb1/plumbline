//! Drawing sets: an ordered list of PDF files with per-sheet metadata, kept as a small JSON
//! `.plset` file, sheet-by-sheet navigation across the files, and text search over the whole set.
//!
//! # The file
//!
//! A `.plset` is UTF-8 JSON:
//!
//! ```json
//! { "format": 1, "name": "Site works", "files": [
//!     { "path": "civil/C-101.pdf", "sheets": [ { "number": "C-101", "title": "Site plan" } ] }
//! ] }
//! ```
//!
//! Paths are written relative to the folder of the `.plset` when the PDF is on the same drive
//! (so a set can be moved or shared with its folder), absolute otherwise. A set can be saved
//! anywhere; it is written through a temporary file and replaced atomically.
//!
//! # Missing files
//!
//! A file that is not there when the set is opened stays in the set, marked
//! [`SetFile::missing`], with the sheets it was saved with. Navigation passes over it, set
//! search reports it as skipped, and [`DrawingSet::relink`] points it at the file's new place.
//!
//! # Sheets
//!
//! Each page of a file is one sheet. When a file is added its sheets are worked out from the
//! PDF ([`sheets_for`]): the sheet number is the page label when the file has labels, else
//! the file name (for a single page) or the file name with the page number; the title is the
//! file name. Both can be edited ([`DrawingSet::set_sheet`]).
//!
//! # Search
//!
//! [`SetSearch`] searches every file in set order through the render backend (PDFium's text,
//! as a document search does). Files are opened one at a time when the search reaches them
//! and closed when it leaves them; a file already open in a [`Session`] is searched through
//! its own display, so its unsaved page edits count. It is stepped a few pages at a time
//! ([`SetSearch::step`]) and hands back the new hits each time, or runs on a worker thread
//! ([`spawn_set_search`]) and streams them; either can be cancelled.

use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver, TryRecvError};
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use pl_geom::Rect;
use pl_render::{DocId, DocSource, RenderBackend, SearchHit, SearchOptions};

use crate::error::{DocError, Result};
use crate::search::DocSearch;
use crate::session::Session;

/// The extension of drawing-set files.
pub const SET_EXTENSION: &str = "plset";
/// The version of the `.plset` format this code writes.
pub const SET_FORMAT: u32 = 1;

/// One sheet: a page of one of the set's files.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetInfo {
    pub number: String,
    pub title: String,
}

/// One file of a set.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SetFile {
    /// Absolute.
    pub path: PathBuf,
    /// One per page.
    pub sheets: Vec<SheetInfo>,
    /// The file was not there when last looked for.
    pub missing: bool,
}

/// A sheet's place in a set: a file (index in the set) and a page of it (zero-based).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SheetRef {
    pub file: usize,
    pub page: u32,
}

#[derive(Serialize, Deserialize)]
struct FileOnDisk {
    path: String,
    #[serde(default)]
    sheets: Vec<SheetInfo>,
}

#[derive(Serialize, Deserialize)]
struct SetOnDisk {
    format: u32,
    #[serde(default)]
    name: String,
    files: Vec<FileOnDisk>,
}

/// An ordered list of PDF files with their sheets (see the module documentation).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct DrawingSet {
    pub name: String,
    files: Vec<SetFile>,
    /// Where the set was last opened from or saved to.
    location: Option<PathBuf>,
}

fn absolute(path: &Path) -> Result<PathBuf> {
    std::path::absolute(path).map_err(|e| DocError::io("resolve", path, e))
}

/// `path` relative to `base` (both absolute), when they share a root (drive); `None` otherwise.
pub fn relative_path(path: &Path, base: &Path) -> Option<PathBuf> {
    let key = |c: &Component<'_>| c.as_os_str().to_string_lossy().to_lowercase();
    let p: Vec<Component<'_>> = path.components().collect();
    let b: Vec<Component<'_>> = base.components().collect();
    // The prefix (drive) and root must match.
    let roots = |v: &[Component<'_>]| {
        v.iter()
            .take_while(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
            .count()
    };
    let (rp, rb) = (roots(&p), roots(&b));
    if rp == 0 || rp != rb || p[..rp].iter().map(key).ne(b[..rb].iter().map(key)) {
        return None;
    }
    let mut common = rp;
    while common < p.len() && common < b.len() && key(&p[common]) == key(&b[common]) {
        common += 1;
    }
    let mut out = PathBuf::new();
    for c in &b[common..] {
        if matches!(c, Component::Normal(_)) {
            out.push("..");
        }
    }
    for c in &p[common..] {
        out.push(c.as_os_str());
    }
    Some(out)
}

fn stem(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default()
}

/// The sheets of a PDF file, worked out as the module documentation says. Parses the file
/// with lopdf (the empty password).
pub fn sheets_for(path: &Path) -> Result<Vec<SheetInfo>> {
    let mut file = pl_pdf::PdfFile::open(path)?;
    let doc = file.document()?;
    Ok(sheets_from(path, doc.page_count(), doc.page_labels()))
}

fn sheets_from(
    path: &Path,
    pages: usize,
    labels: Option<Vec<pl_pdf::PageLabel>>,
) -> Vec<SheetInfo> {
    let name = stem(path);
    (0..pages)
        .map(|i| {
            let label = labels
                .as_ref()
                .and_then(|l| l.get(i))
                .map(|l| l.text())
                .filter(|t| !t.trim().is_empty());
            let number = match label {
                Some(label) => label,
                None if pages == 1 => name.clone(),
                None => format!("{name}-{}", i + 1),
            };
            SheetInfo {
                number,
                title: name.clone(),
            }
        })
        .collect()
}

impl DrawingSet {
    pub fn new(name: impl Into<String>) -> DrawingSet {
        DrawingSet {
            name: name.into(),
            files: Vec::new(),
            location: None,
        }
    }

    /// Reads a `.plset`. Relative paths are taken from the set's folder; files that are not
    /// there are kept and marked missing.
    pub fn open(path: impl AsRef<Path>) -> Result<DrawingSet> {
        let path = absolute(path.as_ref())?;
        let text = std::fs::read_to_string(&path).map_err(|e| DocError::io("read", &path, e))?;
        let disk: SetOnDisk = serde_json::from_str(&text)
            .map_err(|e| DocError::Script(format!("{}: not a drawing set: {e}", path.display())))?;
        if disk.format > SET_FORMAT {
            return Err(DocError::Script(format!(
                "{}: drawing-set format {} is newer than this version reads ({SET_FORMAT})",
                path.display(),
                disk.format
            )));
        }
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let files = disk
            .files
            .into_iter()
            .map(|f| {
                let stored = PathBuf::from(&f.path);
                let full = if stored.is_absolute() {
                    stored
                } else {
                    folder.join(stored)
                };
                let full = std::path::absolute(&full).unwrap_or(full);
                let full = normalise(&full);
                SetFile {
                    missing: !full.is_file(),
                    path: full,
                    sheets: f.sheets,
                }
            })
            .collect();
        Ok(DrawingSet {
            name: disk.name,
            files,
            location: Some(path),
        })
    }

    /// Writes the set to `path` (relative paths where possible; see the module documentation)
    /// and remembers it as the set's place.
    pub fn save(&mut self, path: impl AsRef<Path>) -> Result<()> {
        let path = absolute(path.as_ref())?;
        let folder = path.parent().map(Path::to_path_buf).unwrap_or_default();
        let disk = SetOnDisk {
            format: SET_FORMAT,
            name: self.name.clone(),
            files: self
                .files
                .iter()
                .map(|f| FileOnDisk {
                    path: relative_path(&f.path, &folder)
                        .unwrap_or_else(|| f.path.clone())
                        .to_string_lossy()
                        .replace('\\', "/"),
                    sheets: f.sheets.clone(),
                })
                .collect(),
        };
        let text = serde_json::to_string_pretty(&disk)
            .map_err(|e| DocError::Script(format!("the set could not be written: {e}")))?;
        let tmp = pl_pdf::tmp_path_for(&path);
        std::fs::write(&tmp, text.as_bytes()).map_err(|e| DocError::io("write", &tmp, e))?;
        if let Err(e) = pl_pdf::replace_file(&tmp, &path) {
            let _ = std::fs::remove_file(&tmp);
            return Err(e.into());
        }
        self.location = Some(path);
        Ok(())
    }

    /// Where the set was opened from or last saved to.
    pub fn location(&self) -> Option<&Path> {
        self.location.as_deref()
    }

    pub fn files(&self) -> &[SetFile] {
        &self.files
    }

    pub fn file(&self, index: usize) -> Option<&SetFile> {
        self.files.get(index)
    }

    pub fn len(&self) -> usize {
        self.files.len()
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The index of a file of the set, compared without regard to case or slash direction.
    pub fn position(&self, path: &Path) -> Option<usize> {
        let path = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
        self.files
            .iter()
            .position(|f| crate::journal::same_path(&f.path, &path))
    }

    /// Adds a PDF at the end with its sheets worked out from the file ([`sheets_for`]).
    /// Returns its index. A file already in the set is not added twice.
    pub fn add_file(&mut self, path: impl AsRef<Path>) -> Result<usize> {
        let at = self.files.len();
        self.insert_file(at, path)
    }

    /// As [`add_file`](DrawingSet::add_file), at position `index`.
    pub fn insert_file(&mut self, index: usize, path: impl AsRef<Path>) -> Result<usize> {
        let path = absolute(path.as_ref())?;
        if let Some(i) = self.position(&path) {
            return Ok(i);
        }
        let sheets = sheets_for(&path)?;
        self.insert_with_sheets(index, path, sheets)
    }

    /// Adds a file with sheets the caller already has (no file is read). The file need not
    /// exist; it is then marked missing.
    pub fn insert_with_sheets(
        &mut self,
        index: usize,
        path: impl AsRef<Path>,
        sheets: Vec<SheetInfo>,
    ) -> Result<usize> {
        let path = absolute(path.as_ref())?;
        if let Some(i) = self.position(&path) {
            return Ok(i);
        }
        let index = index.min(self.files.len());
        self.files.insert(
            index,
            SetFile {
                missing: !path.is_file(),
                path,
                sheets,
            },
        );
        Ok(index)
    }

    pub fn remove_file(&mut self, index: usize) -> Option<SetFile> {
        (index < self.files.len()).then(|| self.files.remove(index))
    }

    /// Moves the file at `from` so it is at `to` afterwards. Returns whether anything moved.
    pub fn move_file(&mut self, from: usize, to: usize) -> bool {
        if from >= self.files.len() || to >= self.files.len() || from == to {
            return false;
        }
        let f = self.files.remove(from);
        self.files.insert(to, f);
        true
    }

    /// Edits a sheet's number and title. Returns whether the sheet exists.
    pub fn set_sheet(&mut self, sheet: SheetRef, info: SheetInfo) -> bool {
        match self
            .files
            .get_mut(sheet.file)
            .and_then(|f| f.sheets.get_mut(sheet.page as usize))
        {
            Some(s) => {
                *s = info;
                true
            }
            None => false,
        }
    }

    /// Replaces every sheet of file `index`. Returns whether the file exists.
    pub fn set_sheets(&mut self, index: usize, sheets: Vec<SheetInfo>) -> bool {
        match self.files.get_mut(index) {
            Some(f) => {
                f.sheets = sheets;
                true
            }
            None => false,
        }
    }

    pub fn sheet(&self, sheet: SheetRef) -> Option<&SheetInfo> {
        self.files
            .get(sheet.file)
            .and_then(|f| f.sheets.get(sheet.page as usize))
    }

    /// Works a file's sheets out again from the PDF (after its pages changed). Numbers and
    /// titles edited by hand are replaced by the derived ones.
    pub fn refresh_sheets(&mut self, index: usize) -> Result<()> {
        let file = self
            .files
            .get(index)
            .ok_or_else(|| DocError::Script(format!("the set has no file {index}")))?;
        let fresh = sheets_for(&file.path)?;
        let file = &mut self.files[index];
        file.sheets = fresh;
        file.missing = false;
        Ok(())
    }

    /// Makes file `index`'s sheets follow a page operation on that file (`map`: where each
    /// page went): a sheet moves with its page, numbers and titles edited by hand included; a
    /// removed page's sheet goes; a new page gets a sheet as a new file's pages do. Returns
    /// false (and changes nothing) when the map is not one of this file's pages.
    pub fn follow_pages(&mut self, index: usize, map: &crate::structure::PageMap) -> bool {
        let Some(file) = self.files.get_mut(index) else {
            return false;
        };
        if map.old_to_new.len() != file.sheets.len() {
            return false;
        }
        let name = stem(&file.path);
        let count = map.new_page_count as usize;
        let mut sheets: Vec<Option<SheetInfo>> = vec![None; count];
        for (old, new) in map.old_to_new.iter().enumerate() {
            if let Some(slot) = new.and_then(|n| sheets.get_mut(n as usize)) {
                *slot = Some(file.sheets[old].clone());
            }
        }
        file.sheets = sheets
            .into_iter()
            .enumerate()
            .map(|(i, s)| {
                s.unwrap_or_else(|| SheetInfo {
                    number: if count == 1 {
                        name.clone()
                    } else {
                        format!("{name}-{}", i + 1)
                    },
                    title: name.clone(),
                })
            })
            .collect();
        true
    }

    /// Looks for every file again and updates [`SetFile::missing`]. Returns how many are
    /// missing.
    pub fn check_files(&mut self) -> usize {
        for f in &mut self.files {
            f.missing = !f.path.is_file();
        }
        self.files.iter().filter(|f| f.missing).count()
    }

    /// Points file `index` at a new place (a moved or renamed file). Its sheets are kept when
    /// the new file has the same number of pages, and worked out afresh otherwise.
    pub fn relink(&mut self, index: usize, path: impl AsRef<Path>) -> Result<()> {
        let path = absolute(path.as_ref())?;
        if index >= self.files.len() {
            return Err(DocError::Script(format!("the set has no file {index}")));
        }
        let fresh = sheets_for(&path)?;
        let file = &mut self.files[index];
        if fresh.len() != file.sheets.len() {
            file.sheets = fresh;
        }
        file.path = path;
        file.missing = false;
        Ok(())
    }

    /// Every sheet in set order (missing files included).
    pub fn sheets(&self) -> impl Iterator<Item = (SheetRef, &SheetInfo)> + '_ {
        self.files.iter().enumerate().flat_map(|(file, f)| {
            f.sheets.iter().enumerate().map(move |(page, s)| {
                (
                    SheetRef {
                        file,
                        page: page as u32,
                    },
                    s,
                )
            })
        })
    }

    /// The sheet with this number (compared without regard to case), the first in set order.
    pub fn find_sheet(&self, number: &str) -> Option<SheetRef> {
        let want = number.trim().to_lowercase();
        self.sheets()
            .find(|(_, s)| s.number.trim().to_lowercase() == want)
            .map(|(r, _)| r)
    }

    fn reachable(&self, file: usize) -> bool {
        self.files
            .get(file)
            .is_some_and(|f| !f.missing && !f.sheets.is_empty())
    }

    /// The first sheet of the first file that is there.
    pub fn first_sheet(&self) -> Option<SheetRef> {
        (0..self.files.len())
            .find(|f| self.reachable(*f))
            .map(|file| SheetRef { file, page: 0 })
    }

    /// The last sheet of the last file that is there.
    pub fn last_sheet(&self) -> Option<SheetRef> {
        (0..self.files.len())
            .rev()
            .find(|f| self.reachable(*f))
            .map(|file| SheetRef {
                file,
                page: self.files[file].sheets.len() as u32 - 1,
            })
    }

    /// The sheet after `at`: the next page, or the first page of the next file that is there.
    /// `None` at the end of the set.
    pub fn next_sheet(&self, at: SheetRef) -> Option<SheetRef> {
        if self.reachable(at.file) && (at.page as usize + 1) < self.files[at.file].sheets.len() {
            return Some(SheetRef {
                file: at.file,
                page: at.page + 1,
            });
        }
        (at.file + 1..self.files.len())
            .find(|f| self.reachable(*f))
            .map(|file| SheetRef { file, page: 0 })
    }

    /// The sheet before `at`: the previous page, or the last page of the previous file that is
    /// there. `None` at the start of the set.
    pub fn prev_sheet(&self, at: SheetRef) -> Option<SheetRef> {
        if self.reachable(at.file) && at.page > 0 {
            let last = self.files[at.file].sheets.len() as u32 - 1;
            return Some(SheetRef {
                file: at.file,
                page: (at.page - 1).min(last),
            });
        }
        (0..at.file.min(self.files.len()))
            .rev()
            .find(|f| self.reachable(*f))
            .map(|file| SheetRef {
                file,
                page: self.files[file].sheets.len() as u32 - 1,
            })
    }
}

/// `.` and `..` resolved lexically.
fn normalise(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in path.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

// ---- search ------------------------------------------------------------------------------------

/// One hit of a set search.
#[derive(Clone, Debug, PartialEq)]
pub struct SetHit {
    /// The file's index in the set.
    pub file: usize,
    pub path: PathBuf,
    /// Zero-based page of that file.
    pub page: u32,
    /// The sheet's number and title, when the set has them.
    pub sheet: Option<SheetInfo>,
    /// The hit's rectangles in page space (one per line it covers).
    pub rects: Vec<Rect>,
    /// The hit as the backend gave it (character range and quads).
    pub hit: SearchHit,
}

impl SetHit {
    pub fn sheet_ref(&self) -> SheetRef {
        SheetRef {
            file: self.file,
            page: self.page,
        }
    }
}

struct Target {
    file: usize,
    path: PathBuf,
    sheets: Vec<SheetInfo>,
    missing: bool,
    /// The display of the file in a session, when it is open there.
    open: Option<DocId>,
}

struct Current {
    target: usize,
    doc: DocId,
    /// Opened by this search (and so closed by it).
    owned: bool,
    search: DocSearch,
    reported: usize,
}

/// A text search over every file of a set (see the module documentation).
pub struct SetSearch {
    backend: Arc<dyn RenderBackend>,
    query: String,
    options: SearchOptions,
    targets: Vec<Target>,
    next: usize,
    current: Option<Current>,
    hits: Vec<SetHit>,
    skipped: Vec<(usize, String)>,
    cancel: Arc<AtomicBool>,
    finished: bool,
    cancelled: bool,
}

fn rect_of(quad: &pl_render::Quad) -> Rect {
    let xs = quad.iter().map(|p| p[0]);
    let ys = quad.iter().map(|p| p[1]);
    Rect::new(
        xs.clone().fold(f64::INFINITY, f64::min),
        ys.clone().fold(f64::INFINITY, f64::min),
        xs.fold(f64::NEG_INFINITY, f64::max),
        ys.fold(f64::NEG_INFINITY, f64::max),
    )
}

impl SetSearch {
    /// A search for `query` over `set` that has not looked at anything yet.
    pub fn new(
        backend: Arc<dyn RenderBackend>,
        set: &DrawingSet,
        query: &str,
        options: SearchOptions,
    ) -> SetSearch {
        SetSearch {
            backend,
            query: query.to_string(),
            options,
            targets: set
                .files
                .iter()
                .enumerate()
                .map(|(file, f)| Target {
                    file,
                    path: f.path.clone(),
                    sheets: f.sheets.clone(),
                    missing: f.missing,
                    open: None,
                })
                .collect(),
            next: 0,
            current: None,
            hits: Vec::new(),
            skipped: Vec::new(),
            cancel: Arc::new(AtomicBool::new(false)),
            finished: false,
            cancelled: false,
        }
    }

    /// As [`SetSearch::new`] with the session's backend, searching the files that are open in
    /// the session through their own displays (their structural edits included).
    pub fn in_session(
        session: &Session,
        set: &DrawingSet,
        query: &str,
        options: SearchOptions,
    ) -> SetSearch {
        let mut search = SetSearch::new(session.backend().clone(), set, query, options);
        for t in &mut search.targets {
            if let Some(id) = session.find_by_path(&t.path) {
                if let Ok(doc) = session.document(id) {
                    if !doc.is_saving() {
                        t.open = Some(doc.render_id());
                        t.missing = false;
                    }
                }
            }
        }
        search
    }

    /// A flag that cancels the search when set (from any thread): the next step stops.
    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Stops the search now; the file it had open is closed.
    pub fn cancel(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.stop(true);
    }

    fn stop(&mut self, cancelled: bool) {
        if let Some(cur) = self.current.take() {
            if cur.owned {
                let _ = self.backend.close(cur.doc);
            }
        }
        self.finished = true;
        self.cancelled |= cancelled;
    }

    pub fn is_finished(&self) -> bool {
        self.finished
    }

    pub fn was_cancelled(&self) -> bool {
        self.cancelled
    }

    /// Every hit so far, in set order.
    pub fn hits(&self) -> &[SetHit] {
        &self.hits
    }

    /// Files that could not be searched, with why.
    pub fn skipped(&self) -> &[(usize, String)] {
        &self.skipped
    }

    /// Files done and files in the set.
    pub fn progress(&self) -> (usize, usize) {
        (self.next.min(self.targets.len()), self.targets.len())
    }

    /// Searches up to `max_pages` more pages (opening and closing files as it goes) and returns
    /// the hits found by this call, in set order.
    pub fn step(&mut self, max_pages: u32) -> Vec<SetHit> {
        let first_new = self.hits.len();
        let mut budget = max_pages.max(1);
        while budget > 0 && !self.finished {
            if self.cancel.load(Ordering::Acquire) {
                self.stop(true);
                break;
            }
            if self.current.is_none() && !self.open_next() {
                continue;
            }
            let cur = self.current.as_mut().expect("opened above");
            let before = cur.search.next_page();
            let done = match cur.search.step(self.backend.as_ref(), cur.doc, 1) {
                Ok(done) => done,
                Err(e) => {
                    let target = cur.target;
                    self.skipped.push((
                        self.targets[target].file,
                        format!("page {} could not be searched: {e}", before + 1),
                    ));
                    true
                }
            };
            budget -= 1;
            self.collect();
            if done {
                if let Some(cur) = self.current.take() {
                    if cur.owned {
                        let _ = self.backend.close(cur.doc);
                    }
                }
            }
        }
        self.hits[first_new..].to_vec()
    }

    /// Moves the hits of the current file that have not been reported into `hits`.
    fn collect(&mut self) {
        let Some(cur) = self.current.as_mut() else {
            return;
        };
        let target = &self.targets[cur.target];
        for hit in &cur.search.hits()[cur.reported..] {
            self.hits.push(SetHit {
                file: target.file,
                path: target.path.clone(),
                page: hit.page,
                sheet: target.sheets.get(hit.page as usize).cloned(),
                rects: hit.quads.iter().map(rect_of).collect(),
                hit: hit.clone(),
            });
        }
        cur.reported = cur.search.hits().len();
    }

    /// Opens the next file to search. Returns false when there was none to open (the search
    /// is then finished, or the file was skipped and the caller tries again).
    fn open_next(&mut self) -> bool {
        let Some(i) = (self.next < self.targets.len()).then_some(self.next) else {
            self.stop(false);
            return false;
        };
        self.next += 1;
        let target = &self.targets[i];
        if let Some(doc) = target.open {
            match self.backend.page_count(doc) {
                Ok(count) => {
                    self.current = Some(Current {
                        target: i,
                        doc,
                        owned: false,
                        search: DocSearch::new(&self.query, self.options, count),
                        reported: 0,
                    });
                    return true;
                }
                Err(e) => {
                    self.skipped.push((target.file, e.to_string()));
                    return false;
                }
            }
        }
        if target.missing || !target.path.is_file() {
            self.skipped
                .push((target.file, "the file is missing".into()));
            return false;
        }
        match self
            .backend
            .open(DocSource::file(target.path.clone()), None)
        {
            Ok(info) => {
                self.current = Some(Current {
                    target: i,
                    doc: info.doc,
                    owned: true,
                    search: DocSearch::new(&self.query, self.options, info.page_count),
                    reported: 0,
                });
                true
            }
            Err(e) => {
                self.skipped.push((target.file, e.to_string()));
                false
            }
        }
    }

    /// Searches to the end (or until cancelled) and returns every hit.
    pub fn run(&mut self) -> &[SetHit] {
        while !self.finished {
            self.step(16);
        }
        &self.hits
    }
}

impl Drop for SetSearch {
    fn drop(&mut self) {
        if let Some(cur) = self.current.take() {
            if cur.owned {
                let _ = self.backend.close(cur.doc);
            }
        }
    }
}

/// What a set search on a worker thread reports.
#[derive(Clone, Debug, PartialEq)]
pub enum SetSearchEvent {
    /// Hits, in set order, as they are found.
    Hits(Vec<SetHit>),
    /// A file that could not be searched.
    Skipped { file: usize, reason: String },
    /// The search is over.
    Finished { cancelled: bool },
}

/// A set search running on a worker thread ([`spawn_set_search`]).
pub struct SetSearchJob {
    rx: Receiver<SetSearchEvent>,
    cancel: Arc<AtomicBool>,
    done: bool,
}

impl SetSearchJob {
    /// The events that have arrived since the last call. Never waits.
    pub fn try_events(&mut self) -> Vec<SetSearchEvent> {
        let mut out = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(e) => {
                    self.done |= matches!(e, SetSearchEvent::Finished { .. });
                    out.push(e);
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    if !self.done {
                        self.done = true;
                        out.push(SetSearchEvent::Finished { cancelled: true });
                    }
                    break;
                }
            }
        }
        out
    }

    /// Asks the worker to stop; it finishes the page it is on.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Release);
    }

    pub fn is_finished(&self) -> bool {
        self.done
    }

    /// Waits for the end and returns every hit that had not been taken yet.
    pub fn wait(mut self) -> Vec<SetHit> {
        let mut hits = Vec::new();
        while !self.done {
            match self.rx.recv() {
                Ok(SetSearchEvent::Hits(h)) => hits.extend(h),
                Ok(SetSearchEvent::Finished { .. }) | Err(_) => self.done = true,
                Ok(_) => {}
            }
        }
        hits
    }
}

/// Runs `search` on a worker thread, streaming its hits a page at a time.
pub fn spawn_set_search(mut search: SetSearch) -> Result<SetSearchJob> {
    let (tx, rx) = channel();
    let cancel = search.cancel_token();
    std::thread::Builder::new()
        .name("pl-doc-set-search".into())
        .spawn(move || {
            let mut reported_skips = 0;
            while !search.is_finished() {
                let hits = search.step(1);
                for (file, reason) in &search.skipped()[reported_skips..] {
                    let _ = tx.send(SetSearchEvent::Skipped {
                        file: *file,
                        reason: reason.clone(),
                    });
                }
                reported_skips = search.skipped().len();
                if !hits.is_empty() && tx.send(SetSearchEvent::Hits(hits)).is_err() {
                    // Nobody is listening any more.
                    search.cancel();
                }
            }
            let _ = tx.send(SetSearchEvent::Finished {
                cancelled: search.was_cancelled(),
            });
        })
        .map_err(|e| DocError::Worker(format!("a worker thread could not be started: {e}")))?;
    Ok(SetSearchJob {
        rx,
        cancel,
        done: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sheets_follow_their_pages() {
        let mut set = DrawingSet::new("s");
        let sheet = |n: &str| SheetInfo {
            number: n.into(),
            title: format!("{n} title"),
        };
        set.insert_with_sheets(
            0,
            Path::new(r"C:\x\Plans.pdf"),
            vec![sheet("A-1"), sheet("A-2"), sheet("A-3")],
        )
        .unwrap();
        // A-1 moves to the end, A-2 is deleted, and a new page is inserted first.
        let map = crate::structure::PageMap {
            old_to_new: vec![Some(2), None, Some(1)],
            new_page_count: 3,
        };
        assert!(set.follow_pages(0, &map));
        let numbers: Vec<&str> = set.files()[0]
            .sheets
            .iter()
            .map(|s| s.number.as_str())
            .collect();
        assert_eq!(numbers, ["Plans-1", "A-3", "A-1"]);
        assert_eq!(
            set.files()[0].sheets[2].title,
            "A-1 title",
            "hand edits kept"
        );
        assert_eq!(set.find_sheet("A-1"), Some(SheetRef { file: 0, page: 2 }));
        // A map of another file is refused.
        assert!(!set.follow_pages(0, &crate::structure::PageMap::identity(5)));
        assert!(!set.follow_pages(3, &map));
    }

    #[test]
    fn relative_paths_climb_and_descend_and_need_the_same_drive() {
        let base = Path::new(r"C:\Projects\Site\sets");
        assert_eq!(
            relative_path(Path::new(r"C:\Projects\Site\sets\a.pdf"), base),
            Some(PathBuf::from("a.pdf"))
        );
        assert_eq!(
            relative_path(Path::new(r"C:\Projects\Site\civil\C-1.pdf"), base),
            Some(PathBuf::from(r"..\civil\C-1.pdf"))
        );
        assert_eq!(
            relative_path(Path::new(r"c:\projects\site\SETS\b.pdf"), base),
            Some(PathBuf::from("b.pdf")),
            "case does not matter on Windows"
        );
        assert_eq!(relative_path(Path::new(r"D:\x\a.pdf"), base), None);
        assert_eq!(
            normalise(Path::new(r"C:\a\b\..\c\.\d.pdf")),
            PathBuf::from(r"C:\a\c\d.pdf")
        );
    }

    #[test]
    fn sheets_come_from_labels_or_the_file_name() {
        let one = sheets_from(Path::new(r"C:\x\C-101 Site plan.pdf"), 1, None);
        assert_eq!(
            one,
            vec![SheetInfo {
                number: "C-101 Site plan".into(),
                title: "C-101 Site plan".into()
            }]
        );
        let many = sheets_from(Path::new(r"C:\x\Civil.pdf"), 2, None);
        assert_eq!(many[1].number, "Civil-2");
        assert_eq!(many[1].title, "Civil");
    }

    fn set_of(sizes: &[usize], missing: &[usize]) -> DrawingSet {
        let mut set = DrawingSet::new("t");
        for (i, n) in sizes.iter().enumerate() {
            set.files.push(SetFile {
                path: PathBuf::from(format!(r"C:\nowhere\{i}.pdf")),
                sheets: (0..*n)
                    .map(|p| SheetInfo {
                        number: format!("S{i}.{p}"),
                        title: String::new(),
                    })
                    .collect(),
                missing: missing.contains(&i),
            });
        }
        set
    }

    #[test]
    fn navigation_crosses_files_and_passes_over_missing_ones() {
        let set = set_of(&[2, 3, 1, 0, 2], &[2]);
        let r = |file, page| SheetRef { file, page };
        assert_eq!(set.first_sheet(), Some(r(0, 0)));
        let mut seen = vec![set.first_sheet().unwrap()];
        while let Some(next) = set.next_sheet(*seen.last().unwrap()) {
            seen.push(next);
        }
        assert_eq!(
            seen,
            vec![
                r(0, 0),
                r(0, 1),
                r(1, 0),
                r(1, 1),
                r(1, 2),
                r(4, 0),
                r(4, 1)
            ]
        );
        assert_eq!(set.last_sheet(), Some(r(4, 1)));
        let mut back = vec![set.last_sheet().unwrap()];
        while let Some(prev) = set.prev_sheet(*back.last().unwrap()) {
            back.push(prev);
        }
        back.reverse();
        assert_eq!(back, seen);
        assert_eq!(set.find_sheet("s1.2"), Some(r(1, 2)));
        assert_eq!(set.find_sheet("nope"), None);
    }

    #[test]
    fn files_can_be_reordered_removed_and_their_sheets_edited() {
        let mut set = set_of(&[1, 1, 1], &[]);
        assert!(set.move_file(0, 2));
        assert!(!set.move_file(0, 3));
        assert_eq!(set.files()[2].sheets[0].number, "S0.0");
        assert!(set.set_sheet(
            SheetRef { file: 2, page: 0 },
            SheetInfo {
                number: "A-1".into(),
                title: "Plan".into()
            }
        ));
        assert!(!set.set_sheet(
            SheetRef { file: 2, page: 1 },
            SheetInfo {
                number: String::new(),
                title: String::new()
            }
        ));
        assert_eq!(
            set.sheet(SheetRef { file: 2, page: 0 }).unwrap().title,
            "Plan"
        );
        assert_eq!(set.remove_file(0).unwrap().sheets[0].number, "S1.0");
        assert!(set.remove_file(5).is_none());
        assert_eq!(set.len(), 2);
    }
}

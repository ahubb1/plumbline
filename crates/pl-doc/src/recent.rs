//! The recent-files list, stored as `recent.json` in the application's data directory.
//!
//! The real directory is `%LOCALAPPDATA%\Plumbline` ([`default_data_dir`]). Nothing in this
//! module picks it implicitly: the caller passes the directory to [`RecentFiles::load`], so tests
//! use a temporary one.

use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

/// Folder name under `%LOCALAPPDATA%`.
pub const APP_DIR_NAME: &str = "Plumbline";
/// File name of the list inside the data directory.
pub const RECENT_FILE_NAME: &str = "recent.json";
/// How many entries are kept by default.
pub const DEFAULT_MAX_RECENT: usize = 20;

const FORMAT_VERSION: u32 = 1;

/// `%LOCALAPPDATA%\Plumbline`, or `None` when the variable is unset or not an absolute path.
/// The directory is not created here.
pub fn default_data_dir() -> Option<PathBuf> {
    data_dir_from(std::env::var_os("LOCALAPPDATA").map(PathBuf::from))
}

fn data_dir_from(local_app_data: Option<PathBuf>) -> Option<PathBuf> {
    let base = local_app_data?;
    base.is_absolute().then(|| base.join(APP_DIR_NAME))
}

/// One recently opened file.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentEntry {
    pub path: PathBuf,
    /// When it was last opened, in seconds since the Unix epoch.
    pub opened_unix: u64,
    /// The zero-based page that was showing when it was last closed.
    #[serde(default)]
    pub last_page: u32,
}

#[derive(Serialize, Deserialize)]
struct OnDisk {
    version: u32,
    entries: Vec<RecentEntry>,
}

/// The recent-files list, most recent first.
#[derive(Clone, Debug)]
pub struct RecentFiles {
    file: PathBuf,
    max: usize,
    entries: Vec<RecentEntry>,
}

impl RecentFiles {
    /// Loads the list kept in `dir`. A missing, unreadable or damaged file gives an empty list:
    /// losing the list must never stop the application from starting.
    pub fn load(dir: impl AsRef<Path>) -> RecentFiles {
        RecentFiles::load_with_max(dir, DEFAULT_MAX_RECENT)
    }

    /// As [`load`](RecentFiles::load), keeping at most `max` entries (at least one).
    pub fn load_with_max(dir: impl AsRef<Path>, max: usize) -> RecentFiles {
        let file = dir.as_ref().join(RECENT_FILE_NAME);
        let max = max.max(1);
        let mut entries = std::fs::read(&file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<OnDisk>(&bytes).ok())
            .filter(|d| d.version == FORMAT_VERSION)
            .map(|d| d.entries)
            .unwrap_or_default();
        // A hand-edited file may hold duplicates or too many entries.
        let mut seen: Vec<String> = Vec::new();
        entries.retain(|e| {
            let key = path_key(&e.path);
            if seen.contains(&key) {
                false
            } else {
                seen.push(key);
                true
            }
        });
        entries.truncate(max);
        RecentFiles { file, max, entries }
    }

    /// The file this list is saved to.
    pub fn file(&self) -> &Path {
        &self.file
    }

    /// The entries, most recent first.
    pub fn entries(&self) -> &[RecentEntry] {
        &self.entries
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Records that `path` was opened now. It moves to the front; an existing entry for the
    /// same file (compared without regard to case or slash direction) is replaced and keeps its
    /// last page. The oldest entries beyond the limit are dropped.
    pub fn touch(&mut self, path: impl AsRef<Path>) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        self.touch_at(path, now);
    }

    /// As [`touch`](RecentFiles::touch) with an explicit time.
    pub fn touch_at(&mut self, path: impl AsRef<Path>, opened_unix: u64) {
        let path = path.as_ref().to_path_buf();
        let last_page = self.take(&path).map(|e| e.last_page).unwrap_or(0);
        self.entries.insert(
            0,
            RecentEntry {
                path,
                opened_unix,
                last_page,
            },
        );
        self.entries.truncate(self.max);
    }

    /// Remembers the page a file was showing. Returns `false` if the file is not in the list.
    pub fn set_last_page(&mut self, path: impl AsRef<Path>, page: u32) -> bool {
        let key = path_key(path.as_ref());
        match self.entries.iter_mut().find(|e| path_key(&e.path) == key) {
            Some(e) => {
                e.last_page = page;
                true
            }
            None => false,
        }
    }

    /// The entry for a file, if listed.
    pub fn get(&self, path: impl AsRef<Path>) -> Option<&RecentEntry> {
        let key = path_key(path.as_ref());
        self.entries.iter().find(|e| path_key(&e.path) == key)
    }

    /// Removes a file from the list. Returns whether it was there.
    pub fn remove(&mut self, path: impl AsRef<Path>) -> bool {
        self.take(path.as_ref()).is_some()
    }

    /// Drops entries whose file no longer exists. Returns how many were dropped. This touches
    /// the disk once per entry, so call it when the list is shown, not on every frame.
    pub fn prune_missing(&mut self) -> usize {
        let before = self.entries.len();
        self.entries.retain(|e| e.path.is_file());
        before - self.entries.len()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Writes the list, creating the directory if needed. The file is written beside its final
    /// name and then renamed over it, so a crash leaves either the old list or the new one.
    pub fn save(&self) -> io::Result<()> {
        if let Some(dir) = self.file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let data = OnDisk {
            version: FORMAT_VERSION,
            entries: self.entries.clone(),
        };
        let json = serde_json::to_vec_pretty(&data).map_err(io::Error::other)?;
        let tmp = self.file.with_extension("json.tmp");
        std::fs::write(&tmp, json)?;
        std::fs::rename(&tmp, &self.file).inspect_err(|_| {
            let _ = std::fs::remove_file(&tmp);
        })
    }

    fn take(&mut self, path: &Path) -> Option<RecentEntry> {
        let key = path_key(path);
        let i = self.entries.iter().position(|e| path_key(&e.path) == key)?;
        Some(self.entries.remove(i))
    }
}

/// Windows paths name the same file whatever their case or slash direction.
fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_new_directory_gives_an_empty_list_and_save_creates_it() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("nested").join("Plumbline");
        let mut recent = RecentFiles::load(&dir);
        assert!(recent.is_empty());
        assert!(!dir.exists(), "loading must not create anything");
        recent.touch_at("C:\\jobs\\a.pdf", 100);
        recent.save().unwrap();
        assert!(dir.join(RECENT_FILE_NAME).is_file());
        assert!(!dir.join("recent.json.tmp").exists());
    }

    #[test]
    fn the_list_round_trips_through_disk_most_recent_first() {
        let tmp = tempfile::tempdir().unwrap();
        let mut recent = RecentFiles::load(tmp.path());
        recent.touch_at("C:\\jobs\\a.pdf", 100);
        recent.touch_at("C:\\jobs\\b.pdf", 200);
        recent.touch_at("C:\\jobs\\c.pdf", 300);
        assert!(recent.set_last_page("C:\\jobs\\b.pdf", 7));
        assert!(!recent.set_last_page("C:\\jobs\\zzz.pdf", 1));
        recent.save().unwrap();

        let back = RecentFiles::load(tmp.path());
        let names: Vec<&str> = back
            .entries()
            .iter()
            .map(|e| e.path.file_name().unwrap().to_str().unwrap())
            .collect();
        assert_eq!(names, ["c.pdf", "b.pdf", "a.pdf"]);
        assert_eq!(back.entries(), recent.entries());
        assert_eq!(back.get("C:\\jobs\\b.pdf").unwrap().last_page, 7);
        assert_eq!(back.get("C:\\jobs\\b.pdf").unwrap().opened_unix, 200);
    }

    #[test]
    fn reopening_moves_a_file_to_the_front_without_duplicating_it() {
        let tmp = tempfile::tempdir().unwrap();
        let mut recent = RecentFiles::load(tmp.path());
        recent.touch_at("C:\\jobs\\a.pdf", 100);
        recent.touch_at("C:\\jobs\\b.pdf", 200);
        recent.set_last_page("C:\\jobs\\a.pdf", 3);
        // Same file, different case and slashes.
        recent.touch_at("c:/Jobs/A.PDF", 300);
        assert_eq!(recent.len(), 2);
        assert_eq!(recent.entries()[0].opened_unix, 300);
        assert_eq!(recent.entries()[0].last_page, 3, "the last page survives");
        assert_eq!(recent.entries()[1].path, PathBuf::from("C:\\jobs\\b.pdf"));
    }

    #[test]
    fn the_list_is_capped_and_entries_can_be_removed() {
        let tmp = tempfile::tempdir().unwrap();
        let mut recent = RecentFiles::load_with_max(tmp.path(), 3);
        for i in 0..10 {
            recent.touch_at(format!("C:\\jobs\\{i}.pdf"), i);
        }
        assert_eq!(recent.len(), 3);
        assert_eq!(recent.entries()[0].path, PathBuf::from("C:\\jobs\\9.pdf"));
        assert_eq!(recent.entries()[2].path, PathBuf::from("C:\\jobs\\7.pdf"));
        assert!(recent.remove("C:\\jobs\\8.pdf"));
        assert!(!recent.remove("C:\\jobs\\8.pdf"));
        assert_eq!(recent.len(), 2);
        recent.clear();
        recent.save().unwrap();
        assert!(RecentFiles::load(tmp.path()).is_empty());
    }

    #[test]
    fn a_damaged_or_foreign_file_is_treated_as_empty_and_can_be_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join(RECENT_FILE_NAME);
        for junk in [
            &b"{ not json"[..],
            b"",
            b"[1, 2, 3]",
            br#"{"version": 99, "entries": []}"#,
        ] {
            std::fs::write(&file, junk).unwrap();
            let mut recent = RecentFiles::load(tmp.path());
            assert!(recent.is_empty());
            recent.touch_at("C:\\jobs\\a.pdf", 1);
            recent.save().unwrap();
            assert_eq!(RecentFiles::load(tmp.path()).len(), 1);
        }
    }

    #[test]
    fn duplicates_in_a_hand_edited_file_are_dropped_on_load() {
        let tmp = tempfile::tempdir().unwrap();
        let json = r#"{"version":1,"entries":[
            {"path":"C:\\x\\a.pdf","opened_unix":5},
            {"path":"c:/x/A.pdf","opened_unix":4,"last_page":2},
            {"path":"C:\\x\\b.pdf","opened_unix":3}]}"#;
        std::fs::write(tmp.path().join(RECENT_FILE_NAME), json).unwrap();
        let recent = RecentFiles::load(tmp.path());
        assert_eq!(recent.len(), 2);
        assert_eq!(recent.entries()[0].last_page, 0, "a missing field defaults");
    }

    #[test]
    fn prune_drops_files_that_no_longer_exist() {
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("real.pdf");
        std::fs::write(&real, b"%PDF-1.4").unwrap();
        let mut recent = RecentFiles::load(tmp.path().join("data"));
        recent.touch_at(&real, 1);
        recent.touch_at(tmp.path().join("gone.pdf"), 2);
        assert_eq!(recent.prune_missing(), 1);
        assert_eq!(recent.entries()[0].path, real);
    }

    #[test]
    fn the_default_directory_is_under_local_app_data() {
        assert_eq!(
            data_dir_from(Some(PathBuf::from("D:\\Data\\AppData\\Local"))),
            Some(PathBuf::from("D:\\Data\\AppData\\Local\\Plumbline"))
        );
        assert_eq!(data_dir_from(None), None);
        assert_eq!(data_dir_from(Some(PathBuf::from("relative"))), None);
    }
}

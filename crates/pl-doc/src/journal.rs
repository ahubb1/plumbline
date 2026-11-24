//! The crash journal: a sidecar file of the commands applied since the last save.
//!
//! Every command, undo and redo is appended as one record and handed to the operating system
//! at once, so the journal is the autosave: a process that is killed loses nothing that was
//! done before the kill. When the document is opened again and the file on disk is still the
//! one the journal was started against (same [`FileStamp`]), the records are replayed onto the
//! freshly loaded annotations. A successful save deletes the journal.
//!
//! # Format
//!
//! `PLJ1\n`, then records. A record is the payload length (u32, little endian), the CRC-32 of
//! the payload (u32, little endian) and the payload, which is one [`Record`] as JSON. The first
//! record is a [`Record::Header`]. Reading stops at the first record that is cut short or
//! fails its checksum (a torn last write); everything before it is used.
//!
//! The journal lives under a directory the caller chooses ([`crate::Session::set_journal_dir`]);
//! [`default_journal_dir`] is `%LOCALAPPDATA%\Plumbline\journal`. Nothing is journaled until a
//! directory is set, so tests never write to the real one by accident.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::{Deserialize, Serialize};

use crate::command::Command;

/// The first bytes of a journal file.
pub const JOURNAL_MAGIC: &[u8; 5] = b"PLJ1\n";
/// The extension of journal files.
pub const JOURNAL_EXTENSION: &str = "plj";
/// The largest record payload that is read back (a guard against a corrupt length field).
pub const MAX_RECORD_LEN: u32 = 256 * 1024 * 1024;
/// How many bytes at each end of the file go into a [`FileStamp`].
const STAMP_WINDOW: u64 = 64 * 1024;

/// `%LOCALAPPDATA%\Plumbline\journal`, or `None` when that variable is unset. The directory is
/// not created here.
pub fn default_journal_dir() -> Option<PathBuf> {
    crate::recent::default_data_dir().map(|d| d.join("journal"))
}

/// The CRC-32 (IEEE 802.3, the zlib one) of `data`.
pub fn crc32(data: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = i as u32;
            let mut k = 0;
            while k < 8 {
                c = if c & 1 != 0 {
                    0xedb8_8320 ^ (c >> 1)
                } else {
                    c >> 1
                };
                k += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    let mut crc = 0xffff_ffffu32;
    for b in data {
        crc = TABLE[((crc ^ u32::from(*b)) & 0xff) as usize] ^ (crc >> 8);
    }
    !crc
}

fn fnv1a(mut h: u64, data: &[u8]) -> u64 {
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

const FNV_SEED: u64 = 0xcbf2_9ce4_8422_2325;

/// A cheap fingerprint of a file: its length, its modification time and a hash of its first
/// and last 64 KiB.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileStamp {
    pub len: u64,
    /// Modification time as seconds and nanoseconds since the Unix epoch, when known.
    pub modified: Option<(u64, u32)>,
    pub ends_hash: u64,
}

impl FileStamp {
    pub fn of_file(path: &Path) -> std::io::Result<FileStamp> {
        let mut file = File::open(path)?;
        let meta = file.metadata()?;
        let len = meta.len();
        let window = len.min(STAMP_WINDOW) as usize;
        let mut head = vec![0u8; window];
        file.read_exact(&mut head)?;
        let mut tail = vec![0u8; window];
        file.seek(SeekFrom::Start(len - window as u64))?;
        file.read_exact(&mut tail)?;
        let modified = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
            .map(|d| (d.as_secs(), d.subsec_nanos()));
        Ok(FileStamp {
            len,
            modified,
            ends_hash: fnv1a(fnv1a(FNV_SEED, &head), &tail),
        })
    }
}

fn path_key(path: &Path) -> String {
    path.to_string_lossy().replace('/', "\\").to_lowercase()
}

/// Whether two paths name the same file, without regard to case or slash direction.
pub fn same_path(a: &Path, b: &Path) -> bool {
    path_key(a) == path_key(b)
}

/// Where the journal of the document at `document` lives under `dir`: a name made from a hash
/// of the document's path.
pub fn journal_path(dir: &Path, document: &Path) -> PathBuf {
    let hash = fnv1a(FNV_SEED, path_key(document).as_bytes());
    dir.join(format!("{hash:016x}.{JOURNAL_EXTENSION}"))
}

/// One journal record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub enum Record {
    /// Which document, in which state, the records that follow apply to.
    Header { document: PathBuf, stamp: FileStamp },
    /// A command that was executed; `merge` is its drag key.
    Do {
        command: Command,
        merge: Option<u64>,
    },
    /// The most recent command of this journal was undone.
    Undo,
    /// The command most recently undone was applied again.
    Redo,
    /// A change made by undoing or redoing a command from before this journal began: it is
    /// replayed as given, without touching the history.
    Raw { command: Command },
}

/// What a journal file held.
#[derive(Clone, Debug, PartialEq)]
pub struct JournalContents {
    /// The header, when the file has a readable one.
    pub header: Option<(PathBuf, FileStamp)>,
    /// The records after the header, each with the file offset just past it.
    pub records: Vec<(Record, u64)>,
    /// The offset just past the header record.
    pub header_end: u64,
    /// Whether reading stopped before the end of the file (a torn or corrupt record).
    pub damaged: bool,
}

/// An open journal, positioned to append.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    file: File,
    /// The pictures (image stamps) this run has written into the journal in full; a later
    /// record names them by key only (see [`crate::images`]).
    images: std::collections::HashSet<u64>,
}

fn frame(record: &Record) -> std::io::Result<Vec<u8>> {
    let payload = serde_json::to_vec(record).map_err(std::io::Error::other)?;
    let len = u32::try_from(payload.len())
        .ok()
        .filter(|l| *l <= MAX_RECORD_LEN)
        .ok_or_else(|| std::io::Error::other("the journal record is too large"))?;
    let mut out = Vec::with_capacity(payload.len() + 8);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&crc32(&payload).to_le_bytes());
    out.extend_from_slice(&payload);
    Ok(out)
}

impl Journal {
    /// Starts a new journal for `document` under `dir`, replacing any there was.
    pub fn create(dir: &Path, document: &Path, stamp: FileStamp) -> std::io::Result<Journal> {
        std::fs::create_dir_all(dir)?;
        let path = journal_path(dir, document);
        let mut file = File::create(&path)?;
        let mut start = JOURNAL_MAGIC.to_vec();
        start.extend(frame(&Record::Header {
            document: document.to_path_buf(),
            stamp,
        })?);
        file.write_all(&start)?;
        file.flush()?;
        Ok(Journal {
            path,
            file,
            images: Default::default(),
        })
    }

    /// Opens an existing journal to append after its first `valid_len` bytes; anything beyond
    /// (a torn record) is cut off.
    pub fn resume(path: &Path, valid_len: u64) -> std::io::Result<Journal> {
        let mut file = OpenOptions::new().read(true).write(true).open(path)?;
        file.set_len(valid_len)?;
        file.seek(SeekFrom::End(0))?;
        Ok(Journal {
            path: path.to_path_buf(),
            file,
            // Empty: the first record of this run that uses a picture stores it again, so
            // what is appended never depends on a record that may have been cut off.
            images: Default::default(),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Appends one record and hands it to the operating system. A whole record goes out in a
    /// single write, so a killed process leaves at most one torn record at the end.
    pub fn append(&mut self, record: &Record) -> std::io::Result<()> {
        let bytes = crate::images::eliding_known_images(&mut self.images, || frame(record))?;
        self.file.write_all(&bytes)?;
        self.file.flush()
    }

    /// Asks the operating system to put the journal on the disk itself (protection against
    /// power loss, not just against a killed process).
    pub fn sync(&mut self) -> std::io::Result<()> {
        self.file.sync_data()
    }

    /// Closes and deletes the journal.
    pub fn discard(self) -> std::io::Result<()> {
        let Journal { path, file, .. } = self;
        drop(file);
        remove_if_present(&path)
    }

    /// Reads a journal file. A missing or foreign file gives contents with no header.
    pub fn read(path: &Path) -> std::io::Result<JournalContents> {
        let bytes = std::fs::read(path)?;
        let mut contents = JournalContents {
            header: None,
            records: Vec::new(),
            header_end: 0,
            damaged: false,
        };
        if !bytes.starts_with(JOURNAL_MAGIC) {
            contents.damaged = true;
            return Ok(contents);
        }
        let mut at = JOURNAL_MAGIC.len();
        loop {
            if at == bytes.len() {
                break;
            }
            let Some(record) = read_frame(&bytes, &mut at) else {
                contents.damaged = true;
                break;
            };
            match (record, contents.header.is_some()) {
                (Record::Header { document, stamp }, false) => {
                    contents.header = Some((document, stamp));
                    contents.header_end = at as u64;
                }
                // A second header, or records with no header before them: not ours to trust.
                (Record::Header { .. }, true) | (_, false) => {
                    contents.damaged = true;
                    break;
                }
                (record, true) => contents.records.push((record, at as u64)),
            }
        }
        Ok(contents)
    }

    /// Reads only the header of a journal file: which document and state it belongs to.
    pub fn peek(path: &Path) -> Option<(PathBuf, FileStamp)> {
        let mut file = File::open(path).ok()?;
        let mut start = [0u8; 13];
        file.read_exact(&mut start).ok()?;
        if &start[..5] != JOURNAL_MAGIC {
            return None;
        }
        let len = u32::from_le_bytes(start[5..9].try_into().ok()?);
        if len > 1024 * 1024 {
            return None;
        }
        let mut bytes = start[5..].to_vec();
        bytes.resize(8 + len as usize, 0);
        file.read_exact(&mut bytes[8..]).ok()?;
        match read_frame(&bytes, &mut 0)? {
            Record::Header { document, stamp } => Some((document, stamp)),
            _ => None,
        }
    }
}

fn read_frame(bytes: &[u8], at: &mut usize) -> Option<Record> {
    let head = bytes.get(*at..*at + 8)?;
    let len = u32::from_le_bytes(head[..4].try_into().ok()?);
    let crc = u32::from_le_bytes(head[4..].try_into().ok()?);
    if len > MAX_RECORD_LEN {
        return None;
    }
    let payload = bytes.get(*at + 8..*at + 8 + len as usize)?;
    if crc32(payload) != crc {
        return None;
    }
    let record = serde_json::from_slice(payload).ok()?;
    *at += 8 + len as usize;
    Some(record)
}

/// Deletes a file; a file that is not there is not an error.
pub(crate) fn remove_if_present(path: &Path) -> std::io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_annot::{AnnotId, Annotation, Origin, PdfDate};
    use pl_geom::Rect;

    fn stamp() -> FileStamp {
        FileStamp {
            len: 1234,
            modified: Some((1_700_000_000, 5)),
            ends_hash: 0xdead_beef,
        }
    }

    fn records() -> Vec<Record> {
        let o = Origin::new("T", PdfDate::from_unix(1_700_000_000));
        let a = Annotation::square(0, Rect::new(0.0, 0.0, 10.0, 10.0), &o);
        vec![
            Record::Do {
                command: Command::add(a.clone()),
                merge: None,
            },
            Record::Do {
                command: Command::modify(a.clone()),
                merge: Some(3),
            },
            Record::Undo,
            Record::Redo,
            Record::Raw {
                command: Command::delete(AnnotId::from("x")),
            },
        ]
    }

    #[test]
    fn crc32_matches_the_standard_check_value() {
        assert_eq!(crc32(b"123456789"), 0xcbf4_3926);
        assert_eq!(crc32(b""), 0);
    }

    #[test]
    fn records_round_trip_and_resume_appends() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("Some Drawing.pdf");
        let mut journal = Journal::create(dir.path(), &doc, stamp()).unwrap();
        let all = records();
        for r in &all[..3] {
            journal.append(r).unwrap();
        }
        journal.sync().unwrap();
        let path = journal.path().to_path_buf();
        assert_eq!(path, journal_path(dir.path(), &doc));
        assert_eq!(
            journal_path(dir.path(), Path::new("C:/A/b.PDF")),
            journal_path(dir.path(), Path::new("c:\\a\\B.pdf"))
        );
        drop(journal);

        assert_eq!(Journal::peek(&path), Some((doc.clone(), stamp())));
        let read = Journal::read(&path).unwrap();
        assert!(!read.damaged);
        assert_eq!(read.header, Some((doc.clone(), stamp())));
        let got: Vec<Record> = read.records.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(got, all[..3]);
        let end = read.records.last().unwrap().1;
        assert_eq!(end, std::fs::metadata(&path).unwrap().len());

        let mut journal = Journal::resume(&path, end).unwrap();
        for r in &all[3..] {
            journal.append(r).unwrap();
        }
        drop(journal);
        let read = Journal::read(&path).unwrap();
        let got: Vec<Record> = read.records.iter().map(|(r, _)| r.clone()).collect();
        assert_eq!(got, all);

        Journal::resume(&path, end).unwrap().discard().unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn a_torn_or_corrupt_tail_is_dropped_and_the_rest_kept() {
        let dir = tempfile::tempdir().unwrap();
        let doc = dir.path().join("a.pdf");
        let mut journal = Journal::create(dir.path(), &doc, stamp()).unwrap();
        let all = records();
        for r in &all {
            journal.append(r).unwrap();
        }
        let path = journal.path().to_path_buf();
        drop(journal);
        let whole = std::fs::read(&path).unwrap();
        let ends: Vec<u64> = Journal::read(&path)
            .unwrap()
            .records
            .iter()
            .map(|(_, e)| *e)
            .collect();

        // Cut anywhere inside the last record: the four before it survive.
        for cut in [1, 5, 9, 20] {
            std::fs::write(&path, &whole[..whole.len() - cut]).unwrap();
            let read = Journal::read(&path).unwrap();
            assert!(read.damaged);
            assert_eq!(read.records.len(), all.len() - 1);
            assert_eq!(read.records.last().unwrap().1, ends[all.len() - 2]);
        }
        // A flipped byte in the third record: the first two survive, nothing after is used.
        let mut bad = whole.clone();
        bad[ends[1] as usize + 12] ^= 0x40;
        std::fs::write(&path, &bad).unwrap();
        let read = Journal::read(&path).unwrap();
        assert!(read.damaged);
        assert_eq!(read.records.len(), 2);

        // Resuming after the valid part cuts the damage off.
        let mut journal = Journal::resume(&path, ends[1]).unwrap();
        journal.append(&Record::Undo).unwrap();
        drop(journal);
        let read = Journal::read(&path).unwrap();
        assert!(!read.damaged);
        assert_eq!(read.records.len(), 3);
        assert_eq!(read.records[2].0, Record::Undo);

        // Not a journal at all.
        std::fs::write(&path, b"%PDF-1.7 not a journal").unwrap();
        let read = Journal::read(&path).unwrap();
        assert!(read.damaged && read.header.is_none() && read.records.is_empty());
        assert_eq!(Journal::peek(&path), None);
        // An absurd length field does not allocate.
        let mut huge = JOURNAL_MAGIC.to_vec();
        huge.extend_from_slice(&u32::MAX.to_le_bytes());
        huge.extend_from_slice(&[0; 12]);
        std::fs::write(&path, &huge).unwrap();
        assert!(Journal::read(&path).unwrap().damaged);
        assert_eq!(Journal::peek(&path), None);
    }

    #[test]
    fn a_stamp_follows_the_file_content() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f.bin");
        std::fs::write(&path, vec![7u8; 200_000]).unwrap();
        let a = FileStamp::of_file(&path).unwrap();
        assert_eq!(a.len, 200_000);
        assert_eq!(FileStamp::of_file(&path).unwrap(), a);
        let mut bytes = vec![7u8; 200_000];
        bytes[199_999] = 8;
        std::fs::write(&path, &bytes).unwrap();
        assert_ne!(FileStamp::of_file(&path).unwrap().ends_hash, a.ends_hash);
        std::fs::write(&path, b"").unwrap();
        assert_eq!(FileStamp::of_file(&path).unwrap().len, 0);
        assert!(FileStamp::of_file(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn the_default_directory_is_under_the_data_directory() {
        if let Some(dir) = default_journal_dir() {
            assert!(dir.ends_with("Plumbline\\journal") || dir.ends_with("Plumbline/journal"));
        }
    }
}

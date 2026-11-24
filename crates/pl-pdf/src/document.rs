//! Lazy file handle, parsed document, page tree and annotation access.

use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime};

use lopdf::xref::{XrefEntry, XrefType};
use lopdf::{Dictionary, Document, LoadOptions, Object, ObjectId, Permissions};
use pl_geom::{PageGeometry, Rect, Rotation};

use crate::edit::EditSet;
use crate::error::{PdfError, Result};

/// US Letter, used when a page has no usable `/MediaBox` (PDFium makes the same choice).
const FALLBACK_MEDIA_BOX: [f64; 4] = [0.0, 0.0, 612.0, 792.0];
/// How far up the `/Parent` chain an inherited page attribute is looked for.
const INHERIT_DEPTH_LIMIT: usize = 64;

/// The cross-reference style of the file's newest revision. An incremental save appends a
/// section of the same style.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefStyle {
    /// Classic `xref` table plus `trailer`.
    Table,
    /// Cross-reference stream (PDF 1.5).
    Stream,
}

/// What loading cost.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadStats {
    pub file_len: u64,
    pub read_time: Duration,
    pub parse_time: Duration,
    pub object_count: usize,
}

/// The security handler of a file that was opened with its password.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EncryptionInfo {
    /// `/V` of the encryption dictionary.
    pub version: i64,
    /// `/R` of the encryption dictionary.
    pub revision: i64,
    /// Key length as lopdf reports it (bits), if the dictionary states one.
    pub key_length: Option<usize>,
    pub can_print: bool,
    pub can_modify: bool,
    pub can_copy: bool,
    pub can_annotate: bool,
    pub can_fill_forms: bool,
    pub can_assemble: bool,
}

/// One page of the document.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageInfo {
    /// Zero-based index in page-tree order.
    pub index: usize,
    /// The page object.
    pub id: ObjectId,
    /// `/MediaBox` as written (normalised so x0 <= x1 and y0 <= y1), inherited if need be.
    pub media_box: Rect,
    /// `/CropBox` as written, if the page has or inherits one.
    pub crop_box: Option<Rect>,
    /// `/Rotate`, clockwise.
    pub rotate: Rotation,
    /// `/UserUnit` (1.0 when absent).
    pub user_unit: f64,
    /// The same four facts as pl-geom wants them (crop clipped to media, fallbacks applied).
    pub geometry: PageGeometry,
    /// Number of entries in the page's `/Annots` array.
    pub annotation_count: usize,
}

/// One entry of a page's `/Annots` array, with the dictionary exactly as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct AnnotationEntry {
    pub page_index: usize,
    /// Position in the `/Annots` array.
    pub slot: usize,
    /// The annotation's object id, or `None` when the dictionary is written directly in the
    /// array (such an annotation can be read but not patched or deleted through an edit set).
    pub id: Option<ObjectId>,
    /// Every key the file holds, known or not, untouched.
    pub dict: Dictionary,
}

impl AnnotationEntry {
    /// `/Subtype` as bytes, for example `b"Square"`.
    pub fn subtype(&self) -> Option<&[u8]> {
        self.dict.get(b"Subtype").and_then(Object::as_name).ok()
    }
}

/// A parsed PDF together with the exact bytes it was parsed from.
#[derive(Debug, Clone)]
pub struct PdfDocument {
    pub(crate) bytes: Vec<u8>,
    pub(crate) doc: Document,
    pub(crate) pages: Vec<ObjectId>,
    /// Identifies this parsed state. Every parse (a load, or the re-parse after a save) gets a
    /// new value, so an [`EditSet`] can tell that the document it was started from is gone.
    pub(crate) state: u64,
}

/// Source of [`PdfDocument::state`] tokens; never 0.
static NEXT_STATE: AtomicU64 = AtomicU64::new(1);

impl PdfDocument {
    /// Parse `bytes`. `password` is tried after the empty user password.
    pub fn from_bytes(bytes: Vec<u8>, password: Option<&str>) -> Result<PdfDocument> {
        let doc = parse(&bytes, password)?;
        PdfDocument::from_parts(bytes, doc)
    }

    pub(crate) fn from_parts(bytes: Vec<u8>, doc: Document) -> Result<PdfDocument> {
        let pages: Vec<ObjectId> = doc.page_iter().collect();
        if pages.is_empty() {
            return Err(PdfError::NoPages);
        }
        Ok(PdfDocument {
            bytes,
            doc,
            pages,
            state: NEXT_STATE.fetch_add(1, Ordering::Relaxed),
        })
    }

    /// The bytes the document was parsed from (the whole file, all revisions).
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Header version, for example `"1.7"`.
    pub fn version(&self) -> &str {
        &self.doc.version
    }

    pub fn xref_style(&self) -> XrefStyle {
        match self.doc.reference_table.cross_reference_type {
            XrefType::CrossReferenceTable => XrefStyle::Table,
            XrefType::CrossReferenceStream => XrefStyle::Stream,
        }
    }

    /// True when at least one object lives in a compressed object stream.
    pub fn has_object_streams(&self) -> bool {
        self.doc
            .reference_table
            .entries
            .values()
            .any(XrefEntry::is_compressed)
    }

    /// Byte offset of the newest cross-reference section, 0 if lopdf had to rebuild it.
    pub fn xref_start(&self) -> usize {
        self.doc.xref_start
    }

    /// True when lopdf could not read the cross-reference data as written and repaired it.
    /// Such a file cannot be saved incrementally; use the full rewrite.
    pub fn xref_was_repaired(&self) -> bool {
        self.doc.xref_recovery.is_some()
    }

    /// Number of cross-reference sections in the file (1 for a file never updated).
    pub fn revision_count(&self) -> usize {
        self.doc.xref_section_count
    }

    pub fn is_linearized(&self) -> bool {
        self.doc.linearization.is_some()
    }

    /// The security handler, when the file is encrypted (and was opened with its password).
    pub fn encryption(&self) -> Option<EncryptionInfo> {
        let state = self.doc.encryption_state.as_ref()?;
        let p = state.permissions();
        Some(EncryptionInfo {
            version: state.version(),
            revision: state.revision(),
            key_length: state.key_length(),
            can_print: p.contains(Permissions::PRINTABLE),
            can_modify: p.contains(Permissions::MODIFIABLE),
            can_copy: p.contains(Permissions::COPYABLE),
            can_annotate: p.contains(Permissions::ANNOTABLE),
            can_fill_forms: p.contains(Permissions::FILLABLE),
            can_assemble: p.contains(Permissions::ASSEMBLABLE),
        })
    }

    /// Highest object number in use.
    pub fn max_object_id(&self) -> u32 {
        self.doc.max_id
    }

    pub fn object_count(&self) -> usize {
        self.doc.objects.len()
    }

    /// All object ids, ascending.
    pub fn object_ids(&self) -> impl Iterator<Item = ObjectId> + '_ {
        self.doc.objects.keys().copied()
    }

    pub fn object(&self, id: ObjectId) -> Option<&Object> {
        self.doc.objects.get(&id)
    }

    /// Follow references until a direct object is reached. A dangling reference or a cycle
    /// gives `Object::Null`'s stand-in, `None`.
    pub fn resolve<'a>(&'a self, object: &'a Object) -> Option<&'a Object> {
        self.doc.dereference(object).ok().map(|(_, o)| o)
    }

    /// File offset of an object that is stored directly (not in an object stream), as the
    /// merged cross-reference table records it.
    pub fn object_offset(&self, id: ObjectId) -> Option<u64> {
        match self.doc.reference_table.get(id.0)? {
            XrefEntry::Normal { offset, generation } if *generation == id.1 => {
                Some(u64::from(*offset))
            }
            _ => None,
        }
    }

    pub fn trailer(&self) -> &Dictionary {
        &self.doc.trailer
    }

    pub fn catalog(&self) -> Result<&Dictionary> {
        self.doc.catalog().map_err(PdfError::Parse)
    }

    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Page object ids in page-tree order.
    pub fn page_ids(&self) -> &[ObjectId] {
        &self.pages
    }

    pub fn page_id(&self, index: usize) -> Result<ObjectId> {
        self.pages
            .get(index)
            .copied()
            .ok_or(PdfError::PageOutOfRange {
                index,
                count: self.pages.len(),
            })
    }

    /// Index of the page with this object id.
    pub fn page_index_of(&self, id: ObjectId) -> Option<usize> {
        self.pages.iter().position(|p| *p == id)
    }

    /// Boxes, rotation and user unit of a page, with inherited attributes resolved.
    pub fn page(&self, index: usize) -> Result<PageInfo> {
        let id = self.page_id(index)?;
        let dict = self
            .doc
            .get_dictionary(id)
            .map_err(|_| PdfError::ObjectNotFound(id))?;

        let media = self
            .inherited(dict, b"MediaBox")
            .and_then(|o| rect_array(&self.doc, o))
            .filter(|r| r[0] != r[2] && r[1] != r[3])
            .unwrap_or(FALLBACK_MEDIA_BOX);
        let crop = self
            .inherited(dict, b"CropBox")
            .and_then(|o| rect_array(&self.doc, o));
        let rotate = self
            .inherited(dict, b"Rotate")
            .and_then(number)
            .and_then(|deg| Rotation::from_degrees((deg.round() as i64).rem_euclid(360) as i32))
            .unwrap_or(Rotation::R0);
        // /UserUnit is not inheritable.
        let user_unit = dict
            .get(b"UserUnit")
            .ok()
            .and_then(|o| self.resolve(o))
            .and_then(number)
            .filter(|u| u.is_finite() && *u > 0.0)
            .unwrap_or(1.0);

        let media_box = to_rect(media);
        let crop_box = crop.map(to_rect);
        Ok(PageInfo {
            index,
            id,
            media_box,
            crop_box,
            rotate,
            user_unit,
            geometry: PageGeometry::new(media_box, crop_box, rotate, user_unit),
            annotation_count: annots_array(&self.doc, id).map_or(0, <[Object]>::len),
        })
    }

    /// All pages, in order.
    pub fn pages(&self) -> Result<Vec<PageInfo>> {
        (0..self.pages.len()).map(|i| self.page(i)).collect()
    }

    /// Number of entries in each page's `/Annots` array.
    pub fn annotation_counts(&self) -> Vec<usize> {
        self.pages
            .iter()
            .map(|id| annots_array(&self.doc, *id).map_or(0, <[Object]>::len))
            .collect()
    }

    /// The annotation dictionaries of a page, cloned exactly as stored. An `/Annots` entry
    /// that does not resolve to a dictionary is left out (its slot number is skipped).
    pub fn annotations(&self, page_index: usize) -> Result<Vec<AnnotationEntry>> {
        let page_id = self.page_id(page_index)?;
        let Some(entries) = annots_array(&self.doc, page_id) else {
            return Ok(Vec::new());
        };
        let mut out = Vec::with_capacity(entries.len());
        for (slot, entry) in entries.iter().enumerate() {
            let (id, dict) = match entry {
                Object::Reference(id) => match self.doc.objects.get(id) {
                    Some(Object::Dictionary(d)) => (Some(*id), d),
                    _ => continue,
                },
                Object::Dictionary(d) => (None, d),
                _ => continue,
            };
            out.push(AnnotationEntry {
                page_index,
                slot,
                id,
                dict: dict.clone(),
            });
        }
        Ok(out)
    }

    /// Start an edit set against this document. New object ids are handed out above the
    /// document's current highest object number.
    pub fn edit(&self) -> EditSet {
        EditSet::new(self.doc.max_id, self.state)
    }

    fn inherited<'a>(&'a self, page: &'a Dictionary, key: &[u8]) -> Option<&'a Object> {
        let mut node = page;
        for _ in 0..INHERIT_DEPTH_LIMIT {
            if let Ok(value) = node.get(key) {
                return self.resolve(value);
            }
            let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
            node = self.doc.get_dictionary(parent).ok()?;
        }
        None
    }
}

/// A PDF on disk, parsed only when first needed.
#[derive(Debug)]
pub struct PdfFile {
    path: PathBuf,
    password: Option<String>,
    pub(crate) loaded: Option<PdfDocument>,
    /// What the file on disk looked like when `loaded` was read from it.
    pub(crate) stamp: Option<DiskStamp>,
    /// The state token and stamp of a parsed document that a save consumed without replacing
    /// this file (the save failed, or wrote to another path). If the file still has that stamp
    /// when it is next loaded, the reload is the same state and gets the same token, so the
    /// edit set of a failed save can simply be tried again.
    pub(crate) resume: Option<(u64, DiskStamp)>,
    stats: Option<LoadStats>,
}

/// How many bytes at each end of a file go into a [`DiskStamp`].
const STAMP_WINDOW: usize = 64 * 1024;

/// A cheap fingerprint of a file on disk: its length, its modification time and a hash of its
/// first and last 64 KiB. An in-place save compares the stamp taken at load with the file as it
/// is immediately before the replace, so a file another program saved in the meantime is not
/// silently overwritten.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct DiskStamp {
    pub len: u64,
    pub modified: Option<SystemTime>,
    pub ends_hash: u64,
}

fn fnv1a(seed: u64, data: &[u8]) -> u64 {
    let mut h = seed;
    for b in data {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

impl DiskStamp {
    fn ends_hash(head: &[u8], tail: &[u8]) -> u64 {
        fnv1a(fnv1a(0xcbf2_9ce4_8422_2325, head), tail)
    }

    /// The stamp of bytes that were just read from (or written to) a file with this
    /// modification time.
    pub(crate) fn of_bytes(bytes: &[u8], modified: Option<SystemTime>) -> DiskStamp {
        let head = &bytes[..bytes.len().min(STAMP_WINDOW)];
        let tail = &bytes[bytes.len().saturating_sub(STAMP_WINDOW)..];
        DiskStamp {
            len: bytes.len() as u64,
            modified,
            ends_hash: DiskStamp::ends_hash(head, tail),
        }
    }

    /// Length and modification time only, read without opening the file's data (this works
    /// even while another program holds the file open without sharing).
    pub(crate) fn metadata_only(path: &Path) -> std::io::Result<(u64, Option<SystemTime>)> {
        let meta = std::fs::metadata(path)?;
        Ok((meta.len(), meta.modified().ok()))
    }

    /// The stamp of the file at `path` as it is now.
    pub(crate) fn of_file(path: &Path) -> std::io::Result<DiskStamp> {
        let mut file = std::fs::File::open(path)?;
        let meta = file.metadata()?;
        let len = meta.len();
        let window = (len.min(STAMP_WINDOW as u64)) as usize;
        let mut head = vec![0u8; window];
        file.read_exact(&mut head)?;
        let mut tail = vec![0u8; window];
        file.seek(SeekFrom::Start(len - window as u64))?;
        file.read_exact(&mut tail)?;
        Ok(DiskStamp {
            len,
            modified: meta.modified().ok(),
            ends_hash: DiskStamp::ends_hash(&head, &tail),
        })
    }
}

impl PdfFile {
    /// Open a file. Only the header is read; the object graph is parsed on first use.
    pub fn open(path: impl AsRef<Path>) -> Result<PdfFile> {
        PdfFile::open_inner(path.as_ref(), None)
    }

    /// As [`PdfFile::open`], with a password for an encrypted file. The empty user password is
    /// always tried first.
    pub fn open_with_password(path: impl AsRef<Path>, password: &str) -> Result<PdfFile> {
        PdfFile::open_inner(path.as_ref(), Some(password.to_string()))
    }

    fn open_inner(path: &Path, password: Option<String>) -> Result<PdfFile> {
        let mut file = std::fs::File::open(path).map_err(|e| PdfError::io("open", path, e))?;
        // The header may be preceded by a little junk; readers accept it in the first 1024 bytes.
        let mut head = Vec::with_capacity(1024);
        file.by_ref()
            .take(1024)
            .read_to_end(&mut head)
            .map_err(|e| PdfError::io("read", path, e))?;
        if !head.windows(5).any(|w| w == b"%PDF-") {
            return Err(PdfError::NotPdf {
                path: path.to_path_buf(),
            });
        }
        Ok(PdfFile {
            path: path.to_path_buf(),
            password,
            loaded: None,
            stamp: None,
            resume: None,
            stats: None,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// Set or clear the password used for the next load (after a password prompt, say).
    pub fn set_password(&mut self, password: Option<&str>) {
        self.password = password.map(str::to_string);
    }

    /// True once lopdf has parsed the file.
    pub fn is_loaded(&self) -> bool {
        self.loaded.is_some()
    }

    /// Cost of the most recent parse from disk, if there was one.
    pub fn load_stats(&self) -> Option<LoadStats> {
        self.stats
    }

    /// Drop the parsed document and the in-memory copy of the file. The next access re-reads it.
    pub fn unload(&mut self) {
        self.loaded = None;
    }

    /// The parsed document, loading it now if this is the first use.
    pub fn document(&mut self) -> Result<&PdfDocument> {
        if self.loaded.is_none() {
            let t0 = Instant::now();
            let io = |e| PdfError::io("read", &self.path, e);
            let mut file = std::fs::File::open(&self.path).map_err(io)?;
            // The modification time comes from the same handle the bytes are read through, so
            // the stamp describes exactly what was parsed.
            let meta = file.metadata().map_err(io)?;
            let mut bytes = Vec::with_capacity(meta.len() as usize);
            file.read_to_end(&mut bytes).map_err(io)?;
            drop(file);
            let stamp = DiskStamp::of_bytes(&bytes, meta.modified().ok());
            self.stamp = Some(stamp);
            let read_time = t0.elapsed();
            let t1 = Instant::now();
            let mut document = PdfDocument::from_bytes(bytes, self.password.as_deref())?;
            if let Some((state, _)) = self.resume.take().filter(|(_, was)| *was == stamp) {
                document.state = state;
            }
            self.stats = Some(LoadStats {
                file_len: document.bytes.len() as u64,
                read_time,
                parse_time: t1.elapsed(),
                object_count: document.doc.objects.len(),
            });
            self.loaded = Some(document);
        }
        Ok(self.loaded.as_ref().expect("loaded above"))
    }

    /// Shorthand for `document()?.page_count()`.
    pub fn page_count(&mut self) -> Result<usize> {
        Ok(self.document()?.page_count())
    }

    /// Shorthand for `document()?.edit()`.
    pub fn edit(&mut self) -> Result<EditSet> {
        Ok(self.document()?.edit())
    }
}

/// Parse with lopdf and turn "still encrypted" into a typed password error.
pub(crate) fn parse(bytes: &[u8], password: Option<&str>) -> Result<Document> {
    let options = LoadOptions {
        password: password.map(str::to_string),
        ..LoadOptions::default()
    };
    let doc = Document::load_mem_with_options(bytes, options).map_err(PdfError::from_load)?;
    if doc.is_encrypted() {
        // lopdf leaves the document encrypted when neither the empty password nor ours fits.
        return Err(if password.is_some() {
            PdfError::WrongPassword
        } else {
            PdfError::PasswordRequired
        });
    }
    Ok(doc)
}

/// The entries of a page's `/Annots`, whether it is written in the page or referenced.
pub(crate) fn annots_array(doc: &Document, page_id: ObjectId) -> Option<&[Object]> {
    let page = doc.get_dictionary(page_id).ok()?;
    match page.get(b"Annots").ok()? {
        Object::Reference(id) => doc
            .objects
            .get(id)
            .and_then(|o| o.as_array().ok())
            .map(Vec::as_slice),
        Object::Array(entries) => Some(entries.as_slice()),
        _ => None,
    }
}

/// A PDF number as f64. lopdf stores reals as f32; going through the shortest decimal form
/// gives back the value the file spelled (841.89, not 841.8900146484375).
pub fn number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) if r.is_finite() => r.to_string().parse::<f64>().ok(),
        _ => None,
    }
}

fn rect_array(doc: &Document, object: &Object) -> Option<[f64; 4]> {
    let array = object.as_array().ok()?;
    if array.len() != 4 {
        return None;
    }
    let mut out = [0.0; 4];
    for (slot, item) in out.iter_mut().zip(array) {
        let (_, direct) = doc.dereference(item).ok()?;
        *slot = number(direct)?;
    }
    Some(out)
}

/// A four-number array of direct numbers (an annotation `/Rect`, a form `/BBox`) as a
/// normalised rectangle in page user space.
pub fn rect_from_object(object: &Object) -> Option<Rect> {
    let array = object.as_array().ok()?;
    if array.len() != 4 {
        return None;
    }
    let mut v = [0.0; 4];
    for (slot, item) in v.iter_mut().zip(array) {
        *slot = number(item)?;
    }
    Some(to_rect(v))
}

fn to_rect(v: [f64; 4]) -> Rect {
    Rect::new(
        v[0].min(v[2]),
        v[1].min(v[3]),
        v[0].max(v[2]),
        v[1].max(v[3]),
    )
}

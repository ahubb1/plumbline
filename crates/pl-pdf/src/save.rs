//! Safe save: write `name.pdf.tmp`, verify it, then replace the destination atomically.
//!
//! The destination is not touched until the temporary file has been written, flushed to disk,
//! read back and verified. Any failure before the replace removes the temporary file and
//! leaves the destination byte-for-byte as it was.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};

use std::collections::BTreeMap;

use lopdf::{Document, IncrementalDocument, Object, ObjectId};

use crate::document::{DiskStamp, PdfDocument, PdfFile};
use crate::edit::{EditSet, ResolvedEdits};
use crate::error::{PdfError, Result, VerifyError};
use crate::replace::{replace_file, ReplaceMethod};
use crate::verify::{check_prefix, verify_parsed, Appended, Expected, VerifyReport};

/// Which writer produced the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveKind {
    /// The original bytes followed by the changed objects, a cross-reference section and a
    /// trailer.
    Incremental,
    /// Every live object written afresh into a single revision.
    Full,
}

/// A place where a save can be made to fail, for tests of "a crash during save leaves the
/// original intact".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailPoint {
    /// After the temporary file is complete on disk, before verification.
    AfterTmpWrite,
    /// After verification passed, immediately before the replace.
    BeforeReplace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveOptions {
    /// Re-open the temporary file with lopdf and hayro-syntax before replacing. On by default;
    /// turning it off skips the read-back entirely.
    pub verify: bool,
    /// Test hook: fail with [`PdfError::InjectedFailure`] at this point.
    pub fail_point: Option<FailPoint>,
    /// Keep the temporary file when the save fails (as a killed process would). Off by default.
    pub keep_tmp_on_failure: bool,
}

impl Default for SaveOptions {
    fn default() -> Self {
        SaveOptions {
            verify: true,
            fail_point: None,
            keep_tmp_on_failure: false,
        }
    }
}

/// What a successful save did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveReport {
    pub kind: SaveKind,
    pub path: PathBuf,
    /// Length of the file the document was loaded from.
    pub original_len: u64,
    /// Length of the file now at `path`.
    pub new_len: u64,
    /// Objects written because they are new or changed (incremental), or all live objects (full).
    pub objects_written: usize,
    /// `None` only when verification was switched off.
    pub verification: Option<VerifyReport>,
    pub replace: ReplaceMethod,
}

/// A further check a caller runs on the temporary file after pl-pdf's own verification and
/// before the destination is replaced. It gets the temporary file's path and what the file
/// must contain. An error stops the save with the original untouched, like any other
/// verification failure. The check must not leave the temporary file open or mapped: the
/// replace that follows would fail.
///
/// This is how a reader pl-pdf cannot depend on (PDFium, through pl-render) takes part in
/// "verify, then replace".
pub type ExternalCheck<'a> =
    &'a mut dyn FnMut(&Path, &Expected) -> std::result::Result<(), VerifyError>;

/// The temporary file a save to `dest` writes first: `name.pdf` gives `name.pdf.tmp`.
pub fn tmp_path_for(dest: &Path) -> PathBuf {
    let mut name = dest.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

impl PdfFile {
    /// Append `edits` to the file as an incremental update and replace it in place.
    ///
    /// The appended section uses the cross-reference style of the previous revision. For an
    /// encrypted file opened with its password, the appended objects are encrypted with the
    /// same key and the trailer keeps `/Encrypt`.
    pub fn save_incremental(
        &mut self,
        edits: &EditSet,
        options: &SaveOptions,
    ) -> Result<SaveReport> {
        let dest = self.path().to_path_buf();
        self.save(SaveKind::Incremental, &dest, edits, options, None)
    }

    /// As [`PdfFile::save_incremental`], with a caller-supplied check between verification
    /// and replace (see [`ExternalCheck`]). The check runs whether or not
    /// [`SaveOptions::verify`] is on.
    pub fn save_incremental_checked(
        &mut self,
        edits: &EditSet,
        options: &SaveOptions,
        check: ExternalCheck<'_>,
    ) -> Result<SaveReport> {
        let dest = self.path().to_path_buf();
        self.save(SaveKind::Incremental, &dest, edits, options, Some(check))
    }

    /// As [`PdfFile::save_incremental`], writing to another path. This handle keeps pointing
    /// at the original file, which is not modified.
    pub fn save_incremental_as(
        &mut self,
        dest: &Path,
        edits: &EditSet,
        options: &SaveOptions,
    ) -> Result<SaveReport> {
        self.save(SaveKind::Incremental, dest, edits, options, None)
    }

    /// Apply `edits` and rewrite the whole file as a single revision, in place. Unreferenced
    /// objects (deleted annotations, superseded versions) are dropped. An encrypted file is
    /// encrypted again with the same keys and permissions.
    pub fn save_full(&mut self, edits: &EditSet, options: &SaveOptions) -> Result<SaveReport> {
        let dest = self.path().to_path_buf();
        self.save(SaveKind::Full, &dest, edits, options, None)
    }

    /// As [`PdfFile::save_full`], writing to another path.
    pub fn save_full_as(
        &mut self,
        dest: &Path,
        edits: &EditSet,
        options: &SaveOptions,
    ) -> Result<SaveReport> {
        self.save(SaveKind::Full, dest, edits, options, None)
    }

    /// As [`PdfFile::save_incremental_as`], with a caller-supplied check on the temporary file
    /// before it becomes `dest` (see [`ExternalCheck`]).
    pub fn save_incremental_as_checked(
        &mut self,
        dest: &Path,
        edits: &EditSet,
        options: &SaveOptions,
        check: ExternalCheck<'_>,
    ) -> Result<SaveReport> {
        self.save(SaveKind::Incremental, dest, edits, options, Some(check))
    }

    /// As [`PdfFile::save_full`], with a caller-supplied check between verification and
    /// replace (see [`ExternalCheck`]).
    pub fn save_full_checked(
        &mut self,
        edits: &EditSet,
        options: &SaveOptions,
        check: ExternalCheck<'_>,
    ) -> Result<SaveReport> {
        let dest = self.path().to_path_buf();
        self.save(SaveKind::Full, &dest, edits, options, Some(check))
    }

    /// As [`PdfFile::save_full_as`], with a caller-supplied check on the temporary file before
    /// it becomes `dest` (see [`ExternalCheck`]).
    pub fn save_full_as_checked(
        &mut self,
        dest: &Path,
        edits: &EditSet,
        options: &SaveOptions,
        check: ExternalCheck<'_>,
    ) -> Result<SaveReport> {
        self.save(SaveKind::Full, dest, edits, options, Some(check))
    }

    fn save(
        &mut self,
        kind: SaveKind,
        dest: &Path,
        edits: &EditSet,
        options: &SaveOptions,
        mut external: Option<ExternalCheck<'_>>,
    ) -> Result<SaveReport> {
        let (resolved, expected) = {
            let loaded = self.document()?;
            if kind == SaveKind::Incremental {
                if edits.is_empty() {
                    return Err(PdfError::NothingToSave);
                }
                if loaded.doc.xref_recovery.is_some() || loaded.doc.xref_start == 0 {
                    return Err(PdfError::IncrementalUnsupported(
                        "its cross-reference data was damaged and had to be repaired on load; use a full save".into(),
                    ));
                }
            }
            let resolved = edits.resolve(&loaded.doc, &loaded.pages, loaded.state)?;
            let expected = Expected {
                page_count: loaded.pages.len(),
                annotation_counts: resolved.annotation_counts.clone(),
            };
            (resolved, expected)
        };

        let in_place = dest == self.path();
        let tmp = tmp_path_for(dest);
        let password = self.password().map(str::to_string);

        // Both writers consume the parsed document, so from here on this handle is unloaded.
        // On success in place it gets the verified re-parse; otherwise it reloads on next use.
        let PdfDocument {
            bytes, doc, state, ..
        } = self.loaded.take().expect("document() loaded it");
        // Unless this save replaces the file, a reload of the unchanged file is this same
        // state (see `PdfFile::resume`).
        self.resume = self.stamp.map(|stamp| (state, stamp));
        let original_len = bytes.len() as u64;
        let prev_xref_start = doc.xref_start as u64;

        // An in-place save replaces the file, so the file must still be the one that was
        // loaded. First check, while the loaded bytes are still at hand to settle a changed
        // modification time; the second, cheap one is immediately before the replace.
        let mut stamp = self.stamp;
        if in_place {
            if let Some(stamp) = stamp.as_mut() {
                check_unchanged_with_bytes(dest, stamp, &bytes)?;
            }
        }

        let mut incremental: Option<IncrementalDocument> = None;
        let written = match kind {
            SaveKind::Incremental => {
                let inc = incremental.insert(prepare_incremental(bytes, doc, resolved));
                let count = inc.new_document.objects.len();
                write_tmp(&tmp, |w| inc.save_to(w)).map(|()| count)
            }
            SaveKind::Full => {
                drop(bytes);
                prepare_full(doc, resolved).and_then(|(mut doc, save_options)| {
                    let count = doc.objects.len();
                    write_tmp(&tmp, |w| doc.save_with_options(w, save_options)).map(|()| count)
                })
            }
        };

        let outcome = written.and_then(|objects_written| {
            if options.fail_point == Some(FailPoint::AfterTmpWrite) {
                return Err(PdfError::InjectedFailure(FailPoint::AfterTmpWrite));
            }
            let (verification, reparsed) = if options.verify {
                let saved = std::fs::read(&tmp).map_err(|e| PdfError::io("read back", &tmp, e))?;
                // Compare against the original first, then free it and the old object graph
                // before the two re-parses, which keeps the peak near three file sizes, not four.
                let appended = match incremental.take() {
                    Some(mut inc) => {
                        check_prefix(&saved, inc.get_prev_documents_bytes())?;
                        Some((
                            Appended {
                                original_len,
                                prev_xref_start,
                            },
                            std::mem::take(&mut inc.new_document.objects),
                        ))
                    }
                    None => None,
                };
                let (mut report, document) = verify_parsed(
                    saved,
                    password.as_deref(),
                    &expected,
                    appended.as_ref().map(|(a, _)| *a),
                )?;
                report.prefix_checked = appended.is_some();
                if let Some((_, written)) = &appended {
                    // Counts alone say nothing about a patch-only save: every object that was
                    // written must read back as it was resolved.
                    check_written(written, &document.doc)?;
                }
                (Some(report), Some(document))
            } else {
                (None, None)
            };
            if let Some(check) = external.as_mut() {
                check(&tmp, &expected)?;
            }
            if options.fail_point == Some(FailPoint::BeforeReplace) {
                return Err(PdfError::InjectedFailure(FailPoint::BeforeReplace));
            }
            let new_len = std::fs::metadata(&tmp)
                .map_err(|e| PdfError::io("stat", &tmp, e))?
                .len();
            if in_place {
                if let Some(stamp) = &stamp {
                    check_unchanged(dest, stamp)?;
                }
            }
            let replace = replace_file(&tmp, dest)?;
            Ok((objects_written, verification, reparsed, new_len, replace))
        });

        match outcome {
            Ok((objects_written, verification, reparsed, new_len, replace)) => {
                if in_place {
                    self.loaded = reparsed;
                    self.stamp = DiskStamp::of_file(dest).ok();
                    self.resume = None;
                }
                Ok(SaveReport {
                    kind,
                    path: dest.to_path_buf(),
                    original_len,
                    new_len,
                    objects_written,
                    verification,
                    replace,
                })
            }
            Err(error) => {
                if !options.keep_tmp_on_failure {
                    let _ = std::fs::remove_file(&tmp);
                }
                Err(error)
            }
        }
    }
}

/// Keys of a cross-reference stream's own dictionary that must not be carried into the next
/// trailer: the writer sets `/Type`, `/W`, `/Index`, `/Length` and `/Filter` itself, but a
/// stale `/DecodeParms` (a PNG predictor, as most real files use) would make the new, plain
/// stream unreadable, and `/XRefStm` would point a hybrid reader at an old stream.
pub(crate) const STALE_TRAILER_KEYS: [&[u8]; 2] = [b"DecodeParms", b"XRefStm"];

fn prepare_incremental(
    bytes: Vec<u8>,
    doc: Document,
    resolved: ResolvedEdits,
) -> IncrementalDocument {
    let mut inc = IncrementalDocument::create_from(bytes, doc);
    for key in STALE_TRAILER_KEYS {
        inc.new_document.trailer.remove(key);
    }
    inc.new_document.objects = resolved.objects;
    inc.new_document.max_id = inc.new_document.max_id.max(resolved.max_id);
    inc
}

fn prepare_full(
    mut doc: Document,
    resolved: ResolvedEdits,
) -> Result<(Document, lopdf::SaveOptions)> {
    let had_object_streams = doc
        .reference_table
        .entries
        .values()
        .any(lopdf::xref::XrefEntry::is_compressed);
    let xref_stream = matches!(
        doc.reference_table.cross_reference_type,
        lopdf::xref::XrefType::CrossReferenceStream
    );

    for (id, object) in resolved.objects {
        doc.objects.insert(id, object);
    }
    doc.max_id = doc.max_id.max(resolved.max_id);
    // A single revision has no previous section.
    doc.trailer.remove(b"Prev");
    for key in STALE_TRAILER_KEYS {
        doc.trailer.remove(key);
    }
    // Deleted annotations are no longer in any /Annots. They go only if nothing else (a popup's
    // /Parent, a reply's /IRT) still points at them, so no dangling reference is created.
    doc.prune_objects();

    let encrypted = doc.encryption_state.clone();
    if let Some(state) = &encrypted {
        doc.encrypt(state)
            .map_err(|e| PdfError::Write(std::io::Error::other(e)))?;
    }
    let save_options = lopdf::SaveOptions {
        // lopdf writes an encrypted document object by object in any case.
        use_object_streams: had_object_streams && encrypted.is_none(),
        use_xref_streams: xref_stream,
        ..Default::default()
    };
    Ok((doc, save_options))
}

/// Create `tmp`, let `write` fill it, and force it to disk.
fn write_tmp(
    tmp: &Path,
    write: impl FnOnce(&mut BufWriter<File>) -> std::io::Result<()>,
) -> Result<()> {
    let file = File::create(tmp).map_err(|e| PdfError::io("create", tmp, e))?;
    let mut writer = BufWriter::with_capacity(1 << 20, file);
    write(&mut writer).map_err(PdfError::Write)?;
    writer.flush().map_err(PdfError::Write)?;
    let file = writer
        .into_inner()
        .map_err(|e| PdfError::Write(e.into_error()))?;
    file.sync_all().map_err(PdfError::Write)?;
    Ok(())
}

fn changed_on_disk(dest: &Path) -> PdfError {
    PdfError::ChangedOnDisk {
        path: dest.to_path_buf(),
    }
}

/// The file's stamp now, or `None` when its data cannot be read although the file is there
/// (another program holds it open without sharing). In that case length and modification
/// time, which need no read access, must still match; the replace that follows is then for
/// the operating system to refuse or allow. A missing file counts as changed.
fn stamp_now(dest: &Path, stamp: &DiskStamp) -> Result<Option<DiskStamp>> {
    match DiskStamp::of_file(dest) {
        Ok(now) => Ok(Some(now)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(changed_on_disk(dest)),
        Err(_) => match DiskStamp::metadata_only(dest) {
            Ok((len, modified)) if len == stamp.len && modified == stamp.modified => Ok(None),
            _ => Err(changed_on_disk(dest)),
        },
    }
}

/// Fails with [`PdfError::ChangedOnDisk`] unless the file at `dest` still has the stamp taken
/// when it was loaded.
fn check_unchanged(dest: &Path, stamp: &DiskStamp) -> Result<()> {
    match stamp_now(dest, stamp)? {
        Some(now) if now != *stamp => Err(changed_on_disk(dest)),
        _ => Ok(()),
    }
}

/// As [`check_unchanged`], but a file whose modification time alone differs is compared byte
/// for byte with what was loaded; if it is identical the stamp is brought up to date (a backup
/// or sync tool touched the file without changing it).
fn check_unchanged_with_bytes(dest: &Path, stamp: &mut DiskStamp, loaded: &[u8]) -> Result<()> {
    let Some(now) = stamp_now(dest, stamp)? else {
        return Ok(());
    };
    if now == *stamp {
        return Ok(());
    }
    if now.len != stamp.len || now.ends_hash != stamp.ends_hash {
        return Err(changed_on_disk(dest));
    }
    let on_disk = std::fs::read(dest).map_err(|_| changed_on_disk(dest))?;
    if on_disk != loaded {
        return Err(changed_on_disk(dest));
    }
    // Stamp with the time seen before the read, so a write that raced the comparison still
    // shows up at the second check.
    *stamp = DiskStamp::of_bytes(&on_disk, now.modified);
    Ok(())
}

/// Every object an incremental save wrote must be in the re-parse and equal to what was
/// resolved.
fn check_written(
    written: &BTreeMap<ObjectId, Object>,
    reparsed: &Document,
) -> std::result::Result<(), VerifyError> {
    for (id, want) in written {
        let Some(found) = reparsed.objects.get(id) else {
            return Err(VerifyError::ObjectMismatch {
                id: *id,
                problem: "as missing",
            });
        };
        if !same_object(want, found) {
            return Err(VerifyError::ObjectMismatch {
                id: *id,
                problem: "with different content",
            });
        }
    }
    Ok(())
}

fn as_number(object: &Object) -> Option<f64> {
    match object {
        Object::Integer(i) => Some(*i as f64),
        Object::Real(r) => Some(f64::from(*r)),
        _ => None,
    }
}

/// Stream keys the writer or a filter may legitimately change.
const STREAM_ENCODING_KEYS: [&[u8]; 3] = [b"Length", b"Filter", b"DecodeParms"];

/// The stream's data with its filters undone, when that can be worked out.
fn plain_content(stream: &lopdf::Stream) -> Option<Vec<u8>> {
    if stream.dict.has(b"Filter") {
        stream.decompressed_content().ok()
    } else {
        Some(stream.content.clone())
    }
}

/// Whether two objects are the same after a trip through the writer and the parser: numbers
/// compare by value (a whole real may come back as an integer),
/// strings by their bytes whatever their notation, streams by decoded data.
fn same_object(a: &Object, b: &Object) -> bool {
    if let (Some(x), Some(y)) = (as_number(a), as_number(b)) {
        // lopdf writes the shortest text that reads back as the same single-precision value,
        // so a real survives exactly; the margin is only for the integer/real distinction.
        return (x - y).abs() <= 1e-6 * x.abs().max(1.0);
    }
    match (a, b) {
        (Object::Null, Object::Null) => true,
        (Object::Boolean(x), Object::Boolean(y)) => x == y,
        (Object::Name(x), Object::Name(y)) => x == y,
        (Object::String(x, _), Object::String(y, _)) => x == y,
        (Object::Reference(x), Object::Reference(y)) => x == y,
        (Object::Array(x), Object::Array(y)) => {
            x.len() == y.len() && x.iter().zip(y).all(|(p, q)| same_object(p, q))
        }
        (Object::Dictionary(x), Object::Dictionary(y)) => same_dict(x, y, &[]),
        (Object::Stream(x), Object::Stream(y)) => {
            same_dict(&x.dict, &y.dict, &STREAM_ENCODING_KEYS)
                && match (plain_content(x), plain_content(y)) {
                    (Some(p), Some(q)) => p == q,
                    // A filter neither side can undo here: the dictionaries matched and the
                    // other checks stand.
                    _ => true,
                }
        }
        _ => false,
    }
}

fn same_dict(a: &lopdf::Dictionary, b: &lopdf::Dictionary, skip: &[&[u8]]) -> bool {
    let keys = |d: &lopdf::Dictionary| {
        d.iter()
            .filter(|(k, _)| !skip.contains(&k.as_slice()))
            .count()
    };
    keys(a) == keys(b)
        && a.iter()
            .filter(|(k, _)| !skip.contains(&k.as_slice()))
            .all(|(k, v)| b.get(k).is_ok_and(|w| same_object(v, w)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Stream, StringFormat};

    fn name(n: &str) -> Object {
        Object::Name(n.as_bytes().to_vec())
    }

    #[test]
    fn objects_compare_by_value_not_by_notation() {
        assert!(same_object(&Object::Real(2.0), &Object::Integer(2)));
        assert!(same_object(&Object::Real(841.89), &Object::Real(841.89)));
        assert!(!same_object(&Object::Real(841.89), &Object::Real(841.9)));
        assert!(!same_object(&Object::Real(2.5), &Object::Integer(2)));
        assert!(same_object(
            &Object::String(b"ab".to_vec(), StringFormat::Literal),
            &Object::String(b"ab".to_vec(), StringFormat::Hexadecimal)
        ));
        let a = Object::Dictionary(dictionary! { "A" => 1, "B" => name("X") });
        let b = Object::Dictionary(dictionary! { "B" => name("X"), "A" => 1 });
        let c = Object::Dictionary(dictionary! { "A" => 1 });
        assert!(same_object(&a, &b));
        assert!(!same_object(&a, &c));
        assert!(!same_object(&c, &a));
        assert!(!same_object(&a, &Object::Null));
    }

    #[test]
    fn streams_compare_by_decoded_data() {
        let plain = Stream::new(
            dictionary! { "Type" => name("XObject") },
            b"0 0 m 5 5 l S ".repeat(200),
        );
        let mut packed = plain.clone();
        packed.compress().unwrap();
        assert!(
            packed.dict.has(b"Filter"),
            "the test needs a filtered stream"
        );
        assert!(same_object(
            &Object::Stream(plain.clone()),
            &Object::Stream(packed)
        ));
        let mut other = plain.clone();
        other.content = b"0 0 m 6 6 l S".to_vec();
        assert!(!same_object(
            &Object::Stream(plain.clone()),
            &Object::Stream(other)
        ));
        let mut retyped = plain.clone();
        retyped.dict.set("Type", name("Other"));
        assert!(!same_object(
            &Object::Stream(plain),
            &Object::Stream(retyped)
        ));
    }

    #[test]
    fn a_written_object_that_is_missing_or_different_fails() {
        let mut doc = Document::with_version("1.4");
        doc.objects.insert((5, 0), Object::Integer(7));
        let mut written = BTreeMap::new();
        written.insert((5, 0), Object::Integer(7));
        assert!(check_written(&written, &doc).is_ok());
        written.insert((6, 0), Object::Null);
        assert!(matches!(
            check_written(&written, &doc),
            Err(VerifyError::ObjectMismatch { id: (6, 0), .. })
        ));
        written.remove(&(6, 0));
        written.insert((5, 0), Object::Integer(8));
        assert!(matches!(
            check_written(&written, &doc),
            Err(VerifyError::ObjectMismatch { id: (5, 0), .. })
        ));
    }
}

//! What a user interface needs from a document beyond its commands (milestone M8): a new file
//! combined from several ([`combine_files`]), what to print from ([`Document::print_source`])
//! and every link of the document at once ([`Document::document_links`]).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_annot::AnnotSet;
use pl_pdf::{PdfDocument, PdfError, PdfFile, SaveOptions, SaveReport, SignaturePolicy};
use pl_render::RenderBackend;

use crate::error::{DocError, Result};
use crate::session::{pdfium_check, Document, Origin};
use crate::structure::LinkInfo;

/// One file of a combination, and which of its pages (zero-based, in that order; `None` for
/// all of them).
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct CombinePart {
    pub path: PathBuf,
    pub pages: Option<Vec<u32>>,
    /// The document as it stands in memory, read instead of the file: an open document with
    /// unsaved changes ([`Document::print_source`]).
    pub bytes: Option<Arc<Vec<u8>>>,
    /// The password `bytes` (or the file) opens with; `None` for the empty one.
    pub password: Option<String>,
}

impl CombinePart {
    /// A part read from its file.
    pub fn file(path: impl Into<PathBuf>, pages: Option<Vec<u32>>) -> CombinePart {
        CombinePart {
            path: path.into(),
            pages,
            ..CombinePart::default()
        }
    }
}

/// Reads a part of a combination.
fn load(path: &Path, part: &CombinePart) -> Result<PdfDocument> {
    if let Some(bytes) = &part.bytes {
        return Ok(PdfDocument::from_bytes(
            bytes.to_vec(),
            part.password.as_deref(),
        )?);
    }
    let mut file = match &part.password {
        Some(password) => PdfFile::open_with_password(path, password)?,
        None => PdfFile::open(path)?,
    };
    Ok(file.document()?.clone())
}

/// Writes a new document at `dest` made of `parts` in order: the pages of each part, with their
/// bookmarks, one after the other. The file is written through the full rewrite with its
/// verification (lopdf, hayro-syntax and PDFium through `backend`) before it appears; no source
/// file changes. `dest` must not be one of the parts.
pub fn combine_files(
    backend: &Arc<dyn RenderBackend>,
    parts: &[CombinePart],
    dest: &Path,
    signatures: SignaturePolicy,
) -> Result<SaveReport> {
    if parts.is_empty() {
        return Err(PdfError::InvalidOperation("there are no files to combine".into()).into());
    }
    let dest = std::path::absolute(dest).map_err(|e| DocError::io("resolve", dest, e))?;
    let mut docs = Vec::with_capacity(parts.len());
    for part in parts {
        let path =
            std::path::absolute(&part.path).map_err(|e| DocError::io("resolve", &part.path, e))?;
        if crate::journal::same_path(&path, &dest) {
            return Err(PdfError::InvalidOperation(
                "the combined file cannot replace one of the files it is made from".into(),
            )
            .into());
        }
        let doc = load(&path, part)?;
        let doc = match &part.pages {
            None => doc,
            Some(pages) => {
                if pages.is_empty() {
                    return Err(PdfError::InvalidOperation(format!(
                        "no pages were chosen from {}",
                        path.display()
                    ))
                    .into());
                }
                let pages: Vec<usize> = pages.iter().map(|&p| p as usize).collect();
                doc.extract_pages(&pages)?.0
            }
        };
        docs.push(doc);
    }
    let first = docs.remove(0);
    let combined = if docs.is_empty() {
        first
    } else {
        let others: Vec<&PdfDocument> = docs.iter().collect();
        first.merge(&others)?.0
    };
    let backend = backend.clone();
    let mut check = |tmp: &Path, expected: &pl_pdf::Expected| {
        pdfium_check(backend.as_ref(), tmp, None, expected, &[], &[])
    };
    Ok(combined.save_new_checked(&dest, &SaveOptions::default(), signatures, &mut check)?)
}

/// What a print job reads: the file itself, or the document as it stands in memory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PrintSource {
    File(PathBuf),
    Bytes(Vec<u8>),
}

impl Document {
    /// What to print so that the printout shows the document as it stands now: its own file
    /// when nothing is unsaved, otherwise the bytes a save would write (page edits and unsaved
    /// markups included). The render mirror is not used, because it hides the markups the
    /// interface draws itself.
    pub fn print_source(&mut self) -> Result<PrintSource> {
        let unsaved_markups = self.editor().is_some_and(|e| e.has_unsaved());
        if !self.has_structure_changes() && !unsaved_markups {
            match &self.origin {
                Origin::File(path) => return Ok(PrintSource::File(path.clone())),
                Origin::Memory { bytes, .. } => return Ok(PrintSource::Bytes(bytes.to_vec())),
            }
        }
        Ok(PrintSource::Bytes(self.committed_document()?.to_bytes()?))
    }

    /// The password the document was opened with, for opening it again (to print it).
    pub fn open_password(&self) -> Option<&str> {
        self.password.as_deref()
    }

    /// The links of every page as the structure stands (index = page), each with the id the
    /// annotation store gives it and where it leads in this document. One read of the document
    /// for all pages; unsaved markup changes are not in it.
    pub fn document_links(&mut self) -> Result<Vec<Vec<LinkInfo>>> {
        let doc = self.structure_base()?;
        let set = AnnotSet::load(&doc);
        let mut out = Vec::with_capacity(doc.page_count());
        for page in 0..doc.page_count() {
            let links = doc.links(page).unwrap_or_default();
            out.push(
                links
                    .into_iter()
                    .map(|link| {
                        let id = set
                            .iter()
                            .find(|a| {
                                a.page == page
                                    && a.baseline()
                                        .is_some_and(|b| b.object.is_some() && b.object == link.id)
                            })
                            .map(|a| a.id.clone());
                        let resolved = doc.resolve_link_action(&link.action, page);
                        LinkInfo { id, link, resolved }
                    })
                    .collect(),
            );
        }
        Ok(out)
    }
}

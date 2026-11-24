//! The error type of the session layer.

use std::path::{Path, PathBuf};

use pl_render::RenderError;

use crate::session::DocumentId;

/// Errors from the session layer.
#[derive(Debug, thiserror::Error)]
pub enum DocError {
    #[error(transparent)]
    Render(#[from] RenderError),
    #[error(transparent)]
    Pdf(#[from] pl_pdf::PdfError),
    #[error("{op} {path}: {source}")]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("document {0:?} is not open in this session")]
    UnknownDocument(DocumentId),
    #[error("page {page} is out of range (the document has {count} pages)")]
    PageOutOfRange { page: u32, count: u32 },
    #[error("the document was opened from memory and has no file")]
    NoFile,
    #[error("the geometry of page {page} has not been read yet")]
    GeometryPending { page: u32 },
    #[error("page {page} could not be read from the file")]
    PageUnreadable { page: u32 },
    #[error(
        "{} is still in use by {workers} background task(s) of this document and cannot be \
         replaced yet; try again in a moment or save under another name",
        path.display()
    )]
    FileBusy { path: PathBuf, workers: usize },
    /// A command was refused; nothing changed.
    #[error(transparent)]
    Edit(#[from] crate::command::EditError),
    /// The annotation changes could not be turned into a save.
    #[error(transparent)]
    Annot(#[from] pl_annot::AnnotError),
    #[error("the markups could not be drawn: {0}")]
    Draw(String),
    #[error("the document has unsaved markup changes; save or discard them first")]
    UnsavedMarkups,
    #[error("{} is open in another tab; close it there or choose another name", path.display())]
    OpenElsewhere { path: PathBuf },
    #[error("command file: {0}")]
    Script(String),
    /// The document is being saved on a worker thread and is read-only until that is
    /// finished ([`crate::Document::try_finish_save`]); nothing changed.
    #[error("the document is being saved; try again when the save has finished")]
    Saving,
    /// The document's annotations are being read on a worker thread; nothing changed.
    #[error("the document's markups are still being read; try again in a moment")]
    Loading,
    /// A background worker could not be started, or stopped without a result.
    #[error("background work failed: {0}")]
    Worker(String),
    /// The ticket does not name the save or load that is in flight.
    #[error("that save or load is not running (it was finished already)")]
    NoSuchTicket,
    /// A picture could not be imported.
    #[error(transparent)]
    Image(#[from] crate::images::ImportError),
}

impl DocError {
    pub(crate) fn io(op: &'static str, path: &Path, source: std::io::Error) -> DocError {
        DocError::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }
}

pub type Result<T> = std::result::Result<T, DocError>;

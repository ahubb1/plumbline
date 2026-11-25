//! Typed errors for load, edit, save, verification and replace.

use std::path::PathBuf;

use lopdf::ObjectId;

use crate::save::FailPoint;

pub type Result<T> = std::result::Result<T, PdfError>;

#[derive(Debug, thiserror::Error)]
pub enum PdfError {
    #[error("{op} failed for {}: {source}", path.display())]
    Io {
        op: &'static str,
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("{} is not a PDF file (no %PDF- header)", path.display())]
    NotPdf { path: PathBuf },

    #[error("the PDF could not be parsed: {0}")]
    Parse(#[source] lopdf::Error),

    #[error("the PDF is encrypted and needs a password")]
    PasswordRequired,

    #[error("the password is not valid for this PDF")]
    WrongPassword,

    #[error("the PDF has no readable page tree")]
    NoPages,

    #[error("page index {index} is out of range (the document has {count} pages)")]
    PageOutOfRange { index: usize, count: usize },

    #[error("object {} {} does not exist", .0 .0, .0 .1)]
    ObjectNotFound(ObjectId),

    #[error("object {} {} is not a dictionary or stream, so its keys cannot be edited", .0 .0, .0 .1)]
    NotADictionary(ObjectId),

    #[error("the /Annots entry of page index {page_index} is not an array")]
    AnnotsNotArray { page_index: usize },

    #[error("annotation {} {} is not in the /Annots array of page index {page_index}", id.0, id.1)]
    AnnotationNotOnPage { id: ObjectId, page_index: usize },

    #[error("object id {} {} was reserved in the edit set but never given a value", .0 .0, .0 .1)]
    ReservedIdUnset(ObjectId),

    #[error(
        "the edit set was started against an earlier state of the document (it has been saved or \
         reloaded since); start a new edit set"
    )]
    StaleEditSet,

    #[error(
        "{} was changed on disk by another program after it was loaded; reload it or save under \
         another name",
        path.display()
    )]
    ChangedOnDisk { path: PathBuf },

    #[error("the document has no optional-content layers")]
    NoLayers,

    #[error("the edit set is empty, so there is nothing to append")]
    NothingToSave,

    #[error("incremental save is not possible for this file: {0}")]
    IncrementalUnsupported(String),

    #[error("writing the PDF failed: {0}")]
    Write(#[source] std::io::Error),

    #[error("the saved file failed verification: {0}")]
    Verify(#[from] VerifyError),

    #[error(
        "could not replace {} with {}: ReplaceFileW error {replace_code}, MoveFileExW error {move_code}",
        to.display(),
        from.display()
    )]
    Replace {
        from: PathBuf,
        to: PathBuf,
        replace_code: u32,
        move_code: u32,
    },

    #[error("save stopped at injected fail point {0:?}")]
    InjectedFailure(FailPoint),

    #[error("{0}")]
    InvalidOperation(String),

    #[error(
        "a full rewrite would invalidate the document's digital signatures ({}); confirm to go on",
        fields.join(", ")
    )]
    SignaturesWouldBreak { fields: Vec<String> },
}

impl PdfError {
    pub(crate) fn io(op: &'static str, path: &std::path::Path, source: std::io::Error) -> Self {
        PdfError::Io {
            op,
            path: path.to_path_buf(),
            source,
        }
    }

    /// Map a lopdf load error, pulling the password cases out into their own variants.
    pub(crate) fn from_load(error: lopdf::Error) -> Self {
        match error {
            lopdf::Error::InvalidPassword => PdfError::WrongPassword,
            lopdf::Error::Decryption(lopdf::encryption::DecryptionError::IncorrectPassword) => {
                PdfError::WrongPassword
            }
            other => PdfError::Parse(other),
        }
    }
}

/// Why a freshly written file was rejected. The original file is untouched whenever one of
/// these is returned.
#[derive(Debug, thiserror::Error)]
pub enum VerifyError {
    #[error("the original bytes are not an exact prefix of the saved file (first difference at byte {offset})")]
    PrefixMismatch { offset: u64 },

    #[error("the saved file is shorter than the original ({saved} < {original} bytes)")]
    Truncated { saved: u64, original: u64 },

    #[error("{parser} could not re-open the saved file: {message}")]
    Reparse {
        parser: &'static str,
        message: String,
    },

    #[error(
        "{parser} could only read the saved file by repairing its cross-reference data, so what \
         was written is not what a reader will see"
    )]
    XrefRepaired { parser: &'static str },

    #[error(
        "the newest cross-reference section starts at byte {xref_start}, inside the original \
         {original_len} bytes, so the appended revision is not the one a reader will use"
    )]
    XrefNotAppended { xref_start: u64, original_len: u64 },

    #[error(
        "the appended trailer's /Prev is {found:?}, but the previous revision's cross-reference \
         section starts at byte {expected}"
    )]
    PrevMismatch { expected: u64, found: Option<i64> },

    #[error("object {} {} was written but reads back {problem}", id.0, id.1)]
    ObjectMismatch { id: ObjectId, problem: &'static str },

    #[error(
        "page index {page_index}, which the save did not touch, renders differently in {parser} \
         after the save"
    )]
    RenderChanged {
        parser: &'static str,
        page_index: usize,
    },

    #[error("{parser} counts {found} pages, expected {expected}")]
    PageCount {
        parser: &'static str,
        expected: usize,
        found: usize,
    },

    #[error("{parser} counts {found} annotations on page index {page_index}, expected {expected}")]
    AnnotationCount {
        parser: &'static str,
        page_index: usize,
        expected: usize,
        found: usize,
    },
}

//! Post-save verification: re-open what was written with two parsers that share no code
//! (lopdf, which wrote it, and hayro-syntax) and compare page and annotation counts.
//!
//! Both parsers repair damaged cross-reference data by scanning the file, so "it re-opened" is
//! not enough: lopdf must have read the cross-reference data *as written*, and for an
//! incremental save the newest section must lie in the appended part and chain to the previous
//! revision through `/Prev`. (PDFium is the third reader; it lives in pl-render, so that check
//! is made by pl-doc, which can see both crates.)

use std::path::Path;
use std::sync::Arc;

use hayro_syntax::object::dict::keys::ANNOTS;
use hayro_syntax::object::Array;
use hayro_syntax::{LoadPdfError, Pdf};

use crate::document::{parse, PdfDocument};
use crate::error::{PdfError, Result, VerifyError};

/// What the saved file must contain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expected {
    pub page_count: usize,
    /// `/Annots` length for each page, in page order.
    pub annotation_counts: Vec<usize>,
}

/// How the independent parser fared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecondParser {
    /// hayro-syntax opened the file and its counts matched.
    Passed,
    /// hayro-syntax could not be used on this file, with the reason. Only happens for a file
    /// encrypted in a way hayro-syntax cannot open with the password we hold.
    Skipped(String),
}

/// Result of a successful verification.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerifyReport {
    pub file_len: u64,
    pub page_count: usize,
    pub annotation_counts: Vec<usize>,
    /// True when the original bytes were checked to be an exact prefix.
    pub prefix_checked: bool,
    pub second_parser: SecondParser,
}

/// Verify a file on disk. See [`verify_bytes`].
pub fn verify_file(
    path: &Path,
    password: Option<&str>,
    expected: &Expected,
    original_prefix: Option<&[u8]>,
) -> Result<VerifyReport> {
    let bytes = std::fs::read(path).map_err(|e| PdfError::io("read back", path, e))?;
    verify_bytes(bytes, password, expected, original_prefix).map(|(report, _)| report)
}

/// Verify saved bytes:
///
/// 1. if `original_prefix` is given, it must be an exact prefix of `bytes`;
/// 2. lopdf must re-open the bytes, reading the cross-reference data as written (a file it can
///    only read by repairing is rejected), with the expected page and per-page annotation
///    counts;
/// 3. with `original_prefix`, the newest cross-reference section must start in the appended
///    part and its `/Prev` must be the `startxref` value of the original;
/// 4. hayro-syntax must re-open the bytes with the same counts.
///
/// Returns the report and lopdf's parse, which the caller can keep as the new document state.
pub fn verify_bytes(
    bytes: Vec<u8>,
    password: Option<&str>,
    expected: &Expected,
    original_prefix: Option<&[u8]>,
) -> Result<(VerifyReport, PdfDocument)> {
    let mut appended = None;
    if let Some(prefix) = original_prefix {
        check_prefix(&bytes, prefix)?;
        // Nothing appended (the file is the original itself): there is no new section to
        // check. An original whose own `startxref` cannot be read has no section to chain to;
        // the repair check is then the one that decides.
        if bytes.len() > prefix.len() {
            appended = last_startxref(prefix).map(|prev_xref_start| Appended {
                original_len: prefix.len() as u64,
                prev_xref_start,
            });
        }
    }
    let (mut report, document) = verify_parsed(bytes, password, expected, appended)?;
    report.prefix_checked = original_prefix.is_some();
    Ok((report, document))
}

/// What is known about the revision an incremental save appended to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Appended {
    /// Length of the file before the save.
    pub original_len: u64,
    /// Where the previous revision's newest cross-reference section starts.
    pub prev_xref_start: u64,
}

/// The value after the last `startxref` keyword in `bytes`.
pub(crate) fn last_startxref(bytes: &[u8]) -> Option<u64> {
    const KEY: &[u8] = b"startxref";
    let at = bytes.windows(KEY.len()).rposition(|w| w == KEY)?;
    integer_after(&bytes[at + KEY.len()..]).and_then(|v| u64::try_from(v).ok())
}

/// The integer that follows optional white space at the start of `bytes`.
fn integer_after(bytes: &[u8]) -> Option<i64> {
    let start = bytes.iter().position(|b| !b.is_ascii_whitespace())?;
    let digits = bytes[start..]
        .iter()
        .take_while(|b| b.is_ascii_digit())
        .count();
    if digits == 0 {
        return None;
    }
    std::str::from_utf8(&bytes[start..start + digits])
        .ok()?
        .parse()
        .ok()
}

/// `/Prev` of the cross-reference section that starts at the beginning of `section`: the value
/// in the trailer dictionary of a classic table, or in the dictionary of a cross-reference
/// stream. Only the text up to the stream data or the `startxref` keyword is looked at.
pub(crate) fn section_prev(section: &[u8]) -> Option<i64> {
    let find = |needle: &[u8]| section.windows(needle.len()).position(|w| w == needle);
    let end = [find(b"stream"), find(b"startxref")]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(section.len());
    let head = &section[..end];
    const KEY: &[u8] = b"/Prev";
    let at = head.windows(KEY.len()).position(|w| w == KEY)?;
    integer_after(&head[at + KEY.len()..])
}

/// Steps 2 to 4 of [`verify_bytes`], with the facts about the previous revision supplied by
/// the caller (a save knows them exactly and has already checked the prefix).
pub(crate) fn verify_parsed(
    bytes: Vec<u8>,
    password: Option<&str>,
    expected: &Expected,
    appended: Option<Appended>,
) -> Result<(VerifyReport, PdfDocument)> {
    // Parser 1: lopdf.
    let doc = parse(&bytes, password).map_err(|e| VerifyError::Reparse {
        parser: "lopdf",
        message: e.to_string(),
    })?;
    // lopdf rebuilds or corrects damaged cross-reference data rather than failing. A file that
    // needed that is one whose xref section, trailer or startxref we wrote wrongly.
    if doc.xref_recovery.is_some() || doc.xref_start == 0 {
        return Err(VerifyError::XrefRepaired { parser: "lopdf" }.into());
    }
    if let Some(appended) = appended {
        let xref_start = doc.xref_start as u64;
        if xref_start < appended.original_len || last_startxref(&bytes) != Some(xref_start) {
            return Err(VerifyError::XrefNotAppended {
                xref_start,
                original_len: appended.original_len,
            }
            .into());
        }
        let found = section_prev(&bytes[doc.xref_start..]);
        if found != i64::try_from(appended.prev_xref_start).ok() {
            return Err(VerifyError::PrevMismatch {
                expected: appended.prev_xref_start,
                found,
            }
            .into());
        }
    }
    let bytes = Arc::new(bytes);
    let second_parser = check_with_hayro(&bytes, password, expected)?;
    // hayro's handle is gone, so the Arc is ours alone again.
    let bytes = Arc::try_unwrap(bytes).unwrap_or_else(|shared| (*shared).clone());

    let document = PdfDocument::from_parts(bytes, doc).map_err(|e| VerifyError::Reparse {
        parser: "lopdf",
        message: e.to_string(),
    })?;
    let counts = document.annotation_counts();
    compare("lopdf", expected, document.page_count(), &counts)?;

    let report = VerifyReport {
        file_len: document.bytes().len() as u64,
        page_count: document.page_count(),
        annotation_counts: counts,
        prefix_checked: false,
        second_parser,
    };
    Ok((report, document))
}

pub(crate) fn check_prefix(saved: &[u8], original: &[u8]) -> std::result::Result<(), VerifyError> {
    if saved.len() < original.len() {
        return Err(VerifyError::Truncated {
            saved: saved.len() as u64,
            original: original.len() as u64,
        });
    }
    if saved[..original.len()] != *original {
        let offset = saved
            .iter()
            .zip(original)
            .position(|(a, b)| a != b)
            .unwrap_or(0);
        return Err(VerifyError::PrefixMismatch {
            offset: offset as u64,
        });
    }
    Ok(())
}

fn compare(
    parser: &'static str,
    expected: &Expected,
    page_count: usize,
    counts: &[usize],
) -> std::result::Result<(), VerifyError> {
    if page_count != expected.page_count {
        return Err(VerifyError::PageCount {
            parser,
            expected: expected.page_count,
            found: page_count,
        });
    }
    for (page_index, (found, want)) in counts.iter().zip(&expected.annotation_counts).enumerate() {
        if found != want {
            return Err(VerifyError::AnnotationCount {
                parser,
                page_index,
                expected: *want,
                found: *found,
            });
        }
    }
    if counts.len() != expected.annotation_counts.len() {
        return Err(VerifyError::PageCount {
            parser,
            expected: expected.annotation_counts.len(),
            found: counts.len(),
        });
    }
    Ok(())
}

fn check_with_hayro(
    bytes: &Arc<Vec<u8>>,
    password: Option<&str>,
    expected: &Expected,
) -> std::result::Result<SecondParser, VerifyError> {
    const PARSER: &str = "hayro-syntax";
    // Like lopdf, try the empty user password first, then the supplied one.
    let mut opened = Pdf::new(bytes.clone());
    if let (Err(LoadPdfError::Decryption(_)), Some(password)) = (&opened, password) {
        opened = Pdf::new_with_password(bytes.clone(), password);
    }
    let pdf = match opened {
        Ok(pdf) => pdf,
        Err(LoadPdfError::Decryption(e)) if password.is_some_and(|p| !p.is_empty()) => {
            // hayro-syntax 0.9.0 has no owner-password path for security-handler revisions
            // below 5, so a file we legitimately opened with lopdf can be closed to it.
            return Ok(SecondParser::Skipped(format!(
                "hayro-syntax cannot decrypt this file: {e:?}"
            )));
        }
        Err(e) => {
            return Err(VerifyError::Reparse {
                parser: PARSER,
                message: format!("{e:?}"),
            })
        }
    };
    let pages = pdf.pages();
    let counts: Vec<usize> = pages
        .iter()
        .map(|page| {
            page.raw()
                .get::<Array<'_>>(ANNOTS)
                .map_or(0, |a| a.raw_iter().count())
        })
        .collect();
    compare(PARSER, expected, pages.len(), &counts)?;
    Ok(SecondParser::Passed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_check_reports_the_first_difference() {
        assert!(check_prefix(b"abcdef", b"abc").is_ok());
        assert!(matches!(
            check_prefix(b"abXdef", b"abc"),
            Err(VerifyError::PrefixMismatch { offset: 2 })
        ));
        assert!(matches!(
            check_prefix(b"ab", b"abc"),
            Err(VerifyError::Truncated {
                saved: 2,
                original: 3
            })
        ));
    }

    #[test]
    fn startxref_and_prev_are_read_from_the_text() {
        let table = b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 9 /Prev 1234 /Root 1 0 R >>\nstartxref\n5678\n%%EOF\n";
        assert_eq!(section_prev(table), Some(1234));
        assert_eq!(last_startxref(table), Some(5678));
        let stream =
            b"12 0 obj\n<< /Type /XRef /Prev 77 /W [1 2 1] >>\nstream\n/Prev 99 junk\nendstream";
        assert_eq!(
            section_prev(stream),
            Some(77),
            "stream data is not searched"
        );
        let first =
            b"xref\n0 1\n0000000000 65535 f \ntrailer\n<< /Size 9 >>\nstartxref\n9\n%%EOF\n";
        assert_eq!(section_prev(first), None);
        assert_eq!(
            last_startxref(b"startxref\n1\n%%EOF\nstartxref\r\n  22\r\n%%EOF"),
            Some(22)
        );
        assert_eq!(last_startxref(b"no keyword here"), None);
        assert_eq!(last_startxref(b"startxref\nabc"), None);
    }

    #[test]
    fn garbage_is_rejected_by_the_first_parser() {
        let expected = Expected {
            page_count: 1,
            annotation_counts: vec![0],
        };
        let err =
            verify_bytes(b"%PDF-1.7\nnot really".to_vec(), None, &expected, None).unwrap_err();
        assert!(
            matches!(
                err,
                PdfError::Verify(VerifyError::Reparse {
                    parser: "lopdf",
                    ..
                })
            ),
            "{err:?}"
        );
    }
}

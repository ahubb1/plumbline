//! Page editing, bookmarks, links and flatten for `plumb pages`, `plumb outline`, `plumb links`
//! and `plumb flatten`, and for the matching command-file steps.
//!
//! Everything here works on pl-pdf's document-structure layer. A page operation or a flatten
//! is a full rewrite (`PdfFile::save_restructured`: written to a temporary file, verified by
//! lopdf and hayro-syntax and here also by PDFium, then atomically moved into place); a
//! bookmark edit is an incremental append. A signed file is refused for a rewrite unless the
//! caller passes `--invalidate-signatures`.

use std::collections::BTreeSet;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{anyhow, bail, Context, Result};
use pl_doc::pl_geom::Rect;
use pl_doc::pl_pdf::{
    BlankPage, DestView, Destination, Expected, FlattenOptions, FlattenReport, FlattenSkip,
    FlattenTarget, LinkAction, LinkDest, Object, Outline, OutlineItem, OutlineTarget, PageOpReport,
    PdfDocument, PdfError, PdfFile, RemoteDest, ReplaceOptions, SaveOptions, SaveReport,
    SignaturePolicy, VerifyError,
};
use pl_doc::pl_render::{DocSource, RenderBackend};
use serde::{Deserialize, Serialize};

use crate::num;
use crate::ranges::{format_pages, parse_pages, parse_parts, parse_position, plural};

// ------------------------------------------------------------------------------------------
// Values a command file may give as a number or as text.

/// A page number, position or range list: `3`, `"3"`, `"1-3,5"`, `"last"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum PageText {
    Number(u64),
    Text(String),
}

impl PageText {
    fn text(&self) -> String {
        match self {
            PageText::Number(n) => n.to_string(),
            PageText::Text(t) => t.clone(),
        }
    }
}

impl From<&str> for PageText {
    fn from(s: &str) -> PageText {
        PageText::Text(s.to_string())
    }
}

/// A blank page's size: `[w, h]` in points, or a name (`letter`, `a4`...), or `WxH`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum SizeSpec {
    Points([f64; 2]),
    Name(String),
}

impl SizeSpec {
    fn points(&self) -> Result<[f64; 2]> {
        match self {
            SizeSpec::Points(p) => check_size(*p),
            SizeSpec::Name(n) => parse_size(n),
        }
    }
}

fn check_size(p: [f64; 2]) -> Result<[f64; 2]> {
    // ISO 32000 asks for at least 3 units and at most 14 400 a side.
    if !(p[0].is_finite() && p[1].is_finite()) || p[0] < 3.0 || p[1] < 3.0 {
        bail!("a page must be at least 3 points a side");
    }
    if p[0] > 14_400.0 || p[1] > 14_400.0 {
        bail!("a page can be at most 14400 points (200 inches) a side");
    }
    Ok(p)
}

/// `letter`, `legal`, `tabloid`, `a4`, `a3`, `a2`, `a1`, `a0`, `arch-c`, `arch-d`, `arch-e`,
/// or `WxH` in points (`612x792`), inches (`34x22in`) or millimetres (`420x297mm`).
pub fn parse_size(text: &str) -> Result<[f64; 2]> {
    let t = text.trim().to_ascii_lowercase();
    let named = match t.as_str() {
        "letter" => Some([612.0, 792.0]),
        "legal" => Some([612.0, 1008.0]),
        "tabloid" | "ledger" => Some([792.0, 1224.0]),
        "a4" => Some([595.28, 841.89]),
        "a3" => Some([841.89, 1190.55]),
        "a2" => Some([1190.55, 1683.78]),
        "a1" => Some([1683.78, 2383.94]),
        "a0" => Some([2383.94, 3370.39]),
        "arch-c" => Some([1296.0, 1728.0]),
        "arch-d" => Some([1728.0, 2592.0]),
        "arch-e" => Some([2592.0, 3456.0]),
        _ => None,
    };
    if let Some(p) = named {
        return Ok(p);
    }
    let (body, factor) = if let Some(b) = t.strip_suffix("in") {
        (b, 72.0)
    } else if let Some(b) = t.strip_suffix("mm") {
        (b, 72.0 / 25.4)
    } else if let Some(b) = t.strip_suffix("pt") {
        (b, 1.0)
    } else {
        (t.as_str(), 1.0)
    };
    let Some((w, h)) = body.split_once('x') else {
        bail!("{text:?} is not a page size: give a name such as letter or a4, or WxH");
    };
    let parse = |s: &str| -> Result<f64> {
        s.trim()
            .parse::<f64>()
            .map_err(|_| anyhow!("{text:?} is not a page size"))
    };
    check_size([parse(w)? * factor, parse(h)? * factor])
}

fn one() -> usize {
    1
}

// ------------------------------------------------------------------------------------------
// Page operations.

/// One page operation. Page numbers and ranges count from 1 and refer to the document as the
/// operations before it left it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub enum PageOp {
    /// New blank pages that become pages `at`, `at + 1`... (`at` up to page count + 1, or
    /// `end`). Without `size` they copy the page now at that position (the last page at the
    /// end).
    InsertBlank {
        at: PageText,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        size: Option<SizeSpec>,
        /// Clockwise, a multiple of 90; only with `size`.
        #[serde(default)]
        rotate: i32,
        #[serde(default = "one")]
        count: usize,
    },
    /// Pages of another PDF (all, or `pages` of it) that become pages `at`, `at + 1`...
    InsertFrom {
        at: PageText,
        file: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pages: Option<PageText>,
    },
    Delete {
        pages: PageText,
    },
    /// Moves `pages` (in their order) so that the first of them becomes page `to`.
    Move {
        pages: PageText,
        to: PageText,
    },
    /// Turns pages clockwise by a multiple of 90 degrees (negative: anticlockwise).
    Rotate {
        pages: PageText,
        degrees: i32,
    },
    /// Keeps only `pages`, in the order given.
    Extract {
        pages: PageText,
    },
    /// Replaces pages `at`, `at + 1`... with the pages of another PDF (all, or `pages` of it).
    Replace {
        at: PageText,
        file: PathBuf,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pages: Option<PageText>,
        /// Moves the old pages' markups onto the new ones.
        #[serde(default)]
        keep_markups: bool,
    },
    /// Appends every page of another PDF, with its bookmarks.
    Merge {
        file: PathBuf,
    },
}

/// The page-operation words of `plumb pages`.
pub const PAGE_VERBS: [&str; 9] = [
    "insert-blank",
    "insert-from",
    "delete",
    "move",
    "rotate",
    "extract",
    "replace",
    "merge",
    "split",
];

/// What a `split` asks for: the parts, as range lists of the document the other operations
/// leave, or every so many pages.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SplitSpec {
    /// `1-3,4-8`: one part per item.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<String>,
    /// One part per `every` pages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub every: Option<usize>,
}

impl SplitSpec {
    /// The parts as zero-based first and last pages (inclusive, ascending).
    pub fn ranges(&self, count: usize) -> Result<Vec<std::ops::Range<usize>>> {
        match (&self.parts, self.every) {
            (Some(_), Some(_)) => bail!("split takes parts or every, not both"),
            (None, None) => bail!("split needs its parts (1-3,4-8) or every=N"),
            (None, Some(0)) => bail!("split every=0 makes no parts"),
            (None, Some(n)) => Ok((0..count)
                .step_by(n)
                .map(|a| a..(a + n).min(count))
                .collect()),
            (Some(parts), None) => parse_parts(parts, count)?
                .into_iter()
                .map(|(a, b)| {
                    if a > b {
                        bail!("a split part must run forwards, not {}-{}", a + 1, b + 1);
                    }
                    Ok(a..b + 1)
                })
                .collect(),
        }
    }
}

/// Where part `n` (from 1) of a split goes: `{n}` in `pattern` is replaced by the number;
/// without one, `-n` is added before the extension (`out.pdf` gives `out-1.pdf`).
pub fn part_path(pattern: &Path, n: usize) -> PathBuf {
    let s = pattern.to_string_lossy();
    if s.contains("{n}") {
        return PathBuf::from(s.replace("{n}", &n.to_string()));
    }
    let stem = pattern
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let name = match pattern.extension() {
        Some(ext) => format!("{stem}-{n}.{}", ext.to_string_lossy()),
        None => format!("{stem}-{n}"),
    };
    pattern.with_file_name(name)
}

/// `key=value` options after an operation's own arguments.
fn take_options<'a>(
    tokens: &mut std::iter::Peekable<impl Iterator<Item = &'a String>>,
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    while let Some(t) = tokens.peek() {
        if PAGE_VERBS.contains(&t.as_str()) || OUTLINE_VERBS.contains(&t.as_str()) {
            break;
        }
        match t.split_once('=') {
            Some((k, v)) if !k.is_empty() && !k.contains(['/', '\\']) => {
                out.push((k.to_ascii_lowercase(), v.to_string()));
                tokens.next();
            }
            _ => {
                if t.as_str() == "keep-markups" {
                    out.push(("keep-markups".into(), "yes".into()));
                    tokens.next();
                } else {
                    break;
                }
            }
        }
    }
    out
}

fn yes_no(v: &str, key: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "yes" | "true" | "1" | "on" => Ok(true),
        "no" | "false" | "0" | "off" => Ok(false),
        _ => bail!("{key}={v}: give yes or no"),
    }
}

fn unknown_option(verb: &str, key: &str) -> anyhow::Error {
    anyhow!("{verb} has no option {key:?}")
}

/// Parses the words of `plumb pages`: operations and an optional final `split`.
pub fn parse_page_ops(words: &[String]) -> Result<(Vec<PageOp>, Option<SplitSpec>)> {
    let mut ops = Vec::new();
    let mut split = None;
    let mut it = words.iter().peekable();
    while let Some(verb) = it.next() {
        if split.is_some() {
            bail!("split must be the last operation");
        }
        let mut arg = |what: &str| -> Result<String> {
            it.next()
                .cloned()
                .ok_or_else(|| anyhow!("{verb} needs {what}"))
        };
        let op = match verb.as_str() {
            "insert-blank" => {
                let at = arg("the page number the new page gets (or end)")?;
                let mut size = None;
                let mut rotate = 0;
                let mut count = 1;
                for (k, v) in take_options(&mut it) {
                    match k.as_str() {
                        "size" => size = Some(SizeSpec::Name(v)),
                        "rotate" => {
                            rotate = v
                                .parse()
                                .map_err(|_| anyhow!("rotate={v} is not a number"))?
                        }
                        "count" => {
                            count = v
                                .parse()
                                .map_err(|_| anyhow!("count={v} is not a number"))?
                        }
                        _ => return Err(unknown_option(verb, &k)),
                    }
                }
                PageOp::InsertBlank {
                    at: at.as_str().into(),
                    size,
                    rotate,
                    count,
                }
            }
            "insert-from" | "replace" => {
                let at = arg("a page number")?;
                let file = PathBuf::from(arg("a PDF file")?);
                let mut pages = None;
                let mut keep_markups = false;
                for (k, v) in take_options(&mut it) {
                    match k.as_str() {
                        "pages" => pages = Some(v.as_str().into()),
                        "keep-markups" | "keep_markups" if verb == "replace" => {
                            keep_markups = yes_no(&v, &k)?
                        }
                        _ => return Err(unknown_option(verb, &k)),
                    }
                }
                if verb == "replace" {
                    PageOp::Replace {
                        at: at.as_str().into(),
                        file,
                        pages,
                        keep_markups,
                    }
                } else {
                    PageOp::InsertFrom {
                        at: at.as_str().into(),
                        file,
                        pages,
                    }
                }
            }
            "delete" => PageOp::Delete {
                pages: arg("the pages")?.as_str().into(),
            },
            "extract" => PageOp::Extract {
                pages: arg("the pages")?.as_str().into(),
            },
            "move" => {
                let pages = arg("the pages")?;
                let mut to = arg("the page number they move to")?;
                if to == "to" {
                    to = arg("the page number they move to")?;
                }
                PageOp::Move {
                    pages: pages.as_str().into(),
                    to: to.as_str().into(),
                }
            }
            "rotate" => {
                let pages = arg("the pages")?;
                let d = arg("the degrees (90, 180, 270 or -90)")?;
                let degrees = d
                    .parse()
                    .map_err(|_| anyhow!("rotate: {d:?} is not a number of degrees"))?;
                PageOp::Rotate {
                    pages: pages.as_str().into(),
                    degrees,
                }
            }
            "merge" => PageOp::Merge {
                file: PathBuf::from(arg("a PDF file")?),
            },
            "split" => {
                let mut spec = SplitSpec {
                    parts: None,
                    every: None,
                };
                let first = it.peek().map(|s| s.to_string());
                match first {
                    Some(t) if t.starts_with("every=") => {
                        let v = &t["every=".len()..];
                        spec.every = Some(
                            v.parse()
                                .map_err(|_| anyhow!("every={v} is not a number"))?,
                        );
                        it.next();
                    }
                    Some(t) if !t.contains('=') => {
                        spec.parts = Some(t);
                        it.next();
                    }
                    _ => bail!("split needs its parts (1-3,4-8) or every=N"),
                }
                split = Some(spec);
                continue;
            }
            other => bail!(
                "{other:?} is not a page operation (use {})",
                PAGE_VERBS.join(", ")
            ),
        };
        if let Some(extra) = it.peek() {
            if !PAGE_VERBS.contains(&extra.as_str()) {
                bail!("{verb}: unexpected {extra:?}");
            }
        }
        ops.push(op);
    }
    Ok((ops, split))
}

/// Opens a PDF with pl-pdf, trying the password if one is given.
pub fn open_pdf(path: &Path, password: Option<&str>) -> Result<PdfFile> {
    let mut file = PdfFile::open(path).with_context(|| format!("open {}", path.display()))?;
    file.set_password(password);
    file.document()
        .with_context(|| format!("read {}", path.display()))?;
    Ok(file)
}

/// One line about what an operation did.
fn describe_report(report: &PageOpReport) -> String {
    let mut parts = Vec::new();
    if !report.pages_affected.is_empty() {
        let mut p = report.pages_affected.clone();
        p.sort_unstable();
        parts.push(format!("pages {}", format_pages(&p)));
    }
    // `what` is "<noun> <verb>", e.g. "page removed": the noun takes the plural.
    let n = |parts: &mut Vec<String>, count: usize, what: &str| {
        if count > 0 {
            let (noun, verb) = what.rsplit_once(' ').unwrap_or((what, ""));
            parts.push(format!("{count} {noun}{} {verb}", plural(count)).trim_end().to_owned());
        }
    };
    n(&mut parts, report.pages_removed.len(), "page removed");
    n(
        &mut parts,
        report.outline_items_removed.len(),
        "bookmark removed",
    );
    n(
        &mut parts,
        report.outline_items_cleared.len(),
        "bookmark cleared",
    );
    n(&mut parts, report.outline_items_added, "bookmark added");
    n(&mut parts, report.links_removed.len(), "link removed");
    n(
        &mut parts,
        report.named_dests_removed.len(),
        "named destination removed",
    );
    if report.open_action_removed {
        parts.push("opening view removed".into());
    }
    n(
        &mut parts,
        report.fields_removed.len(),
        "form field removed",
    );
    n(&mut parts, report.layers_merged, "layer merged");
    n(
        &mut parts,
        report.bluebeam_columns_added.len(),
        "markup column added",
    );
    n(&mut parts, report.markups_kept, "markup kept");
    n(&mut parts, report.markups_dropped, "markup removed");
    for (from, to) in &report.fields_renamed {
        parts.push(format!("field {from:?} renamed {to:?}"));
    }
    parts.join(", ")
}

/// Applies `ops` to a copy of `doc`. `resolve` turns a file named by an operation into a path.
/// Returns the new document and one line per operation.
/// Refuses an operation an encrypted file's permissions forbid: page and bookmark changes
/// need the assemble (or modify) permission, flattening (`modify`) the modify permission.
pub fn check_permission(doc: &PdfDocument, modify: bool, what: &str) -> Result<()> {
    if let Some(enc) = doc.encryption() {
        let allowed = enc.can_modify || (!modify && enc.can_assemble);
        if !allowed {
            bail!("the author of this document does not allow it to {what}");
        }
    }
    Ok(())
}

pub fn apply_page_ops(
    doc: &PdfDocument,
    ops: &[PageOp],
    resolve: &dyn Fn(&Path) -> PathBuf,
    password: Option<&str>,
) -> Result<(PdfDocument, Vec<String>, Vec<String>)> {
    check_permission(doc, false, "change its pages")?;
    let mut r = doc.restructure()?;
    let mut lines = Vec::new();
    let mut notes = Vec::new();
    for (i, op) in ops.iter().enumerate() {
        let count = r.page_count();
        let (name, report) = (|| -> Result<(String, PageOpReport)> {
            Ok(match op {
                PageOp::InsertBlank {
                    at,
                    size,
                    rotate,
                    count: n,
                } => {
                    let at = parse_position(&at.text(), count + 1, "insert-blank at")?;
                    if *n == 0 {
                        bail!("insert-blank count=0 inserts nothing");
                    }
                    let blank = match size {
                        Some(s) => {
                            let [w, h] = s.points()?;
                            if rotate.rem_euclid(90) != 0 {
                                bail!("rotate={rotate} is not a multiple of 90");
                            }
                            BlankPage::Size {
                                media_box: Rect::new(0.0, 0.0, w, h),
                                rotate: rotate.rem_euclid(360),
                            }
                        }
                        None => {
                            if *rotate != 0 {
                                bail!("insert-blank rotate= needs a size=");
                            }
                            BlankPage::LikeNeighbour
                        }
                    };
                    let mut report = PageOpReport::default();
                    for k in 0..*n {
                        report.absorb(r.insert_blank(at + k, blank)?);
                    }
                    report.pages_affected = (at..at + n).collect();
                    (format!("insert-blank at {}", at + 1), report)
                }
                PageOp::InsertFrom { at, file, pages } => {
                    let at = parse_position(&at.text(), count + 1, "insert-from at")?;
                    let path = resolve(file);
                    let mut src = open_pdf(&path, password)?;
                    let src = src.document()?;
                    let pages = match pages {
                        Some(p) => parse_pages(&p.text(), src.page_count())?,
                        None => (0..src.page_count()).collect(),
                    };
                    let report = r.insert_from(at, src, &pages)?;
                    (
                        format!(
                            "insert-from {} at {}",
                            path.file_name().unwrap_or_default().to_string_lossy(),
                            at + 1
                        ),
                        report,
                    )
                }
                PageOp::Delete { pages } => {
                    let pages = parse_pages(&pages.text(), count)?;
                    (
                        format!("delete {}", format_pages(&sorted(&pages))),
                        r.delete(&pages)?,
                    )
                }
                PageOp::Move { pages, to } => {
                    let pages = parse_pages(&pages.text(), count)?;
                    let max = count - pages.len() + 1;
                    let to = parse_position(&to.text(), max, "move to")?;
                    let mut sorted_pages = pages.clone();
                    sorted_pages.sort_unstable();
                    let report = if sorted_pages == pages {
                        r.move_pages(&pages, to)?
                    } else {
                        // Pages written out of order move in that order.
                        let rest: Vec<usize> = (0..count).filter(|p| !pages.contains(p)).collect();
                        let mut order = rest[..to].to_vec();
                        order.extend(&pages);
                        order.extend(&rest[to..]);
                        r.reorder(&order)?
                    };
                    (
                        format!("move {} to {}", format_pages(&pages), to + 1),
                        report,
                    )
                }
                PageOp::Rotate { pages, degrees } => {
                    let pages = parse_pages(&pages.text(), count)?;
                    (
                        format!("rotate {} by {degrees}", format_pages(&sorted(&pages))),
                        r.rotate(&pages, *degrees)?,
                    )
                }
                PageOp::Extract { pages } => {
                    let pages = parse_pages(&pages.text(), count)?;
                    (
                        format!("extract {}", format_pages(&pages)),
                        r.extract(&pages)?,
                    )
                }
                PageOp::Replace {
                    at,
                    file,
                    pages,
                    keep_markups,
                } => {
                    let at = parse_position(&at.text(), count, "replace at")?;
                    let path = resolve(file);
                    let mut src = open_pdf(&path, password)?;
                    let src = src.document()?;
                    let pages = match pages {
                        Some(p) => parse_pages(&p.text(), src.page_count())?,
                        None => (0..src.page_count()).collect(),
                    };
                    let report = r.replace(
                        at,
                        src,
                        &pages,
                        ReplaceOptions {
                            keep_markups: *keep_markups,
                        },
                    )?;
                    (
                        format!(
                            "replace {} with {}",
                            format_pages(&(at..at + pages.len()).collect::<Vec<_>>()),
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ),
                        report,
                    )
                }
                PageOp::Merge { file } => {
                    let path = resolve(file);
                    let mut src = open_pdf(&path, password)?;
                    let src = src.document()?;
                    let report = r.insert_document(count, src, true)?;
                    (
                        format!(
                            "merge {}",
                            path.file_name().unwrap_or_default().to_string_lossy()
                        ),
                        report,
                    )
                }
            })
        })()
        .with_context(|| format!("operation {}", i + 1))?;
        let detail = describe_report(&report);
        lines.push(if detail.is_empty() {
            name
        } else {
            format!("{name}: {detail}")
        });
        notes.extend(report.notes);
    }
    Ok((r.finish()?, lines, notes))
}

fn sorted(pages: &[usize]) -> Vec<usize> {
    let mut p = pages.to_vec();
    p.sort_unstable();
    p
}

// ------------------------------------------------------------------------------------------
// Saving.

/// Whether two paths name the same file (the second may not exist yet).
pub fn same_file(a: &Path, b: &Path) -> bool {
    match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
}

/// PDFium as a further check on the temporary file of a save: it must open it and count the
/// pages and annotations the save meant to write. The file is read into memory, so nothing
/// keeps it open for the replace that follows.
fn pdfium_check(
    backend: &dyn RenderBackend,
    tmp: &Path,
    password: Option<&str>,
    expected: &Expected,
) -> std::result::Result<(), VerifyError> {
    let reparse = |message: String| VerifyError::Reparse {
        parser: "PDFium",
        message,
    };
    let bytes = std::fs::read(tmp).map_err(|e| reparse(e.to_string()))?;
    let info = backend
        .open(DocSource::Bytes(bytes), password)
        .map_err(|e| reparse(e.to_string()))?;
    let result = (|| {
        let found = info.page_count as usize;
        if found != expected.page_count {
            return Err(VerifyError::PageCount {
                parser: "PDFium",
                expected: expected.page_count,
                found,
            });
        }
        // PDFium reads every page's annotations: a page that fails to load shows up here.
        for page in 0..found {
            backend
                .annotations(info.doc, page as u32)
                .map_err(|e| reparse(format!("page {}: {e}", page + 1)))?;
        }
        Ok(())
    })();
    let _ = backend.close(info.doc);
    result
}

/// What a rewrite does about signatures.
pub fn signature_policy(invalidate: bool) -> SignaturePolicy {
    if invalidate {
        SignaturePolicy::Invalidate
    } else {
        SignaturePolicy::Refuse
    }
}

/// Explains a refused rewrite of a signed file.
fn explain(error: PdfError) -> anyhow::Error {
    match error {
        PdfError::SignaturesWouldBreak { fields } => anyhow!(
            "the file is signed ({}); rewriting it invalidates the signature{}. Pass \
             --invalidate-signatures (\"invalidate_signatures\": true in a command file) to go \
             ahead",
            fields.join("; "),
            plural(fields.len())
        ),
        other => other.into(),
    }
}

/// Saves a restructured document over `file` or to `out`: the full rewrite, verified by
/// pl-pdf and PDFium, then moved into place.
pub fn save_rewrite(
    file: &mut PdfFile,
    doc: PdfDocument,
    out: &Path,
    policy: SignaturePolicy,
    backend: &dyn RenderBackend,
    password: Option<&str>,
) -> Result<SaveReport> {
    let mut check =
        |tmp: &Path, expected: &Expected| pdfium_check(backend, tmp, password, expected);
    let options = SaveOptions::default();
    let result = if same_file(file.path(), out) {
        file.save_restructured_checked(doc, &options, policy, &mut check)
    } else {
        file.save_restructured_as_checked(out, doc, &options, policy, &mut check)
    };
    result
        .map_err(explain)
        .with_context(|| format!("save {}", out.display()))
}

/// Saves a new document (a part of a split) to `out`.
pub fn save_new(
    doc: PdfDocument,
    out: &Path,
    policy: SignaturePolicy,
    backend: &dyn RenderBackend,
) -> Result<SaveReport> {
    let mut check = |tmp: &Path, expected: &Expected| pdfium_check(backend, tmp, None, expected);
    doc.save_new_checked(out, &SaveOptions::default(), policy, &mut check)
        .map_err(explain)
        .with_context(|| format!("save {}", out.display()))
}

/// Warning lines for the signatures a rewrite invalidated.
pub fn signature_warnings(doc: &PdfDocument) -> Vec<String> {
    doc.signatures()
        .descriptions()
        .into_iter()
        .map(|d| format!("warning: signature invalidated: {d}"))
        .collect()
}

// ------------------------------------------------------------------------------------------
// Page listing.

/// A page's fingerprint: FNV-1a of its decoded content streams, in order. It follows a page
/// through any reordering and changes when its content does.
pub fn content_fingerprint(doc: &PdfDocument, page: usize) -> Result<u64> {
    const LIMIT: usize = 512 * 1024 * 1024;
    let id = doc.page_id(page)?;
    let dict = match doc.object(id) {
        Some(Object::Dictionary(d)) => d,
        _ => bail!("page {} is not a dictionary", page + 1),
    };
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            hash ^= u64::from(*b);
            hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
        }
    };
    let contents = match dict.get(b"Contents") {
        Ok(o) => doc.resolve(o).cloned(),
        Err(_) => None,
    };
    let streams: Vec<Object> = match contents {
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(|o| doc.resolve(o).cloned())
            .collect(),
        Some(o) => vec![o],
        None => Vec::new(),
    };
    for s in streams {
        if let Object::Stream(s) = s {
            match s.decompressed_content_with_limit(LIMIT) {
                Ok(bytes) => feed(&bytes),
                Err(_) => feed(&s.content),
            }
            // A separator, so two streams never hash like their concatenation.
            feed(&[0]);
        }
    }
    Ok(hash)
}

/// `plumb pages <pdf>` with no operations: one line per page.
pub fn list_pages(out: &mut impl Write, doc: &PdfDocument, path: &Path) -> Result<()> {
    let labels = doc.page_labels();
    writeln!(out, "file: {}", path.display())?;
    writeln!(out, "pages: {}", doc.page_count())?;
    for i in 0..doc.page_count() {
        let info = doc.page(i)?;
        let size = info.geometry.display_size_points();
        let label = labels
            .as_ref()
            .and_then(|l| l.get(i))
            .map(|l| format!("label {:?}, ", l.text()))
            .unwrap_or_default();
        writeln!(
            out,
            "page {}: {label}{} x {} pt, rotate {}, user unit {}, annotations {}, content {:016x}",
            i + 1,
            num(size.width, 2),
            num(size.height, 2),
            info.rotate.degrees(),
            num(info.user_unit, 4),
            info.annotation_count,
            content_fingerprint(doc, i)?
        )?;
    }
    let sig = doc.signatures();
    if sig.is_signed() {
        for d in sig.descriptions() {
            writeln!(out, "signed: {d}")?;
        }
    }
    Ok(())
}

// ------------------------------------------------------------------------------------------
// Bookmarks.

/// Outline item paths as written: `2.1` is the first child of the second top-level item.
pub fn parse_path(text: &str) -> Result<Vec<usize>> {
    let t = text.trim();
    if t.is_empty() {
        bail!("an empty bookmark path");
    }
    t.split('.')
        .map(|p| match p.trim().parse::<usize>() {
            Ok(0) | Err(_) => Err(anyhow!(
                "{t:?} is not a bookmark path (numbers from 1 joined by dots, such as 2.1)"
            )),
            Ok(n) => Ok(n - 1),
        })
        .collect()
}

fn format_path(path: &[usize]) -> String {
    path.iter()
        .map(|i| (i + 1).to_string())
        .collect::<Vec<_>>()
        .join(".")
}

/// How a destination shows its page, in words.
fn describe_view(view: &DestView) -> String {
    let p = |v: Option<f64>| v.map_or_else(|| "-".to_string(), |v| num(v, 2));
    match *view {
        DestView::Xyz { left, top, zoom } => {
            let mut s = format!("at {},{}", p(left), p(top));
            if let Some(z) = zoom.filter(|z| *z > 0.0) {
                s.push_str(&format!(" zoom {}", num(z, 4)));
            }
            s
        }
        DestView::Fit => "fit".into(),
        DestView::FitB => "fit contents".into(),
        DestView::FitH { top } | DestView::FitBH { top } => format!("fit width at top {}", p(top)),
        DestView::FitV { left } | DestView::FitBV { left } => {
            format!("fit height at left {}", p(left))
        }
        DestView::FitR {
            left,
            bottom,
            right,
            top,
        } => format!(
            "fit [{} {} {} {}]",
            num(left, 2),
            num(bottom, 2),
            num(right, 2),
            num(top, 2)
        ),
    }
}

fn describe_dest(dest: &Destination) -> String {
    format!("page {} {}", dest.page_index + 1, describe_view(&dest.view))
}

fn action_name(action: &Object) -> String {
    match action {
        Object::Dictionary(d) => match d.get(b"S") {
            Ok(Object::Name(n)) => String::from_utf8_lossy(n).into_owned(),
            _ => "action".into(),
        },
        _ => "action".into(),
    }
}

/// One line per bookmark in pre-order, indented by level, then a count.
pub fn list_outline(out: &mut impl Write, doc: &PdfDocument) -> Result<usize> {
    let outline = doc.outline();
    fn walk(
        out: &mut dyn Write,
        doc: &PdfDocument,
        items: &[OutlineItem],
        path: &mut Vec<usize>,
    ) -> Result<()> {
        for (i, item) in items.iter().enumerate() {
            path.push(i);
            let target = match &item.target {
                OutlineTarget::None => "no destination".to_string(),
                OutlineTarget::Page { .. } => match doc.outline_target_dest(&item.target) {
                    Some(d) => describe_dest(&d),
                    None => "broken".into(),
                },
                OutlineTarget::Named { name, .. } => {
                    let n = String::from_utf8_lossy(name);
                    match doc.outline_target_dest(&item.target) {
                        Some(d) => format!("named {n:?} {}", describe_dest(&d)),
                        None => format!("named {n:?} broken"),
                    }
                }
                OutlineTarget::Action(a) => format!("action {}", action_name(a)),
                OutlineTarget::Unresolved { .. } => "broken".into(),
            };
            let mut flags = Vec::new();
            if !item.children.is_empty() && !item.open {
                flags.push("closed");
            }
            if item.bold {
                flags.push("bold");
            }
            if item.italic {
                flags.push("italic");
            }
            let flags = if flags.is_empty() {
                String::new()
            } else {
                format!(" ({})", flags.join(", "))
            };
            writeln!(
                out,
                "{}{} {:?} -> {target}{flags}",
                "  ".repeat(path.len() - 1),
                format_path(path),
                item.title
            )?;
            walk(out, doc, &item.children, path)?;
            path.pop();
        }
        Ok(())
    }
    walk(out, doc, &outline.items, &mut Vec::new())?;
    let n = outline.len();
    writeln!(out, "{n} bookmark{}", plural(n))?;
    Ok(n)
}

/// One bookmark edit. Paths are dotted numbers from 1 (`2.1`); pages count from 1.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "do", rename_all = "snake_case", deny_unknown_fields)]
pub enum OutlineEdit {
    /// A new bookmark at `path` (`3` is the third top-level item; `2.4` the fourth child of
    /// item 2; the last number may be one past the end), leading to `page` (none: a heading),
    /// with the view at `at` (`[x, y]`, page space) or the whole page.
    Add {
        path: String,
        title: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        page: Option<u32>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<[f64; 2]>,
    },
    Rename {
        path: String,
        title: String,
    },
    /// Removes a bookmark and its children.
    Delete {
        path: String,
    },
    /// Moves a bookmark (with its children) to `to`, read in the tree after it is taken out.
    Move {
        path: String,
        to: String,
    },
    /// Points a bookmark at a page.
    Dest {
        path: String,
        page: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        at: Option<[f64; 2]>,
    },
}

/// The bookmark-edit words of `plumb outline`.
pub const OUTLINE_VERBS: [&str; 5] = ["add", "rename", "delete", "move", "dest"];

fn parse_at(v: &str) -> Result<[f64; 2]> {
    let (x, y) = v
        .split_once(',')
        .ok_or_else(|| anyhow!("at={v}: give x,y"))?;
    let f = |s: &str| {
        s.trim()
            .parse::<f64>()
            .map_err(|_| anyhow!("at={v}: {s:?} is not a number"))
    };
    Ok([f(x)?, f(y)?])
}

fn parse_page_word(v: &str) -> Result<Option<u32>> {
    if v.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    match v.parse::<u32>() {
        Ok(0) | Err(_) => bail!("{v:?} is not a page number (or none)"),
        Ok(n) => Ok(Some(n)),
    }
}

/// Parses the words of `plumb outline <pdf> <edit...>`.
pub fn parse_outline_edits(words: &[String]) -> Result<Vec<OutlineEdit>> {
    let mut edits = Vec::new();
    let mut it = words.iter().peekable();
    while let Some(verb) = it.next() {
        let mut arg = |what: &str| -> Result<String> {
            it.next()
                .cloned()
                .ok_or_else(|| anyhow!("{verb} needs {what}"))
        };
        let edit = match verb.as_str() {
            "add" => {
                let path = arg("a bookmark path")?;
                let title = arg("a title")?;
                let page = parse_page_word(&arg("a page number (or none)")?)?;
                let mut at = None;
                for (k, v) in take_options(&mut it) {
                    match k.as_str() {
                        "at" => at = Some(parse_at(&v)?),
                        _ => return Err(unknown_option(verb, &k)),
                    }
                }
                OutlineEdit::Add {
                    path,
                    title,
                    page,
                    at,
                }
            }
            "rename" => OutlineEdit::Rename {
                path: arg("a bookmark path")?,
                title: arg("a title")?,
            },
            "delete" => OutlineEdit::Delete {
                path: arg("a bookmark path")?,
            },
            "move" => {
                let path = arg("a bookmark path")?;
                let mut to = arg("the path it moves to")?;
                if to == "to" {
                    to = arg("the path it moves to")?;
                }
                OutlineEdit::Move { path, to }
            }
            "dest" => {
                let path = arg("a bookmark path")?;
                let page = parse_page_word(&arg("a page number")?)?
                    .ok_or_else(|| anyhow!("dest needs a page number"))?;
                let mut at = None;
                for (k, v) in take_options(&mut it) {
                    match k.as_str() {
                        "at" => at = Some(parse_at(&v)?),
                        _ => return Err(unknown_option(verb, &k)),
                    }
                }
                OutlineEdit::Dest { path, page, at }
            }
            other => bail!(
                "{other:?} is not a bookmark edit (use {})",
                OUTLINE_VERBS.join(", ")
            ),
        };
        if let Some(extra) = it.peek() {
            if !OUTLINE_VERBS.contains(&extra.as_str()) {
                bail!("{verb}: unexpected {extra:?}");
            }
        }
        edits.push(edit);
    }
    Ok(edits)
}

fn destination(doc: &PdfDocument, page: u32, at: Option<[f64; 2]>) -> Result<Destination> {
    let count = doc.page_count();
    if page == 0 || page as usize > count {
        bail!(
            "page {page} is out of range: the document has {count} page{}",
            plural(count)
        );
    }
    let view = match at {
        Some([x, y]) => DestView::at(x, y),
        None => DestView::Fit,
    };
    Ok(Destination::new(page as usize - 1, view))
}

/// Applies bookmark edits to the document's outline; returns it and one line per edit.
pub fn apply_outline_edits(
    doc: &PdfDocument,
    edits: &[OutlineEdit],
) -> Result<(Outline, Vec<String>)> {
    check_permission(doc, false, "change its bookmarks")?;
    let mut outline = doc.outline();
    let mut lines = Vec::new();
    for (i, edit) in edits.iter().enumerate() {
        let line = (|| -> Result<String> {
            Ok(match edit {
                OutlineEdit::Add {
                    path,
                    title,
                    page,
                    at,
                } => {
                    let p = parse_path(path)?;
                    let (last, parent) = p.split_last().expect("not empty");
                    let target = match page {
                        Some(page) => OutlineTarget::Page {
                            dest: destination(doc, *page, *at)?,
                            as_action: false,
                        },
                        None => OutlineTarget::None,
                    };
                    outline.insert(parent, *last, OutlineItem::new(title.clone(), target))?;
                    format!("add {} {title:?}", format_path(&p))
                }
                OutlineEdit::Rename { path, title } => {
                    let p = parse_path(path)?;
                    outline.rename(&p, title.clone())?;
                    format!("rename {} {title:?}", format_path(&p))
                }
                OutlineEdit::Delete { path } => {
                    let p = parse_path(path)?;
                    let removed = outline.remove(&p)?;
                    let n = 1 + removed.descendant_count();
                    format!(
                        "delete {}: {n} bookmark{} removed",
                        format_path(&p),
                        plural(n)
                    )
                }
                OutlineEdit::Move { path, to } => {
                    let from = parse_path(path)?;
                    let to = parse_path(to)?;
                    let (last, parent) = to.split_last().expect("not empty");
                    outline.move_item(&from, parent, *last)?;
                    format!("move {} to {}", format_path(&from), format_path(&to))
                }
                OutlineEdit::Dest { path, page, at } => {
                    let p = parse_path(path)?;
                    let dest = destination(doc, *page, *at)?;
                    outline.set_destination(&p, dest)?;
                    format!("dest {} {}", format_path(&p), describe_dest(&dest))
                }
            })
        })()
        .with_context(|| format!("edit {}", i + 1))?;
        lines.push(line);
    }
    Ok((outline, lines))
}

/// Saves an outline as an incremental update over `file` or to `out` (a bookmark edit leaves
/// signatures intact).
pub fn save_outline(
    file: &mut PdfFile,
    outline: &Outline,
    out: &Path,
    backend: &dyn RenderBackend,
    password: Option<&str>,
) -> Result<SaveReport> {
    let mut edits = file.edit()?;
    file.document()?.outline_edits(&mut edits, outline)?;
    let mut check =
        |tmp: &Path, expected: &Expected| pdfium_check(backend, tmp, password, expected);
    let options = SaveOptions::default();
    let result = if same_file(file.path(), out) {
        file.save_incremental_checked(&edits, &options, &mut check)
    } else {
        file.save_incremental_as_checked(out, &edits, &options, &mut check)
    };
    result.with_context(|| format!("save {}", out.display()))
}

// ------------------------------------------------------------------------------------------
// Links.

/// One link as `plumb links` lists it.
#[derive(Debug, Clone, PartialEq)]
pub struct LinkRow {
    /// Zero-based.
    pub page: usize,
    pub rect: Rect,
    /// `goto`, `goto-named`, `uri`, `gotor`, `launch`, `named`, `other` or `none`.
    pub kind: &'static str,
    /// The name, address, file or action name; empty for an explicit goto.
    pub value: String,
    /// Where it leads in this document.
    pub target: Option<Destination>,
    /// For `gotor`: the page (from 1) or name in the other file.
    pub remote: Option<String>,
    /// A local goto whose destination is not in the document.
    pub broken: bool,
}

pub fn link_rows(doc: &PdfDocument) -> Result<Vec<LinkRow>> {
    let mut rows = Vec::new();
    for page in 0..doc.page_count() {
        for link in doc.links(page)? {
            let resolved = doc.resolve_link_action(&link.action, page);
            let target = resolved.map(|r| Destination::new(r.page_index, r.view));
            let mut remote = None;
            let (kind, value) = match &link.action {
                LinkAction::GoTo(LinkDest::Named(n)) => {
                    ("goto-named", String::from_utf8_lossy(n).into_owned())
                }
                LinkAction::GoTo(_) => ("goto", String::new()),
                LinkAction::GoToR { file, dest, .. } => {
                    remote = dest.as_ref().map(|d| match d {
                        RemoteDest::Page { page_index, view } => {
                            format!("page {} {}", page_index + 1, describe_view(view))
                        }
                        RemoteDest::Named(n) => format!("named {:?}", String::from_utf8_lossy(n)),
                    });
                    ("gotor", file.clone())
                }
                LinkAction::Uri(u) => ("uri", u.clone()),
                LinkAction::Launch { file, .. } => ("launch", file.clone()),
                LinkAction::Named(n) => ("named", n.clone()),
                LinkAction::Other(s) => ("other", s.clone()),
                LinkAction::None => ("none", String::new()),
            };
            let broken = matches!(link.action, LinkAction::GoTo(_)) && target.is_none();
            rows.push(LinkRow {
                page,
                rect: link.rect,
                kind,
                value,
                target,
                remote,
                broken,
            });
        }
    }
    Ok(rows)
}

/// `plumb links`: one line per link with where it leads, then a count.
pub fn list_links(out: &mut impl Write, doc: &PdfDocument) -> Result<usize> {
    let rows = link_rows(doc)?;
    let mut pages = BTreeSet::new();
    for row in &rows {
        pages.insert(row.page);
        let r = row.rect;
        let value = if row.value.is_empty() {
            String::new()
        } else {
            format!(" {:?}", row.value)
        };
        let target = match (&row.target, &row.remote) {
            (Some(d), _) => format!(" -> {}", describe_dest(d)),
            (None, Some(remote)) => format!(" -> {remote} of that file"),
            (None, None) if row.broken => " -> broken".into(),
            (None, None) => String::new(),
        };
        writeln!(
            out,
            "page {}: [{} {} {} {}] {}{value}{target}",
            row.page + 1,
            num(r.x0, 2),
            num(r.y0, 2),
            num(r.x1, 2),
            num(r.y1, 2),
            row.kind,
        )?;
    }
    writeln!(
        out,
        "{} link{} on {} page{}",
        rows.len(),
        plural(rows.len()),
        pages.len(),
        plural(pages.len())
    )?;
    Ok(rows.len())
}

// ------------------------------------------------------------------------------------------
// Flatten.

/// What a flatten should burn in.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FlattenArgs {
    /// Range list; `None` for every page.
    pub pages: Option<String>,
    /// What prints rather than what shows on screen.
    pub print: bool,
    /// Form fields too.
    pub forms: bool,
}

pub fn flatten_options(doc: &PdfDocument, args: &FlattenArgs) -> Result<FlattenOptions> {
    let pages = match &args.pages {
        Some(p) => Some(parse_pages(p, doc.page_count())?.into_iter().collect()),
        None => None,
    };
    Ok(FlattenOptions {
        pages,
        ids: None,
        target: if args.print {
            FlattenTarget::Print
        } else {
            FlattenTarget::Screen
        },
        include_form_fields: args.forms,
    })
}

/// One line about a flatten.
pub fn describe_flatten(report: &FlattenReport) -> String {
    let n = report.flattened.len();
    let pages: BTreeSet<usize> = report.flattened.iter().map(|f| f.page_index).collect();
    let pages: Vec<usize> = pages.into_iter().collect();
    let mut s = format!("{n} annotation{} flattened", plural(n));
    if !pages.is_empty() {
        s.push_str(&format!(" on pages {}", format_pages(&pages)));
    }
    let mut reasons: Vec<(&str, usize)> = Vec::new();
    for (_, _, why) in &report.skipped {
        let w = match why {
            FlattenSkip::NotShown => "not shown",
            FlattenSkip::NoAppearance => "no appearance",
            FlattenSkip::Degenerate => "empty box",
            FlattenSkip::Link => "link",
            FlattenSkip::FormField => "form field",
            FlattenSkip::NoRotateOnRotatedPage => "no-rotate on a rotated page",
        };
        match reasons.iter_mut().find(|(r, _)| *r == w) {
            Some((_, c)) => *c += 1,
            None => reasons.push((w, 1)),
        }
    }
    if !reasons.is_empty() {
        let list: Vec<String> = reasons.iter().map(|(w, c)| format!("{c} {w}")).collect();
        s.push_str(&format!(
            ", {} left ({})",
            report.skipped.len(),
            list.join(", ")
        ));
    }
    if report.popups_removed > 0 {
        s.push_str(&format!(
            ", {} popup{} removed",
            report.popups_removed,
            plural(report.popups_removed)
        ));
    }
    if !report.fields_removed.is_empty() {
        let f = report.fields_removed.len();
        s.push_str(&format!(", {f} form field{} removed", plural(f)));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(s: &str) -> Vec<String> {
        s.split_whitespace().map(String::from).collect()
    }

    #[test]
    fn page_and_bookmark_changes_respect_the_files_permissions() {
        for fixture in [pl_testkit::encrypted_rc4(), pl_testkit::encrypted_aes256()] {
            let doc = PdfDocument::from_bytes(fixture.bytes.clone(), None).unwrap();
            let (ops, _) = parse_page_ops(&words("rotate 1 90")).unwrap();
            let err =
                apply_page_ops(&doc, &ops, &|p: &Path| p.to_path_buf(), None).expect_err("refused");
            assert!(err.to_string().contains("does not allow"), "{err}");
            assert!(apply_outline_edits(&doc, &[]).is_err());
            assert!(check_permission(&doc, true, "be flattened").is_err());
        }
        let (plain, _) = pl_testkit::structure_main();
        let doc = PdfDocument::from_bytes(plain.bytes, None).unwrap();
        assert!(check_permission(&doc, true, "be flattened").is_ok());
    }

    #[test]
    fn page_operations_parse_from_words() {
        let (ops, split) = parse_page_ops(&words(
            "delete 2,4-5 rotate 1-3 -90 move 6 to 1 insert-blank end size=a4 rotate=90 count=2 \
             insert-from 3 b.pdf pages=1-2 replace 2 c.pdf keep-markups merge d.pdf extract 1-3 \
             split 1,2-3",
        ))
        .unwrap();
        assert_eq!(ops.len(), 8);
        assert_eq!(
            ops[0],
            PageOp::Delete {
                pages: "2,4-5".into()
            }
        );
        assert_eq!(
            ops[1],
            PageOp::Rotate {
                pages: "1-3".into(),
                degrees: -90
            }
        );
        assert_eq!(
            ops[2],
            PageOp::Move {
                pages: "6".into(),
                to: "1".into()
            }
        );
        assert_eq!(
            ops[3],
            PageOp::InsertBlank {
                at: "end".into(),
                size: Some(SizeSpec::Name("a4".into())),
                rotate: 90,
                count: 2
            }
        );
        assert_eq!(
            ops[4],
            PageOp::InsertFrom {
                at: "3".into(),
                file: "b.pdf".into(),
                pages: Some("1-2".into())
            }
        );
        assert!(matches!(
            &ops[5],
            PageOp::Replace {
                keep_markups: true,
                pages: None,
                ..
            }
        ));
        assert!(matches!(&ops[6], PageOp::Merge { file } if file == Path::new("d.pdf")));
        assert_eq!(split.unwrap().parts.as_deref(), Some("1,2-3"));

        assert!(parse_page_ops(&words("split 1-2 delete 1")).is_err());
        assert!(parse_page_ops(&words("explode 1")).is_err());
        assert!(parse_page_ops(&words("delete")).is_err());
        assert!(parse_page_ops(&words("delete 1 2")).is_err());
        assert!(parse_page_ops(&words("rotate 1 ninety")).is_err());
        assert!(parse_page_ops(&words("insert-blank 1 colour=red")).is_err());
        let (_, split) = parse_page_ops(&words("split every=3")).unwrap();
        assert_eq!(split.unwrap().every, Some(3));
    }

    #[test]
    fn page_operations_read_from_json() {
        let ops: Vec<PageOp> = serde_json::from_str(
            r#"[{"do": "delete", "pages": 2},
                {"do": "insert_blank", "at": "end", "size": [612, 792]},
                {"do": "replace", "at": 1, "file": "b.pdf", "keep_markups": true}]"#,
        )
        .unwrap();
        assert_eq!(
            ops[0],
            PageOp::Delete {
                pages: PageText::Number(2)
            }
        );
        assert!(matches!(
            &ops[1],
            PageOp::InsertBlank { size: Some(SizeSpec::Points(p)), count: 1, .. } if *p == [612.0, 792.0]
        ));
        assert!(serde_json::from_str::<PageOp>(r#"{"do": "delete", "page": 2}"#).is_err());
    }

    #[test]
    fn sizes_splits_and_part_names() {
        assert_eq!(parse_size("letter").unwrap(), [612.0, 792.0]);
        assert_eq!(parse_size("34x22in").unwrap(), [2448.0, 1584.0]);
        assert_eq!(parse_size("612x792").unwrap(), [612.0, 792.0]);
        let mm = parse_size("210x297mm").unwrap();
        assert!((mm[0] - 595.28).abs() < 0.01 && (mm[1] - 841.89).abs() < 0.01);
        assert!(parse_size("1x1").is_err());
        assert!(parse_size("huge").is_err());
        assert!(parse_size("300x300in").is_err());

        let every = SplitSpec {
            parts: None,
            every: Some(3),
        };
        assert_eq!(every.ranges(8).unwrap(), vec![0..3, 3..6, 6..8]);
        let parts = SplitSpec {
            parts: Some("1-3,4-".into()),
            every: None,
        };
        assert_eq!(parts.ranges(8).unwrap(), vec![0..3, 3..8]);
        let backwards = SplitSpec {
            parts: Some("3-1".into()),
            every: None,
        };
        assert!(backwards.ranges(8).is_err());

        assert_eq!(
            part_path(Path::new("out/a.pdf"), 2),
            Path::new("out/a-2.pdf")
        );
        assert_eq!(
            part_path(Path::new("out/part{n}.pdf"), 3),
            Path::new("out/part3.pdf")
        );
    }

    #[test]
    fn bookmark_edits_parse_from_words_and_json() {
        let edits = parse_outline_edits(&words(
            "add 1 Cover 1 at=72,700 rename 2.1 Plans delete 3 move 2 to 1.1 dest 1 4 add 5 Notes none",
        ))
        .unwrap();
        assert_eq!(edits.len(), 6);
        assert_eq!(
            edits[0],
            OutlineEdit::Add {
                path: "1".into(),
                title: "Cover".into(),
                page: Some(1),
                at: Some([72.0, 700.0])
            }
        );
        assert!(matches!(&edits[3], OutlineEdit::Move { to, .. } if to == "1.1"));
        assert!(matches!(&edits[5], OutlineEdit::Add { page: None, .. }));
        assert!(parse_outline_edits(&words("add 1 Cover")).is_err());
        assert!(parse_outline_edits(&words("dest 1 none")).is_err());
        assert!(parse_outline_edits(&words("frob 1")).is_err());
        assert_eq!(parse_path("2.1").unwrap(), vec![1, 0]);
        assert!(parse_path("0").is_err());
        assert!(parse_path("1..2").is_err());
        let e: OutlineEdit =
            serde_json::from_str(r#"{"do": "dest", "path": "1", "page": 3}"#).unwrap();
        assert_eq!(
            e,
            OutlineEdit::Dest {
                path: "1".into(),
                page: 3,
                at: None
            }
        );
    }

    #[test]
    fn views_read_as_words() {
        assert_eq!(describe_view(&DestView::at(72.0, 700.5)), "at 72,700.5");
        assert_eq!(describe_view(&DestView::Fit), "fit");
        assert_eq!(
            describe_view(&DestView::FitH { top: Some(500.0) }),
            "fit width at top 500"
        );
        assert_eq!(
            describe_view(&DestView::Xyz {
                left: None,
                top: Some(10.0),
                zoom: Some(2.0)
            }),
            "at -,10 zoom 2"
        );
    }
}

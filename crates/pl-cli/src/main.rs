//! `plumb`: the Plumbline command line.
//!
//! ```text
//! plumb info <pdf>                                  pages, sizes, rotation, counts
//! plumb render <pdf> --page N --scale S -o out.png  one page as a PNG
//! plumb search <pdf> <text>                         every occurrence, with its page and box
//! plumb text <pdf> --page N                         the text of one page
//! plumb annots <pdf>                                every annotation: kind, page, box, author
//! plumb measure <pdf>                               page scales, viewports and measurements
//! plumb run <commands.json>                         run a JSON command file (the test driver)
//! plumb export <pdf> --csv out.csv | --xlsx out.xlsx | --xfdf out.xfdf
//!                                                   the markups list, or the annotations
//! plumb pages <pdf>                                 every page: label, size, rotation, fingerprint
//! plumb pages <pdf> <op...> -o out.pdf              insert, delete, move, rotate, extract,
//!                                                   replace, merge, split
//! plumb outline <pdf> [<edit...> -o out.pdf]        the bookmark tree, or edit it
//! plumb links <pdf>                                 every link and where it leads
//! plumb flatten <pdf> -o out.pdf [--pages 1-3]      burn the markups into the pages
//! plumb set create|add|list|search <set.plset> ...  a group of PDF files, searched together
//! ```
//!
//! `plumb pages` takes its operations in order; each one's page numbers refer to the document
//! as the operations before it left it. Pages are written `1-3,5`, `4-` (to the end), `last`
//! or `all`:
//!
//! ```text
//! insert-blank <at> [size=letter|a4|612x792|34x22in] [rotate=90] [count=N]
//! insert-from <at> <file.pdf> [pages=1-2]     the inserted pages become pages <at>, <at>+1...
//! delete <pages>
//! move <pages> [to] <n>                       the first moved page becomes page <n>
//! rotate <pages> <degrees>                    clockwise; 90, 180, 270 or -90
//! extract <pages>                             keep only these, in this order
//! replace <at> <file.pdf> [pages=1-2] [keep-markups]
//! merge <file.pdf>                            append every page, with its bookmarks
//! split <parts>|every=N                       last: one file per part, out-1.pdf, out-2.pdf
//!                                             ({n} in the -o name is the part number)
//! ```
//!
//! `plumb outline` edits are `add <path> <title> <page|none> [at=x,y]`, `rename <path>
//! <title>`, `delete <path>`, `move <path> [to] <path>` and `dest <path> <page> [at=x,y]`;
//! a path is dotted numbers from 1 (`2.1` is the first child of the second bookmark).
//!
//! Page operations and flatten rewrite the whole file (verified, then moved into place); a
//! bookmark edit is appended. A signed file is refused for a rewrite unless
//! `--invalidate-signatures` is given. `-o` may name the input itself.
//!
//! `plumb run` executes the steps of a command file (open, add, modify, delete, undo, redo,
//! save, save_as, render, ...; the format is described in `pl_doc::script`) and prints one
//! line per step. It stops at the first failing step with exit code 2.
//!
//! `plumb measure` prints each page's own scale and viewports (the `/VP` array) and then every
//! measurement: its page, kind, value as its caption shows it, value in SI units, the scale
//! it was taken at (`/R`) and the viewport, if any, and its id.
//!
//! `plumb export` writes the file's markups list as CSV (`--csv`) or as an Excel workbook
//! (`--xlsx`), and its annotations as XFDF (`--xfdf`); any of the three can be given together.
//! The list is the one pl-takeoff builds: one row per markup with its status, checkmark,
//! replies and columns. `--columns` names a JSON file with the custom columns
//! (`pl_takeoff::Schema`) whose values the list should show, and `--layout` one with the
//! column order, sort, grouping and filter (`pl_takeoff::Layout`); without them the list has
//! the built-in columns in their default order. `--takeoff` lays it out for a takeoff instead
//! (`pl_takeoff::Layout::takeoff`): the adding quantity columns (count, length, perimeter,
//! area, volume, volume base area, wall area), the ones that do not add up (angle, radius,
//! diameter), each with its unit in the heading, scale and space, grouped by subject with
//! subtotals and a total. A count is one row with its item total, however many annotations
//! carry its items. A Volume's base area is in its own Base Area column and is not part of
//! the Area total. In the CSV of a takeoff, a `Subtotal: <subject>` record follows each
//! subject's rows and a `Total` record ends the file, as in the workbook. `--units
//! imperial|metric` chooses their units (by default those of the drawings' scales). Spaces are
//! the polygons marked as spaces and the regions the document settings define. Numbers are
//! written rounded to their column's places plus six (`pl_takeoff::file_number`).
//!
//! Page numbers are 1-based, as a person counts them. Exit codes: 0 success, 1 `search` found
//! nothing, 2 error (clap also uses 2 for bad arguments).
//!
//! `pdfium.dll` is loaded from next to the executable, or from the path `--pdfium` names.
//! Debug builds also accept the absolute path in `PLUMBLINE_PDFIUM_DLL` and the workspace's
//! vendored copy; a release build ignores both. The DLL search path is never used.
//!
//! The synthetic test PDFs are written by pl-testkit's own `pl-fixtures` binary; the test
//! generator is not part of this program.

mod ranges;
mod script_ext;
mod set;
mod structure;

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use pl_doc::pl_render::{Bitmap, PdfiumBackend, RenderBackend, RenderOptions, SearchOptions};
use pl_doc::script::{Script, ScriptHost};
use pl_doc::{quads_bounds, Document, DocumentId, Session, SessionOptions};

#[derive(Parser)]
#[command(
    name = "plumb",
    version,
    about = "Plumbline command line",
    max_term_width = 100
)]
struct Cli {
    /// Absolute or relative path of pdfium.dll (default: next to plumb.exe).
    #[arg(long, global = true, value_name = "DLL")]
    pdfium: Option<PathBuf>,

    /// Password for an encrypted file (the empty user password is tried without it).
    #[arg(long, global = true, value_name = "TEXT")]
    password: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Show page count, page sizes, rotation and counts of bookmarks, annotations and text.
    Info { pdf: PathBuf },
    /// Render one page to a PNG file.
    Render {
        pdf: PathBuf,
        /// Page number, starting at 1.
        #[arg(long, short, default_value_t = 1, value_name = "N")]
        page: u32,
        /// Pixels per point: 1 is 72 dpi, 2 is 144 dpi.
        #[arg(long, short, default_value_t = 1.0, value_name = "S")]
        scale: f64,
        /// Output PNG file.
        #[arg(long, short, value_name = "FILE")]
        output: PathBuf,
        /// Leave annotations out of the picture.
        #[arg(long)]
        no_annotations: bool,
    },
    /// Find text in every page. Exit code 1 when nothing is found.
    Search {
        pdf: PathBuf,
        text: String,
        /// Upper and lower case must match.
        #[arg(long)]
        match_case: bool,
        /// Only match whole words.
        #[arg(long)]
        whole_word: bool,
    },
    /// Print the text of one page.
    Text {
        pdf: PathBuf,
        /// Page number, starting at 1.
        #[arg(long, short, default_value_t = 1, value_name = "N")]
        page: u32,
    },
    /// List every annotation: page, kind, rectangle, author, subject and id.
    Annots { pdf: PathBuf },
    /// List the page scales, viewports and every measurement with its value and scale.
    Measure { pdf: PathBuf },
    /// Run a JSON command file: open, add, modify, delete, undo, redo, save, render.
    Run {
        /// The command file. Relative paths inside it are relative to its folder.
        commands: PathBuf,
        /// Keep crash journals in this folder (default: no journal).
        #[arg(long, value_name = "DIR")]
        journal: Option<PathBuf>,
    },
    /// Export the markups list as CSV or XLSX, or the annotations as XFDF.
    Export {
        pdf: PathBuf,
        /// Write the markups list as a CSV file.
        #[arg(long, value_name = "FILE")]
        csv: Option<PathBuf>,
        /// Write the markups list as an Excel workbook.
        #[arg(long, value_name = "FILE")]
        xlsx: Option<PathBuf>,
        /// Write the annotations as an XFDF file.
        #[arg(long, value_name = "FILE")]
        xfdf: Option<PathBuf>,
        /// A JSON file with the custom columns to show.
        #[arg(long, value_name = "JSON")]
        columns: Option<PathBuf>,
        /// A JSON file with the list's column order, sort, grouping and filter.
        #[arg(long, value_name = "JSON")]
        layout: Option<PathBuf>,
        /// Lay the list out for a takeoff: the quantity columns (count, length, perimeter,
        /// area, volume, base area, wall area, angle, radius, diameter), scale and space,
        /// grouped by subject with subtotals and a total (in the CSV too).
        #[arg(long, conflicts_with = "layout")]
        takeoff: bool,
        /// The units of the quantity columns: `imperial` (ft, sq ft, cu yd) or `metric` (m,
        /// sq m, cu m). By default, those of the drawings' scales.
        #[arg(long, value_name = "SYSTEM", value_parser = ["imperial", "metric"])]
        units: Option<String>,
    },
    /// List the pages, or insert, delete, move, rotate, extract, replace, merge and split them.
    Pages {
        pdf: PathBuf,
        /// Operations, in order: insert-blank, insert-from, delete, move, rotate, extract,
        /// replace, merge, split (see `plumb pages --help`). None: list the pages.
        #[arg(value_name = "OP", allow_negative_numbers = true)]
        ops: Vec<String>,
        /// The file to write (may be the input). For split, the name of the parts: {n} is
        /// the part number, or -1, -2... go before the extension.
        #[arg(long, short, value_name = "FILE")]
        output: Option<PathBuf>,
        /// Go ahead with a signed file, invalidating its signatures.
        #[arg(long)]
        invalidate_signatures: bool,
    },
    /// Print the bookmark tree, or edit it: add, rename, delete, move, dest.
    Outline {
        pdf: PathBuf,
        /// Edits, in order (paths are dotted numbers from 1, such as 2.1). None: print.
        #[arg(value_name = "EDIT")]
        edits: Vec<String>,
        /// The file to write (may be the input).
        #[arg(long, short, value_name = "FILE")]
        output: Option<PathBuf>,
    },
    /// List every link with where it leads. Nothing is opened or run.
    Links { pdf: PathBuf },
    /// Burn annotations into the page content.
    Flatten {
        pdf: PathBuf,
        /// The file to write (may be the input).
        #[arg(long, short, value_name = "FILE")]
        output: PathBuf,
        /// Only these pages (such as 1-3,5).
        #[arg(long, value_name = "PAGES")]
        pages: Option<String>,
        /// What prints (the Print flag) rather than what shows on screen.
        #[arg(long)]
        print: bool,
        /// Form fields too (they stop being fields).
        #[arg(long)]
        forms: bool,
        /// Go ahead with a signed file, invalidating its signatures.
        #[arg(long)]
        invalidate_signatures: bool,
    },
    /// Sets: a .plset file naming a group of PDF files to list and search together.
    #[command(subcommand)]
    Set(SetCommand),
}

#[derive(Subcommand)]
enum SetCommand {
    /// Make a new set file, with any files given.
    Create {
        set: PathBuf,
        files: Vec<PathBuf>,
        /// The set's name.
        #[arg(long, value_name = "TEXT")]
        name: Option<String>,
    },
    /// Add PDF files to a set.
    Add {
        set: PathBuf,
        #[arg(required = true)]
        files: Vec<PathBuf>,
    },
    /// List a set's files with their page counts.
    List { set: PathBuf },
    /// Find text in every file of a set. Exit code 1 when nothing is found.
    Search {
        set: PathBuf,
        text: String,
        /// Upper and lower case must match.
        #[arg(long)]
        match_case: bool,
        /// Only match whole words.
        #[arg(long)]
        whole_word: bool,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match run(cli) {
        Ok(code) => code,
        Err(error) => {
            // `{:#}` prints the whole chain on one line: "open x.pdf: could not open ...".
            eprintln!("plumb: {error:#}");
            ExitCode::from(2)
        }
    }
}

fn run(cli: Cli) -> Result<ExitCode> {
    let out = std::io::stdout();
    let mut out = out.lock();
    let pdfium = cli.pdfium.as_deref();
    let password = cli.password.as_deref();
    let result = match &cli.command {
        Command::Info { pdf } => info(&mut out, pdfium, password, pdf),
        Command::Render {
            pdf,
            page,
            scale,
            output,
            no_annotations,
        } => render(
            &mut out,
            pdfium,
            password,
            pdf,
            *page,
            *scale,
            output,
            !*no_annotations,
        ),
        Command::Search {
            pdf,
            text,
            match_case,
            whole_word,
        } => {
            let options = SearchOptions {
                match_case: *match_case,
                whole_word: *whole_word,
            };
            search(&mut out, pdfium, password, pdf, text, options)
        }
        Command::Text { pdf, page } => text(&mut out, pdfium, password, pdf, *page),
        Command::Annots { pdf } => annots(&mut out, pdfium, password, pdf),
        Command::Measure { pdf } => measure(&mut out, pdfium, password, pdf),
        Command::Run { commands, journal } => {
            run_commands(&mut out, pdfium, commands, journal.as_deref())
        }
        Command::Export {
            pdf,
            csv,
            xlsx,
            xfdf,
            columns,
            layout,
            takeoff,
            units,
        } => export(
            &mut out,
            pdfium,
            password,
            &ExportArgs {
                pdf,
                csv: csv.as_deref(),
                xlsx: xlsx.as_deref(),
                xfdf: xfdf.as_deref(),
                columns: columns.as_deref(),
                layout: layout.as_deref(),
                takeoff: *takeoff,
                units: units.as_deref(),
            },
        ),
        Command::Pages {
            pdf,
            ops,
            output,
            invalidate_signatures,
        } => pages(
            &mut out,
            pdfium,
            password,
            pdf,
            ops,
            output.as_deref(),
            *invalidate_signatures,
        ),
        Command::Outline { pdf, edits, output } => {
            outline(&mut out, pdfium, password, pdf, edits, output.as_deref())
        }
        Command::Links { pdf } => links(&mut out, password, pdf),
        Command::Flatten {
            pdf,
            output,
            pages,
            print,
            forms,
            invalidate_signatures,
        } => flatten(
            &mut out,
            pdfium,
            password,
            pdf,
            output,
            &structure::FlattenArgs {
                pages: pages.clone(),
                print: *print,
                forms: *forms,
            },
            *invalidate_signatures,
        ),
        Command::Set(command) => set_command(&mut out, pdfium, password, command),
    };
    match result {
        // A closed pipe (`plumb text x.pdf | head`) is not an error.
        Err(e) if is_broken_pipe(&e) => Ok(ExitCode::SUCCESS),
        other => other,
    }
}

fn is_broken_pipe(error: &anyhow::Error) -> bool {
    error
        .downcast_ref::<std::io::Error>()
        .is_some_and(|e| e.kind() == std::io::ErrorKind::BrokenPipe)
}

/// Starts PDFium and a session that settles each document at open: a command answers
/// everything at once and then exits.
fn start(pdfium: Option<&Path>) -> Result<Session> {
    Ok(Session::with_options(
        backend(pdfium)?,
        SessionOptions::complete(),
    ))
}

/// Starts PDFium.
fn backend(pdfium: Option<&Path>) -> Result<Arc<dyn RenderBackend>> {
    let explicit = match pdfium {
        Some(p) => {
            Some(std::path::absolute(p).with_context(|| format!("resolve {}", p.display()))?)
        }
        None => None,
    };
    let backend = PdfiumBackend::new(explicit.as_deref()).context("start PDFium")?;
    Ok(Arc::new(backend))
}

/// Starts PDFium and opens one file.
fn open(
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
) -> Result<(Session, DocumentId)> {
    let mut session = start(pdfium)?;
    let id = session
        .open(pdf, password)
        .with_context(|| format!("open {}", pdf.display()))?;
    Ok((session, id))
}

/// Converts a 1-based page number from the command line to a page index.
fn page_index(doc: &Document, page: u32) -> Result<u32> {
    if page == 0 || page > doc.page_count() {
        bail!(
            "page {page} is out of range: the document has {} page{}",
            doc.page_count(),
            if doc.page_count() == 1 { "" } else { "s" }
        );
    }
    Ok(page - 1)
}

/// A number with up to `places` decimals and no trailing zeros: 612, 595.28, 8.5.
fn num(value: f64, places: usize) -> String {
    let s = format!("{value:.places$}");
    if !s.contains('.') {
        return s;
    }
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.into()
    }
}

fn info(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
) -> Result<ExitCode> {
    let (mut session, id) = open(pdfium, password, pdf)?;
    let doc = session.document_mut(id)?;
    let pages = doc.page_count();
    let bookmarks = doc.bookmarks()?.len();

    let mut lines = Vec::with_capacity(pages as usize);
    let (mut annotations, mut characters) = (0usize, 0usize);
    for page in 0..pages {
        let geometry = *doc.page_geometry(page)?;
        let size = geometry.display_size_points();
        let annots = doc.annotations(page)?.len();
        let chars = doc
            .page_text(page)?
            .chars
            .iter()
            .filter(|c| !c.generated)
            .count();
        annotations += annots;
        characters += chars;
        lines.push(format!(
            "page {}: {} x {} pt ({} x {} in), rotate {}, user unit {}, annotations {}, characters {}",
            page + 1,
            num(size.width, 2),
            num(size.height, 2),
            num(size.width / 72.0, 2),
            num(size.height / 72.0, 2),
            geometry.rotate().degrees(),
            num(geometry.user_unit(), 4),
            annots,
            chars,
        ));
    }

    let path = doc
        .path()
        .map(|p| p.display().to_string())
        .unwrap_or_default();
    writeln!(out, "file: {path}")?;
    writeln!(out, "pages: {pages}")?;
    writeln!(out, "bookmarks: {bookmarks}")?;
    writeln!(out, "annotations: {annotations}")?;
    writeln!(out, "characters: {characters}")?;
    for line in lines {
        writeln!(out, "{line}")?;
    }
    for warning in doc.warnings() {
        writeln!(out, "warning: {warning}")?;
    }
    Ok(ExitCode::SUCCESS)
}

#[allow(clippy::too_many_arguments)]
fn render(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
    page: u32,
    scale: f64,
    output: &Path,
    annotations: bool,
) -> Result<ExitCode> {
    if !scale.is_finite() || scale <= 0.0 {
        bail!("--scale must be a positive number, not {scale}");
    }
    let (session, id) = open(pdfium, password, pdf)?;
    let doc = session.document(id)?;
    let index = page_index(doc, page)?;
    let geometry = doc.page_geometry(index)?;
    // `--scale` is pixels per point; the backend counts pixels per user-space unit.
    let per_unit = scale * geometry.user_unit();
    let bitmap = doc
        .backend()
        .render_page_with(
            doc.render_id(),
            index,
            per_unit,
            RenderOptions { annotations },
        )
        .with_context(|| format!("render page {page}"))?;
    image::save_buffer_with_format(
        output,
        &bitmap.pixels,
        bitmap.width,
        bitmap.height,
        image::ExtendedColorType::Rgba8,
        image::ImageFormat::Png,
    )
    .with_context(|| format!("write {}", output.display()))?;
    writeln!(
        out,
        "wrote {}: page {page}, {} x {} px",
        output.display(),
        bitmap.width,
        bitmap.height
    )?;
    Ok(ExitCode::SUCCESS)
}

fn search(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
    query: &str,
    options: SearchOptions,
) -> Result<ExitCode> {
    if query.trim().is_empty() {
        bail!("the search text is empty");
    }
    let (mut session, id) = open(pdfium, password, pdf)?;
    let doc = session.document_mut(id)?;
    let found = doc.search(query, options)?;
    for hit in found.hits() {
        // What was actually matched, on one line (a hit may span a line break).
        let text = doc.page_text(hit.page)?;
        let matched = pl_doc::select_hit(&text, hit)
            .map(|s| s.text.split_whitespace().collect::<Vec<_>>().join(" "))
            .unwrap_or_default();
        let b = quads_bounds(&hit.quads).unwrap_or([0.0; 4]);
        writeln!(
            out,
            "page {}: [{} {} {} {}] {}",
            hit.page + 1,
            num(b[0], 2),
            num(b[1], 2),
            num(b[2], 2),
            num(b[3], 2),
            matched
        )?;
    }
    let pages = {
        let mut pages: Vec<u32> = found.hits().iter().map(|h| h.page).collect();
        pages.dedup();
        pages.len()
    };
    writeln!(
        out,
        "{} hit{} on {} page{}",
        found.len(),
        if found.len() == 1 { "" } else { "s" },
        pages,
        if pages == 1 { "" } else { "s" }
    )?;
    Ok(if found.is_empty() {
        ExitCode::from(1)
    } else {
        ExitCode::SUCCESS
    })
}

fn text(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
    page: u32,
) -> Result<ExitCode> {
    let (mut session, id) = open(pdfium, password, pdf)?;
    let doc = session.document_mut(id)?;
    let index = page_index(doc, page)?;
    let text = doc.page_text(index)?.text();
    // PDFium separates lines with CR LF and may emit NULs for unmapped glyphs.
    let text = text.replace("\r\n", "\n").replace(['\r', '\0'], "");
    out.write_all(text.as_bytes())?;
    if !text.ends_with('\n') {
        writeln!(out)?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Text for one line of output: quoted, or `-` when there is none.
fn quoted(text: Option<&str>) -> String {
    match text {
        Some(text) => format!("{text:?}"),
        None => "-".into(),
    }
}

fn annots(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
) -> Result<ExitCode> {
    let (mut session, id) = open(pdfium, password, pdf)?;
    let doc = session.document_mut(id)?;
    let editor = doc.annots().context("read the annotations")?;
    let set = editor.set();
    let mut pages = 0usize;
    for page in 0..editor.page_count() {
        let mut any = false;
        for a in set.on_page(page) {
            any = true;
            // The box the file has for it: /Rect as written, or as a save would write it.
            let r = a
                .original_dict()
                .and_then(|d| d.get(b"Rect").ok())
                .and_then(pl_doc::pl_pdf::rect_from_object)
                .unwrap_or_else(|| a.bounds());
            writeln!(
                out,
                "page {}: {} [{} {} {} {}] author {} subject {} id {}",
                page + 1,
                a.kind.pdf_name(),
                num(r.x0, 2),
                num(r.y0, 2),
                num(r.x1, 2),
                num(r.y1, 2),
                quoted(a.meta.author.as_deref()),
                quoted(a.meta.subject.as_deref()),
                a.id
            )?;
        }
        pages += usize::from(any);
    }
    writeln!(
        out,
        "{} annotation{} on {} page{}",
        set.len(),
        if set.len() == 1 { "" } else { "s" },
        pages,
        if pages == 1 { "" } else { "s" }
    )?;
    for warning in doc.warnings() {
        writeln!(out, "warning: {warning}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// The SI value of a measurement: `6.096 m`, `74.322 m2`, `90 deg`, `3`.
fn si_value(q: &pl_doc::pl_annot::Quantities) -> String {
    match (q.volume, q.area, q.length, q.angle, q.count) {
        (Some(v), ..) => format!("{} m3", num(v, 6)),
        (None, Some(a), ..) => format!("{} m2", num(a, 6)),
        (None, None, Some(l), ..) => format!("{} m", num(l, 6)),
        (None, None, None, Some(r), _) => format!("{} deg", num(r.to_degrees(), 6)),
        (None, None, None, None, Some(n)) => n.to_string(),
        _ => "-".into(),
    }
}

/// `plumb measure`: the scales of every page and every measurement with its value.
fn measure(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
) -> Result<ExitCode> {
    use pl_doc::pl_annot::ScaleRef;
    let (mut session, id) = open(pdfium, password, pdf)?;
    let doc = session.document_mut(id)?;
    let editor = doc.annots().context("read the annotations")?;
    let scales = editor.scales();
    let ratio = |spec: Option<&pl_doc::pl_annot::MeasureSpec>| {
        spec.map_or_else(|| "none".to_string(), |s| format!("{:?}", s.ratio))
    };
    for page in 0..scales.page_count() {
        let Some(vps) = scales.page(page) else {
            continue;
        };
        if vps.page_scale.is_none() && vps.viewports.is_empty() {
            continue;
        }
        writeln!(
            out,
            "page {}: scale {}",
            page + 1,
            ratio(vps.page_scale.as_ref())
        )?;
        for v in &vps.viewports {
            writeln!(
                out,
                "page {}: viewport {:?} [{} {} {} {}] scale {}",
                page + 1,
                v.name,
                num(v.bbox.x0, 2),
                num(v.bbox.y0, 2),
                num(v.bbox.x1, 2),
                num(v.bbox.y1, 2),
                ratio(v.measure.as_ref())
            )?;
        }
    }
    if let Some(default) = &scales.settings().default_scale {
        writeln!(
            out,
            "default scale: {} {} = {} {}",
            num(default.paper, 6),
            default.paper_unit,
            num(default.real, 6),
            default.real_unit
        )?;
    }
    let rows = pl_doc::measure_ops::measurements(editor.set());
    for row in &rows {
        writeln!(
            out,
            "page {}: {} {:?} ({}) scale {}{} id {}",
            row.page + 1,
            row.kind.name(),
            row.text,
            si_value(&row.quantities),
            row.ratio
                .as_deref()
                .map_or_else(|| "none".to_string(), |r| format!("{r:?}")),
            match &row.scale {
                ScaleRef::Page => String::new(),
                ScaleRef::Viewport(name) => format!(" in viewport {name:?}"),
            },
            row.id
        )?;
    }
    writeln!(
        out,
        "{} measurement{}",
        rows.len(),
        if rows.len() == 1 { "" } else { "s" }
    )?;
    for warning in doc.warnings() {
        writeln!(out, "warning: {warning}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// What `plumb export` writes and from what.
struct ExportArgs<'a> {
    pdf: &'a Path,
    csv: Option<&'a Path>,
    xlsx: Option<&'a Path>,
    xfdf: Option<&'a Path>,
    columns: Option<&'a Path>,
    layout: Option<&'a Path>,
    /// The takeoff layout instead of the default one.
    takeoff: bool,
    /// `imperial` or `metric` for the quantity columns.
    units: Option<&'a str>,
}

/// The list laid out for a takeoff: pl-takeoff's own (`Layout::takeoff`), so the CLI and the
/// app show the same columns.
fn takeoff_layout(schema: &pl_doc::pl_takeoff::Schema) -> pl_doc::pl_takeoff::Layout {
    pl_doc::pl_takeoff::Layout::takeoff(schema)
}

fn read_json<T: serde::de::DeserializeOwned>(path: &Path, what: &str) -> Result<T> {
    let text = std::fs::read_to_string(path).with_context(|| format!("read {}", path.display()))?;
    serde_json::from_str(&text).with_context(|| format!("{}: not a {what} file", path.display()))
}

/// `plumb export`: the markups list as CSV or XLSX, the annotations as XFDF. The list is
/// pl-takeoff's (`Table`, `View`, `Layout`); this only reads the file and writes the outputs.
fn export(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    args: &ExportArgs<'_>,
) -> Result<ExitCode> {
    use pl_doc::pl_takeoff::{
        export_xfdf, to_csv, to_xlsx, CsvOptions, Layout, ListContext, RowOptions, Schema, Table,
        View, XfdfOptions,
    };
    if args.csv.is_none() && args.xlsx.is_none() && args.xfdf.is_none() {
        bail!("nothing to export: give --csv, --xlsx or --xfdf with an output file");
    }
    let mut schema: Schema = match args.columns {
        Some(path) => {
            let schema: Schema = read_json(path, "column list")?;
            schema
                .validate()
                .with_context(|| format!("{}", path.display()))?;
            schema
        }
        None => Schema::new(),
    };
    let mut layout: Layout = match (args.layout, args.takeoff) {
        (Some(path), _) => read_json(path, "list layout")?,
        (None, true) => takeoff_layout(&schema),
        (None, false) => Layout::for_schema(&schema),
    };
    layout.reconcile(&schema);

    let (mut session, id) = open(pdfium, password, args.pdf)?;
    let doc = session.document_mut(id)?;
    // A file without layers, or whose layers cannot be read, lists its markups all the same.
    let layers: Vec<(pl_doc::pl_pdf::ObjectId, String)> = doc
        .layers()
        .map(|layers| layers.iter().map(|l| (l.id, l.name.clone())).collect())
        .unwrap_or_default();
    let set = doc.annots()?.set().clone();
    // The document's own columns (Bluebeam Revu's /BSIAnnotColumns) are listed after the
    // given ones.
    for column in doc.bluebeam_columns() {
        if schema.get(&column.id).is_none() {
            schema.columns.push(column);
        }
    }
    layout.reconcile(&schema);
    let context = ListContext {
        page_labels: &[],
        layers: &layers,
        schema: Some(&schema),
        options: RowOptions::default(),
    };
    // The quantity columns are worked out from the measurements and their scales; the spaces
    // the document settings define count besides the polygons marked as spaces.
    let spaces: Vec<pl_doc::pl_takeoff::Space> = doc
        .doc_settings()?
        .spaces
        .iter()
        .map(|s| {
            let outline = s
                .outline
                .iter()
                .map(|p| pl_doc::pl_geom::Point::new(p[0], p[1]))
                .collect();
            pl_doc::pl_takeoff::Space::new(&s.name, s.page, outline, Vec::new())
        })
        .collect();
    let units = match args.units {
        Some("metric") => Some(pl_doc::pl_takeoff::QuantityUnits::metric()),
        Some(_) => Some(pl_doc::pl_takeoff::QuantityUnits::imperial()),
        None => None,
    };
    let takeoff = pl_doc::pl_takeoff::TakeoffContext {
        units,
        spaces: &spaces,
        ..pl_doc::pl_takeoff::TakeoffContext::default()
    };
    let table = Table::build_takeoff(set.iter(), &context, &takeoff);
    let view = View::of(&table, &layout);
    let write = |path: &Path, bytes: &[u8]| {
        std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
    };
    let markups = view.rows().len();
    let plural = |n: usize| if n == 1 { "" } else { "s" };
    if let Some(path) = args.csv {
        // A takeoff's CSV carries its subtotals and total, as the workbook does.
        let options = CsvOptions {
            totals: args.takeoff,
            ..CsvOptions::default()
        };
        let csv = to_csv(&table, &view, &layout, &options);
        write(path, csv.as_bytes())?;
        writeln!(
            out,
            "csv: {markups} markup{} of {} to {}",
            plural(markups),
            table.roots.len(),
            path.display()
        )?;
    }
    if let Some(path) = args.xlsx {
        let workbook = to_xlsx(&table, &view, &layout)
            .with_context(|| format!("build the workbook for {}", path.display()))?;
        write(path, &workbook)?;
        writeln!(
            out,
            "xlsx: {markups} markup{} of {} to {}",
            plural(markups),
            table.roots.len(),
            path.display()
        )?;
    }
    if let Some(path) = args.xfdf {
        let href = args
            .pdf
            .file_name()
            .map(|n| n.to_string_lossy().into_owned());
        let exported = export_xfdf(set.iter(), &XfdfOptions { href });
        write(path, exported.xml.as_bytes())?;
        writeln!(
            out,
            "xfdf: {} annotation{} to {}, {} left out",
            exported.written,
            plural(exported.written),
            path.display(),
            exported.skipped.len()
        )?;
        for (id, reason) in &exported.skipped {
            writeln!(out, "  left out {id}: {reason:?}")?;
        }
    }
    for warning in session.document(id)?.warnings() {
        writeln!(out, "warning: {warning}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// Where a command file's pictures and progress lines go.
struct CliHost<'a, W: Write> {
    out: &'a mut W,
    /// The first failure to write a progress line (a closed pipe); the run goes on.
    failed: Option<std::io::Error>,
}

impl<W: Write> ScriptHost for CliHost<'_, W> {
    fn write_png(&mut self, path: &Path, bitmap: &Bitmap) -> std::io::Result<()> {
        image::save_buffer_with_format(
            path,
            &bitmap.pixels,
            bitmap.width,
            bitmap.height,
            image::ExtendedColorType::Rgba8,
            image::ImageFormat::Png,
        )
        .map_err(std::io::Error::other)
    }

    fn log(&mut self, line: &str) {
        if self.failed.is_none() {
            if let Err(e) = writeln!(self.out, "{line}") {
                self.failed = Some(e);
            }
        }
    }
}

fn run_commands(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    commands: &Path,
    journal: Option<&Path>,
) -> Result<ExitCode> {
    // A file with page, bookmark, flatten or set steps is run by `script_ext` (which hands the
    // other steps to pl-doc); any other file goes to pl-doc as it is.
    let plan = match std::fs::read_to_string(commands) {
        Ok(json) => script_ext::plan(&json).with_context(|| format!("{}", commands.display()))?,
        Err(_) => None,
    };
    let script = match plan {
        Some(_) => None,
        None => Some(Script::read(commands).with_context(|| format!("{}", commands.display()))?),
    };
    let commands =
        std::path::absolute(commands).with_context(|| format!("resolve {}", commands.display()))?;
    let base = commands.parent().unwrap_or(Path::new("."));
    let mut session = start(pdfium)?;
    if let Some(dir) = journal {
        let dir = std::path::absolute(dir).with_context(|| format!("resolve {}", dir.display()))?;
        session.set_journal_dir(Some(dir));
    }
    let mut host = CliHost { out, failed: None };
    let steps = match (&plan, &script) {
        (Some(plan), _) => script_ext::run(&mut session, plan, base, &mut host)?,
        (None, Some(script)) => pl_doc::script::run(&mut session, script, base, &mut host)?.steps,
        (None, None) => unreachable!("one of the two is read"),
    };
    if let Some(e) = host.failed {
        return Err(e.into());
    }
    writeln!(
        out,
        "{} step{} done",
        steps,
        if steps == 1 { "" } else { "s" }
    )?;
    Ok(ExitCode::SUCCESS)
}

/// `plumb pages`: list the pages, or run page operations and write the result.
fn pages(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
    words: &[String],
    output: Option<&Path>,
    invalidate_signatures: bool,
) -> Result<ExitCode> {
    let (ops, split) = structure::parse_page_ops(words)?;
    let mut file = structure::open_pdf(pdf, password)?;
    if ops.is_empty() && split.is_none() {
        if output.is_some() {
            bail!(
                "no page operations to write: give some (delete 2, rotate 1 90...) or leave out -o"
            );
        }
        structure::list_pages(out, file.document()?, pdf)?;
        return Ok(ExitCode::SUCCESS);
    }
    let Some(output) = output else {
        bail!("give the file to write with -o (it may be the input)");
    };
    let backend = backend(pdfium)?;
    let resolve = |p: &Path| p.to_path_buf();
    let (doc, lines, notes) =
        structure::apply_page_ops(file.document()?, &ops, &resolve, password)?;
    for line in &lines {
        writeln!(out, "{line}")?;
    }
    for note in &notes {
        writeln!(out, "note: {note}")?;
    }
    let policy = structure::signature_policy(invalidate_signatures);
    let warnings = structure::signature_warnings(&doc);
    match split {
        None => {
            let n = doc.page_count();
            structure::save_rewrite(&mut file, doc, output, policy, backend.as_ref(), password)?;
            writeln!(
                out,
                "wrote {}: {n} page{}",
                output.display(),
                ranges::plural(n)
            )?;
        }
        Some(spec) => {
            let parts = spec.ranges(doc.page_count())?;
            let pieces = doc.split(&parts)?;
            for (k, ((part, _), range)) in pieces.into_iter().zip(&parts).enumerate() {
                let path = structure::part_path(output, k + 1);
                let n = part.page_count();
                structure::save_new(part, &path, policy, backend.as_ref())?;
                writeln!(
                    out,
                    "wrote {}: pages {}-{} ({n} page{})",
                    path.display(),
                    range.start + 1,
                    range.end,
                    ranges::plural(n)
                )?;
            }
        }
    }
    for w in warnings {
        writeln!(out, "{w}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `plumb outline`: print the bookmarks, or edit them and write the result.
fn outline(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
    words: &[String],
    output: Option<&Path>,
) -> Result<ExitCode> {
    let edits = structure::parse_outline_edits(words)?;
    let mut file = structure::open_pdf(pdf, password)?;
    if edits.is_empty() {
        if output.is_some() {
            bail!("no bookmark edits to write: give some (add, rename, delete, move, dest) or leave out -o");
        }
        structure::list_outline(out, file.document()?)?;
        return Ok(ExitCode::SUCCESS);
    }
    let Some(output) = output else {
        bail!("give the file to write with -o (it may be the input)");
    };
    let backend = backend(pdfium)?;
    let (tree, lines) = structure::apply_outline_edits(file.document()?, &edits)?;
    for line in &lines {
        writeln!(out, "{line}")?;
    }
    structure::save_outline(&mut file, &tree, output, backend.as_ref(), password)?;
    let n = tree.len();
    writeln!(
        out,
        "wrote {}: {n} bookmark{}",
        output.display(),
        ranges::plural(n)
    )?;
    Ok(ExitCode::SUCCESS)
}

/// `plumb links`: every link with where it leads.
fn links(out: &mut impl Write, password: Option<&str>, pdf: &Path) -> Result<ExitCode> {
    let mut file = structure::open_pdf(pdf, password)?;
    structure::list_links(out, file.document()?)?;
    Ok(ExitCode::SUCCESS)
}

/// `plumb flatten`: burn annotations in and write the result.
fn flatten(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    pdf: &Path,
    output: &Path,
    args: &structure::FlattenArgs,
    invalidate_signatures: bool,
) -> Result<ExitCode> {
    let mut file = structure::open_pdf(pdf, password)?;
    let doc = file.document()?;
    structure::check_permission(doc, true, "be flattened")?;
    let options = structure::flatten_options(doc, args)?;
    let (flat, report) = doc.flatten(&options)?;
    let warnings = structure::signature_warnings(&flat);
    let backend = backend(pdfium)?;
    structure::save_rewrite(
        &mut file,
        flat,
        output,
        structure::signature_policy(invalidate_signatures),
        backend.as_ref(),
        password,
    )?;
    writeln!(out, "{}", structure::describe_flatten(&report))?;
    writeln!(out, "wrote {}", output.display())?;
    for w in warnings {
        writeln!(out, "{w}")?;
    }
    Ok(ExitCode::SUCCESS)
}

/// `plumb set`: make, extend, list and search set files.
fn set_command(
    out: &mut impl Write,
    pdfium: Option<&Path>,
    password: Option<&str>,
    command: &SetCommand,
) -> Result<ExitCode> {
    use set::SetFile;
    match command {
        SetCommand::Create { set, files, name } => {
            if set.exists() {
                bail!("{} already exists (use plumb set add)", set.display());
            }
            let mut s = SetFile::new(name.as_deref().unwrap_or_default());
            for f in files {
                if !s.add(f)? {
                    writeln!(out, "already in the set: {}", f.display())?;
                }
            }
            s.write(set)?;
            writeln!(
                out,
                "wrote {}: {} file{}",
                set.display(),
                s.len(),
                ranges::plural(s.len())
            )?;
        }
        SetCommand::Add { set, files } => {
            let mut s = SetFile::read(set)?;
            let mut added = 0;
            for f in files {
                if s.add(f)? {
                    added += 1;
                } else {
                    writeln!(out, "already in the set: {}", f.display())?;
                }
            }
            s.write(set)?;
            writeln!(
                out,
                "added {added}: {} has {} file{}",
                set.display(),
                s.len(),
                ranges::plural(s.len())
            )?;
        }
        SetCommand::List { set } => {
            let s = SetFile::read(set)?;
            if !s.set.name.is_empty() {
                writeln!(out, "set: {}", s.set.name)?;
            }
            for (i, path) in s.resolved().into_iter().enumerate() {
                let entry = s.shown(i, set);
                let pages = structure::open_pdf(&path, password)
                    .and_then(|mut f| Ok(f.document()?.page_count()));
                match pages {
                    Ok(n) => writeln!(out, "{}: {} ({n} page{})", i + 1, entry, ranges::plural(n))?,
                    Err(e) => writeln!(out, "{}: {} (cannot be read: {e:#})", i + 1, entry)?,
                }
            }
            writeln!(out, "{} file{}", s.len(), ranges::plural(s.len()))?;
        }
        SetCommand::Search {
            set,
            text,
            match_case,
            whole_word,
        } => {
            let s = SetFile::read(set)?;
            let mut session = start(pdfium)?;
            let options = SearchOptions {
                match_case: *match_case,
                whole_word: *whole_word,
            };
            let found = set::search(&mut session, &s, text, options, password)?;
            for hit in &found.hits {
                let b = hit.bounds;
                writeln!(
                    out,
                    "{} page {}: [{} {} {} {}] {}",
                    s.shown(hit.file, set),
                    hit.page + 1,
                    num(b[0], 2),
                    num(b[1], 2),
                    num(b[2], 2),
                    num(b[3], 2),
                    hit.text
                )?;
            }
            for (path, why) in &found.failed {
                writeln!(out, "warning: could not search {}: {why}", path.display())?;
            }
            let files: std::collections::BTreeSet<usize> =
                found.hits.iter().map(|h| h.file).collect();
            writeln!(
                out,
                "{} hit{} in {} of {} file{}",
                found.hits.len(),
                ranges::plural(found.hits.len()),
                files.len(),
                found.files,
                ranges::plural(found.files)
            )?;
            if found.hits.is_empty() {
                return Ok(ExitCode::from(1));
            }
        }
    }
    Ok(ExitCode::SUCCESS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn the_argument_definitions_are_consistent() {
        Cli::command().debug_assert();
    }

    #[test]
    fn si_values_name_their_unit() {
        use pl_doc::pl_annot::Quantities;
        let q = |f: fn(&mut Quantities)| {
            let mut q = Quantities::default();
            f(&mut q);
            si_value(&q)
        };
        assert_eq!(q(|q| q.length = Some(6.096)), "6.096 m");
        assert_eq!(
            q(|q| {
                q.length = Some(36.576);
                q.area = Some(74.322432);
            }),
            "74.322432 m2"
        );
        assert_eq!(
            q(|q| {
                q.area = Some(1.0);
                q.volume = Some(0.5);
            }),
            "0.5 m3"
        );
        assert_eq!(q(|q| q.angle = Some(std::f64::consts::FRAC_PI_2)), "90 deg");
        assert_eq!(q(|q| q.count = Some(3)), "3");
        assert_eq!(q(|_| {}), "-");
    }

    #[test]
    fn numbers_print_without_trailing_zeros() {
        assert_eq!(num(612.0, 2), "612");
        assert_eq!(num(8.5, 2), "8.5");
        assert_eq!(num(595.276, 2), "595.28");
        assert_eq!(num(-0.001, 2), "0");
        assert_eq!(num(100.0, 0), "100");
        assert_eq!(num(2.0, 4), "2");
    }

    #[test]
    fn subcommands_parse_with_their_options() {
        let cli = Cli::try_parse_from([
            "plumb", "render", "a.pdf", "--page", "3", "--scale", "2.5", "-o", "out.png",
        ])
        .unwrap();
        match cli.command {
            Command::Render {
                page,
                scale,
                output,
                no_annotations,
                ..
            } => {
                assert_eq!((page, scale, no_annotations), (3, 2.5, false));
                assert_eq!(output, PathBuf::from("out.png"));
            }
            _ => panic!("expected render"),
        }
        let cli = Cli::try_parse_from([
            "plumb",
            "search",
            "a.pdf",
            "wall",
            "--pdfium",
            "C:\\x\\pdfium.dll",
        ])
        .unwrap();
        assert_eq!(cli.pdfium, Some(PathBuf::from("C:\\x\\pdfium.dll")));
        assert!(matches!(cli.command, Command::Search { .. }));
        // `render` needs an output file; `text` defaults to page 1.
        assert!(Cli::try_parse_from(["plumb", "render", "a.pdf"]).is_err());
        let cli = Cli::try_parse_from(["plumb", "text", "a.pdf"]).unwrap();
        assert!(matches!(cli.command, Command::Text { page: 1, .. }));
        assert!(Cli::try_parse_from(["plumb"]).is_err());
        let cli = Cli::try_parse_from(["plumb", "annots", "a.pdf"]).unwrap();
        assert!(matches!(cli.command, Command::Annots { .. }));
        let cli = Cli::try_parse_from(["plumb", "measure", "a.pdf"]).unwrap();
        assert!(matches!(cli.command, Command::Measure { pdf } if pdf == Path::new("a.pdf")));
        assert!(Cli::try_parse_from(["plumb", "measure"]).is_err());
        let cli = Cli::try_parse_from(["plumb", "run", "steps.json", "--journal", "j"]).unwrap();
        match cli.command {
            Command::Run { commands, journal } => {
                assert_eq!(commands, PathBuf::from("steps.json"));
                assert_eq!(journal, Some(PathBuf::from("j")));
            }
            _ => panic!("expected run"),
        }
        assert!(Cli::try_parse_from(["plumb", "run"]).is_err());
        let cli = Cli::try_parse_from([
            "plumb",
            "export",
            "a.pdf",
            "--csv",
            "a.csv",
            "--xfdf",
            "a.xfdf",
            "--columns",
            "c.json",
        ])
        .unwrap();
        match cli.command {
            Command::Export {
                pdf,
                csv,
                xlsx,
                xfdf,
                columns,
                layout,
                takeoff,
                units,
            } => {
                assert_eq!(pdf, PathBuf::from("a.pdf"));
                assert_eq!(csv, Some(PathBuf::from("a.csv")));
                assert_eq!(xfdf, Some(PathBuf::from("a.xfdf")));
                assert_eq!(columns, Some(PathBuf::from("c.json")));
                assert!(xlsx.is_none() && layout.is_none());
                assert!(!takeoff && units.is_none());
            }
            _ => panic!("expected export"),
        }
        assert!(Cli::try_parse_from(["plumb", "export"]).is_err());
        let cli = Cli::try_parse_from([
            "plumb",
            "export",
            "a.pdf",
            "--takeoff",
            "--units",
            "metric",
            "--csv",
            "a.csv",
        ])
        .unwrap();
        assert!(matches!(
            cli.command,
            Command::Export { takeoff: true, units: Some(ref u), .. } if u == "metric"
        ));
        assert!(Cli::try_parse_from([
            "plumb", "export", "a.pdf", "--units", "furlongs", "--csv", "a.csv"
        ])
        .is_err());
    }
}

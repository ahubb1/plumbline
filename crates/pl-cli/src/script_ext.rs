//! Command-file steps for page editing, bookmarks, links, flatten and sets, run by `plumb run`
//! beside pl-doc's own steps (`pl_doc::script`).
//!
//! ```json
//! { "steps": [
//!   { "op": "open", "path": "drawing.pdf" },
//!   { "op": "add", "kind": "Square", "page": 1, "rect": [100, 100, 200, 200] },
//!   { "op": "save" },
//!   { "op": "pages", "ops": [ { "do": "delete", "pages": "2,4-5" },
//!                             { "do": "rotate", "pages": "1", "degrees": 90 },
//!                             { "do": "move", "pages": "3", "to": 1 },
//!                             { "do": "insert_blank", "at": "end", "size": "letter" },
//!                             { "do": "insert_from", "at": 2, "file": "other.pdf", "pages": "1-2" },
//!                             { "do": "extract", "pages": "1-3" },
//!                             { "do": "replace", "at": 1, "file": "new.pdf", "keep_markups": true },
//!                             { "do": "merge", "file": "more.pdf" } ] },
//!   { "op": "split", "input": "drawing.pdf", "parts": "1-2,3-", "output": "part-{n}.pdf" },
//!   { "op": "outline", "edits": [ { "do": "add", "path": "1", "title": "Cover", "page": 1 },
//!                                 { "do": "rename", "path": "2", "title": "Plans" },
//!                                 { "do": "move", "path": "3", "to": "2.1" },
//!                                 { "do": "dest", "path": "1", "page": 2, "at": [72, 700] },
//!                                 { "do": "delete", "path": "4" } ] },
//!   { "op": "flatten", "pages": "1-2", "print": false, "forms": false, "output": "flat.pdf" },
//!   { "op": "expect_pages", "count": 3, "text": ["PLM-A-03", "", "PLM-A-01"],
//!     "rotate": [0, 0, 90], "labels": ["iii", "1", "i"] },
//!   { "op": "expect_outline", "titles": ["Cover", "Plans"], "levels": [0, 0], "pages": [1, null] },
//!   { "op": "expect_links", "page": 1, "count": 2, "kinds": ["goto", "uri"], "targets": [3, null] },
//!   { "op": "set_create", "set": "plans.plset", "name": "Plans", "files": ["a.pdf", "b.pdf"] },
//!   { "op": "set_add", "set": "plans.plset", "files": ["c.pdf"] },
//!   { "op": "set_list", "set": "plans.plset", "count": 3 },
//!   { "op": "set_search", "set": "plans.plset", "text": "KERB",
//!     "expect": [ { "file": "b.pdf", "page": 2 } ] }
//! ] }
//! ```
//!
//! * `input` is the file a step reads; without one it is the open document (the last `open`,
//!   or the file a `save_as` moved it to). `output` is where it writes; without one the step
//!   rewrites its input in place. The open document stays the one that was open (it is
//!   re-read after an in-place step); `open` the output to go on with it.
//! * A page, bookmark, flatten or set step needs the open document saved: one with unsaved
//!   changes stops the run. The steps after it start from the file as the step left it, so
//!   the clipboard and columns defined before it are gone.
//! * `pages` and `flatten` (and `split`) rewrite the whole file; a signed file is refused
//!   unless the step says `"invalidate_signatures": true`. `outline` appends its changes.
//! * `expect_*` steps fail the run unless the file is as described; every key is optional.
//!   `text` checks that each page's text contains its string (`""` matches any page),
//!   `targets` the page each link leads to (`null` for none), `pages` each bookmark's page.
//! * `set_search` fails when `expect` is given and the distinct (file, page) pairs it found
//!   differ, or when `count` is given and the number of hits differs.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use pl_doc::pl_render::{DocSource, RenderBackend, SearchOptions};
use pl_doc::script::{Script, ScriptHost, Step};
use pl_doc::{DocError, DocumentId, Session};
use serde::Deserialize;
use serde_json::Value;

use crate::ranges::plural;
use crate::set::{path_key, SetFile};
use crate::structure::{
    apply_outline_edits, apply_page_ops, describe_flatten, flatten_options, link_rows, open_pdf,
    part_path, same_file, save_new, save_outline, save_rewrite, signature_policy,
    signature_warnings, FlattenArgs, OutlineEdit, PageOp, PageText, SplitSpec,
};

/// One expected search hit.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HitSpec {
    pub file: PathBuf,
    pub page: u32,
}

/// A step this module runs.
#[derive(Debug, Clone, PartialEq, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case", deny_unknown_fields)]
pub enum ExtStep {
    Pages {
        #[serde(default)]
        input: Option<PathBuf>,
        ops: Vec<PageOp>,
        #[serde(default)]
        output: Option<PathBuf>,
        #[serde(default)]
        invalidate_signatures: bool,
    },
    Split {
        #[serde(default)]
        input: Option<PathBuf>,
        #[serde(default)]
        parts: Option<String>,
        #[serde(default)]
        every: Option<usize>,
        /// A pattern: `{n}` is the part number; without it `-n` goes before the extension.
        output: PathBuf,
        #[serde(default)]
        invalidate_signatures: bool,
    },
    Outline {
        #[serde(default)]
        input: Option<PathBuf>,
        #[serde(default)]
        edits: Vec<OutlineEdit>,
        #[serde(default)]
        output: Option<PathBuf>,
    },
    Flatten {
        #[serde(default)]
        input: Option<PathBuf>,
        #[serde(default)]
        pages: Option<PageText>,
        #[serde(default)]
        print: bool,
        #[serde(default)]
        forms: bool,
        #[serde(default)]
        output: Option<PathBuf>,
        #[serde(default)]
        invalidate_signatures: bool,
    },
    ExpectPages {
        #[serde(default)]
        input: Option<PathBuf>,
        #[serde(default)]
        count: Option<usize>,
        #[serde(default)]
        text: Option<Vec<String>>,
        #[serde(default)]
        rotate: Option<Vec<i32>>,
        #[serde(default)]
        labels: Option<Vec<String>>,
    },
    ExpectOutline {
        #[serde(default)]
        input: Option<PathBuf>,
        #[serde(default)]
        count: Option<usize>,
        #[serde(default)]
        titles: Option<Vec<String>>,
        #[serde(default)]
        levels: Option<Vec<usize>>,
        #[serde(default)]
        pages: Option<Vec<Option<u32>>>,
    },
    ExpectLinks {
        #[serde(default)]
        input: Option<PathBuf>,
        #[serde(default)]
        page: Option<u32>,
        #[serde(default)]
        count: Option<usize>,
        #[serde(default)]
        kinds: Option<Vec<String>>,
        #[serde(default)]
        targets: Option<Vec<Option<u32>>>,
    },
    SetCreate {
        set: PathBuf,
        #[serde(default)]
        name: Option<String>,
        #[serde(default)]
        files: Vec<PathBuf>,
    },
    SetAdd {
        set: PathBuf,
        files: Vec<PathBuf>,
    },
    SetList {
        set: PathBuf,
        #[serde(default)]
        count: Option<usize>,
    },
    SetSearch {
        set: PathBuf,
        text: String,
        #[serde(default)]
        match_case: bool,
        #[serde(default)]
        whole_word: bool,
        #[serde(default)]
        count: Option<usize>,
        #[serde(default)]
        expect: Option<Vec<HitSpec>>,
    },
}

/// The `op` names this module runs.
pub const EXT_OPS: [&str; 13] = [
    "pages",
    "split",
    "outline",
    "flatten",
    "expect_pages",
    "expect_outline",
    "expect_links",
    "set_create",
    "set_add",
    "set_list",
    "set_search",
    // Reserved spellings that would otherwise reach pl-doc and fail there less clearly.
    "links",
    "set",
];

impl ExtStep {
    fn name(&self) -> &'static str {
        match self {
            ExtStep::Pages { .. } => "pages",
            ExtStep::Split { .. } => "split",
            ExtStep::Outline { .. } => "outline",
            ExtStep::Flatten { .. } => "flatten",
            ExtStep::ExpectPages { .. } => "expect_pages",
            ExtStep::ExpectOutline { .. } => "expect_outline",
            ExtStep::ExpectLinks { .. } => "expect_links",
            ExtStep::SetCreate { .. } => "set_create",
            ExtStep::SetAdd { .. } => "set_add",
            ExtStep::SetList { .. } => "set_list",
            ExtStep::SetSearch { .. } => "set_search",
        }
    }

    /// The PDF the step reads, as written (`None`: the open document); `None` for set steps.
    fn input(&self) -> Option<&Option<PathBuf>> {
        match self {
            ExtStep::Pages { input, .. }
            | ExtStep::Split { input, .. }
            | ExtStep::Outline { input, .. }
            | ExtStep::Flatten { input, .. }
            | ExtStep::ExpectPages { input, .. }
            | ExtStep::ExpectOutline { input, .. }
            | ExtStep::ExpectLinks { input, .. } => Some(input),
            _ => None,
        }
    }
}

/// A command file split into runs of pl-doc steps and steps of this module.
#[derive(Debug)]
pub struct Plan {
    author: Option<String>,
    date: Option<String>,
    items: Vec<Item>,
    steps: usize,
}

#[derive(Debug)]
enum Item {
    /// pl-doc steps; `first` is the number (from 1) of the first in the file.
    Doc {
        first: usize,
        steps: Vec<Step>,
    },
    Ext {
        number: usize,
        step: Box<ExtStep>,
    },
}

/// Reads a command file. `None` when it has none of this module's steps: it then runs exactly
/// as pl-doc runs it.
pub fn plan(json: &str) -> Result<Option<Plan>> {
    let Ok(value) = serde_json::from_str::<Value>(json) else {
        return Ok(None);
    };
    let Some(steps) = value.get("steps").and_then(Value::as_array) else {
        return Ok(None);
    };
    let op = |s: &Value| s.get("op").and_then(Value::as_str).map(str::to_string);
    if !steps
        .iter()
        .any(|s| op(s).is_some_and(|o| EXT_OPS.contains(&o.as_str())))
    {
        return Ok(None);
    }
    let text = |key: &str| -> Result<Option<String>> {
        match value.get(key) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.clone())),
            Some(_) => bail!("\"{key}\" must be a string"),
        }
    };
    let mut plan = Plan {
        author: text("author")?,
        date: text("date")?,
        items: Vec::new(),
        steps: steps.len(),
    };
    for (i, raw) in steps.iter().enumerate() {
        let number = i + 1;
        let name = op(raw).unwrap_or_else(|| "?".into());
        if EXT_OPS.contains(&name.as_str()) {
            let step: ExtStep = serde_json::from_value(raw.clone())
                .map_err(|e| anyhow!("step {number} ({name}): {e}"))?;
            plan.items.push(Item::Ext {
                number,
                step: Box::new(step),
            });
            continue;
        }
        let one = serde_json::json!({ "steps": [raw] });
        let step = Script::from_json(&one.to_string())
            .map_err(|e| anyhow!("step {number} ({name}): {e}"))?
            .steps
            .pop()
            .expect("one step");
        match plan.items.last_mut() {
            Some(Item::Doc { steps, .. }) => steps.push(step),
            _ => plan.items.push(Item::Doc {
                first: number,
                steps: vec![step],
            }),
        }
    }
    Ok(Some(plan))
}

/// Passes pl-doc's progress lines on with the step numbers of the whole file, leaving out
/// the lines of the steps this module added.
struct Renumber<'a> {
    inner: &'a mut dyn ScriptHost,
    /// Number (in the file) of the segment's first own step.
    first: usize,
    /// Steps added in front of the segment's own.
    added: usize,
}

impl Renumber<'_> {
    fn map(&self, n: usize) -> Option<usize> {
        (n > self.added).then(|| self.first + n - 1 - self.added)
    }
}

impl ScriptHost for Renumber<'_> {
    fn write_png(
        &mut self,
        path: &Path,
        bitmap: &pl_doc::pl_render::Bitmap,
    ) -> std::io::Result<()> {
        self.inner.write_png(path, bitmap)
    }

    fn log(&mut self, line: &str) {
        let Some((n, rest)) = line.split_once(": ") else {
            self.inner.log(line);
            return;
        };
        match n.parse::<usize>() {
            Ok(n) => {
                if let Some(m) = self.map(n) {
                    self.inner.log(&format!("{m}: {rest}"));
                }
            }
            Err(_) => self.inner.log(line),
        }
    }
}

/// The document the steps act on.
#[derive(Debug, Default)]
struct Current {
    path: Option<PathBuf>,
    password: Option<String>,
    /// Open in the session (a pl-doc segment just ran).
    open: Option<DocumentId>,
}

/// Runs a plan. Returns the number of steps run.
pub fn run(
    session: &mut Session,
    plan: &Plan,
    base: &Path,
    host: &mut dyn ScriptHost,
) -> Result<usize> {
    let backend: Arc<dyn RenderBackend> = session.backend().clone();
    let mut current = Current::default();
    let mut done = 0;
    for item in &plan.items {
        match item {
            Item::Doc { first, steps } => {
                let mut script = Script {
                    author: plan.author.clone(),
                    date: plan.date.clone(),
                    steps: steps.clone(),
                };
                let mut added = 0;
                if current.open.is_none() {
                    if let Some(path) = &current.path {
                        script.steps.insert(
                            0,
                            Step::Open {
                                path: path.clone(),
                                password: current.password.clone(),
                            },
                        );
                        added = 1;
                    }
                }
                // The password of the document the segment leaves open: its last `open`.
                for step in steps {
                    if let Step::Open { password, .. } = step {
                        current.password = password.clone();
                    }
                }
                let mut renumber = Renumber {
                    inner: &mut *host,
                    first: *first,
                    added,
                };
                let report = match pl_doc::script::run(session, &script, base, &mut renumber) {
                    Ok(r) => r,
                    Err(DocError::Script(msg)) => {
                        return Err(anyhow!(renumber_error(
                            &msg,
                            &renumber,
                            current.path.as_deref()
                        )))
                    }
                    Err(e) => return Err(e.into()),
                };
                done += report.steps - added;
                if let Some(id) = report.document {
                    current.open = Some(id);
                    current.path = session.document(id)?.path().map(Path::to_path_buf);
                }
            }
            Item::Ext { number, step } => {
                let name = step.name();
                // The open document is saved and closed first, so the file can be read and
                // replaced; the next pl-doc steps open it again.
                if let Some(id) = current.open {
                    if step.input().is_some() {
                        if session.document(id)?.is_modified() {
                            bail!(
                                "step {number} ({name}): the open document has unsaved changes; \
                                 save it first"
                            );
                        }
                        session.close(id)?;
                        current.open = None;
                    }
                }
                let lines = ext_step(step, base, &current, backend.as_ref(), session)
                    .map_err(|e| anyhow!("step {number} ({name}): {e:#}"))?;
                let mut lines = lines.into_iter();
                let head = lines.next().unwrap_or_default();
                if head.is_empty() {
                    host.log(&format!("{number}: {name}"));
                } else {
                    host.log(&format!("{number}: {name} {head}"));
                }
                for line in lines {
                    host.log(&format!("  {line}"));
                }
                done += 1;
            }
        }
    }
    debug_assert!(done <= plan.steps);
    Ok(done)
}

fn renumber_error(msg: &str, r: &Renumber<'_>, reopened: Option<&Path>) -> String {
    // pl-doc names the step as "step N (name): ...".
    if let Some(rest) = msg.strip_prefix("step ") {
        if let Some((n, tail)) = rest.split_once(' ') {
            if let Ok(n) = n.parse::<usize>() {
                return match r.map(n) {
                    Some(m) => format!("step {m} {tail}"),
                    None => format!(
                        "reopening {} before step {}: {tail}",
                        reopened
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        r.first
                    ),
                };
            }
        }
    }
    msg.to_string()
}

/// The PDF a step reads.
fn input_path(input: &Option<PathBuf>, base: &Path, current: &Current) -> Result<PathBuf> {
    match input {
        Some(p) => Ok(base.join(p)),
        None => current
            .path
            .clone()
            .ok_or_else(|| anyhow!("there is no \"input\" and no open document")),
    }
}

fn output_path(output: &Option<PathBuf>, base: &Path, input: &Path) -> PathBuf {
    match output {
        Some(p) => base.join(p),
        None => input.to_path_buf(),
    }
}

fn password(current: &Current, input: &Path) -> Option<String> {
    match &current.path {
        Some(p) if same_file(p, input) => current.password.clone(),
        _ => None,
    }
}

fn check_list<T: PartialEq + std::fmt::Debug>(
    what: &str,
    found: &[T],
    expected: &[T],
) -> Result<()> {
    if found != expected {
        bail!("expected {what} {expected:?}, found {found:?}");
    }
    Ok(())
}

/// Runs one step; returns its progress lines (the first goes after the step name).
fn ext_step(
    step: &ExtStep,
    base: &Path,
    current: &Current,
    backend: &dyn RenderBackend,
    session: &mut Session,
) -> Result<Vec<String>> {
    let resolve = |p: &Path| base.join(p);
    match step {
        ExtStep::Pages {
            input,
            ops,
            output,
            invalidate_signatures,
        } => {
            let input = input_path(input, base, current)?;
            let out = output_path(output, base, &input);
            let pw = password(current, &input);
            let mut file = open_pdf(&input, pw.as_deref())?;
            let (doc, op_lines, notes) =
                apply_page_ops(file.document()?, ops, &resolve, pw.as_deref())?;
            let warnings = signature_warnings(&doc);
            let pages = doc.page_count();
            save_rewrite(
                &mut file,
                doc,
                &out,
                signature_policy(*invalidate_signatures),
                backend,
                pw.as_deref(),
            )?;
            let mut lines = vec![format!(
                "wrote {} ({pages} page{})",
                out.display(),
                plural(pages)
            )];
            lines.extend(op_lines);
            lines.extend(notes.into_iter().map(|n| format!("note: {n}")));
            lines.extend(warnings);
            Ok(lines)
        }
        ExtStep::Split {
            input,
            parts,
            every,
            output,
            invalidate_signatures,
        } => {
            let input = input_path(input, base, current)?;
            let pw = password(current, &input);
            let mut file = open_pdf(&input, pw.as_deref())?;
            let doc = file.document()?;
            let spec = SplitSpec {
                parts: parts.clone(),
                every: *every,
            };
            let ranges = spec.ranges(doc.page_count())?;
            let pieces = doc.split(&ranges)?;
            let pattern = base.join(output);
            let mut lines = vec![format!("{} part{}", pieces.len(), plural(pieces.len()))];
            for (k, ((part, _), range)) in pieces.into_iter().zip(&ranges).enumerate() {
                let path = part_path(&pattern, k + 1);
                save_new(
                    part,
                    &path,
                    signature_policy(*invalidate_signatures),
                    backend,
                )?;
                lines.push(format!(
                    "wrote {}: pages {}-{}",
                    path.display(),
                    range.start + 1,
                    range.end
                ));
            }
            Ok(lines)
        }
        ExtStep::Outline {
            input,
            edits,
            output,
        } => {
            let input = input_path(input, base, current)?;
            let pw = password(current, &input);
            let mut file = open_pdf(&input, pw.as_deref())?;
            if edits.is_empty() {
                let n = file.document()?.outline().len();
                return Ok(vec![format!("{n} bookmark{}", plural(n))]);
            }
            let out = output_path(output, base, &input);
            let (outline, edit_lines) = apply_outline_edits(file.document()?, edits)?;
            save_outline(&mut file, &outline, &out, backend, pw.as_deref())?;
            let n = outline.len();
            let mut lines = vec![format!(
                "wrote {} ({n} bookmark{})",
                out.display(),
                plural(n)
            )];
            lines.extend(edit_lines);
            Ok(lines)
        }
        ExtStep::Flatten {
            input,
            pages,
            print,
            forms,
            output,
            invalidate_signatures,
        } => {
            let input = input_path(input, base, current)?;
            let out = output_path(output, base, &input);
            let pw = password(current, &input);
            let mut file = open_pdf(&input, pw.as_deref())?;
            let args = FlattenArgs {
                pages: pages.as_ref().map(|p| match p {
                    PageText::Number(n) => n.to_string(),
                    PageText::Text(t) => t.clone(),
                }),
                print: *print,
                forms: *forms,
            };
            let doc = file.document()?;
            crate::structure::check_permission(doc, true, "be flattened")?;
            let options = flatten_options(doc, &args)?;
            let (flat, report) = doc.flatten(&options)?;
            let warnings = signature_warnings(&flat);
            save_rewrite(
                &mut file,
                flat,
                &out,
                signature_policy(*invalidate_signatures),
                backend,
                pw.as_deref(),
            )?;
            let mut lines = vec![format!(
                "wrote {}: {}",
                out.display(),
                describe_flatten(&report)
            )];
            lines.extend(warnings);
            Ok(lines)
        }
        ExtStep::ExpectPages {
            input,
            count,
            text,
            rotate,
            labels,
        } => {
            let input = input_path(input, base, current)?;
            let pw = password(current, &input);
            let mut file = open_pdf(&input, pw.as_deref())?;
            let doc = file.document()?;
            let n = doc.page_count();
            if let Some(c) = count {
                if *c != n {
                    bail!("expected {c} page{}, found {n}", plural(*c));
                }
            }
            if let Some(rotate) = rotate {
                let found: Vec<i32> = (0..n)
                    .map(|i| doc.page(i).map(|p| p.rotate.degrees()))
                    .collect::<std::result::Result<_, _>>()?;
                check_list("rotations", &found, rotate)?;
            }
            if let Some(labels) = labels {
                let found: Vec<String> = match doc.page_labels() {
                    Some(l) => l.iter().map(|l| l.text()).collect(),
                    None => (1..=n).map(|i| i.to_string()).collect(),
                };
                check_list("labels", &found, labels)?;
            }
            if let Some(text) = text {
                if text.len() != n {
                    bail!("\"text\" has {} entries for {n} pages", text.len());
                }
                let bytes =
                    std::fs::read(&input).with_context(|| format!("read {}", input.display()))?;
                let info = backend.open(DocSource::Bytes(bytes), pw.as_deref())?;
                let result = (|| -> Result<()> {
                    for (i, want) in text.iter().enumerate() {
                        if want.is_empty() {
                            continue;
                        }
                        let got = backend.page_text(info.doc, i as u32)?.text();
                        if !got.contains(want.as_str()) {
                            bail!("page {} does not have the text {want:?}", i + 1);
                        }
                    }
                    Ok(())
                })();
                let _ = backend.close(info.doc);
                result?;
            }
            Ok(vec![format!("{n} page{} as expected", plural(n))])
        }
        ExtStep::ExpectOutline {
            input,
            count,
            titles,
            levels,
            pages,
        } => {
            let input = input_path(input, base, current)?;
            let pw = password(current, &input);
            let mut file = open_pdf(&input, pw.as_deref())?;
            let doc = file.document()?;
            let flat = doc.outline().flatten_with(doc);
            if let Some(c) = count {
                if *c != flat.len() {
                    bail!("expected {c} bookmark{}, found {}", plural(*c), flat.len());
                }
            }
            if let Some(t) = titles {
                let found: Vec<String> = flat.iter().map(|f| f.title.clone()).collect();
                check_list("bookmark titles", &found, t)?;
            }
            if let Some(l) = levels {
                let found: Vec<usize> = flat.iter().map(|f| f.level).collect();
                check_list("bookmark levels", &found, l)?;
            }
            if let Some(p) = pages {
                let found: Vec<Option<u32>> = flat
                    .iter()
                    .map(|f| f.dest.map(|d| d.page_index as u32 + 1))
                    .collect();
                check_list("bookmark pages", &found, p)?;
            }
            Ok(vec![format!(
                "{} bookmark{} as expected",
                flat.len(),
                plural(flat.len())
            )])
        }
        ExtStep::ExpectLinks {
            input,
            page,
            count,
            kinds,
            targets,
        } => {
            let input = input_path(input, base, current)?;
            let pw = password(current, &input);
            let mut file = open_pdf(&input, pw.as_deref())?;
            let doc = file.document()?;
            let rows: Vec<_> = link_rows(doc)?
                .into_iter()
                .filter(|r| page.is_none_or(|p| r.page + 1 == p as usize))
                .collect();
            if let Some(c) = count {
                if *c != rows.len() {
                    bail!("expected {c} link{}, found {}", plural(*c), rows.len());
                }
            }
            if let Some(k) = kinds {
                let found: Vec<String> = rows.iter().map(|r| r.kind.to_string()).collect();
                check_list("link kinds", &found, k)?;
            }
            if let Some(t) = targets {
                let found: Vec<Option<u32>> = rows
                    .iter()
                    .map(|r| r.target.map(|d| d.page_index as u32 + 1))
                    .collect();
                check_list("link targets", &found, t)?;
            }
            Ok(vec![format!(
                "{} link{} as expected",
                rows.len(),
                plural(rows.len())
            )])
        }
        ExtStep::SetCreate { set, name, files } => {
            let path = base.join(set);
            if path.exists() {
                bail!("{} already exists", path.display());
            }
            let mut s = SetFile::new(name.as_deref().unwrap_or_default());
            for f in files {
                s.add(&base.join(f))?;
            }
            s.write(&path)?;
            Ok(vec![format!(
                "{} ({} file{})",
                path.display(),
                s.len(),
                plural(s.len())
            )])
        }
        ExtStep::SetAdd { set, files } => {
            let path = base.join(set);
            let mut s = SetFile::read(&path)?;
            let mut added = 0;
            for f in files {
                added += usize::from(s.add(&base.join(f))?);
            }
            s.write(&path)?;
            Ok(vec![format!(
                "{added} added, {} file{}",
                s.len(),
                plural(s.len())
            )])
        }
        ExtStep::SetList { set, count } => {
            let path = base.join(set);
            let s = SetFile::read(&path)?;
            if let Some(c) = count {
                if *c != s.len() {
                    bail!("expected {c} file{}, found {}", plural(*c), s.len());
                }
            }
            Ok(vec![format!("{} file{}", s.len(), plural(s.len()))])
        }
        ExtStep::SetSearch {
            set,
            text,
            match_case,
            whole_word,
            count,
            expect,
        } => {
            let path = base.join(set);
            let s = SetFile::read(&path)?;
            let options = SearchOptions {
                match_case: *match_case,
                whole_word: *whole_word,
            };
            let found = crate::set::search(session, &s, text, options, None)?;
            if let Some((p, why)) = found.failed.first() {
                bail!("could not search {}: {why}", p.display());
            }
            if let Some(c) = count {
                if *c != found.hits.len() {
                    bail!("expected {c} hit{}, found {}", plural(*c), found.hits.len());
                }
            }
            let pairs: BTreeSet<(String, u32)> = found
                .hits
                .iter()
                .map(|h| (path_key(&h.path), h.page + 1))
                .collect();
            if let Some(expect) = expect {
                let want: BTreeSet<(String, u32)> = expect
                    .iter()
                    .map(|h| (path_key(&base.join(&h.file)), h.page))
                    .collect();
                if want != pairs {
                    bail!("expected hits on {want:?}, found {pairs:?}");
                }
            }
            Ok(vec![format!(
                "{} hit{} on {} page{}",
                found.hits.len(),
                plural(found.hits.len()),
                pairs.len(),
                plural(pairs.len())
            )])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_without_these_steps_is_left_to_pl_doc() {
        assert!(plan(r#"{"steps": [{"op": "open", "path": "a.pdf"}]}"#)
            .unwrap()
            .is_none());
        assert!(plan("not json").unwrap().is_none());
    }

    #[test]
    fn steps_split_into_segments_with_their_numbers() {
        let p = plan(
            r#"{"author": "T", "steps": [
                {"op": "open", "path": "a.pdf"},
                {"op": "save"},
                {"op": "pages", "ops": [{"do": "delete", "pages": "2"}]},
                {"op": "expect", "count": 0},
                {"op": "set_search", "set": "s.plset", "text": "x",
                 "expect": [{"file": "a.pdf", "page": 1}]}
            ]}"#,
        )
        .unwrap()
        .unwrap();
        assert_eq!(p.steps, 5);
        assert_eq!(p.author.as_deref(), Some("T"));
        assert!(matches!(&p.items[0], Item::Doc { first: 1, steps } if steps.len() == 2));
        assert!(matches!(&p.items[1], Item::Ext { number: 3, .. }));
        assert!(matches!(&p.items[2], Item::Doc { first: 4, steps } if steps.len() == 1));
        assert!(matches!(&p.items[3], Item::Ext { number: 5, .. }));

        let e = plan(r#"{"steps": [{"op": "pages", "ops": [{"do": "shred"}]}]}"#).unwrap_err();
        assert!(e.to_string().starts_with("step 1 (pages)"), "{e}");
        let e = plan(r#"{"steps": [{"op": "flatten"}, {"op": "explode"}]}"#).unwrap_err();
        assert!(e.to_string().starts_with("step 2 (explode)"), "{e}");
        let e = plan(r#"{"steps": [{"op": "links"}]}"#).unwrap_err();
        assert!(e.to_string().starts_with("step 1 (links)"), "{e}");
    }

    #[test]
    fn progress_lines_and_errors_get_the_file_step_numbers() {
        struct Keep(Vec<String>);
        impl ScriptHost for Keep {
            fn write_png(
                &mut self,
                _: &Path,
                _: &pl_doc::pl_render::Bitmap,
            ) -> std::io::Result<()> {
                Ok(())
            }
            fn log(&mut self, line: &str) {
                self.0.push(line.into());
            }
        }
        let mut keep = Keep(Vec::new());
        let mut r = Renumber {
            inner: &mut keep,
            first: 5,
            added: 1,
        };
        r.log("1: open a.pdf");
        r.log("2: add box");
        r.log("3: save");
        assert_eq!(
            renumber_error("step 3 (save): failed", &r, None),
            "step 6 (save): failed"
        );
        assert!(
            renumber_error("step 1 (open): gone", &r, Some(Path::new("a.pdf")))
                .starts_with("reopening a.pdf before step 5")
        );
        assert_eq!(keep.0, vec!["5: add box", "6: save"]);
    }
}

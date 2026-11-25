# Plumbline

Plumbline is an original Windows application for marking up PDF drawings and taking
measurements and quantities off them (a "markup and takeoff" tool), written in Rust. It is an
early preview: markup, the markups list, measurement and takeoff work, Bluebeam Revu
measurements, custom columns and statuses are read and written, and pages, bookmarks, links,
flatten, drawing sets and printing are in (milestone M8). Compare and overlay, OCR and an
installer are still to come. The design and milestones are in [docs/plan.md](docs/plan.md).

"Plumbline" is a working codename.

## Download

Get `Plumbline.exe` from the [Releases page](https://github.com/ahubb1/plumbline/releases) and double-click it. It is
a single file with nothing to install: on first run it unpacks the app into
`%LOCALAPPDATA%\Plumbline\app` and starts it. Windows 10 or 11, 64-bit.

The exe is not code-signed yet, so Windows warns about it:

1. Right-click the downloaded file, choose **Properties**, tick **Unblock**, then **OK**.
2. If a blue "Windows protected your PC" box appears, click **More info**, then **Run anyway**.

If Smart App Control is on (Windows 11, Settings > Privacy & security > Windows Security > App &
browser control), it blocks unsigned apps outright and the app cannot run until it is signed.

To remove it, delete `Plumbline.exe` and the `%LOCALAPPDATA%\Plumbline` folder.

The launcher that builds the single exe is in `pack/`: it embeds the release `plumbline.exe`,
`pdfium.dll` and `THIRD-PARTY-LICENSES.txt` from `pack/payload/`. To rebuild it, run
`cargo run -p xtask -- pack`: it does the `dist` release build, stages those three files in
`pack/payload/`, builds `pack/` in release mode and writes `target/dist/Plumbline.exe`,
printing its size and SHA-256.

## Troubleshooting

**"Do you want to allow this app from an unknown publisher to make changes to your device?"**
Version 0.1.0 had no application manifest, so Windows could take it for an installer and ask for
administrator rights. 0.1.1 and later declare that they run as the current user (`asInvoker`)
and should not ask. If you still see the prompt, you have the old file: download it again.
Plumbline never needs administrator rights, so "No" is always a safe answer.

**Nothing opens, or a message says Plumbline could not start.** Every start is written to a log:

```
%LOCALAPPDATA%\Plumbline\logs\plumbline.log
```

Paste that line into the address bar of File Explorer to open it. The log records the app
version, the Windows version, whether the app ran as administrator, which graphics renderer was
tried and why it failed. It holds no document contents. When it reaches 1 MB it moves to
`plumbline.log.1` and a new one starts.

**Choosing a renderer.** Plumbline can draw its window three ways: `wgpu` (Direct3D 12 on the
graphics card; Vulkan is never used), `glow` (OpenGL) and `software` (Direct3D 12's software
renderer, WARP, which is part of Windows: slower, and it does not draw with the graphics
driver; Windows still asks the driver about the card while it lists the Direct3D 12 devices, so
a driver that crashes even then breaks this one too, and only `glow` avoids Direct3D 12
altogether). `Plumbline.exe` tries them in that order, one window at a time: when one crashes or
fails before the first picture is on screen, it starts the app again with the next. The one that
worked is remembered in `%LOCALAPPDATA%\Plumbline\renderer.txt` and tried first next time; delete
that file to start the search again (after a driver update, for example). If none works, one
message gives the exit codes and the log's location. Started directly (the unpacked
`plumbline.exe`), the app tries the same order inside one run, but a crash inside the graphics
driver ends that run.

If the window stays blank, flickers or the app closes later on, an outdated or unusual graphics
driver is the usual cause. Updating it (Settings > Windows Update > Advanced options > Optional
updates, or the PC maker's website) is the best fix; otherwise choose a renderer yourself:

1. Right-click `Plumbline.exe` and choose **Create shortcut** (on Windows 11, it is under
   **Show more options**).
2. Right-click the shortcut, choose **Properties**, and add ` --renderer glow` (or
   ` --renderer software`) to the end of **Target**, after the closing quote, for example
   `"C:\Downloads\Plumbline.exe" --renderer glow`.
3. Start Plumbline from that shortcut.

`--renderer wgpu|glow|software` uses only that renderer, with no fallback and nothing
remembered. Setting the environment variable `PLUMBLINE_RENDERER` to one of them does the same
for every start; the command-line flag wins over it. If the app crashes inside the graphics
driver, the log gets a `native crash` line naming the exception code, the driver file and the
renderer.

**Reporting a problem.** Send `plumbline.log` (and `plumbline.log.1` if it is there), with a
sentence on what you did and what you saw. The log contains folder paths of this PC, which can
include your Windows user name; edit them out if you prefer.

## Layout

One cargo workspace. Library crates live under `crates/` (`pl-geom`, `pl-draw`, `pl-pdf`,
`pl-render`, `pl-scene`, `pl-annot`, `pl-takeoff`, `pl-compare`, `pl-text`, `pl-tools`,
`pl-doc`, `pl-testkit`), with two binaries:

- `pl-app` builds `plumbline.exe`, the desktop app (the only crate that uses egui).
- `pl-cli` builds `plumb.exe`, the command-line tool.

`crates/xtask` holds the build tasks described below. Section 6 of the plan says what each crate
is for. All geometry is in unrotated page user space: PDF points, y up.

## Requirements

- Windows 10 or 11, 64-bit.
- Rust via [rustup](https://rustup.rs). The toolchain version is pinned in
  `rust-toolchain.toml` and is installed automatically.
- MSVC Build Tools (the C++ workload), which Rust on Windows needs for linking.
- `curl.exe` and `tar.exe`, which ship with Windows.

No Node, .NET or admin rights are needed.

## Fetch PDFium

Rendering uses a prebuilt PDFium DLL that is not kept in git. Fetch it once:

```
cargo run -p xtask -- fetch-pdfium
```

This downloads PDFium build 8125 from
[bblanchon/pdfium-binaries](https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium/8125),
checks its SHA-256 against the value pinned in `crates/xtask/src/fetch.rs`, and unpacks it to
`vendor/pdfium/8125`. A download with the wrong hash is deleted and the command fails. If a
valid copy is already there the command does nothing; `--force` unpacks it again.

## Build and test

```
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets
```

To work on one crate, use `-p <crate>`, for example `cargo test -p pl-geom`. All tests are
headless; none opens a window. The PDFium-backed tests need the DLL fetched above and fail,
rather than skip, without it.

Measured figures, the budgets set from them (render time on a dense sheet, lopdf memory on a
large file) and the outcome of the OCR spike are in
[docs/measurements.md](docs/measurements.md). The budgets are asserted by ordinary tests.

`qpdf --check` on saved files runs when `qpdf` is on the search path or `PLUMBLINE_QPDF`
names `qpdf.exe`; without it that one check is reported as skipped.

`pdfium.dll` is loaded by absolute path from beside the executable, or from the path given
with `--pdfium`. Debug builds also accept `PLUMBLINE_PDFIUM_DLL` and the vendored copy, for
development; a release build ignores both.

The synthetic test PDFs can be written to a folder with
`cargo run -p pl-testkit --bin pl-fixtures -- <dir>`.

`plumb annots <pdf>` lists a file's annotations, and `plumb run <commands.json>` runs a JSON
command file (open, add, modify, delete, undo, redo, save, save_as, render): the headless
driver the editing tests use. The format is described at the top of
`crates/pl-doc/src/script.rs`; besides shapes it has steps for text boxes, callouts, stamps,
pictures, text markups over the page's text, review status, replies, checkmarks, locks and
custom columns. `plumb export <pdf> --csv out.csv | --xlsx out.xlsx | --xfdf out.xfdf` writes
the file's markups list as CSV or as an Excel workbook, and its annotations as XFDF.

## Markup editing in the app

`plumbline.exe` draws and edits standard PDF markups. The tool bar on the left has Select
(V), Pan (H), Rectangle (R), Ellipse (E), Line (L), Arrow (A), Polyline (Y), Polygon (G),
Cloud (C), Cloud Box (Q), Pen (P), Highlighter (M), Eraser (X), Note (N), Text Box (T),
Typewriter (F), Callout (O), Cloud Callout (K), Stamp (B), Image (I) and, for the page's own
text, Highlight Text (J), Underline Text (U), Strikeout Text (W) and Squiggly Underline (Z);
S turns snapping to the drawing's own geometry on and off. The properties panel (Ctrl+E) edits the selection's or the active tool's
colours, line width, dash, opacity, line endings, cloud border, author, subject and comment,
and keeps named presets per tool in `presets.json` in the app-data folder. Edit has undo and
redo (Ctrl+Z, Ctrl+Y or Ctrl+Shift+Z), cut, copy, paste, duplicate (Ctrl+D) and "Copy
snapshot" (the view or a dragged region, as an image); Arrange aligns, distributes and
restacks. Save (Ctrl+S) appends an incremental update, Save as writes another file, Save
optimised rewrites the file. A tab with unsaved markups is marked with `*` and asks before it
closes; unsaved markups are journaled under `journal` in the app-data folder and offered back
after a crash. The Layers page of the side panel lists the file's layers (optional-content
groups) with a box each to show or hide it in the window; that is a view setting and never
changes the file (`crates/pl-doc/src/layers.rs`). The single keys (tool letters, Enter, Delete,
the arrows, Space to pan) act on the page only while no other control has keyboard focus.

Text boxes, callouts and typewriter text are typed in a small editor beside the markup, while
the page shows the text as pl-draw lays it out, which is what is saved; Ctrl+Enter or a click
elsewhere keeps the text, Esc throws it away. Text on the page can use the characters of
WinAnsi only for now: anything else is refused as it is typed, with a message that names it.
The properties panel has the font size and face, the alignment, the text, border and fill
colours of a text box, the five built-in stamps (APPROVED, REVISED, FOR REVIEW, VOID, DRAFT)
and "Image as stamp", which places a PNG or JPEG. A double-click on a note opens a small
window with its text, its review status and its replies.

The markups list (Ctrl+L, or its button under the tools) is docked at the bottom: one row per
markup, replies as child rows. Click a heading to sort (Shift adds a key), drag its edge to
resize, use Columns to show, hide and reorder, "Group by" for collapsible groups with counts
and subtotals, the search box and the filter row to filter. Status, checkmark, comment and
the user's own columns (Text, Number, Choice, Checkmark, Date; defined under Columns, Custom
columns) are edited in the rows, each as one undo step. Selecting rows selects the markups on
the page and the other way round. Export writes the list as CSV or XLSX and the markups as
XFDF; XFDF can be imported (a markup the document cannot take, such as a text box with
characters outside Western European, is left out and counted; the rest come in as one undo
step). The list's layout and the custom columns are kept in `markups_list.json` in the
app-data folder.

A PDF with Bluebeam Revu custom columns (`/BSIAnnotColumns`) shows them in the list as soon as
it is open, after the user's own columns, and its values are read from and written to each
markup's `/BSIColumnData`, position by position, so Revu sees the same values. Revu's custom
status sets are not read from their definitions: only the states of custom sets that the
file's status replies already use appear in the Status drop-down, and `/BSIStatus` is not
written. The Custom columns dialog can add a column to the
PDF's own Bluebeam columns. Details, and what is still unconfirmed, are in
[docs/interop-notes.md](docs/interop-notes.md).

Two keys of a text box are written the way other applications read them, which is not
quite what ISO 32000 says. `/DA` carries the *border* colour as its non-stroking colour
(Acrobat redraws the border from it and takes the text colour from `/DS`), and `/RD` is
left, bottom, right, top (the order PDFBox, MuPDF and Poppler use; the standard words it
"left, top, right, bottom"). The `/RD` order has not been checked against a callout saved
by Acrobat or Bluebeam: it is on the owner's interop checklist.

Saving, the first read of a document's markups and the reading of its layers run on worker
threads: the window keeps drawing, the status bar shows a small mark, and while a document is
being saved it cannot be edited. The markups are read when something first needs them (the
pointer over the page with a markup tool, or the list).

The code is in `crates/pl-app/src`: `markup.rs` (tools, commands), `paint.rs`
(the DrawList-to-egui painter), `overlay.rs` (Multiply compositing, selection handles, the
snapshot image), `panels.rs` (tool bar, properties, dialogs), `textedit.rs` (the text editor
and the note window), `list.rs` (the markups list) and `jobs.rs` (saves and loads on worker
threads).

Windows builds link the C runtime statically (`.cargo/config.toml`), so the exes do not need
the Visual C++ redistributable.

## Measurement and takeoff in the app

The Measure group of the tool bar has Calibrate, Viewport, Length, Polylength, Perimeter, Area,
Cutout, Volume, Angle, Radius, Diameter, Count and Space, each on Shift and a letter (Shift+K,
W, L, P, R, A, U, V, G, D, M, C, O; the tooltips say which). Measurements are standard PDF
measurement annotations (`/IT`, `/Measure`, page `/VP`), with the value in `/Contents` and in
the appearance.

- **Scale.** The status bar shows the scale under the pointer (a viewport's name when its own
  scale applies there) or **Scale not set**; a click on it, or Measure > Scale, opens the scale
  panel: architectural, engineering and metric presets, a typed scale (`1/4 in = 1 ft`,
  `1:100`), the pages to apply it to, and the units and precision values are shown in.
  Calibrate picks two points (snapped) and asks for their real distance in a dialog, for the
  page or the viewport they are in. Length and the other scaled tools refuse a page with no
  scale and open the panel instead of writing a value at the wrong scale.
- **While measuring.** With snapping on (S), the pointer snaps to the drawing's endpoints,
  midpoints, intersections (and, if asked, the nearest point on a line) and to the points of
  markups and measurements; each kind has its own marker. Right-click the snap button, or
  Measure > Snap to, to choose the kinds. A read-out beside the pointer gives the segment,
  running total, area, angle or count; Shift keeps a segment to angle steps.
- **Properties.** A selected measurement's units, precision, caption, depth (an area given a
  depth becomes a volume), wall height (the list's Wall Area is length times height) and count
  symbol, besides its colour and line endings; its vertices, cutout corners, dimension-line
  offset and value are dragged with the Select tool.
- **Markups list.** The quantity columns (Count, Length, Perimeter, Area, Volume, Wall Area,
  Angle, Radius, Depth, Scale, Unit, Space) have their unit in the heading; Columns > Takeoff
  layout shows them grouped by subject with subtotals and a total; Columns > Formula column
  adds a column worked out per row (`[Length] * [Rate]`), with the parser's message while the
  formula is wrong; a choice column's options may stand for numbers (`Steel=12.5`). Export
  writes them as shown.
- **Legends, spaces, numbering.** Measure > Insert legend places a legend (counts, lengths,
  areas or volumes by subject) as a text box whose text is worked out again, in the same undo
  step, whenever the markups change. The Space tool draws a named region (rename it under
  Properties); the list groups by Space. The Count tool can number each new count from a
  sequence kept in the document (`C-01`, `C-02`, in the list's Number column).

`plumb measure <pdf>` lists a file's scales, viewports and measurements, and `plumb export
<pdf> --takeoff [--units imperial|metric] --csv out.csv` writes the takeoff layout of the list
(`pl_takeoff::Layout::takeoff`): the adding columns (Count, Length, Perimeter, Area, Volume,
Base Area, Wall Area) and the ones that do not add up (Angle, Radius, Diameter), a
`Subtotal: <subject>` record after each subject and a `Total` record at the end, as in the
workbook. A Volume's plan area is in Base Area and never in Area, and a Diameter is in
Diameter (and Radius) and never in Length, so the Area and Length totals hold only areas and
runs. A count is one row with its item total, however many annotations carry its items (Revu
writes one per item). Numbers in CSV and XLSX are rounded to the column's places plus six,
which drops float noise; the list and its formulas keep full precision.
The code is in `crates/pl-app/src/measure.rs`.

## Pages, bookmarks, links, sets and printing in the app

Changes to a document's structure (its pages, bookmarks and links, and flatten) are each one
undo step, like a markup edit, and are kept in memory until the document is saved. Saving
such a document rewrites the whole file (written to a temporary file, checked with lopdf,
hayro-syntax and PDFium, then put in place of the original); a document with only markup
changes is still saved by appending. Unsaved markups are written into the structure before a
structural step, so a markup moves with its page. A document with digital signatures asks
first: the step goes ahead only when the user agrees that the signatures will no longer be
valid once it is saved. The model behind all of this is pl-doc's
(`crates/pl-doc/src/structure.rs`).

- **Pages.** The Pages page of the side panel has a bar for Insert blank page (the size of its
  neighbour), Rotate (this changes the file; the view's own rotation is under View), Delete
  and More (insert pages from another PDF, replace pages with the first pages of another PDF,
  extract pages to a new file, Combine files); a thumbnail's right-click menu has the same
  operations, and so does Edit > Pages. A click on a thumbnail shows that page, Ctrl+click
  adds or removes a page, Shift+click picks a run; the operations act on the picked pages, or
  on the page in view. Dragging thumbnails moves the pages, with a line where they will go.
  Delete asks first, in the window. File > Combine files makes a new PDF from a list of files
  (reordered with the arrows), each with all its pages or a range such as `1-3, 5`, and opens
  it.
- **Bookmarks.** The Bookmarks page is editable: + adds a bookmark for the place in view
  (after the chosen bookmark) and starts renaming it; a double-click or Rename renames in
  place (Enter keeps it, Esc cancels); Set to view points the chosen bookmark at the place in
  view; the cross deletes it. Dragging a bookmark onto the upper or lower edge of another puts
  it before or after that one, onto its middle puts it inside. A click goes to the bookmark.
- **Links.** The pointer over a link says where it leads; a click with the Pan tool (H), or a
  double-click with Select, follows it. A link within the document moves the view, and Back
  and Forward (Alt+Left, Alt+Right, also in the View menu) return; a link to another PDF opens
  it in a tab at its page; a web address opens in the browser only after a yes in the window;
  a link that would start a program or open another kind of file is refused with a message.
  The Link tool (D) drags out the area of a new link and then asks where it leads: a page of
  this document, a page of another PDF (named relative to this document's folder), or a web
  address. Links are selected, moved and deleted with Select; the properties panel shows where
  a selected link leads, with Change target and Follow.
- **Flatten.** Edit > Flatten markups draws all markups, the selected ones or those on given
  pages into the page content (form fields too, if asked), so they look the same but are no
  longer markups. The dialog says so: Undo takes it back until the document is saved, after
  that it is permanent.
- **Drawing sets.** A set (`.plset`, a small JSON file kept beside the drawings, with paths
  relative to its folder) lists PDF files and a sheet per page; sheet numbers come from the
  page labels, or the file name. File > Open set opens one, File > New set from open files
  makes one of the open documents. The Set page of the side panel lists the sheets by file; a
  click opens the sheet (in the tab its file already has, or a new one), and Alt+Page Down and
  Alt+Page Up go to the next and previous sheet. Its search box searches every file of the set
  on a worker thread and lists the matches by file and page; a click shows the match. A file
  that is not where the set says is marked missing, and Find points the set at its new place.
- **Printing.** File > Print (Ctrl+P) opens Plumbline's own print panel (not the Windows
  dialog): the printer, all pages, the page in view or a range, fit to paper or actual size,
  markups or not, and copies; each page's orientation is chosen for it (a wide sheet turns
  onto portrait paper when it prints larger that way), and a small picture shows where the
  first page lands. The pages are drawn by PDFium in bands at the printer's resolution and
  sent through GDI on a worker thread, with progress and Cancel. What prints is the document
  as it stands, unsaved changes included. The layout and the banded rendering are in
  `crates/pl-render/src/print.rs` (tested against an in-memory device); the Windows printer
  is `crates/pl-app/src/print_win32.rs`, which only the window's start-up gives the app, so
  no test can reach a real printer.

The code is in `crates/pl-app/src`: `pages.rs`, `bookmarks.rs`, `links.rs`, `structure.rs`
(the confirmations and the flatten dialog), `setpanel.rs`, `print.rs` and `print_win32.rs`.

From the command line, `plumb pages <pdf>` lists the pages (label, size, rotation, user unit,
annotations and a content fingerprint) and `plumb pages <pdf> <op...> -o <out>` runs page
operations in order (`insert-blank`, `insert-from`, `delete`, `move`, `rotate`, `extract`,
`replace`, `merge`, and `split` last); `plumb outline` lists and edits bookmarks (edits are
appended, so signatures stay valid), `plumb links` lists links without following any, `plumb
flatten` flattens, and `plumb set create|add|list|search` works on the same `.plset` files as
the app. A signed file is refused for a rewrite unless `--invalidate-signatures` is given.
`plumb --help` has the details; the command-file steps are described at the top of
`crates/pl-cli/src/script_ext.rs`.

## Build tasks

Run as `cargo run -p xtask -- <command>`:

| Command | What it does |
|---|---|
| `fetch-pdfium` | Downloads, verifies and unpacks PDFium (above). |
| `check-dll [file]` | Checks that a DLL imports only Windows system DLLs. Defaults to the vendored `pdfium.dll`. Reads the import table itself and, when the MSVC tools are installed, confirms the result with `dumpbin /dependents`. |
| `dist` | Builds `plumbline.exe` and `plumb.exe` in release mode and assembles the portable folder `target/dist/plumbline/` with the exes, `pdfium.dll`, fonts and licence files. `--zip` also writes `target/dist/plumbline.zip`, then unpacks it into an empty temporary folder and runs its `plumb.exe info` there with an empty environment, failing if that does not work; `--no-build` packages what is already built. The release build remaps the builder's folders (`--remap-path-prefix` for the home folder, `CARGO_HOME`, `RUSTUP_HOME` and the workspace, keeping the static CRT), and the exes are then scanned for the home-folder path; the build fails if it is found. |
| `pack` | Runs `dist`'s release build, copies `plumbline.exe`, `pdfium.dll` and `THIRD-PARTY-LICENSES.txt` into `pack/payload/`, builds the launcher in `pack/` in release mode with the same remapping, checks it the same way, and copies it to `target/dist/Plumbline.exe`, printing its size and SHA-256. |
| `licenses` | Lists the declared licence of every crate linked into the shipped programs (dev- and build-dependencies are left out). `--out <file>` writes the list to a file. |

## Licence policy

Only permissive licences are accepted for dependencies. The rules are in `deny.toml` and are
checked with [cargo-deny](https://github.com/EmbarkStudios/cargo-deny):

```
cargo install cargo-deny --locked
cargo deny check licenses bans sources
```

One exception is open for the owner to confirm: `colored` (MPL-2.0) is allowed by name in
`deny.toml`. It is pulled in only by the snapshot-test harness (`egui_kittest`), is not linked
into `plumbline.exe` or `plumb.exe`, and is left out of `THIRD-PARTY-LICENSES.txt` and the
zip (an `xtask` test checks that no MPL-2.0 crate is in the shipped listing).

Bundled third-party files: PDFium (its notices are in `vendor/pdfium/8125/licenses`) and the
Liberation Sans fonts in `vendor/fonts` (SIL Open Font License 1.1, see `vendor/fonts/LICENSE`).

`THIRD-PARTY-LICENSES.txt`, written by `dist` and embedded by `pack`, carries the full texts in
one file: the crate listing; the licence and notice files each shipped crate's package carries
(one copy of each distinct text, with the crates that share it), or for a crate whose package
has none, the standard SPDX text of its licence with its authors and repository (a shipped
crate with no text at all fails the build); the Liberation fonts' OFL; and PDFium's licence
with the notices of the libraries it bundles.

Plumbline's own code is licensed `MIT OR Apache-2.0`.

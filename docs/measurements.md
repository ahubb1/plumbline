# Measurements, budgets and spike outcomes

This file records what the plan (docs/plan.md, section 8) asks to be measured, the budgets
set from those measurements, and where each budget is asserted. Figures are from the
development PC on 2026-11-24 with PDFium 8125; they are not a claim about other machines.
Every budget has several times its measurement as headroom, so a test fails on a real
regression and not because the machine was busy.

## 1. Dense sheet at 400% (M1; gates the worker pool)

Criterion: "A 200k-segment sheet at 400% meets the M0 budget; this gates the worker pool."

Fixture: `pl_testkit::dense_sheet()`, 200,000 stroked segments on a 36 x 24 inch sheet, in
a 1920 x 1080 viewport centred on the sheet, cold (file opened inside the measurement).

| What | Measured (release) | Budget | Constant |
|---|---|---|---|
| Open to first visible tile | 95 to 105 ms | 500 ms | `pl_render::budget::DENSE_FIRST_TILE` |
| Open to all visible tiles, 100% scaling (35 tiles) | 180 to 190 ms | 1000 ms | `DENSE_VIEWPORT` |
| Open to all visible tiles, 200% scaling (104 tiles) | about 295 ms | 1500 ms | `DENSE_VIEWPORT_HIDPI` |

Detail from the same run: PDFium parses the page once in about 100 ms; after that a tile at
this zoom takes 2.1 to 4.0 ms (mean 2.4 ms). A 256 px thumbnail of the whole sheet takes
about 155 ms and a fit-width render (1920 x 1280) about 170 ms, because both draw all
200,000 segments.

Asserted by `dense_sheet_at_400_percent_meets_the_budget` in
`crates/pl-render/tests/bench_dense.rs`, which runs with every `cargo test` (best of three
cold runs; PDFium is the same optimised DLL in debug and release test builds). The detailed
figures come from the ignored benchmark in the same file:

```
cargo test -p pl-render --release --test bench_dense -- --ignored --nocapture
```

**Decision: the budget is met about five times over on the single PDFium thread, so the
`render-worker.exe` process pool is not built.** The gate stays in the test suite; if it
ever fails, the pool is the planned response.

## 2. lopdf on a large file (M0)

Criterion: "lopdf parse time and peak memory on the big fixture are recorded and a budget is
set."

Fixture: the scan set, `pl_testkit::write_scan_set` (Letter pages at 300 dpi, one
incompressible 8-bit image each, streamed to a temporary file, never committed). The "300 MB
set" is 38 pages, 319,789,952 bytes (305 MiB).

```
cargo run --release -p pl-pdf --example measure -- scan <dir> 300
cargo run --release -p pl-pdf --example measure -- parse <dir>\scan_set_300mb.pdf
```

| File | Size | Read | Parse | Peak working set after load | Verified incremental save | Peak after save |
|---|---|---|---|---|---|---|
| Scan set, 38 pages | 305 MiB | 63 ms | 14 ms | 616 MiB (2.0 x file) | 324 ms | 922 MiB (3.0 x file) |
| 5,000 vector pages | 42.6 MiB | 9 ms | 20 ms | 120 MiB | 97 ms | 163 MiB |
| 500 vector pages | 4.3 MiB | 1 ms | 4 ms | 19 MiB | 18 ms | 25 MiB |
| Dense sheet, 200k segments | 1.3 MiB | 0.4 ms | 0.8 ms | 8 MiB | 10 ms | 11 MiB |

lopdf holds the file's bytes and a copy of every stream's data, so a load costs twice the
file in memory; a verified save holds the loaded state, the file read back and its re-parse,
three times the file. Time is not the constraint; memory is.

Budget (`pl_pdf::budget`), for a file of F bytes:

| What | Budget |
|---|---|
| Peak heap while loaded | 32 MiB + 2.5 x F |
| Peak heap during a verified incremental save | 32 MiB + 3.5 x F |
| Read plus parse | 500 ms + 500 ms per 100 MiB |
| Verified incremental save | 500 ms + 1500 ms per 100 MiB |

Asserted by `lopdf_on_a_scan_set_stays_inside_the_budget` in
`crates/pl-pdf/tests/budget.rs` on a 48 MiB scan set generated on the spot, with a counting
allocator (measured there: 2.01 x and 3.01 x the file). The full 300 MB set is measured by
hand with the commands above, because writing 300 MB on every test run is not worth it.

**Consequence for the design:** a lopdf parse must never happen at open. It does not:

- Opening a document asks PDFium for the page sizes only. No page is loaded and the file is
  mapped, not read. `crates/pl-doc/tests/memory.rs` opens a 48 MiB scan set whose pages carry
  `/UserUnit` and measures 0.03 MiB of heap for the open, the `/UserUnit` probe included.
- `/UserUnit`, which PDFium cannot report, is read by hayro-syntax straight from the mapped
  file on a worker thread, object streams included. lopdf is only the fallback for a file
  hayro-syntax cannot read (an encrypted one) that also has `/UserUnit` in the clear.

## 3. Scene build caps (M0, M6)

`pl_testkit::dense_sheet()` builds its 200,000-segment scene in about 260 ms (interpret
42 ms, index 217 ms), inside the 4 s time cap; asserted in `crates/pl-scene/tests/scene.rs`.

Curve flattening is inside the caps. `crates/pl-scene/tests/caps.rs` runs a one-curve page
under a `cm` matrix of up to 1e12 with limits of 1,000 segments, 1 MiB and 50 ms, and
measures 5 KiB of heap and under 0.1 ms; before the fix the same input used 262 MiB and
six times the time cap.

## 4. OCR spike (M0)

Criterion: "OCR spike from an unpackaged exe passes or records the failure."

`Windows.Media.Ocr` is documented by Microsoft as supported only for apps with package
identity. The spike calls it from a plain test executable
(`crates/pl-text/tests/ocr_spike.rs`), through the `pl_text::OcrEngine` trait, on the
`plain` fixture rendered by PDFium at 200 dpi.

Outcome on the development PC (Windows 11 Pro 10.0.26300, OCR language pack `en-US`):
**passed.** The engine was created without package identity (`max image side` 10,000 px)
and recognised 16 of the page's 19 known words, all 16 at their place on the page. The three
it missed ("Plumbline", "quick", "0123456789") each lost one character to a drawn line that
crosses the text on that fixture.

What this does and does not settle, for the owner's OCR-engine decision (plan, open
decision 3):

- It works unpackaged on this machine and this Windows build. That is evidence, not a
  guarantee: Microsoft does not support it, so a Windows update could withdraw it.
- It needs an OCR language pack for one of the user's profile languages. A PC without one
  gets `OcrError::NoLanguage`, which the spike records instead of failing; the friend's PC
  has not been checked.
- A bundled engine (`ocrs` or Tesseract) can go behind the same trait if either point
  becomes a problem. Neither has been tried.

## 5. Save verification readers (M0)

Criterion: "Incremental save on each fixture keeps the original bytes as an exact prefix and
reopens in PDFium, hayro-syntax and `qpdf --check`."

| Reader | Where | Status |
|---|---|---|
| Exact prefix, lopdf, hayro-syntax | `crates/pl-pdf/tests/save.rs`, all 17 fixtures | Passing. A file lopdf can only read by repairing its cross-reference data is rejected. |
| PDFium | `crates/pl-doc/tests/review.rs`, all 17 fixtures, through `Document::save_incremental` | Passing. Page count, annotation counts and renders of untouched pages are checked on the temporary file before it replaces the original. |
| `qpdf --check` | Same test, when `qpdf` is on the search path or `PLUMBLINE_QPDF` names it | **Not run: qpdf is not installed on the development PC.** The test prints `QPDF SKIPPED` with the reason. Installing qpdf 12 (plan section 10, item 5) turns the check on with no code change. |

The fixtures include a linearised file (`pl_testkit::linearized()`). It has the linearised
layout and correct `/L`, `/O`, `/E`, `/N` and `/T`, and lopdf recognises it as linearised,
but its hint stream is a zero-filled placeholder: it is a structural stand-in, not the
output of a real lineariser. An owner-supplied linearised drawing would be a better test.

## 6. Save and first annotation load off the UI thread (M4)

A save parses the file with lopdf, writes it, reads it back with three parsers and reopens the
page display; the first load of the annotations parses it too. Both now also run on a worker
thread (`Document::start_save`, `Document::start_load_annots`; the blocking calls remain for
the command line and tests) and are picked up by a per-frame poll that never waits.

| What | Measured (debug test build) | Where |
|---|---|---|
| Real sample, 12 sheets, 14.6 MB: first annotation load on a worker | 15 ms; longest frame meanwhile 21 us | `the_real_sample_loads_and_saves_in_the_background_without_holding_a_frame_up` |
| Same file: verified incremental save-as on a worker | 571 ms; 384 frames ran meanwhile, longest 26 us | same test |
| `annotations` fixture behind a backend slowed to 150 ms per open, close and thumbnail: save | 610 ms; 230 frames ran meanwhile, longest 0.6 ms | `the_frame_loop_keeps_running_while_a_slow_save_is_in_flight` |
| Same, with an injected failure before the replace | 611 ms; 229 frames, longest 0.4 ms | `a_failed_background_save_leaves_everything_as_it_was` |

Budget: a frame of the polling loop takes under 100 ms while a save or load is in flight
(`FRAME_BUDGET` in `crates/pl-doc/tests/background.rs`, well under one slowed backend
request, so a frame that waited for the backend or the worker even once fails), and the
frame loop makes no backend call of its own during a save. The 571 ms is what the same save
costs the window when it is run with the blocking call.

## 7. The markups list with 50,000 rows (M5)

Criterion: "50k-row sort/group/filter is correct and within budget." The engine is measured
in pl-takeoff's own tests; this is the list in the window, on a mock document of 100 pages
with 50,000 rectangles.

| What | Measured (debug test build) | Where |
|---|---|---|
| A frame with the list open and nothing changed | under 1 ms; 24 cells laid out for the 8 rows on screen | `fifty_thousand_rows_cost_a_frame_no_more_than_a_screenful` in `crates/pl-app/tests/list.rs` |
| Changing the sort: rows built again, sorted, and one frame | 112 ms (72 ms in a release build) | same test |
| Opening the list on the document: first frames | 123 ms | same test |

Budget: a frame of the open list takes under 150 ms and lays out no more rows than fit the
panel, whatever the length of the list; the rows are built again only when the annotations,
the layout or the custom columns change (the test counts the builds). Sorting 50,000 rows
takes under 5 s.

Not part of the list: adding the 50,000 markups to the document through one
`AddAnnotations` command took 17.8 s in that test, which points at something quadratic in
the annotation store. It is recorded here because it was seen, not because it is budgeted.

## 8. Takeoff recalculation with formula columns (M7)

Criterion: "50,000 rows with 5 formula columns recalculates within a generous asserted
budget." Fixture: 50,000 Length measurements, each with a choice column whose options carry
numbers and a number column, and five chained formula columns (declared out of dependency
order, using `if`, `min`, `max`, `abs`, `ceil` and `round`).

| What | Measured | Budget | Where |
|---|---|---|---|
| Build the table: rows, quantities from the measurements, spaces, all formulas | 160 ms (debug test build), 260 ms (release) | 20 s | `fifty_thousand_rows_with_five_formulas_recalculate_within_budget` in `crates/pl-takeoff/tests/takeoff.rs` |
| Recalculate the five formula columns alone | 40 ms (debug test build), 67 ms (release) | 5 s | same test |

The release figure being the slower one is run-to-run noise on a busy machine; both are far
inside the budget. The test prints its timings (`TIMING`) with `--nocapture`.

# Corrected final recommendation: Rust Bluebeam-class PDF app (working codename "Plumbline")

The stack survives review, but six claims were wrong or overstated and the milestone plan was too optimistic; all are corrected below and itemised in "Changes made by review". Nothing was written to disk. Every crate version and licence was read from the crates.io API on 2026-11-24. API claims come from docs.rs and PDFium source; none of the code was run.

## 1. Verdict on feasibility

**Realistic:** a staged, original Windows app covering the daily-use core of all four areas, in Rust, with no Node or .NET. A usable viewer, markup tool, markups list and measurement tool lands at M6 (was M5; two overloaded milestones were split).

**Not realistic:**
- Full Revu parity.
- Reading Bluebeam's private data (counts, custom columns, spaces, `.btx`/`.bax`).
- Flood-fill takeoff, symbol search, in-place editing of page text, 3D, plugins, JavaScript and dynamic stamps, AI features.
- Agent-verified Bluebeam/Acrobat interop. Agents can only test against PDFium, hayro and qpdf; an owner checklist stays.
- A guarantee that Bluebeam treats our measurements as live measurements. Whether it reads the standard `/Measure` dictionary or only its own private keys could not be verified; expect them to open as ordinary markups with the value in the caption until the owner checks.

**Licences:** code dependencies are MIT, Apache-2.0 or BSD. Two exceptions need allow-list entries: fonts under OFL-1.1, and egui's default fonts under `Ubuntu-font-1.0` unless that feature is turned off (it is, see section 3).

**Size:** multi-month even with agents. This is engineering judgement, not a measured figure.

## 2. Winner and grafts

All three plans chose the same stack, so the decision is about structure and ordering.

| Criterion | mvp-first | fidelity-first | agent-buildable |
|---|---|---|---|
| Time to daily tool | Best | Worst | Good |
| Performance ceiling | Medium | Best | Good |
| Interop | Good | Best | Good |
| Licence safety | Clean | Clean | Clean, gated in tests |
| Agent testability | Good | Good | Best |
| Dependency risk | Lowest | Highest | Medium |

**Winner: agent-buildable.** It supplies strict crate boundaries, `DrawList` with a CPU rasteriser as the appearance oracle, pure-function tools, a `RenderBackend` trait and a `cargo deny` gate.

**Grafted from mvp-first:**
- UI and CLI from M1, portable zip from M3.
- Single-style text first, using standard-14 Helvetica (WinAnsi characters only) with Liberation Sans metrics.
- Lazy lopdf parse on first edit.
- No lyon or wgpu callback pipeline in v1.
- The "render view" (in-memory append), now kept only for layer overrides and as a fallback; see section 7.

**Grafted from fidelity-first:**
- `render-worker.exe` process pool, built only if the M1 benchmark fails. `RenderBackend` requests and replies are plain serialisable data from M1, so moving out of process is a transport change.
- Decoded image pyramids for scanned sheets.
- Existing signatures preserved by incremental save, with a warning before any full rewrite.
- Overlay export as optional-content layers.
- Property test of page operations against a reference list model.
- `dumpbin /dependents` packaging check.

## 3. GUI stack

**Chosen:** eframe/egui 0.37.0 + egui_tiles 0.18.0 + egui_table 0.11.0 + egui_kittest 0.37.0 (`snapshot`, `wgpu`) + rfd 0.18.0 + arboard 3.7.0 (image clipboard).

**eframe features:** take `wgpu` and `accesskit` (both confirmed as defaults) but disable `default_fonts`. `epaint_default_fonts` is licensed `(MIT OR Apache-2.0) AND OFL-1.1 AND Ubuntu-font-1.0`, so ship one OFL UI font instead and keep the allow-list short. Also compile the `glow` feature as a fallback renderer for PCs where wgpu fails to start; that fallback is unverified.

**Reasons:**
- Best headless test surface of any Rust toolkit. egui_kittest drives input through AccessKit, takes snapshots without a window, and fails a test when an input widget has no accessible name.
- One language.
- Page tiles as textures plus Painter overlays suit a canvas app.

**Accepted costs:**
- Breaking releases every few weeks (0.37.0 is dated 2026-10-15), so pin the set with `=`.
- Text editing inside the canvas is ours to build. egui's `TextEdit` cannot be rotated or zoomed with exact PDF metrics, so v1 edits FreeText in an upright screen-space editor and lays out with our own code on commit.
- egui has no blend modes, so Multiply highlights cannot be Painter shapes (section 7).
- egui_tiles has no drag-out. eframe does support several native windows (`show_viewport_deferred`), so "open tab in a second window" is feasible in M12. It is not testable in kittest as far as I could tell.
- Snapshot goldens depend on the GPU adapter. Keep them few and prefer geometry and `DrawList` goldens.
- Printing goes through Win32.

**Rejected:**

| Stack | Reason |
|---|---|
| gpui | Upstream uncertainty, weak Windows pixel tests |
| iced 0.14.0 | No accessibility tree, no docking, last release 2025-12-07 (first fallback) |
| Slint 1.19.0 | `GPL-3.0-only OR` royalty-free `OR` commercial; no docking |
| Tauri / Dioxus | JS and Node |
| Xilem | Alpha |
| Makepad | Stale |
| Raw Win32 | Too much hand-written UI |

## 4. PDF engine

Three engines with one job each. Only lopdf writes files.

1. **PDFium via pdfium-render 0.9.6** (`pdfium_8125`, `thread_safe`, `image_025`). Jobs: tiles, thumbnails, form-field display, text with character boxes, search, appearances of untouched annotations, and the test oracle.
   - It runs on one dedicated thread. `thread_safe` is only a mutex; it gives no parallelism.
   - Its in-memory document is a "render mirror" that we mutate but never save.
   - 8125 is the newest binding 0.9.6 pins, although pdfium-binaries is already at 8179.
2. **lopdf 0.47.0**: the only writer. Jobs: incremental save, annotation model, appearance streams, page surgery, flatten, OCR text layer, redaction.
3. **hayro-interpret / hayro-syntax 0.9.0**: geometry scene for snapping and vector diff, and the independent post-save parser. hayro as a second renderer is a benchmark curiosity only, not a mitigation. Its documented limits:
   - The `hayro` crate docs say no effort has yet gone into performance and list blending and isolation as missing.
   - Its README lists timeouts and out-of-memory on hostile input as open work.
   - hayro-syntax does not read password-protected files.
   - Scene builds therefore run on a worker thread with time and memory caps, and snapping degrades to "no content snap" on failure.

Helpers: pdf-writer 0.15.0 (content-stream bytes), krilla 0.8.2 (new report PDFs only), subsetter 0.2.6 (M10).

**OCR:** `Windows.Media.Ocr` via windows 0.62.2, behind a trait. Microsoft documents this namespace as supported only for apps with package identity (MSIX). Unpackaged use is widely reported to work, but it is unsupported. The fallback behind the same trait is `ocrs` 0.14.0 (MIT OR Apache-2.0, pure Rust; its model files' licence is not verified) or Tesseract (Apache-2.0).

**Rejected:**
- MuPDF: the `mupdf` crate is AGPL-3.0; this stays an escape hatch needing an explicit owner decision.
- Poppler: GPL.
- pdf-rs: adds nothing.
- PDFium's own authoring and save APIs. Its header lists creatable subtypes as circle, fileattachment, freetext, highlight, ink, link, popup, square, squiggly, stamp, strikeout, text and underline. Line, PolyLine and Polygon are absent, and there is a number getter but no number setter. Its incremental save was reported broken for years and may since be fixed; that no longer matters to the decision.

**Facts checked:**
- **lopdf incremental save.** `IncrementalDocument::save_to` uses the previous revision's `cross_reference_type`. It refuses only a still-encrypted previous revision. When the file was opened with its password, it re-encrypts appended objects and restores `/Encrypt`. This is a source read, not a run. https://docs.rs/lopdf/0.47.0/lopdf/struct.IncrementalDocument.html
- **hayro Device.** It receives `BezPath` plus `DrawProps { transform: Affine, .. }`; text arrives as glyph runs. `RenderContent` has separate `PAGE` and `ANNOTATIONS` flags. https://docs.rs/hayro-interpret/0.9.0/hayro_interpret/trait.Device.html
- **PDFium per-annotation control.** `FPDFAnnot_SetFlags`, `FPDFAnnot_SetRect` and `FPDFPage_RemoveAnnot` exist. pdfium-render exposes `set_is_hidden`, `set_bounds` and `delete_annotation`. https://raw.githubusercontent.com/chromium/pdfium/main/public/fpdf_annot.h
- **PDFium matrix rendering.** It does draw annotations when `FPDF_ANNOT` is set (`RenderPageImpl` in `cpdfsdk_renderpage.cpp`). `FPDF_FFLDraw`, which paints form fields, has no matrix variant. https://groups.google.com/g/pdfium/c/VyI2mebQin0
- **PDFium layers.** I found no public API to switch optional-content groups in the six public headers I searched. The renderer reads `/OCProperties /D` (`BaseState`, `ON`, `OFF`).
- **PDFium 8125 asset.** `pdfium-win-x64.tgz` is 3,904,987 bytes, SHA-256 `9a964261760cff52570aabf038ab346d3a5034dd8b30d0d5fe8861235d356b8b`, confirmed from the GitHub release API. Only DLL builds are published. https://github.com/bblanchon/pdfium-binaries/releases/tag/chromium/8125

## 5. Key crates

All rows read from crates.io on 2026-11-24.

| Crate | Version | Licence |
|---|---|---|
| eframe / egui / egui_kittest | 0.37.0 | MIT OR Apache-2.0 (MSRV 1.95) |
| egui_tiles / egui_table | 0.18.0 / 0.11.0 | MIT OR Apache-2.0 |
| rfd / arboard | 0.18.0 / 3.7.0 | MIT / MIT OR Apache-2.0 |
| pdfium-render | 0.9.6 (2026-11-21) | MIT OR Apache-2.0 |
| lopdf | 0.47.0 (2026-11-04) | MIT |
| hayro-interpret / hayro-syntax | 0.9.0 (2026-11-02) | Apache-2.0 OR MIT |
| pdf-writer / krilla / subsetter | 0.15.0 / 0.8.2 / 0.2.6 | MIT OR Apache-2.0 |
| tiny-skia | 0.12.0 | BSD-3-Clause |
| parley | 0.13.0 | Apache-2.0 OR MIT |
| kurbo / rstar / i_overlay | 0.13.1 / 0.14.0 / 9.2.0 | MIT and/or Apache-2.0 |
| image / imageproc / fast_image_resize | 0.25.10 / 0.27.0 / 6.2.0 | MIT OR Apache-2.0 / MIT / MIT OR Apache-2.0 |
| quick_cache / ttf-parser | 0.7.0 / 0.25.1 | MIT / MIT OR Apache-2.0 |
| rust_xlsxwriter / csv | 0.104.0 / 1.4.0 | MIT OR Apache-2.0 / Unlicense OR MIT |
| windows | 0.62.2 | MIT OR Apache-2.0 |
| serde / thiserror / anyhow / clap | 1.0.229 / 2.0.21 / 1.0.105 / 4.6.9 | MIT OR Apache-2.0 |
| rayon / crc32fast / sha2 / flate2 / tempfile | 1.12.0 / 1.5.5 / 0.11.1 / 1.1.11 / 3.27.0 | MIT OR Apache-2.0 |
| insta / proptest | 1.49.0 / 1.12.0 | Apache-2.0 / MIT OR Apache-2.0 |
| cargo-deny / cargo-nextest / cargo-about (tools) | 0.22.0 / 0.9.152 / 0.9.3 | MIT and/or Apache-2.0 |
| cargo-packager (tool) | 0.11.8 (2025-11-27) | stale; not used |

Not checked: serde_json's version, and the licences of transitive dependencies. The deny gate in M0 settles both.

**Avoided:**
- evalexpr: 14.0.0 is `AGPL-3.0-only`; formulas use our own Pratt parser.
- geo boolean operations.
- egui_dock and egui-data-table.
- lyon.
- egui's default fonts.

## 6. Workspace layout (`C:\Dev\plumbline`)

One workspace with `rust-toolchain.toml` at 1.100, a committed `Cargo.lock`, `=` pins in `[workspace.dependencies]`, and `deny.toml`. There are 14 crates plus `xtask`.

- `pl-geom`: transforms, units and fractions, area and offset, similarity/affine solve. It owns the one coordinate convention: unrotated page user space, PDF points, y-up. `/Rotate`, `/UserUnit` and CropBox offset are handled only in the view transform.
- `pl-draw`: `DrawList`, tiny-skia rasteriser (including Multiply), content-stream emitter.
- `pl-pdf`: sole lopdf importer. Load, incremental and full save, atomic replace, verification, layer-override builder, page operations, outlines, labels, links.
- `pl-render`: sole pdfium-render importer. `RenderBackend`, render mirror, scheduler, cache, thumbnails, text, form display.
- `pl-scene`: hayro Device, R-tree, snap queries, word grouping, with time and memory caps.
- `pl-annot`: typed model plus original dictionary, appearance generators, `/Measure`, `/VP`, XFDF.
- `pl-takeoff`: scales, quantities, list rows, columns, formulas, grouping, CSV/XLSX, legends.
- `pl-compare`: alignment, raster diff, overlay, vector diff.
- `pl-text`: OCR trait and implementations, text layer, redaction, flatten.
- `pl-tools`: tool state machines, split out of `pl-doc`.
- `pl-doc`: `Session`, `Command` enum, undo, journal, sets.
- `pl-cli` (bin `plumb`), `pl-app` (bin `plumbline.exe`, the only egui user), `pl-testkit` (dev only).
- `xtask`: `fetch-pdfium`, `licenses`, `dist`, `installer`, `bench`.

Agent rules: one crate per task, interface changes as separate tasks, lockfile changes only in dependency tasks, one git worktree per agent.

## 7. Architecture

- **Layering:** `pl-app` → `pl-doc` / `pl-tools` → domain crates → `pl-pdf` / `pl-render` / `pl-scene` → `pl-draw` / `pl-geom`. Everything below `pl-app` runs in `cargo test` with no window.
- **Document:** the original file is memory-mapped, not copied.
  - PDFium reads it through `load_pdf_from_reader`, so there is no second copy.
  - lopdf parses lazily on first edit.
  - The map is dropped before the file is replaced on save and re-made afterwards.
  - M0 measures peak memory on a 300 MB set.
- **Render:**
  - 512 px tiles at power-of-two levels.
  - Tiles are drawn with origin offsets (`PdfRenderConfig::set_origin`, added in 0.9.3) rather than a matrix, so the form-field pass lines up.
  - Tile key: doc, page, content revision, level, x, y, layer hash.
  - Page handles stay open in a small LRU so PDFium does not re-parse a dense sheet per tile.
  - Queue order is visible tiles centre-out, then a prefetch ring, then thumbnails.
  - Generation-based cancellation, a byte-budgeted LRU (512 MB default), upscaled placeholders.
- **Annotation display:**
  - Untouched annotations are drawn by PDFium from their own `/AP`, so Bluebeam- and Acrobat-authored markups look as their author made them.
  - When an annotation is first edited, it is hidden in the render mirror (`set_is_hidden`), tiles under its rectangle are invalidated once, and it is drawn from its `DrawList` from then on.
  - New markups never touch tiles.
  - Unmodelled types can only be moved or deleted. A move updates the rectangle in the mirror and invalidates the old and new regions.
  - Save does not reload the mirror, so there is no redraw flash.
- **Blend modes:** highlighter and any Multiply markup are composited on the CPU with tiny-skia against the base tile and uploaded as an overlay texture.
- **Layers:** toggling a layer rebuilds the mirror from original bytes plus an in-memory appended `/OCProperties /D` override and re-applies the hidden set. This is an M0 spike.
- **Annotation writes:**
  - Start from the original dictionary and change only dirty keys.
  - Untouched annotations are never rewritten.
  - Private data sits under one prefixed key, always beside a standard type with a full appearance.
  - Every markup carries `/NM`, `/M`, `/CreationDate`, `/T`, `/Subj`, `/F` (print), `/CA`, `/BS` dashes and `/LE` endings.
- **Appearance:** one `DrawList` per markup feeds the egui Painter, the `/AP /N` emitter and the tiny-skia oracle.
- **Save:**
  - Incremental append to `name.pdf.tmp`.
  - Verify with hayro-syntax and PDFium: page and annotation counts, plus a render hash of sampled untouched pages.
  - Replace with `ReplaceFileW`, falling back to `MoveFileExW` on shares that refuse it.
  - Structural edits and "Save optimised" use the full rewrite.
- **Encrypted files:**
  - Files that open with an empty user password are editable by incremental save if the M0 spike confirms lopdf's re-encryption.
  - Files needing a password prompt are editable after the prompt, on the same condition.
  - Content snapping and vector compare are off for encrypted files, because hayro cannot read them.
- **Undo and journal:** invertible commands with drag merging, and a CRC-checked sidecar journal under `%LOCALAPPDATA%`. Autosave is a journal flush.
- **Tools:** `(ToolState, ToolInput, &DocView) -> (ToolState, Vec<Command>, preview DrawList)`, with no egui types.
- **Measurement:** scale per page or viewport, SI f64 storage, standard `/Measure` (RL), `/VP`, `/IT`, dimension leaders (`/LL`, `/LLE`, `/Cap`), with the result in `/Contents` and in the appearance.
- **Compare:** pure functions over grey renders; alignment is applied in B's render transform.
- **Scripting:** a JSON command file run by `plumb`, used as the test driver. It is not a user-facing language in v1.
- **Loading:** `pdfium.dll` is loaded by absolute path from the app folder, never from the search path.
- **UI:** original names, icons and layout, restrained styling, every control with an accessible name.

## 8. Milestones with automated acceptance

| # | Scope | Acceptance (all headless) |
|---|---|---|
| M0 | Workspace, `fetch-pdfium`, deny gate, fixture generator, spikes. Fixtures include rotated pages, `/UserUnit`, offset CropBox, owner-password encryption, xref and object streams, linearised, a filled form, layers, and a 300 MB scan set. | Hash mismatch fails the fetch. Incremental save on each fixture keeps the original bytes as an exact prefix and reopens in PDFium, hayro-syntax and `qpdf --check`. Encrypted incremental save passes or the limit is recorded. Hidden-flag and layer-override spikes change the PDFium render as expected. Hayro dump returns the known segment count within its time cap. One kittest click, drag, key and snapshot passes. `dumpbin` shows `pdfium.dll` imports only system DLLs. OCR spike from an unpackaged exe passes or records the failure. lopdf parse time and peak memory on the big fixture are recorded and a budget is set. |
| M1 | Viewer: tabs, splits, tiles, thumbnails, bookmarks, search, text select and copy, form-field display, drag-and-drop open, recent files; `plumb info/render/search` | Stitched tiles equal a full-page render within 1 grey level on plain, rotated, UserUnit and CropBox fixtures. The filled form shows its values. LRU never exceeds its budget. Stale jobs are never rendered (mock backend). A 200k-segment sheet at 400% meets the M0 budget; this gates the worker pool. Zoom-to-cursor invariant within 0.5 px. Copied text equals PDFium extraction. |
| M2 | Annotation model, generators (Square, Circle, Line, PolyLine, Polygon, Ink, Highlight, Text, Link, Popup) with dashes, endings and opacity, untouched-appearance display, Multiply compositing, layers, safe save, undo | Per type: exact prefix, untouched objects byte-identical, PDFium render matches the tiny-skia `DrawList` within tolerance, load-save-load gives an equal model. Each passes on a rotated page. Text under a highlight keeps its dark pixels. An untouched foreign markup renders identically before and after an unrelated save. Layer toggle changes the render. 1,000 random command sequences undo to the start state. Kill during save leaves the original intact. |
| M3 | Shape tools, selection and arrange, properties, presets, copy/paste, snapshot to clipboard, journal, first portable zip | Scripted events give golden geometry for every tool. Paste across pages and documents gives an equal model. Journal replay equals the pre-kill state. The zip runs `plumb info` from a clean temp folder. |
| M4 | FreeText, callout, cloud, stamp, image, text highlight/underline/strikeout | FreeText wrap matches the PDFium render within 1 pt and extraction returns the typed string. Quads match PDFium character boxes. Characters outside WinAnsi are rejected with a message, not silently dropped. |
| M5 | Markups list: sort, group, filter, status, replies, custom columns, CSV/XLSX, XFDF | 50k-row sort/group/filter is correct and within budget. `/IRT`, `/State`, `/StateModel` verified at object level. CSV golden; XLSX reparsed for `SUM` formulas. XFDF round-trips. |
| M6 | Calibration, viewports, length, polylength, perimeter, area, volume, angle, radius, count, cutout, snapping | Analytic values and fraction tables pass. Snap returns the exact endpoint, intersection or midpoint; index build stays within its cap, and a capped build degrades without error. `/Measure` `/R` and `/C` are golden; the extracted caption equals the computed value. |
| M7 | Takeoff depth: formulas, choice columns, legends, spaces, sequences | Formula table and property tests pass with typed errors. Legend text updates after an edit. Cutout area matches a raster estimate. |
| M8 | Page editing, bookmarks, links, sets, basic print (page range, fit or actual size, with markups) | Random page-operation sequences match the reference list model by page fingerprint. Links and outlines resolve. Flatten leaves the render unchanged. Set search returns the right file and page. Banded print render to a memory device equals a direct render. |
| M9 | Compare, overlay, point alignment, batch | Planted changes are all found; zero regions for an identical pair or a 0.3 px shift. Solved transform within tolerance. Overlay golden and OCG structure pass. Vector diff is exact. |
| M10 | Embedded subset fonts and mixed-style text (parley), auto labels and links, slip-sheet | Non-Latin text extracts correctly. 100% of labels are correct on the generated set with no decoy links; a re-run is idempotent. |
| M11 | OCR and searchable text layer | OCR words overlap ground truth, or the test skips with the recorded reason. Render is unchanged after adding the layer. |
| M12 | Reports, print options, themes, keymap, second window, file association, NSIS installer, licences | Command-file golden outputs match. Silent install, smoke test and uninstall leave no files. No VC++ runtime imports. The licence file covers every crate, PDFium with its bundled third-party notices, and the fonts. |
| M13 (v1.1) | Redaction, reduce size, form fill, hardening | No redacted string in extraction, in any decoded stream, or in raw bytes. Single revision. The malformed-file loop produces zero panics. |

## 9. Parity table

| Bluebeam area | Status |
|---|---|
| Viewing, tabs, splits, thumbnails, bookmarks, search, text select and copy | v1 |
| Layers on/off | v1, subject to the M0 spike |
| Standard markups, clouds, callouts, static stamps, images, snapshot | v1 |
| Tool presets and an original symbol set | v1 |
| Markups list, statuses, replies, custom columns, CSV/XLSX/PDF summary | v1 |
| Calibration, viewports, measurement tools, count, cutout, snapping | v1 |
| Formula columns, legends, spaces, sequences | v1 |
| Compare, overlay, point alignment, batch | v1 |
| Page operations, combine/split, links, flatten, sets | v1 |
| Printing | basic at M8, options at M12 |
| Batch linking, auto labels, slip-sheet, OCR | v1 |
| Markup text outside Western European characters | M10 |
| Second window for a detached tab | M12 |
| Redaction, reduce size, form fill | v1.1 |
| User-facing scripting, MCP server | Later |
| Flood-fill takeoff, symbol search, auto-alignment, live Excel link | Later |
| Digital signatures, PDF/A, form authoring | Later |
| Custom line-style and hatch editors, snap to arc centres | Later |
| Reading Bluebeam private data, `.btx` / `.bax` | Later; needs owner-supplied samples |
| Real-time collaboration, 3D, print driver, plugins | Not planned |
| JavaScript and dynamic stamps, editing page text or graphics, AI review | Not planned |

## 10. Downloads and installs

None needs admin rights. Everything lands under `C:\Dev\plumbline` and the cargo cache.

1. PDFium: https://github.com/bblanchon/pdfium-binaries/releases/download/chromium/8125/pdfium-win-x64.tgz (SHA-256 in section 4), unpacked to `vendor\pdfium\8125\`.
2. Crates via cargo, roughly 1-2 GB of build output (estimate).
3. Liberation Sans 2.1.5 from https://github.com/liberationfonts/liberation-fonts/releases. The repository `LICENSE` is SIL OFL 1.1 with Reserved Font Name "Liberation"; ship it unmodified.
4. `cargo install cargo-deny cargo-nextest cargo-about`.
5. Test-only: qpdf 12.4.2 `msvc64.zip` from https://github.com/qpdf/qpdf/releases. qpdf is Apache-2.0, so it could later be shipped as a repair helper.
6. M12 only: the NSIS 3.x zip from https://nsis.sourceforge.io/Download. The licence (zlib/libpng, with the LZMA module under CPL 1.0) was not re-checked in this review.
7. M11 only if the Windows OCR spike fails: `ocrs` model files or a Tesseract build.

Not needed: Node, .NET, WiX, vcpkg, LLVM, MuPDF.

Owner-supplied: Bluebeam- and Acrobat-saved samples (including measurement markups), a few real drawing sheets, and one owner-password-protected issued drawing.

## 11. Packaging

- Static CRT (`+crt-static`).
- Portable zip from M3 containing the exe, `plumb.exe`, `pdfium.dll`, fonts and `THIRD-PARTY-LICENSES`.
- Hand-written NSIS installer at M12, per-user, with `.pdf` "Open with" registration and no default-app takeover.
- Unsigned. The friend sees a SmartScreen prompt. If Smart App Control is enforcing on their PC, the app is blocked until it is signed or Smart App Control is turned off.

## 12. Top risks

1. **Interop cannot be verified by agents.** Mitigation: standard keys, full appearances, and an owner checklist at M3, M6 and M12. Bluebeam's treatment of our measurements is the largest unknown.
2. **lopdf on real files** (eager parse time, memory, repaired input, encryption). Mitigation: M0 measurements with a budget, dual-parser verification, and a fallback appender built on hayro-syntax plus our own serialiser.
3. **Single-threaded PDFium on dense sheets.** Mitigation: page-handle cache, the M1 benchmark gate, then the worker-process pool. hayro is not a mitigation.
4. **A PDFium crash or hayro runaway on a malformed file takes the app down.** Mitigation: journal, caps on hayro, and a `RenderBackend` that can move out of process.
5. **Layer override and hidden-flag mirror are unproven.** Both are M0 spikes; the fallback is the appended render view.
6. **Windows OCR is officially unsupported without package identity**, and depends on the friend's language packs. Mitigation: the trait and a second engine.
7. **Text metrics and character coverage** until embedded fonts arrive in M10.
8. **egui, hayro and pdfium-render churn.** Mitigation: exact pins and a thin UI crate.
9. **Redaction leaks.** Built last, with byte-level tests.
10. **Scope.** The order protects M1-M6.
11. **Codename "Plumbline" has not been trademark-checked.**

## Changes made by review

**Factual corrections**
1. **Per-annotation hiding does exist.** The claim that PDFium has no such API was wrong: `FPDFAnnot_SetFlags` and pdfium-render's `set_is_hidden` / `delete_annotation` are there. The design now mutates an in-memory render mirror; the appended render view is kept for layers and as a fallback.
2. **"Encrypted originals are read-only" contradicted the lopdf source.** lopdf refuses only still-encrypted input and re-encrypts appended objects otherwise. This matters because issued drawings are often owner-password protected. It is now an M0 spike with an editable default.
3. **hayro was oversold.** It cannot read encrypted files, its own docs disclaim performance work, and its README lists timeouts and out-of-memory as open. It was removed as a render fallback and given caps. https://docs.rs/hayro/0.9.0/hayro/ and https://github.com/LaurenzV/hayro
4. **"MIT/Apache throughout" was wrong for the GUI.** `epaint_default_fonts` carries OFL-1.1 and Ubuntu-font-1.0. The default fonts are now disabled.
5. **"No native tear-off windows" was overstated.** eframe supports multiple native windows; only egui_tiles drag-out is missing. https://docs.rs/egui/0.37.0/egui/viewport/index.html
6. **Windows OCR was listed as merely "unverified".** Microsoft states the namespace is supported only with package identity. A fallback engine was added. https://learn.microsoft.com/en-us/uwp/api/windows.media.ocr?view=winrt-26100
7. **"Layers" in M1 assumed a PDFium call that does not appear to exist.** It moved to M2 behind a spike.
8. **The render-view risk was aimed at the wrong thing.** Annotations do render under a matrix; form fields do not. Tiles now use origin offsets.
9. **"PDFium incremental save reported broken" was softened.** The reports are old and a newer upstream test suggests a fix. https://groups.google.com/g/pdfium/c/IgJpPo3pUWs

**Confirmed as stated**
- Every crate version and licence in the original, including the "not re-checked" and "unverified" lists.
- The PDFium 8125 size and hash.
- The lopdf xref behaviour and the hayro `DrawProps.transform` field.
- The Liberation OFL 1.1 licence.
- evalexpr and MuPDF as AGPL; cargo-packager and iced as stale.

**Missing day-one capabilities added**
- Text select and copy.
- Form-field display.
- Rotated, UserUnit and CropBox pages as fixtures in every acceptance test.
- Untouched foreign markups shown from their own appearance.
- Correct Multiply highlights.
- Copy/paste of markups and snapshot to clipboard.
- Line endings, dashes, opacity, popups and author fields.
- Dimension leaders.
- Drag-and-drop open and recent files.
- Basic printing moved from M10 to M8.
- File association.

**Milestones re-cut**
- M3 was split into shape tools (M3) and text/stamp/image markups (M4).
- Text highlight moved from M9 to M4.
- M9 was split into fonts and labels (M10) and OCR (M11).
- M10's user scripting language was cut to a JSON command file for tests.
- The daily-tool point moved from M5 to M6.

**Architecture**
- Memory-mapped original with a PDFium reader instead of several in-RAM copies.
- A stated coordinate convention.
- Region invalidation for annotation changes.
- Save no longer reloads the renderer.
- Stronger save verification.
- `pl-tools` split from `pl-doc`.
- `pdfium.dll` loaded by absolute path.
- A `glow` fallback renderer.

**Not verified**
- Rust 1.100 on the machine, and the build-output size.
- NSIS licence text (not re-checked).
- Bluebeam's handling of standard `/Measure`.
- Whether wgpu falls back cleanly on a PC without a usable GPU.
- Multi-window testing in kittest.
- The `ocrs` model licence.
- Whether lopdf's encrypted incremental save works in practice.
- Transitive dependency licences.

## Open decisions for the owner

1. **Encrypted drawings.** May the app add markups to owner-password-protected files, and may it offer "save an unencrypted copy" when the permission flags forbid changes? I recommend yes to markups, since Bluebeam and Acrobat allow commenting on most such files. I recommend no to stripping protection unless you ask for it.
2. **Code signing.** Unsigned is free but blocked by Smart App Control. A signing certificate costs money and needs identity checks. Decide before M12; until then the friend needs Smart App Control off.
3. **OCR engine.** Decide after the M0 spike: Windows built-in OCR (no download, officially unsupported for this kind of app) or a bundled engine (larger install, works everywhere).
4. **Crash isolation.** Rendering in a helper process costs roughly a milestone and protects against bad PDFs crashing the app. The default is in-process with the journal; say if you want isolation in v1.
5. **Non-Western text in markups.** It waits until M10 unless you want it pulled forward at the cost of delaying measurement.
6. **Name.** "Plumbline" needs a trademark and domain check, or a replacement, before the installer milestone.
7. **Samples.** The Bluebeam- and Acrobat-saved files are needed by M2; without them, interop stays untested.
8. **MuPDF.** It remains the AGPL escape hatch if PDFium plus lopdf prove inadequate. Using it would mean giving your friend the source on request.
# Interop notes from real third-party files

Test files (local only, under `samples/external/`, not committed):

- `cyantakeoff-revu/` — 3 PDFs saved by **Bluebeam Revu x64 (Bluebeam PDF Library 20)**, from
  github.com/kabooHD/CyanTakeoff `tests/fixtures/revu` (Apache-2.0, LICENSE.txt alongside).
  `measure.pdf` has one of each Revu measurement on a page calibrated at `0.25 in = 1 ft`.
- `pdfjs/` — annotation test PDFs from github.com/mozilla/pdf.js `test/pdfs` (Apache-2.0).
  Confirmed **Adobe Acrobat Pro** output: `freetexts.pdf`, `rotated_freetexts.pdf`, `highlight_popup.pdf`,
  `rotated_ink.pdf`, `stamps.pdf` (inks.pdf: created in Acrobat, re-saved by macOS Quartz).

- `redcolumn/` — the "A3 Bluebeam compatibility test set" from github.com/divincentis/redcolumn
  (`packages/markup/test/compat/a3/sample`, Apache-2.0). **Not saved by Revu** (producer: pdf-lib), but written in
  Revu's private format and, per redcolumn PR #84 (github.com/ColbyGallagher/redcolumn/pull/84), opened and
  checked in Revu by its author. `A3-07-columns-statuses-metadata.pdf` has 11 custom columns of all 7 Revu column
  types, review statuses plus a custom status set, replies; `A3-00-ALL-combined.pdf` has 931 markups of 43 types;
  `manifest.csv` lists every markup with its expected status and column values. redcolumn's source
  (`packages/markup/src`, `test/compat/a3/columns.ts`) documents the format it reverse-engineered.

## Bluebeam custom columns and statuses (from redcolumn's Revu-checked files and public posts)

- Column definitions: catalog key `/BSIAnnotColumns`, which **must be an indirect reference to an array**
  (Revu ignores a direct array — redcolumn PR #84).
- Per-markup values: `/BSIColumnData [ (v1) (v2) ... ]`, a positional array of strings, one per defined column,
  empty string for no value (also seen in pypdf discussion #2270 and a UC2 Brainery post).
- Review statuses: standard reply Text annotations with `/IRT`, `/State`, `/StateModel`; custom status sets use
  their own `/StateModel` name (e.g. `BSI_A3TESTSET0001`).
- Counts: one Polygon per item, `/IT /PolygonCount`, `/CountStyle`, grouped (matches measure.pdf).
- Arcs: Circle with `/IT /CircleArc` and two angles. Hatches: `/Pattern`, `/PatternName`, `/PatternColor`.
- Dash patterns: Revu may only keep its own fixed set when it edits a markup (open question in PR #84).
- Bluebeam config/markup blobs that start with hex `789c` are zlib-compressed.
- No genuine Revu-saved file with custom columns was found publicly (searched 2026-11-24); one from the owner's
  friend would confirm the format.

### The entry structure (read from A3-07 and A3-00 and redcolumn's `bluebeam.ts`, 2026-11-24)

```
/BSIAnnotColumns 16 0 R          16 0 obj [ << ... >> << ... >> ... ]   (one dictionary per column)
<< /Subtype /Choice    /DisplayOrder 0 /Name (Discipline) /Items [ (Architectural) (Structural) ... ] >>
<< /Subtype /Choice    /DisplayOrder 1 /Name (Priority) /DefaultValue (Medium) /Items [ (Low) ... ] >>
<< /Subtype /Text      /DisplayOrder 2 /Name (Ref No.) >>
<< /Subtype /Text      /DisplayOrder 3 /Name (Review Notes) /Multiline true >>
<< /Subtype /Number    /DisplayOrder 4 /Name (Qty) /Format /Normal /Precision 0 >>
<< /Subtype /Formula   /DisplayOrder 6 /Name (Total Cost) /Format /Normal /Precision 2 /Expression ([Qty] * [Unit Cost]) >>
<< /Subtype /Date      /DisplayOrder 7 /Name (Due Date) /Format (MM/dd/yyyy) >>
<< /Subtype /Checkmark /DisplayOrder 8 /Name (Verified) /DefaultValue (False) >>
/BSIColumnData [ (Structural) () () () (2) (13.50) () () (True) () () ]      (on each markup; may be absent)
```

- Seven types by `/Subtype`: Text (`/Multiline true` = multi-line text), Number (`/Format /Normal`, `/Precision`),
  Choice (`/Items`; redcolumn also reads `Dropdown`/`List` and an item that is an array, last text = label), Date
  (`/Format` is only the display format; values are PDF dates `D:20261225000000`), Checkmark (`True`/`False`),
  Formula (`/Expression`, columns in `[Name]`). All strings in the files are UTF-16BE with a BOM, hex.
- **Values are positional by array order**, not `/DisplayOrder` (which is only the order Revu shows them). Formula
  positions hold an empty string in these files. A removed column is kept marked `/Deleted true` (redcolumn) so
  later positions keep their place. A column listed twice: redcolumn keeps the first.
- No "required" or "hidden" key is in the files: redcolumn's README calls Priority required, but its entry has only
  `/DefaultValue`. So no such flag is read.
- Statuses: no `/BSIStatus` catalog entry in these files (redcolumn reads one, `[<< /M model /S state /C colour >>]`,
  when present); the custom set's states are known only from the replies that use them (`Open`, `In Progress`,
  `Closed - No Action` under `BSI_A3TESTSET0001`). One markup's history mixes `Review` and the custom model; the
  status shown is the latest of either (manifest: `a3-progress`). `Review` with an unknown state (`Mystery-status`)
  occurs and is kept as text.
- Edge values on page Z-101 (all stored as given): 255 `R`s, `Réf № ① ≥ ± °`, XML/PDF specials, a 2000-character
  multi-line text, an out-of-list choice (`Orphan value`, `Urgent!!`), `-12`, `-12.5`, `1234567.891`, `0.005`,
  `abc`, `1,5`, `D:20280229000000`, `D:19991231000000`, `tomorrow`, an empty checkmark.
- Spaces (`/BSISpaces` on the page, `/Type /Space /Title /Path`) have no `/BSIColumnData`: the manifest's column
  values for Room 101 and Corridor L1 exist only in redcolumn's private `/NBData`.
- A Cloud+ is written with subject `Cloud+` on both of its annotations (the manifest names them by redcolumn's own
  subjects); a text box without a subject gets redcolumn's default `Text Box`.

### What Plumbline does with them (2026-11-24)

**Single source of truth.** A column the document defines in `/BSIAnnotColumns` is the document's: its definition
is the catalog entry and its values are the markups' `/BSIColumnData` at the entry's position. Nothing of it is
copied into Plumbline's private storage (`/PlumblineData /Columns`) or into the list's `markups_list.json`, so the
two can never disagree. Plumbline's own columns (the list's, kept per user in `markups_list.json`) keep their
values in `/PlumblineData /Columns` exactly as before; a file without Bluebeam columns is untouched by any of this.
(pl-takeoff `bluebeam.rs`, `CustomColumn::bluebeam`.)

1. **Read** (`BluebeamColumns::read`, `columns()`): every live entry becomes a list column (id `bb_<name>`), in
   `/DisplayOrder`, typed Text / multi-line Text / Number (places from `/Precision`) / Choice / Date / Checkmark /
   Formula. A stored text that does not read as the column's type (`abc`, `1,5` in a number column, `tomorrow`, an
   invalid day, a checkmark that is not `True`/`False`) is shown as that text, never as a number; an out-of-list
   choice is shown as it is. A Bluebeam formula is worked out by Plumbline when every name in it is a Bluebeam
   column and it parses in Plumbline's formula language (each `[Name]` becomes the column's id; a checkmark counts
   1/0 as in Revu: `[Qty] * [Verified]` → `[bb_qty] * if([bb_verified], 1, 0)`). Any other formula (one naming a
   Revu measurement column, whose unit Plumbline cannot be sure of, or syntax it does not read) and any unknown
   `/Subtype` is a **read-only** column showing what Bluebeam stored (`/Expression` kept in the definition).
   The full status history is kept: `AnnotSet::status_history` / `status` / `status_sets` treat every
   `/StateModel` but `Marked` alike; the list's Status shows the latest of any model.
2. **Write**: setting a value replaces only that position of `/BSIColumnData` (UTF-16BE hex, as Revu's files);
   every other entry keeps its object (bytes and string form), positions past the defined columns are kept, and a
   value equal to what is stored (13.5 for `13.50`) writes nothing. A markup without `/BSIColumnData` gets one
   padded with empty strings to the number of columns. Checkmarks are written `True`/`False`, dates `D:YYYYMMDDHHmmSS`,
   numbers as the shortest exact decimal. Untouched annotations are not written at all; a count whose change is
   only to `/BSIColumnData` (or `/M`, `/T`, `/Subj`, `/F`, `/OC`) is patched alone, its markers' shapes left as they are
   (each of its further annotations takes the same key patch, so every marker keeps the same values),
   and a change that leaves a measurement's shape and text alone no longer rewords its `/Contents`.
3. **Add a column** (the Custom columns dialog's "Keep it in this PDF as a Bluebeam column", ticked by default for
   a document that has Bluebeam columns; `markup_ops::add_bluebeam_column`): an entry in Revu's structure
   (`/Subtype`, `/DisplayOrder` after the last, `/Name`, plus `/Items`, `/Multiline true`,
   `/Format /Normal /Precision 2` or `/Format (MM/dd/yyyy)`) is appended to the array, written into the array's
   own object (the catalog's reference is left as it is), and every `/BSIColumnData` the markups have is padded,
   all as one undo step. A file with no `/BSIAnnotColumns` gets a new indirect array; a direct one in a file is
   written as an indirect one when it changes. Formula columns are not added to Bluebeam's list (Plumbline's
   formula syntax is not known to be Revu's); choices that stand for numbers are refused there.
4. **Statuses** chosen from a custom set are written as a status reply under that set's `/StateModel`, later than
   every status of any model the markup has (`markup_ops::set_status_named`).
5. **Tests**: `crates/pl-takeoff/tests/bluebeam.rs` compares all 931 markups of A3-00 with `manifest.csv`: status and
   the seven listed columns, as stored bytes and as typed values — **0 mismatches** (the 2 spaces have no
   `/BSIColumnData` and are skipped; redcolumn's custom-set ids `a3-open`/`a3-progress`/`a3-closed` are mapped to
   the state names). `crates/pl-doc/tests/bluebeam.rs`: a value and a custom status edited, saved and re-read, the
   save an incremental update after the untouched bytes, every other annotation dictionary equal, every other
   value position equal, `/BSIAnnotColumns` the same indirect array; a column added pads every `/BSIColumnData`
   and is one undo step; a file without columns gets an indirect array. `crates/pl-app/tests/list.rs`
   (kittest): A3-07 opens with its columns and a custom state in the list, a Bluebeam cell edit is undone and redone,
   a custom state is picked, the dialog adds a column. The Revu-saved `cyantakeoff-revu` files: no columns, an
   unchanged save writes nothing.
6. Review fixes (2026-11-24): a subject, lock or column value set on a Revu count group (one list row) is set on
   every annotation of the group (`markup_ops::modified_group`); a typed number such as `1,500` (a comma before
   exactly three digits: thousands or decimal?) is refused rather than read as 1.5; a hyperlink with
   `/BSIColumnData` or a `/Subj` is a list row (and so exported) without `include_links`; a Bluebeam formula
   none of whose columns holds a value is blank, not 0 (one with some inputs empty still counts them as 0, as
   Plumbline's own formulas do: what Revu shows there is unconfirmed); a date column also takes a date typed in
   its `/Format` (`MM/dd/yyyy`), though the list still shows dates as ISO; XFDF carries `/BSIColumnData` as
   `pl:bscolumns` (positional, so it means the columns of the file it is imported into).
7. Open: whether Revu itself accepts Plumbline's appended entry and UTF-16 values (only provable in Revu); Revu's
   own formula grammar beyond `[Name]` arithmetic; whether `/DefaultValue` should fill new markups' values (not
   done); `/BSIStatus` writing (custom states are only written as replies); the note window's status drop-down still
   offers review states only (the list's offers the custom sets).

## How Bluebeam Revu stores measurements (read from measure.pdf, 2026-11-24)

- **Scale**: page `/VP [ << /Type /Viewport /BBox [page box] /Measure ... /NM ... >> ]`, and **also a full
  `/Measure` dictionary on every measurement annotation** (the PDF 2.0 / Acrobat-extension placement).
- `/Measure /Subtype /RL /R (0.25 in = 1 ft' in")` with standard `/X /D /A`, plus Revu extras:
  `/T` (angle format, `U (°)`), `/V` (volume format, `U (cu ft)`), `/TargetUnitConversion`.
  Feet-inches is a two-element `/D` array: `[U(') C 1 ... SS(-)] [U(") C 12 ...]`. `/X` C = 0.05555556 ft per pt.
- `/Contents` holds the value text (e.g. `5'-5"`, `56.37 sf`, `96.69°`), `/RC` holds the same as XHTML, `/DS` the style.
- Revu private keys on each: `/MeasurementTypes` (bitfield), `/DepthUnit`, `/SlopeType`, `/PitchRun`,
  `/AlignOnSegment`, `/RiseDrop`, `/Label`, `/FillOpacity`.

| Revu tool | Subtype | /IT | MeasurementTypes |
|---|---|---|---|
| Length | Line (`/LL 10 /LLE 2 /Cap true /LE [ClosedArrow ClosedArrow]`) | LineDimension | 130 |
| Polylength | PolyLine | PolyLineDimension | 130 |
| **Perimeter** | **PolyLine** (in measure.pdf left *open*: 4 vertices, last ≠ first; its value 22'-4 1/2" is the open length) | PolyLineDimension | 130 |
| Area | Polygon | PolygonDimension | 129 |
| Volume | Polygon | **PolygonVolume** (non-standard) | 132 |
| Diameter | Circle (`/RD`) | **CircleDimension** (non-standard) | 384 |
| Radius | Polygon (3 vertices) | **PolygonRadius** (non-standard) | 130 |
| Angle | PolyLine (3 vertices) | **PolyLineAngle** (non-standard) | 1152 |
| Count | **one Polygon per counted item** (circle outline vertices) | **PolygonCount** (non-standard) | 128 |

Count items carry `/CountStyle /Circle`, `/NumCounts 1`, `/Version 1` and their scale under **`/BBMeasure`**
(not `/Measure`). Revu groups them into one "count" by subject/style, not by one annotation holding many points.

### Confirmed by Plumbline's golden test (`crates/pl-annot/tests/revu.rs`, 2026-11-24)

All ten values of measure.pdf are worked out from geometry + the annotation's own `/Measure` and come out as
Revu's `/Contents` exactly: `5'-5"`, `56.37 sf`, `18'-11 1/2"`, `22'-4 1/2"`, `4'-9"`, `3'-9"`, `2'-4"`,
`96.69°`, `0 cu ft`, `1`. What it took:

- **Revu's number formats are not ISO's.** Every Revu `NumberFormat` has `/FD true`, yet Revu *reduces* fractions
  (`11 1/2"` at `/D 4`, not `11 2/4"`) and shows decimals *without trailing zeros* (`0 cu ft` and, in
  markup-space.pdf, `81.2°` for 81.2026° at `/D 100`). Plumbline reads a measure dictionary that has Revu's
  `/TargetUnitConversion` key with those rules (`Dialect::Revu` in `measure.rs`); every other one by ISO 32000.
- Missing `/PS` and `/SS` take the ISO default (a space): `56.37 sf`, `0 cu ft`. `/RT` absent = `,`.
- **Radius** (`PolygonRadius`): vertices are *point on circle, centre, point on circle*; both radii equal
  (|v1−v0| = |v1−v2|). The radius is |v1−v0|. In measure.pdf the first has v2 ≈ v0 (a full circle), the second a
  different v2 (an arc's end).
- **Diameter** (`CircleDimension`): the circle is `/Rect` less `/RD`; Revu's `/RD [5 4.5 4.999992 4.499992]` is
  symmetric, so it does not tell left/bottom/right/top from left/top/right/bottom.
- **Volume** (`PolygonVolume`): no depth key at all; the value `0 cu ft` means depth 0. `/DepthUnit` is a
  NumberFormat array `[<< /U (') /C 0.001157407 /D 100 /FD true >>]`: 0.001157407 = 1/864 = feet per point at
  1:1 (the same as `/TargetUnitConversion`). No Revu file with a non-zero depth was available, so the key Revu
  stores a depth under is **unknown**; Plumbline reads a foreign volume with depth 0.
- **Angle** (`PolyLineAngle`): the angle at the middle vertex, formatted with `/T`.
- `/Cap true` is on every Revu measurement that shows its value, polylines and polygons included.

### Counts (markup-space.pdf: 24 items, markup-deep-spaces.pdf: 13 items)

- Revu groups the items of one count with the **standard annotation group**: the first item has
  `/GroupNesting [(Count Measurement) /NM1 /NM2 ...]` (its own name first), every other item `/IRT <first> /RT /Group`.
- **`/NumCounts` is the total of the group**, repeated on every item (15 on each of a 15-item group), not 1 per item.
- `/Contents` is `Label` + CR + total when the count has a `/Label` (`Check Mark\r15`), else the total (`1`).
- `/CountStyle` seen: `Circle` (filled: `/IC` set, `/BS /W 0`), `Checkmark`, `Square`. The polygon is the
  symbol's outline (a 24-point circle, 18 pt across, in a 27 pt `/Rect`).
- Groups found: markup-space 15 + 4 + five singles; markup-deep-spaces 3 + ten singles.

## What Plumbline does with it (pl-annot, 2026-11-24)

1. **Read**: every Revu intent above is a live, modelled measurement (Circle + `CircleDimension` and Polygon +
   `PolygonRadius` become a Line in the model; each `PolygonCount` item is a count of one, grouped by
   `Annotation::count_group()` = the group's first item). The annotation's own `/Measure` (or `/BBMeasure`) and
   number formats are used; `/V` and `/TargetUnitConversion` are read into `MeasureSpec::volume` /
   `target_unit_conversion`. An open `PolyLineDimension` is a polylength, a closed one a perimeter.
2. **Untouched** Revu measurements are never written (a save of an unedited Revu file is empty). An edited one is
   patched key by key: its `/Measure`/`/BBMeasure` is kept as the same object when it still reads as the same scale,
   and Revu's own keys (`/MeasurementTypes`, `/CountStyle`, `/DepthUnit`, `/SlopeType`, `/PitchRun`,
   `/AlignOnSegment`, `/RiseDrop`, `/Label`, `/FillOpacity`, `/Version`, `/DS`) stay as they were. `/Contents` is
   the bare value as Revu writes it (no `R ` / `Ø ` before a radius or diameter; only the drawn caption has them),
   and Revu's `/RC` is only rewritten when `/Contents` changes. A radius or diameter never gains `/IC`, `/LE` or `/CP`
   (on Revu's Polygon/Circle `/IC` fills the shape); Plumbline keeps its own endings, arrow fill and caption position
   in the private part. Its `/RD` is recomputed with the new `/Rect` (Revu's 5/4.5 margins are not kept).
   **Counts**: `/NumCounts` and `/Contents` are the total of the whole `/IRT`+`/RT /Group` group on every item: a marker
   added to (or taken off) one item of a Revu group rewrites `/NumCounts`, `/Contents` (label kept: `Label\rN`) and
   `/RC` on every other item of the group, and nothing else of them. A new item written for a Revu count copies the
   first item's `/Version`, `/DS`, `/Label`, `/DepthUnit` (etc.), shares its `/BBMeasure` object and has its `/IC`
   (a filled `/Circle`: `/IC` set, `/BS /W 0`).
3. **Write** (ours): Revu's subtype and `/IT` for every kind (table above), `/MeasurementTypes`, `/Cap`, a
   `/Measure` on every scaled annotation besides the page `/VP` (`/T` always; a `/V` made from the volume format on
   a volume), counts as **one small Polygon per marker** (`/IT /PolygonCount`, `/CountStyle`, `/NumCounts` = total,
   `/BBMeasure` when scaled), the further items grouped under the first with `/IRT` + `/RT /Group` and named in
   Plumbline's private data so a load folds them back into one count. `/Contents` and a full `/AP` on all.
   Earlier Plumbline forms (Ink counts, Line radius/diameter, angles and volumes without Revu's intent) are read and
   rewritten this way on the next save. Plumbline does not write `/GroupNesting` (not needed by the standard group).
   A new count takes the scale where it is placed (and follows page-scale changes like every other kind), so it has
   `/BBMeasure` on a scaled page. A Dot marker is `/CountStyle /Circle` with `/IC`; a Ring is `/Circle` without `/IC`
   (outlined), which reads back as a Ring.
4. A test strips Plumbline's private data from its own saved measurements and reads them by Revu's keys alone:
   the same kinds and values, except a volume's depth (only in the private data; see "unknown" above).
5. Still only provable in Revu itself: whether Revu *recognises* and edits measurements written by Plumbline.
6. **Lists** (pl-takeoff rows, `plumb measure`): a count is one row per `Annotation::count_group()` (per page), so
   each Revu group is one row with its total (markup-space: 15, 4 and five 1s; markup-deep-spaces: 3 and ten 1s) and
   counts that only look alike are not merged. The Perimeter column holds Perimeter measurements only (an Area's or
   Volume's outline still feeds Wall Area). Revu's Perimeter tool saved open reads as a Polylength (Length column):
   no key tells it from a Polylength except `/Subj` (and a `/RiseDrop` seen on the Polylength only).
   A save summary counts markups, not objects: adding a marker to a count is one modified markup.

## Adobe Acrobat FreeText (`pdfjs/freetexts.pdf`, `rotated_freetexts.pdf`; checked 2026-11-24)

- Only one FreeText in these files was written by Acrobat itself (`freetexts.pdf`, a typewriter, `/RC` with
  `xfa:APIVersion="Acrobat:23.1.0"`): `/DA (0 G 0 g 0 Tc 0 Tw 100 Tz 18.959 TL 0 Ts 0 Tr /Cour 14 Tf)`,
  `/DS (font: Helv 14.0pt; ... color:#6BD929)`, `/BS << /W 0 >>`, no `/C`, no `/RD`. Its appearance paints the text
  in #6BD929. So **the `/DA` colours are not the text colour**; Acrobat takes that from `/DS` (and `/RC`). With no
  border, the file cannot show whether `/DA`'s colour is the border's; Plumbline's reading (`/DA` stroke or fill =
  border, `/DS` = text) agrees with everything in it, so nothing was changed.
- The others are from Okular, Preview, Edge (PDFium), Foxit and pdf.js (rotated_freetexts.pdf: `/DA (/Helv 10 Tf 0 g)`,
  `/Rotate` 0/90/180/270, no `/DS`).
- **No FreeText in either file has `/RD`**, so the order of its numbers (Plumbline: left, bottom, right, top) is
  still **unconfirmed**; it needs a callout or a bordered box saved by Acrobat or Revu.
- Tests (`crates/pl-annot/tests/acrobat.rs`): each of the five Acrobat files reads and an unchanged save writes
  nothing; every FreeText made editable, its text changed and saved is the *only* object written again (every
  other object keeps its bytes and offset), and its `/RC` becomes the new text as XHTML `<p>` paragraphs; the
  same for the comment of a highlight, an ink and a stamp.

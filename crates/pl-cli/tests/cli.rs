//! Runs the built `plumb` binary against the pl-testkit fixtures. `plumb` is a console program:
//! nothing here opens a window (the child is started with CREATE_NO_WINDOW as well).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::OnceLock;

fn vendor_dll() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("vendor")
        .join("pdfium")
        .join("8125")
        .join("bin")
        .join("pdfium.dll");
    std::path::absolute(p).unwrap()
}

/// A `plumb` command that can find PDFium. A debug build is pointed at it through the
/// development environment variable. A release build ignores the environment (it loads the DLL
/// from its own folder or from `--pdfium` only), so there the option is passed.
fn plumb() -> Command {
    let mut cmd = bare_plumb();
    if cfg!(debug_assertions) {
        cmd.env("PLUMBLINE_PDFIUM_DLL", vendor_dll());
    } else {
        cmd.arg("--pdfium").arg(vendor_dll());
    }
    cmd
}

/// A `plumb` command with nothing said about PDFium.
fn bare_plumb() -> Command {
    plumb_at(Path::new(env!("CARGO_BIN_EXE_plumb")))
}

/// A command for the copy of `plumb` at `exe`, started without a console window.
fn plumb_at(exe: &Path) -> Command {
    let mut cmd = Command::new(exe);
    cmd.env_remove("PLUMBLINE_PDFIUM_DLL");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    cmd
}

struct Run {
    code: i32,
    stdout: String,
    stderr: String,
}

fn run(cmd: &mut Command) -> Run {
    let Output {
        status,
        stdout,
        stderr,
    } = cmd.output().expect("start plumb");
    Run {
        code: status.code().expect("exit code"),
        stdout: String::from_utf8(stdout).expect("utf-8 stdout"),
        stderr: String::from_utf8(stderr).expect("utf-8 stderr"),
    }
}

fn ok(cmd: &mut Command) -> String {
    let r = run(cmd);
    assert_eq!(r.code, 0, "stdout:\n{}\nstderr:\n{}", r.stdout, r.stderr);
    assert!(r.stderr.is_empty(), "unexpected stderr: {}", r.stderr);
    r.stdout
}

/// The fixture directory, written once per test run by pl-testkit (a dev-dependency: the
/// generator is not part of `plumb`).
fn fixtures() -> &'static Path {
    static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();
    let dir = DIR.get_or_init(|| {
        let dir = tempfile::tempdir().unwrap();
        pl_testkit::write_all(dir.path(), &pl_testkit::Options::small()).unwrap();
        dir
    });
    dir.path()
}

fn fixture(name: &str) -> PathBuf {
    fixtures().join(format!("{name}.pdf"))
}

/// The line of `plumb info` output for a 1-based page.
fn page_line(info: &str, page: usize) -> &str {
    let prefix = format!("page {page}: ");
    info.lines()
        .find(|l| l.starts_with(&prefix))
        .unwrap_or_else(|| panic!("no line for page {page} in:\n{info}"))
}

fn png(path: &Path) -> image::RgbaImage {
    image::open(path).expect("readable PNG").to_rgba8()
}

fn is_dark(p: &image::Rgba<u8>) -> bool {
    p.0[0].min(p.0[1]).min(p.0[2]) < 128
}

fn dark_near(img: &image::RgbaImage, x: f64, y: f64, radius: i64) -> bool {
    let (cx, cy) = (x.round() as i64, y.round() as i64);
    (cy - radius..=cy + radius).any(|yy| {
        (cx - radius..=cx + radius).any(|xx| {
            xx >= 0
                && yy >= 0
                && (xx as u32) < img.width()
                && (yy as u32) < img.height()
                && is_dark(img.get_pixel(xx as u32, yy as u32))
        })
    })
}

#[test]
fn the_fixture_generator_is_not_part_of_plumb() {
    // pl-testkit is "dev only": the shipped program has no subcommand that links it in.
    let r = run(plumb().arg("fixtures").arg(fixtures()));
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert!(r.stderr.contains("fixtures"), "{}", r.stderr);
    let help = ok(plumb().arg("--help"));
    assert!(!help.contains("fixtures"), "{help}");
    // The files the tests use come from the testkit itself.
    assert!(fixture("plain").is_file() && fixture("inherited_boxes").is_file());
}

#[test]
fn info_reports_pages_sizes_rotation_and_counts() {
    let out = ok(plumb().arg("info").arg(fixture("plain")));
    assert!(out.contains("pages: 1\n"), "{out}");
    assert!(out.contains("bookmarks: 0\n"), "{out}");
    assert!(out.contains("annotations: 0\n"), "{out}");
    assert!(out.lines().next().unwrap().ends_with("plain.pdf"), "{out}");
    let line = page_line(&out, 1);
    assert!(
        line.contains("612 x 792 pt (8.5 x 11 in), rotate 0, user unit 1,"),
        "{line}"
    );
    // The four strings of the plain fixture, spaces included.
    let chars: usize = pl_testkit::plain().manifest.pages[0]
        .texts
        .iter()
        .map(|t| t.text.chars().count())
        .sum();
    assert!(line.ends_with(&format!("characters {chars}")), "{line}");
    assert!(out.contains(&format!("characters: {chars}\n")), "{out}");

    // Rotation swaps the displayed size.
    let out = ok(plumb().arg("info").arg(fixture("rotated_pages")));
    assert!(out.contains("pages: 4\n"), "{out}");
    assert!(
        page_line(&out, 1).contains("612 x 792 pt") && page_line(&out, 1).contains("rotate 0,")
    );
    assert!(
        page_line(&out, 2).contains("792 x 612 pt") && page_line(&out, 2).contains("rotate 90,")
    );
    assert!(
        page_line(&out, 3).contains("612 x 792 pt") && page_line(&out, 3).contains("rotate 180,")
    );
    assert!(
        page_line(&out, 4).contains("792 x 612 pt") && page_line(&out, 4).contains("rotate 270,")
    );

    // /UserUnit 2 on a 306 x 396 page: Letter size on paper.
    let out = ok(plumb().arg("info").arg(fixture("user_unit")));
    assert!(
        page_line(&out, 1).contains("612 x 792 pt (8.5 x 11 in), rotate 0, user unit 2,"),
        "{out}"
    );

    // Bookmarks and annotations are counted.
    let out = ok(plumb().arg("info").arg(fixture("multi_page")));
    assert!(
        out.contains("pages: 24\n") && out.contains("bookmarks: 28\n"),
        "{out}"
    );
    assert_eq!(out.lines().filter(|l| l.starts_with("page ")).count(), 24);
    let out = ok(plumb().arg("info").arg(fixture("annotations")));
    assert!(out.contains("annotations: 4\n"), "{out}");
    assert!(page_line(&out, 1).contains("annotations 4,"), "{out}");

    // Every fixture opens, encrypted ones included, with the page count of its manifest.
    for f in pl_testkit::all_fixtures_with(&pl_testkit::Options::small()) {
        let out = ok(plumb().arg("info").arg(fixture(&f.manifest.name)));
        assert!(
            out.contains(&format!("pages: {}\n", f.manifest.page_count())),
            "{}: {out}",
            f.manifest.name
        );
        assert!(!out.contains("warning:"), "{}: {out}", f.manifest.name);
    }
}

#[test]
fn render_writes_a_png_of_the_expected_size_with_the_page_content() {
    let dir = tempfile::tempdir().unwrap();
    let out_png = dir.path().join("plain.png");
    let out = ok(plumb()
        .arg("render")
        .arg(fixture("plain"))
        .args(["--page", "1", "--scale", "2", "-o"])
        .arg(&out_png));
    assert!(out.contains("1224 x 1584 px"), "{out}");
    let img = png(&out_png);
    assert_eq!((img.width(), img.height()), (1224, 1584));

    // Not blank: mostly white paper, with ink where the fixture draws.
    let total = (img.width() * img.height()) as usize;
    let dark = img.pixels().filter(|p| is_dark(p)).count();
    let white = img.pixels().filter(|p| p.0[..3] == [255, 255, 255]).count();
    assert!(dark > 2_000, "only {dark} dark pixels");
    assert!(
        white > total / 2,
        "only {white} of {total} pixels are white"
    );
    assert!(img.pixels().all(|p| p.0[3] == 255), "opaque");

    // Every black segment of the manifest has ink at its midpoint; the page corner does not.
    let truth = &pl_testkit::plain().manifest.pages[0];
    let mut checked = 0;
    for seg in truth
        .segments
        .iter()
        .filter(|s| s.rgb == pl_testkit::BLACK && s.width >= 1.0)
    {
        let m = seg.midpoint();
        let (x, y) = (m[0] * 2.0, (792.0 - m[1]) * 2.0);
        assert!(dark_near(&img, x, y, 3), "no ink at {m:?} (pixel {x}, {y})");
        checked += 1;
    }
    assert!(checked >= 4);
    assert!(!dark_near(&img, 20.0, 20.0, 15));
    // The first string is drawn at (90, 690) in 18 pt: ink in its box.
    let text_box_dark = (0..img.height())
        .flat_map(|y| (0..img.width()).map(move |x| (x, y)))
        .filter(|(x, y)| (182..500).contains(x) && (1584 - 2 * 706..1584 - 2 * 690).contains(y))
        .filter(|(x, y)| is_dark(img.get_pixel(*x, *y)))
        .count();
    assert!(
        text_box_dark > 200,
        "text area has {text_box_dark} dark pixels"
    );
}

#[test]
fn render_follows_rotation_user_unit_scale_and_page_selection() {
    let dir = tempfile::tempdir().unwrap();
    let size = |pdf: &str, page: &str, scale: &str| {
        let out_png = dir.path().join(format!("{pdf}-{page}-{scale}.png"));
        ok(plumb()
            .arg("render")
            .arg(fixture(pdf))
            .args(["--page", page, "--scale", scale, "--output"])
            .arg(&out_png));
        let img = png(&out_png);
        assert!(img.pixels().any(is_dark), "{pdf} page {page} is blank");
        (img.width(), img.height())
    };
    assert_eq!(size("rotated_pages", "1", "1"), (612, 792));
    assert_eq!(size("rotated_pages", "2", "1"), (792, 612));
    assert_eq!(size("rotated_pages", "4", "0.5"), (396, 306));
    // Scale is pixels per point, so /UserUnit 2 doubles the pixels of a 306 x 396 page.
    assert_eq!(size("user_unit", "1", "1"), (612, 792));
    assert_eq!(size("multi_page", "24", "1.5"), (918, 1188));

    // Different pages give different pictures.
    let a = png(&dir.path().join("multi_page-24-1.5.png"));
    assert_eq!(size("multi_page", "1", "1.5"), (918, 1188));
    let b = png(&dir.path().join("multi_page-1-1.5.png"));
    assert_ne!(a.as_raw(), b.as_raw());

    // Annotations can be left out.
    let with = dir.path().join("with.png");
    let without = dir.path().join("without.png");
    ok(plumb()
        .arg("render")
        .arg(fixture("annotations"))
        .arg("-o")
        .arg(&with));
    ok(plumb()
        .arg("render")
        .arg(fixture("annotations"))
        .arg("--no-annotations")
        .arg("-o")
        .arg(&without));
    let (with, without) = (png(&with), png(&without));
    assert_eq!(with.dimensions(), without.dimensions());
    let differing = with
        .pixels()
        .zip(without.pixels())
        .filter(|(a, b)| a != b)
        .count();
    assert!(
        differing > 500,
        "annotations changed only {differing} pixels"
    );
}

#[test]
fn search_lists_hits_with_pages_and_boxes() {
    let token = pl_testkit::multi_page_token(16);
    let out = ok(plumb().arg("search").arg(fixture("multi_page")).arg(&token));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(lines[0].starts_with("page 17: ["), "{out}");
    assert!(lines[0].ends_with(&format!("] {token}")), "{out}");
    assert_eq!(lines[1], "1 hit on 1 page");
    // The box starts where the fixture drew the token (x = 72, baseline y = 690).
    let inside = lines[0]
        .split('[')
        .nth(1)
        .unwrap()
        .split(']')
        .next()
        .unwrap();
    let b: Vec<f64> = inside.split(' ').map(|v| v.parse().unwrap()).collect();
    assert_eq!(b.len(), 4);
    assert!(
        (b[0] - 72.0).abs() < 2.0 && (b[1] - 690.0).abs() < 4.0,
        "{b:?}"
    );
    assert!(b[2] > b[0] + 50.0 && b[3] > b[1] + 6.0, "{b:?}");

    // Case-insensitive by default, across all pages.
    let out = ok(plumb()
        .arg("search")
        .arg(fixture("multi_page"))
        .arg("plumb-page"));
    assert!(out.ends_with("24 hits on 24 pages\n"), "{out}");
    assert_eq!(out.lines().filter(|l| l.starts_with("page ")).count(), 24);

    // Options, and exit code 1 when nothing is found.
    let r = run(plumb()
        .arg("search")
        .arg(fixture("multi_page"))
        .arg("plumb-page")
        .arg("--match-case"));
    assert_eq!(r.code, 1, "{}", r.stderr);
    assert_eq!(r.stdout, "0 hits on 0 pages\n");
    let out = ok(plumb()
        .arg("search")
        .arg(fixture("multi_page"))
        .args(["Sheet 2", "--whole-word"]));
    assert!(out.ends_with("1 hit on 1 page\n"), "{out}");
    let out = ok(plumb()
        .arg("search")
        .arg(fixture("multi_page"))
        .arg("Sheet 2"));
    assert!(out.ends_with("6 hits on 6 pages\n"), "{out}");

    // An encrypted file opens with the empty user password.
    let out = ok(plumb()
        .arg("search")
        .arg(fixture("encrypted_rc4"))
        .arg(pl_testkit::encrypted_token(0)));
    assert!(out.contains("1 hit on 1 page"), "{out}");
}

#[test]
fn text_prints_one_page() {
    let out = ok(plumb()
        .arg("text")
        .arg(fixture("multi_page"))
        .args(["--page", "3"]));
    assert_eq!(
        out.lines().collect::<Vec<_>>(),
        ["Sheet 3 of 24", pl_testkit::multi_page_token(2).as_str()]
    );
    assert!(!out.contains('\r'));

    // The default page is 1, and the lines are the fixture's strings.
    let out = ok(plumb().arg("text").arg(fixture("plain")));
    for t in &pl_testkit::plain().manifest.pages[0].texts {
        assert!(
            out.lines().any(|l| l == t.text),
            "{:?} missing from:\n{out}",
            t.text
        );
    }
}

#[test]
fn errors_go_to_stderr_with_exit_code_2() {
    // A page that does not exist.
    let dir = tempfile::tempdir().unwrap();
    let out_png = dir.path().join("never.png");
    let r = run(plumb()
        .arg("render")
        .arg(fixture("plain"))
        .args(["--page", "2", "-o"])
        .arg(&out_png));
    assert_eq!(r.code, 2);
    assert!(r.stdout.is_empty());
    assert!(
        r.stderr
            .contains("page 2 is out of range: the document has 1 page"),
        "{}",
        r.stderr
    );
    assert!(!out_png.exists());
    let r = run(plumb()
        .arg("text")
        .arg(fixture("plain"))
        .args(["--page", "0"]));
    assert_eq!(r.code, 2);

    // A bad scale.
    let r = run(plumb()
        .arg("render")
        .arg(fixture("plain"))
        .args(["--scale", "0", "-o"])
        .arg(&out_png));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("--scale"), "{}", r.stderr);

    // A missing file and a file that is not a PDF.
    let r = run(plumb().arg("info").arg(dir.path().join("missing.pdf")));
    assert_eq!(r.code, 2);
    assert!(
        r.stderr.starts_with("plumb: open ") && r.stderr.contains("missing.pdf"),
        "{}",
        r.stderr
    );
    let junk = dir.path().join("junk.pdf");
    std::fs::write(&junk, b"not a pdf").unwrap();
    let r = run(plumb().arg("info").arg(&junk));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("junk.pdf"), "{}", r.stderr);

    // A PDFium path that does not exist is reported, not silently replaced by another copy.
    let r = run(plumb()
        .arg("--pdfium")
        .arg(dir.path().join("nowhere").join("pdfium.dll"))
        .arg("info")
        .arg(fixture("plain")));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("start PDFium"), "{}", r.stderr);

    // Bad arguments are clap's exit code 2 with usage on stderr.
    let r = run(plumb().arg("render").arg(fixture("plain")));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("--output"), "{}", r.stderr);
}

#[test]
fn pdfium_can_be_named_on_the_command_line() {
    let out = ok(bare_plumb()
        .arg("info")
        .arg(fixture("plain"))
        .arg("--pdfium")
        .arg(vendor_dll()));
    assert!(out.contains("pages: 1\n"), "{out}");
}

#[test]
fn only_a_debug_build_takes_the_dll_from_the_environment() {
    // Plan section 7: the DLL is loaded from the app folder, never from somewhere the
    // environment chooses. The variable is a development convenience in debug builds.
    // A copy of plumb in an empty folder has no DLL beside it and no vendored fallback.
    let dir = tempfile::tempdir().unwrap();
    let alone = dir.path().join("plumb.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_plumb"), &alone).unwrap();
    let mut cmd = plumb_at(&alone);
    cmd.env("PLUMBLINE_PDFIUM_DLL", vendor_dll())
        .arg("info")
        .arg(fixture("plain"));
    let r = run(&mut cmd);
    if cfg!(debug_assertions) {
        assert_eq!(r.code, 0, "{}", r.stderr);
    } else {
        assert_eq!(
            r.code, 2,
            "a release build loaded the DLL the environment named"
        );
        assert!(
            r.stderr.contains("pdfium.dll was not found"),
            "{}",
            r.stderr
        );
        assert!(!r.stderr.contains("PLUMBLINE_PDFIUM_DLL"), "{}", r.stderr);
    }
    // An environment value that is not an absolute path is never searched for.
    let mut cmd = plumb_at(&alone);
    cmd.env("PLUMBLINE_PDFIUM_DLL", "pdfium.dll")
        .arg("info")
        .arg(fixture("plain"));
    assert_eq!(run(&mut cmd).code, 2);
}

#[test]
fn help_and_version_describe_the_tool() {
    let out = ok(plumb().arg("--help"));
    for word in ["info", "render", "search", "text", "--pdfium"] {
        assert!(out.contains(word), "{word} missing from help:\n{out}");
    }
    let out = ok(plumb().arg("--version"));
    assert_eq!(out.trim(), format!("plumb {}", env!("CARGO_PKG_VERSION")));
}

// ---- annots and run -------------------------------------------------------------------------

/// The `page N: ...` lines of `plumb annots` output.
fn annot_lines(listing: &str) -> Vec<&str> {
    listing.lines().filter(|l| l.starts_with("page ")).collect()
}

/// A path as a JSON string.
fn json_path(path: &Path) -> String {
    format!("{:?}", path.to_string_lossy())
}

#[test]
fn annots_lists_kind_page_rect_author_and_subject() {
    let listing = ok(plumb().arg("annots").arg(fixture("annotations")));
    let lines = annot_lines(&listing);
    assert_eq!(
        lines,
        [
            "page 1: Square [100 400 220 480] author \"Foreign Author\" subject \"Square\" id fixture-square-1",
            "page 1: Line [295 415 505 475] author \"Foreign Author\" subject \"Line\" id fixture-line-1",
            "page 1: FreeText [100 250 340 300] author \"Foreign Author\" subject \"FreeText\" id fixture-freetext-1",
            lines[3],
        ],
        "{listing}"
    );
    assert!(
        lines[3].starts_with("page 1: Highlight [72 596 "),
        "{listing}"
    );
    assert!(
        lines[3].ends_with("subject \"Highlight\" id fixture-highlight-1"),
        "{listing}"
    );
    assert_eq!(listing.lines().last(), Some("4 annotations on 1 page"));

    // A file without annotations, and one that is not there.
    let listing = ok(plumb().arg("annots").arg(fixture("multi_page")));
    assert_eq!(listing.trim_end(), "0 annotations on 0 pages");
    let r = run(plumb().arg("annots").arg(fixtures().join("missing.pdf")));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("missing.pdf"), "{}", r.stderr);
}

#[test]
fn run_executes_a_command_file() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("work.pdf");
    std::fs::copy(fixture("rotated_pages"), &pdf).unwrap();
    let original = std::fs::read(&pdf).unwrap();
    let commands = dir.path().join("steps.json");
    std::fs::write(
        &commands,
        r#"{
          "author": "Driver",
          "date": "D:20261124101500Z",
          "steps": [
            {"op": "open", "path": "work.pdf"},
            {"op": "add", "id": "box", "kind": "Square", "page": 2, "rect": [100, 100, 260, 200],
             "style": {"stroke": [0, 0, 0], "fill": [0, 0, 0], "width": 2}, "subject": "Solid"},
            {"op": "add", "id": "ring", "kind": "Circle", "page": 2, "rect": [300, 100, 380, 180]},
            {"op": "add", "id": "arrow", "kind": "Line", "page": 3,
             "points": [[100, 100], [300, 180]], "style": {"line_end": "OpenArrow"}},
            {"op": "add", "id": "pen", "kind": "Ink", "page": 1,
             "strokes": [[[100, 100], [140, 160], [200, 120]]]},
            {"op": "modify", "id": "ring", "move": [20, 20], "style": {"width": 5}},
            {"op": "delete", "id": "pen"},
            {"op": "undo"},
            {"op": "delete", "id": "ring"},
            {"op": "undo"},
            {"op": "redo"},
            {"op": "expect", "count": 3},
            {"op": "render", "page": 2, "scale": 1, "output": "unsaved.png"},
            {"op": "save"},
            {"op": "render", "page": 2, "scale": 1, "output": "saved.png"},
            {"op": "save"},
            {"op": "add", "id": "late", "kind": "Polygon", "page": 4,
             "points": [[100, 100], [200, 100], [150, 180]]},
            {"op": "save_as", "path": "copy.pdf"}
          ]
        }"#,
    )
    .unwrap();

    let out = ok(plumb().arg("run").arg(&commands));
    let lines: Vec<&str> = out.lines().collect();
    assert_eq!(lines.len(), 19, "{out}");
    assert_eq!(lines[1], "2: add Square box on page 2");
    assert_eq!(lines[6], "7: delete pen (1 removed)");
    assert_eq!(lines[7], "8: undo");
    assert_eq!(lines[11], "12: expect 3 annotation(s)");
    assert_eq!(lines[13], "14: save wrote 3 added, 0 modified, 0 deleted");
    assert_eq!(lines[15], "16: save nothing to write");
    assert!(
        lines[17].ends_with("copy.pdf: wrote 1 added, 0 modified, 0 deleted"),
        "{out}"
    );
    assert_eq!(lines[18], "18 steps done");

    // The file was appended to once; the copy has one markup more.
    let saved = std::fs::read(&pdf).unwrap();
    assert!(saved.len() > original.len() && saved.starts_with(&original));
    let copy = dir.path().join("copy.pdf");
    assert!(std::fs::read(&copy).unwrap().starts_with(&saved));
    let listing = ok(plumb().arg("annots").arg(&pdf));
    assert_eq!(
        annot_lines(&listing),
        [
            annot_lines(&listing)[0],
            "page 2: Square [100 100 260 200] author \"Driver\" subject \"Solid\" id box",
            annot_lines(&listing)[2],
        ],
        "{listing}"
    );
    assert!(
        annot_lines(&listing)[2].starts_with("page 3: Line ["),
        "{listing}"
    );
    assert!(
        annot_lines(&listing)[0].starts_with("page 1: Ink ["),
        "{listing}"
    );
    assert!(
        annot_lines(&listing)[0].ends_with("author \"Driver\" subject \"Pen\" id pen"),
        "{listing}"
    );
    assert_eq!(listing.lines().last(), Some("3 annotations on 3 pages"));
    let listing = ok(plumb().arg("annots").arg(&copy));
    assert_eq!(listing.lines().last(), Some("4 annotations on 4 pages"));
    assert!(listing.contains("page 4: Polygon ["), "{listing}");
    let info = ok(plumb().arg("info").arg(&copy));
    for page in 1..=4 {
        assert!(page_line(&info, page).contains("annotations 1,"), "{info}");
    }

    // The picture before the save (PDFium plus the overlay) and the one after it (PDFium
    // alone) both show the solid box: page 2 is turned 90 degrees, so the page-space box
    // 100..260 x 100..200 is at x 100..200, y 100..260 from the top-left of the picture.
    let unsaved = png(&dir.path().join("unsaved.png"));
    let after = png(&dir.path().join("saved.png"));
    assert_eq!(unsaved.dimensions(), after.dimensions());
    assert_eq!(unsaved.dimensions(), (792, 612));
    for img in [&unsaved, &after] {
        assert!(is_dark(img.get_pixel(150, 180)), "inside the box");
        assert!(is_dark(img.get_pixel(110, 250)), "inside the box");
        assert!(!is_dark(img.get_pixel(230, 180)), "beside the box");
        assert!(!is_dark(img.get_pixel(150, 290)), "below the box");
    }
    let differing = unsaved
        .pixels()
        .zip(after.pixels())
        .filter(|(a, b)| (0..3).any(|c| a.0[c].abs_diff(b.0[c]) > 60))
        .count();
    assert!(
        differing < 400,
        "{differing} pixels differ between overlay and saved file"
    );

    // A step that fails stops the run with exit code 2 and names the step; earlier steps
    // have been done.
    let bad = dir.path().join("bad.json");
    std::fs::write(
        &bad,
        r#"{"steps": [
            {"op": "open", "path": "copy.pdf"},
            {"op": "delete", "id": "late"},
            {"op": "delete", "id": "no-such"},
            {"op": "save"}
        ]}"#,
    )
    .unwrap();
    let before = std::fs::read(&copy).unwrap();
    let r = run(plumb().arg("run").arg(&bad));
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert_eq!(r.stdout.lines().count(), 2, "{}", r.stdout);
    assert!(
        r.stderr
            .contains("step 3 (delete): there is no annotation no-such"),
        "{}",
        r.stderr
    );
    assert_eq!(std::fs::read(&copy).unwrap(), before, "nothing was saved");

    // A command file that is not valid, and one that is not there.
    std::fs::write(&bad, r#"{"steps": [{"op": "fly"}]}"#).unwrap();
    let r = run(plumb().arg("run").arg(&bad));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("bad.json"), "{}", r.stderr);
    let r = run(plumb().arg("run").arg(dir.path().join("none.json")));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("none.json"), "{}", r.stderr);
}

#[test]
fn run_with_a_journal_recovers_unsaved_work() {
    let dir = tempfile::tempdir().unwrap();
    let journal = dir.path().join("journal");
    let pdf = dir.path().join("work.pdf");
    std::fs::copy(fixture("plain"), &pdf).unwrap();
    let original = std::fs::read(&pdf).unwrap();
    let first = dir.path().join("first.json");
    // The run ends without saving: for the journal that is a crash.
    std::fs::write(
        &first,
        r#"{"steps": [
            {"op": "open", "path": "work.pdf"},
            {"op": "add", "id": "a", "kind": "Square", "page": 1, "rect": [100, 100, 200, 200]},
            {"op": "add", "id": "b", "kind": "Circle", "page": 1, "rect": [300, 100, 400, 200]},
            {"op": "delete", "id": "a"},
            {"op": "undo"}
        ]}"#,
    )
    .unwrap();
    ok(plumb()
        .arg("run")
        .arg(&first)
        .arg("--journal")
        .arg(&journal));
    assert_eq!(std::fs::read(&pdf).unwrap(), original);
    assert_eq!(std::fs::read_dir(&journal).unwrap().count(), 1);

    let second = dir.path().join("second.json");
    std::fs::write(
        &second,
        r#"{"steps": [
            {"op": "open", "path": "work.pdf"},
            {"op": "expect", "count": 2, "modified": true},
            {"op": "redo"},
            {"op": "expect", "count": 1},
            {"op": "save"}
        ]}"#,
    )
    .unwrap();
    let out = ok(plumb()
        .arg("run")
        .arg(&second)
        .arg("--journal")
        .arg(&journal));
    assert!(
        out.lines()
            .next()
            .unwrap()
            .ends_with("4 unsaved change(s) recovered"),
        "{out}"
    );
    assert!(
        out.contains("5: save wrote 1 added, 0 modified, 0 deleted"),
        "{out}"
    );
    assert_eq!(
        std::fs::read_dir(&journal).unwrap().count(),
        0,
        "saved: no journal left"
    );
    let listing = ok(plumb().arg("annots").arg(&pdf));
    assert_eq!(annot_lines(&listing).len(), 1);
    assert!(
        listing.contains("page 1: Circle [300 100 400 200]"),
        "{listing}"
    );
}

fn sample_path() -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("site-development-plans.pdf");
    path.is_file().then(|| std::path::absolute(path).unwrap())
}

/// The per-page annotation counts in `plumb info` output.
fn annotation_counts(info: &str) -> Vec<usize> {
    info.lines()
        .filter(|l| l.starts_with("page "))
        .map(|l| {
            let rest = l
                .split("annotations ")
                .nth(1)
                .expect("an annotations field");
            rest.split(',').next().unwrap().trim().parse().unwrap()
        })
        .collect()
}

#[test]
fn run_adds_markups_to_the_real_sample_and_the_saved_copy_has_them() {
    let Some(sample) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let stamp = |p: &Path| {
        std::fs::metadata(p)
            .map(|m| (m.len(), m.modified().ok()))
            .unwrap()
    };
    let stamp_before = stamp(&sample);
    let before = annotation_counts(&ok(plumb().arg("info").arg(&sample)));
    assert!(before.len() >= 2, "the sample has several sheets");

    // The marked copy goes to a temporary folder under `target` that is deleted with the
    // test: the sample is never copied anywhere else.
    let dir = sample_work_dir();
    let commands = dir.path().join("sample.json");
    let marked = dir.path().join("marked.pdf");
    let picture = dir.path().join("sheet1.png");
    std::fs::write(
        &commands,
        format!(
            r#"{{
              "author": "Driver",
              "steps": [
                {{"op": "open", "path": {sample}}},
                {{"op": "add", "kind": "Square", "page": 1, "rect": [200, 200, 420, 340],
                  "style": {{"stroke": [1, 0, 0], "width": 4, "cloud": 1.0}}, "contents": "Revise"}},
                {{"op": "add", "kind": "Circle", "page": 1, "rect": [460, 200, 600, 340],
                  "style": {{"fill": [1, 1, 0], "opacity": 0.4}}}},
                {{"op": "add", "kind": "Line", "page": 1, "points": [[200, 380], [600, 440]],
                  "style": {{"width": 3, "line_end": "ClosedArrow", "dash": [9, 4]}}}},
                {{"op": "add", "kind": "Ink", "page": 1,
                  "strokes": [[[220, 480], [260, 540], [320, 500], [380, 560]]]}},
                {{"op": "add", "kind": "Highlight", "page": 1, "quads": [[200, 600, 520, 630]]}},
                {{"op": "add", "id": "gone", "kind": "Text", "page": 1, "rect": [640, 200, 660, 220],
                  "contents": "Removed again"}},
                {{"op": "delete", "id": "gone"}},
                {{"op": "add", "kind": "Polygon", "page": 2,
                  "points": [[200, 200], [400, 200], [300, 360]], "style": {{"fill": [0, 0.6, 1]}}}},
                {{"op": "add", "kind": "PolyLine", "page": 2,
                  "points": [[200, 400], [300, 460], [400, 400], [500, 460]]}},
                {{"op": "add", "id": "note", "kind": "Text", "page": 2, "rect": [520, 200, 540, 220],
                  "contents": "Check dimension"}},
                {{"op": "modify", "id": "note", "move": [30, 0]}},
                {{"op": "undo"}},
                {{"op": "redo"}},
                {{"op": "expect", "count": 5, "page": 1}},
                {{"op": "expect", "count": 3, "page": 2}},
                {{"op": "render", "page": 1, "scale": 0.25, "output": {picture}}},
                {{"op": "save_as", "path": {marked}}}
              ]
            }}"#,
            sample = json_path(&sample),
            picture = json_path(&picture),
            marked = json_path(&marked),
        ),
    )
    .unwrap();
    let out = ok(plumb().arg("run").arg(&commands));
    assert!(
        out.contains("wrote 8 added, 0 modified, 0 deleted"),
        "the save line"
    );
    assert_eq!(out.lines().last(), Some("18 steps done"));

    // The sample itself is untouched; the copy starts with its bytes and has the markups.
    assert_eq!(stamp(&sample), stamp_before, "the sample file was modified");
    assert!(marked.is_file() && stamp(&marked).0 > stamp_before.0);
    let after = annotation_counts(&ok(plumb().arg("info").arg(&marked)));
    assert_eq!(after.len(), before.len(), "page count");
    for (page, (now, then)) in after.iter().zip(&before).enumerate() {
        let added = [5, 3].get(page).copied().unwrap_or(0);
        assert_eq!(*now, then + added, "sheet {}", page + 1);
    }
    let listing = ok(plumb().arg("annots").arg(&marked));
    let ours: Vec<&str> = annot_lines(&listing)
        .into_iter()
        .filter(|l| l.contains("author \"Driver\""))
        .collect();
    assert_eq!(ours.len(), 8);
    let kinds: Vec<&str> = ours.iter().map(|l| l.split(' ').nth(2).unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "Square",
            "Circle",
            "Line",
            "Ink",
            "Highlight",
            "Polygon",
            "PolyLine",
            "Text"
        ]
    );
    assert_eq!(ours.iter().filter(|l| l.starts_with("page 1: ")).count(), 5);
    assert_eq!(ours.iter().filter(|l| l.starts_with("page 2: ")).count(), 3);
    assert!(picture.is_file());
    let (w, h) = png(&picture).dimensions();
    assert!(w > 100 && h > 100, "{w} x {h}");
}

// ---- text boxes, stamps, text markups, the list, export --------------------------------------

/// A small picture file: left half red, right half blue.
fn write_flag(path: &Path) {
    let img = image::RgbaImage::from_fn(80, 40, |x, _| {
        if x < 40 {
            image::Rgba([230, 20, 20, 255])
        } else {
            image::Rgba([20, 20, 230, 255])
        }
    });
    img.save(path).unwrap();
}

/// The steps that mark up every page of the `rotated_pages` fixture (pages with `/Rotate` 0,
/// 90, 180 and 270, each labelled `ROTATE <degrees>` in its content).
fn rotated_steps() -> String {
    let mut steps = vec![
        r#"{"op": "define_column", "title": "Trade", "kind": "Text"}"#.to_string(),
        r#"{"op": "define_column", "title": "Cost", "kind": "Number"}"#.to_string(),
    ];
    for (i, degrees) in [0, 90, 180, 270].into_iter().enumerate() {
        let p = i + 1;
        steps.push(format!(
            r#"{{"op": "add", "id": "T{p}", "kind": "FreeText", "page": {p},
                "rect": [300, 300, 480, 348], "text": "Sheet {p} note",
                "text_style": {{"size": 11, "align": "centre", "v_align": "middle"}},
                "style": {{"fill": [1, 1, 0.8]}}}}"#
        ));
        steps.push(format!(
            r#"{{"op": "markup_text", "id": "H{p}", "kind": "Highlight", "page": {p},
                "find": "ROTATE {degrees}"}}"#
        ));
        steps.push(format!(
            r#"{{"op": "expect_annot", "id": "T{p}", "rotation": {degrees}}}"#
        ));
    }
    steps.extend(
        [
            r#"{"op": "add", "id": "call", "kind": "FreeText", "intent": "callout", "page": 1,
                "rect": [300, 420, 440, 460], "tip": [200, 520], "text": "Kerb here"}"#,
            r#"{"op": "add", "id": "typed", "kind": "FreeText", "intent": "typewriter", "page": 1,
                "rect": [300, 200, 500, 220], "text": "Typed on the sheet"}"#,
            r#"{"op": "add", "id": "ok", "kind": "Stamp", "page": 2, "rect": [300, 400, 344, 520],
                "stamp": "approved", "detail": "Driver"}"#,
            r#"{"op": "add", "id": "pic", "kind": "Stamp", "page": 3, "rect": [300, 400, 0, 0],
                "image": "flag.png"}"#,
            r#"{"op": "markup_text", "id": "S1", "kind": "StrikeOut", "page": 1,
                "from": [60, 728], "to": [74, 728]}"#,
            r#"{"op": "markup_text", "id": "Q4", "kind": "Squiggly", "page": 4, "find": "rotate"}"#,
            r#"{"op": "markup_text", "id": "U4", "kind": "Underline", "page": 4, "find": "270"}"#,
            r#"{"op": "set_text", "id": "T2", "text": "Sheet 2 note, revised"}"#,
            r#"{"op": "status", "id": "T1", "state": "Rejected", "author": "Bo"}"#,
            r#"{"op": "status", "id": "T1", "state": "Accepted", "author": "Cy"}"#,
            r#"{"op": "reply", "id": "T1", "reply_id": "R1", "text": "Agreed.", "author": "Bo"}"#,
            r#"{"op": "reply", "id": "R1", "reply_id": "R2", "text": "Thanks.", "author": "Cy"}"#,
            r#"{"op": "reply", "id": "T1", "reply_id": "R3", "text": "Withdrawn"}"#,
            r#"{"op": "delete_reply", "id": "R3"}"#,
            r#"{"op": "check", "id": "H2"}"#,
            r#"{"op": "check", "id": "H3"}"#,
            r#"{"op": "check", "id": "H3", "checked": false}"#,
            r#"{"op": "lock", "id": "ok"}"#,
            r#"{"op": "column", "id": "T1", "column": "trade", "value": "Drainage"}"#,
            r#"{"op": "column", "id": "T1", "column": "cost", "value": "1250.5"}"#,
            r#"{"op": "column", "id": "H2", "column": "trade", "value": "Kerbs, \"dropped\""}"#,
            r#"{"op": "popup", "id": "T1", "open": true}"#,
        ]
        .map(str::to_string),
    );
    steps.join(",\n")
}

/// What the marked file must hold, as steps: run right after the edits, and again on the
/// saved file in a fresh process.
fn rotated_expectations() -> String {
    let mut steps = Vec::new();
    for (i, degrees) in [0, 90, 180, 270].into_iter().enumerate() {
        let p = i + 1;
        steps.push(format!(
            r#"{{"op": "expect_annot", "id": "H{p}", "kind": "Highlight", "page": {p},
                "quads": 1, "contents": "ROTATE {degrees}", "author": "Driver"}}"#
        ));
        steps.push(format!(
            r#"{{"op": "expect_annot", "id": "T{p}", "kind": "FreeText", "page": {p},
                "rotation": {degrees}}}"#
        ));
    }
    steps.extend(
        [
            r#"{"op": "expect_annot", "id": "T1", "contents": "Sheet 1 note", "status": "Accepted",
                "replies": 1, "popup": true, "open": true, "checked": false, "locked": false}"#,
            r#"{"op": "expect_annot", "id": "T2", "contents": "Sheet 2 note, revised"}"#,
            r#"{"op": "expect_annot", "id": "R1", "kind": "Text", "contents": "Agreed.",
                "author": "Bo", "replies": 1}"#,
            r#"{"op": "expect_annot", "id": "S1", "kind": "StrikeOut", "contents": "RO", "quads": 1}"#,
            r#"{"op": "expect_annot", "id": "Q4", "kind": "Squiggly", "contents": "ROTATE"}"#,
            r#"{"op": "expect_annot", "id": "U4", "kind": "Underline", "contents": "270"}"#,
            r#"{"op": "expect_annot", "id": "H2", "checked": true}"#,
            r#"{"op": "expect_annot", "id": "H3", "checked": false}"#,
            r#"{"op": "expect_annot", "id": "ok", "kind": "Stamp", "locked": true, "rotation": 90}"#,
            r#"{"op": "expect_annot", "id": "pic", "kind": "Stamp", "rotation": 180}"#,
            r#"{"op": "expect_annot", "id": "call", "kind": "FreeText", "contents": "Kerb here"}"#,
            // Page 1: text box, highlight, callout, typewriter, strike-out, two statuses,
            // two replies and a popup. Page 2: text box, highlight, stamp, a checkmark.
            // Page 3: text box, highlight, picture, two checkmark records. Page 4: text box
            // and three text markups.
            r#"{"op": "expect", "count": 10, "page": 1}"#,
            r#"{"op": "expect", "count": 4, "page": 2}"#,
            r#"{"op": "expect", "count": 5, "page": 3}"#,
            r#"{"op": "expect", "count": 4, "page": 4}"#,
            r#"{"op": "expect", "count": 23}"#,
        ]
        .map(str::to_string),
    );
    steps.join(",\n")
}

#[test]
fn run_adds_text_boxes_stamps_and_text_markups_on_rotated_pages_and_export_lists_them() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("work.pdf");
    std::fs::copy(fixture("rotated_pages"), &pdf).unwrap();
    let original = std::fs::read(&pdf).unwrap();
    write_flag(&dir.path().join("flag.png"));
    let marked = dir.path().join("marked.pdf");
    let commands = dir.path().join("steps.json");
    std::fs::write(
        &commands,
        format!(
            r#"{{"author": "Driver", "date": "D:20261124101500Z", "steps": [
                {{"op": "open", "path": "work.pdf"}},
                {edits},
                {expectations},
                {{"op": "save_as", "path": "marked.pdf"}}
            ]}}"#,
            edits = rotated_steps(),
            expectations = rotated_expectations(),
        ),
    )
    .unwrap();
    let out = ok(plumb().arg("run").arg(&commands));
    assert!(
        out.contains("wrote 23 added, 0 modified, 0 deleted"),
        "{out}"
    );
    assert!(out.contains("\"Trade\" is column trade"), "{out}");
    assert!(
        out.lines()
            .any(|l| l.contains("markup_text Highlight H2 on page 2: 1 line(s), 9 character(s)")),
        "{out}"
    );
    assert_eq!(std::fs::read(&pdf).unwrap(), original, "the original");
    assert!(std::fs::read(&marked).unwrap().starts_with(&original));

    // A fresh process reads the saved file: the same counts, kinds, rotation and text.
    let reopen = dir.path().join("reopen.json");
    std::fs::write(
        &reopen,
        format!(
            r#"{{"steps": [
                {{"op": "open", "path": "marked.pdf"}},
                {{"op": "define_column", "title": "Trade", "kind": "Text"}},
                {{"op": "define_column", "title": "Cost", "kind": "Number"}},
                {expectations},
                {{"op": "expect_annot", "id": "T1",
                  "columns": {{"trade": "Drainage", "cost": "1250.5"}}}},
                {{"op": "expect", "count": 23, "modified": false}}
            ]}}"#,
            expectations = rotated_expectations(),
        ),
    )
    .unwrap();
    let out = ok(plumb().arg("run").arg(&reopen));
    assert!(out.contains("open 4 pages, 23 annotations"), "{out}");
    assert_eq!(
        annotation_counts(&ok(plumb().arg("info").arg(&marked))),
        [10, 4, 5, 4]
    );
    let listing = ok(plumb().arg("annots").arg(&marked));
    let lines = annot_lines(&listing);
    for p in 1..=4 {
        let on_page = |kind: &str| {
            lines
                .iter()
                .filter(|l| l.starts_with(&format!("page {p}: {kind} ")))
                .count()
        };
        assert_eq!(on_page("Highlight"), 1, "{listing}");
        assert!(on_page("FreeText") >= 1, "{listing}");
    }
    assert_eq!(listing.lines().last(), Some("23 annotations on 4 pages"));

    // The list, exported three ways.
    let columns = dir.path().join("columns.json");
    std::fs::write(
        &columns,
        r#"{"version": 1, "columns": [
            {"id": "trade", "title": "Trade", "kind": "Text"},
            {"id": "cost", "title": "Cost", "kind": "Number"}
        ]}"#,
    )
    .unwrap();
    let (csv, xlsx, xfdf) = (
        dir.path().join("list.csv"),
        dir.path().join("list.xlsx"),
        dir.path().join("marks.xfdf"),
    );
    let out = ok(plumb()
        .arg("export")
        .arg(&marked)
        .arg("--csv")
        .arg(&csv)
        .arg("--xlsx")
        .arg(&xlsx)
        .arg("--xfdf")
        .arg(&xfdf)
        .arg("--columns")
        .arg(&columns));
    // 15 markups: four text boxes, four highlights, callout, typewriter text, strike-out,
    // squiggle, underline, stamp and picture. Replies, statuses and the popup are not rows.
    assert!(out.contains("csv: 15 markups of 15 to "), "{out}");
    assert!(out.contains("xlsx: 15 markups of 15 to "), "{out}");
    assert!(out.contains("xfdf: 22 annotations to "), "{out}");
    assert!(out.contains("left out pic: ImageStamp"), "{out}");

    let text = std::fs::read_to_string(&csv).unwrap();
    let text = text
        .strip_prefix('\u{feff}')
        .expect("a byte-order mark for Excel");
    let mut records = text.split("\r\n");
    let header = records.next().unwrap();
    for title in ["Subject", "Author", "Comment", "Status", "Trade", "Cost"] {
        assert!(header.split(',').any(|h| h == title), "{title} in {header}");
    }
    let field = |record: &str, title: &str| -> String {
        // No field of these rows holds a comma inside quotes except the one checked below.
        let i = header.split(',').position(|h| h == title).unwrap();
        record.split(',').nth(i).unwrap_or_default().to_string()
    };
    let rows: Vec<&str> = records.filter(|r| !r.is_empty()).collect();
    let row_of = |comment: &str| -> &str {
        rows.iter()
            .find(|r| field(r, "Comment") == comment)
            .unwrap_or_else(|| panic!("no row for {comment:?} in\n{text}"))
    };
    let t1 = row_of("Sheet 1 note");
    assert_eq!(field(t1, "Status"), "Accepted");
    assert_eq!(field(t1, "Author"), "Driver");
    assert_eq!(field(t1, "Trade"), "Drainage");
    assert_eq!(field(t1, "Cost"), "1250.5");
    assert_eq!(field(row_of("ROTATE 90"), "Subject"), "Highlight");
    assert_eq!(field(row_of("ROTATE 270"), "Subject"), "Highlight");
    // A value with a comma and quotes is quoted, with the quotes doubled.
    assert!(text.contains(r#""Kerbs, ""dropped""""#), "{text}");
    // The replies are listed under their markup.
    assert!(
        text.contains("Agreed.") && text.contains("Thanks."),
        "{text}"
    );
    assert!(!text.contains("Withdrawn"), "the deleted reply is gone");

    let workbook = std::fs::read(&xlsx).unwrap();
    assert!(
        workbook.starts_with(b"PK\x03\x04"),
        "an .xlsx is a zip package"
    );
    assert!(workbook.len() > 2000);

    // The XFDF reads back as the same annotations (the picture aside).
    let xml = std::fs::read_to_string(&xfdf).unwrap();
    assert!(xml.contains("<xfdf") && xml.contains("marked.pdf"), "{xml}");
    let imported = pl_doc::pl_takeoff::import_xfdf(&xml).unwrap();
    assert!(imported.skipped.is_empty(), "{:?}", imported.skipped);
    assert_eq!(imported.annotations.len(), 22);
    assert_eq!(imported.href.as_deref(), Some("marked.pdf"));
    let by_id = |id: &str| {
        imported
            .annotations
            .iter()
            .find(|a| a.id.as_str() == id)
            .unwrap_or_else(|| panic!("{id} is not in the XFDF"))
    };
    assert_eq!(by_id("H3").meta.contents.as_deref(), Some("ROTATE 180"));
    assert_eq!(by_id("H3").page, 2);
    assert_eq!(
        by_id("T2").meta.contents.as_deref(),
        Some("Sheet 2 note, revised")
    );
    assert_eq!(by_id("T2").rotation(), 90);
    assert_eq!(
        by_id("R2").meta.reply_to.as_ref().map(|r| r.as_str()),
        Some("R1")
    );
    assert!(by_id("T1").popup.is_some());

    // One output is enough; the others are not written.
    let only = dir.path().join("only.csv");
    let out = ok(plumb().arg("export").arg(&marked).arg("--csv").arg(&only));
    assert_eq!(out.lines().count(), 1, "{out}");
    let plain = std::fs::read_to_string(&only).unwrap();
    assert!(
        !plain.lines().next().unwrap().contains("Trade"),
        "no custom columns without --columns"
    );
    // A file with no annotations exports an empty list.
    let empty = dir.path().join("empty.csv");
    let out = ok(plumb()
        .arg("export")
        .arg(fixture("plain"))
        .arg("--csv")
        .arg(&empty));
    assert!(out.contains("csv: 0 markups of 0 to "), "{out}");
    assert_eq!(
        std::fs::read_to_string(&empty)
            .unwrap()
            .matches("\r\n")
            .count(),
        1
    );
}

#[test]
fn export_says_what_is_missing() {
    let dir = tempfile::tempdir().unwrap();
    let r = run(plumb().arg("export").arg(fixture("annotations")));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("nothing to export"), "{}", r.stderr);
    assert!(r.stdout.is_empty());

    let r = run(plumb()
        .arg("export")
        .arg(fixtures().join("missing.pdf"))
        .arg("--csv")
        .arg(dir.path().join("x.csv")));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("missing.pdf"), "{}", r.stderr);
    assert!(!dir.path().join("x.csv").exists());

    let bad = dir.path().join("columns.json");
    std::fs::write(&bad, "{ not json").unwrap();
    let r = run(plumb()
        .arg("export")
        .arg(fixture("annotations"))
        .arg("--csv")
        .arg(dir.path().join("x.csv"))
        .arg("--columns")
        .arg(&bad));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("columns.json"), "{}", r.stderr);

    // An output that cannot be written is an error that names it.
    let r = run(plumb()
        .arg("export")
        .arg(fixture("annotations"))
        .arg("--xfdf")
        .arg(dir.path().join("no-such-folder").join("x.xfdf")));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("x.xfdf"), "{}", r.stderr);

    // The fixture's own markups: three can be described in XFDF, and the foreign text box
    // too once it is read as one.
    let xfdf = dir.path().join("a.xfdf");
    let out = ok(plumb()
        .arg("export")
        .arg(fixture("annotations"))
        .arg("--xfdf")
        .arg(&xfdf));
    assert!(out.starts_with("xfdf: "), "{out}");
    assert!(std::fs::read_to_string(&xfdf)
        .unwrap()
        .contains("fixture-square-1"));
}

/// A folder under the workspace's `target` for copies of the real sample: the sample is
/// never copied anywhere else.
fn sample_work_dir() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("tmp");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(std::path::absolute(base).unwrap()).unwrap()
}

#[test]
fn run_marks_text_and_places_text_boxes_and_stamps_on_the_real_sample() {
    let Some(sample) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let size_before = std::fs::metadata(&sample).unwrap().len();
    let before = annotation_counts(&ok(plumb().arg("info").arg(&sample)));
    let dir = sample_work_dir();
    let marked = dir.path().join("marked.pdf");

    // A word of sheet 1 to mark. The sheet's text is never printed: not here, and not by a
    // failing assertion.
    let r = run(plumb().arg("text").arg(&sample).arg("--page").arg("1"));
    assert_eq!(r.code, 0, "plumb text failed on the sample");
    let word = r
        .stdout
        .split_whitespace()
        .find(|w| w.len() >= 6 && w.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_string);
    drop(r);
    let mark_step = match &word {
        Some(word) => format!(
            r#"{{"op": "markup_text", "id": "found", "kind": "Highlight", "page": 1,
                "find": {word:?}}},
               {{"op": "expect_annot", "id": "found", "kind": "Highlight", "quads": 1,
                "author": "Driver"}},"#
        ),
        None => {
            eprintln!("SAMPLE TEXT SKIPPED: sheet 1 has no plain word to mark");
            String::new()
        }
    };
    let commands = dir.path().join("sample.json");
    std::fs::write(
        &commands,
        format!(
            r#"{{"author": "Driver", "steps": [
                {{"op": "open", "path": {sample}}},
                {mark_step}
                {{"op": "add", "id": "box", "kind": "FreeText", "page": 1,
                  "rect": [200, 200, 420, 260], "text": "Checked against the survey."}},
                {{"op": "add", "id": "lead", "kind": "FreeText", "intent": "callout", "page": 1,
                  "rect": [460, 300, 620, 340], "tip": [400, 420], "text": "See detail"}},
                {{"op": "add", "id": "stamp", "kind": "Stamp", "page": 2,
                  "rect": [200, 200, 380, 260], "stamp": "draft"}},
                {{"op": "status", "id": "box", "state": "Completed"}},
                {{"op": "reply", "id": "box", "reply_id": "re", "text": "Noted."}},
                {{"op": "check", "id": "stamp"}},
                {{"op": "expect_annot", "id": "box", "kind": "FreeText", "status": "Completed",
                  "replies": 1}},
                {{"op": "save_as", "path": {marked}}}
            ]}}"#,
            sample = json_path(&sample),
            marked = json_path(&marked),
        ),
    )
    .unwrap();
    let r = run(plumb().arg("run").arg(&commands));
    assert_eq!(r.code, 0, "plumb run failed on the sample: {}", r.stderr);
    let found = usize::from(word.is_some());

    assert_eq!(std::fs::metadata(&sample).unwrap().len(), size_before);
    let after = annotation_counts(&ok(plumb().arg("info").arg(&marked)));
    assert_eq!(after.len(), before.len(), "page count");
    // Sheet 1: two text boxes, a status, a reply and the highlight. Sheet 2: the stamp and
    // its checkmark.
    assert_eq!(after[0], before[0] + 4 + found, "sheet 1");
    assert_eq!(after[1], before[1] + 2, "sheet 2");
    assert_eq!(after[2..], before[2..], "the other sheets");

    // The list of the marked copy, exported beside it.
    let (csv, xfdf) = (dir.path().join("list.csv"), dir.path().join("marks.xfdf"));
    let r = run(plumb()
        .arg("export")
        .arg(&marked)
        .arg("--csv")
        .arg(&csv)
        .arg("--xfdf")
        .arg(&xfdf));
    assert_eq!(
        r.code, 0,
        "plumb export failed on the marked sample: {}",
        r.stderr
    );
    let rows = 3 + found;
    assert!(
        r.stdout
            .contains(&format!("csv: {rows} markups of {rows} to ")),
        "the csv line"
    );
    let list = std::fs::read_to_string(&csv).unwrap();
    assert!(list.contains("Checked against the survey.") && list.contains("Completed"));
    let imported =
        pl_doc::pl_takeoff::import_xfdf(&std::fs::read_to_string(&xfdf).unwrap()).unwrap();
    assert_eq!(imported.annotations.len(), 6 + found);
}

// ---- measurement -----------------------------------------------------------------------------

#[test]
fn run_measures_and_plumb_measure_lists_the_values_and_scales() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("sheet.pdf");
    std::fs::copy(fixture("user_unit"), &pdf).unwrap();
    let commands = dir.path().join("measure.json");
    // A /UserUnit 2 page: 72 units are 2 in of paper. Calibrated as 144 units = 40 ft, a
    // 72-unit line is 20 ft; inside the viewport at 1" = 5' it is 10 ft.
    std::fs::write(
        &commands,
        r#"{
          "author": "Driver",
          "date": "D:20261124101500Z",
          "steps": [
            {"op": "open", "path": "sheet.pdf"},
            {"op": "calibrate", "page": 1, "from": [20, 20], "to": [164, 20],
             "length": "40'", "unit": "ft"},
            {"op": "expect_scale", "page": 1, "ratio": "1 in = 10 ft", "viewports": []},
            {"op": "measure", "id": "run", "kind": "Length", "page": 1,
             "points": [[20, 40], [92, 40]]},
            {"op": "expect_measure", "id": "run", "text": "20.00'", "length": 6.096},
            {"op": "add_viewport", "page": 1, "name": "Detail", "bbox": [150, 150, 300, 300],
             "scale": "1\" = 5'"},
            {"op": "measure", "id": "detail", "kind": "Length", "page": 1,
             "points": [[160, 200], [232, 200]]},
            {"op": "expect_measure", "id": "detail", "text": "10.00'", "viewport": "Detail",
             "ratio": "1 in = 5 ft"},
            {"op": "measure", "id": "pad", "kind": "Area", "page": 1,
             "points": [[20, 60], [92, 60], [92, 132], [20, 132]]},
            {"op": "cutout", "id": "pad", "points": [[30, 70], [66, 70], [66, 106], [30, 106]]},
            {"op": "expect_measure", "id": "pad", "text": "300.00 sq ft"},
            {"op": "depth", "id": "pad", "depth": "1'"},
            {"op": "expect_measure", "id": "pad", "kind": "Volume", "volume": 8.49505,
             "tolerance": 1e-5},
            {"op": "measure", "id": "doors", "kind": "Count", "page": 1,
             "points": [[100, 100]], "symbol": "Hexagon"},
            {"op": "count", "id": "doors", "add": [110, 100]},
            {"op": "count", "id": "doors", "add": [120, 100]},
            {"op": "expect_measure", "id": "doors", "count": 3, "text": "3"},
            {"op": "set_scale", "page": 1, "scale": "1\" = 20'"},
            {"op": "expect_measure", "id": "run", "text": "40.00'"},
            {"op": "expect_measure", "id": "detail", "text": "10.00'"},
            {"op": "undo"},
            {"op": "expect_measure", "id": "run", "text": "20.00'"},
            {"op": "save"}
          ]
        }"#,
    )
    .unwrap();
    let out = ok(plumb().arg("run").arg(&commands));
    assert!(out.contains("calibrate page 1: 1 in = 10 ft"), "{out}");
    assert!(
        out.contains("measure Length run on page 1: 20.00'"),
        "{out}"
    );
    assert!(out.trim_end().ends_with("23 steps done"), "{out}");

    let listing = ok(plumb().arg("measure").arg(&pdf));
    let lines: Vec<&str> = listing.lines().collect();
    assert_eq!(lines[0], "page 1: scale \"1 in = 10 ft\"", "{listing}");
    assert_eq!(
        lines[1], "page 1: viewport \"Detail\" [150 150 300 300] scale \"1 in = 5 ft\"",
        "{listing}"
    );
    let line = |id: &str| {
        *lines
            .iter()
            .find(|l| l.ends_with(&format!(" id {id}")))
            .unwrap_or_else(|| panic!("no {id} in:\n{listing}"))
    };
    assert_eq!(
        line("run"),
        "page 1: Length \"20.00'\" (6.096 m) scale \"1 in = 10 ft\" id run"
    );
    assert_eq!(
        line("detail"),
        "page 1: Length \"10.00'\" (3.048 m) scale \"1 in = 5 ft\" in viewport \"Detail\" id detail"
    );
    assert!(line("pad").starts_with("page 1: Volume "), "{listing}");
    // A count carries the page's scale too (Revu's /BBMeasure).
    assert_eq!(
        line("doors"),
        "page 1: Count \"3\" (3) scale \"1 in = 10 ft\" id doors"
    );
    assert_eq!(lines.last(), Some(&"4 measurements"));

    // A file with no scale and no measurement.
    let listing = ok(plumb().arg("measure").arg(fixture("plain")));
    assert_eq!(listing.trim_end(), "0 measurements");
}

/// The numbers of one column of a CSV export, by its heading.
fn csv_column(csv: &str, heading: &str) -> Vec<f64> {
    let mut lines = csv.lines();
    let header: Vec<&str> = lines.next().unwrap_or_default().split(',').collect();
    let index = header
        .iter()
        .position(|h| h.trim_matches('"') == heading)
        .unwrap_or_else(|| panic!("no {heading} in {header:?}"));
    lines
        .filter_map(|l| l.split(',').nth(index))
        .filter_map(|v| v.trim_matches('"').replace(' ', "").parse::<f64>().ok())
        .collect()
}

#[test]
fn export_takeoff_writes_the_quantity_columns_in_the_chosen_units() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("sheet.pdf");
    std::fs::copy(fixture("plain"), &pdf).unwrap();
    let commands = dir.path().join("measure.json");
    // 1 in = 10 ft: 72 pt is 10 ft, 144 pt is 20 ft; a 72 pt square is 100 sq ft.
    std::fs::write(
        &commands,
        r#"{
          "steps": [
            {"op": "open", "path": "sheet.pdf"},
            {"op": "set_scale", "page": 1, "scale": "1\" = 10'"},
            {"op": "measure", "id": "a", "kind": "Length", "page": 1,
             "points": [[100, 400], [172, 400]]},
            {"op": "measure", "id": "b", "kind": "Length", "page": 1,
             "points": [[100, 450], [244, 450]]},
            {"op": "measure", "id": "c", "kind": "Area", "page": 1,
             "points": [[300, 300], [372, 300], [372, 372], [300, 372]]},
            {"op": "save"}
          ]
        }"#,
    )
    .unwrap();
    ok(plumb().arg("run").arg(&commands));

    let csv = dir.path().join("takeoff.csv");
    let out = ok(plumb()
        .arg("export")
        .arg(&pdf)
        .arg("--takeoff")
        .arg("--csv")
        .arg(&csv));
    assert!(out.contains("csv: 3 markups"), "{out}");
    let text = std::fs::read_to_string(&csv).unwrap();
    let lengths = csv_column(&text, "Length (ft)");
    for want in [10.0, 20.0] {
        assert!(
            lengths.iter().any(|v| (v - want).abs() < 1e-9),
            "{want} ft in {lengths:?}\n{text}"
        );
    }
    let areas = csv_column(&text, "Area (sq ft)");
    assert!(areas.iter().any(|v| (v - 100.0).abs() < 1e-9), "{areas:?}");

    // The same in metric units.
    let csv = dir.path().join("metric.csv");
    ok(plumb()
        .arg("export")
        .arg(&pdf)
        .arg("--takeoff")
        .arg("--units")
        .arg("metric")
        .arg("--csv")
        .arg(&csv));
    let text = std::fs::read_to_string(&csv).unwrap();
    let lengths = csv_column(&text, "Length (m)");
    assert!(
        lengths.iter().any(|v| (v - 6.096).abs() < 0.006),
        "{lengths:?}\n{text}"
    );
    // A layout file and the takeoff layout are one or the other.
    let r = run(plumb()
        .arg("export")
        .arg(&pdf)
        .arg("--takeoff")
        .arg("--layout")
        .arg(&commands)
        .arg("--csv")
        .arg(&csv));
    assert_eq!(r.code, 2, "{}", r.stderr);
}

/// The takeoff CSV: subtotals per subject and a total like the workbook, the angle, radius,
/// diameter and base area columns, a volume kept out of Area and a count per row.
#[test]
fn export_takeoff_csv_has_subtotals_a_total_and_the_new_columns() {
    let dir = tempfile::tempdir().unwrap();
    let pdf = dir.path().join("sheet.pdf");
    std::fs::copy(fixture("plain"), &pdf).unwrap();
    let commands = dir.path().join("measure.json");
    // 1 in = 10 ft: 72 pt is 10 ft; a 72 pt square is 100 sq ft.
    std::fs::write(
        &commands,
        r#"{
          "steps": [
            {"op": "open", "path": "sheet.pdf"},
            {"op": "set_scale", "page": 1, "scale": "1\" = 10'"},
            {"op": "measure", "id": "a", "kind": "Length", "page": 1,
             "points": [[100, 400], [172, 400]]},
            {"op": "measure", "id": "b", "kind": "Length", "page": 1,
             "points": [[100, 450], [244, 450]]},
            {"op": "measure", "id": "c", "kind": "Area", "page": 1,
             "points": [[300, 300], [372, 300], [372, 372], [300, 372]]},
            {"op": "measure", "id": "v", "kind": "Volume", "page": 1,
             "points": [[400, 300], [472, 300], [472, 372], [400, 372]], "depth": "3'"},
            {"op": "measure", "id": "g", "kind": "Angle", "page": 1,
             "points": [[100, 200], [100, 250], [150, 250]]},
            {"op": "measure", "id": "r", "kind": "Radius", "page": 1,
             "points": [[200, 200], [236, 200]]},
            {"op": "measure", "id": "d", "kind": "Diameter", "page": 1,
             "points": [[300, 200], [372, 200]]},
            {"op": "measure", "id": "n1", "kind": "Count", "page": 1,
             "points": [[100, 100]], "symbol": "Ring"},
            {"op": "measure", "id": "n2", "kind": "Count", "page": 1,
             "points": [[120, 100]], "symbol": "Ring"},
            {"op": "save"}
          ]
        }"#,
    )
    .unwrap();
    ok(plumb().arg("run").arg(&commands));

    let csv = dir.path().join("takeoff.csv");
    ok(plumb()
        .arg("export")
        .arg(&pdf)
        .arg("--takeoff")
        .arg("--units")
        .arg("imperial")
        .arg("--csv")
        .arg(&csv));
    let text = std::fs::read_to_string(&csv).unwrap();
    let text = text.trim_start_matches('\u{FEFF}');
    let lines: Vec<Vec<&str>> = text.lines().map(|l| l.split(',').collect()).collect();
    let header = &lines[0];
    let at = |h: &str| {
        header
            .iter()
            .position(|x| *x == h)
            .unwrap_or_else(|| panic!("no {h} in {header:?}"))
    };
    for h in [
        "Angle (\u{b0})",
        "Radius (ft)",
        "Diameter (ft)",
        "Base Area (sq ft)",
        "Volume (cu yd)",
    ] {
        at(h);
    }
    // A subtotal after each subject's rows, and the total last.
    let subtotals = lines
        .iter()
        .filter(|l| l[at("Subject")].starts_with("Subtotal: "))
        .count();
    assert!(subtotals >= 2, "{text}");
    let total = lines.last().unwrap();
    assert_eq!(total[at("Subject")], "Total", "{text}");
    // 10 ft + 20 ft: the diameter is not a length.
    assert_eq!(total[at("Length (ft)")], "30.00", "{text}");
    // The area alone; the slab's 100 sq ft is its base area.
    assert_eq!(total[at("Area (sq ft)")], "100.00", "{text}");
    assert_eq!(total[at("Base Area (sq ft)")], "100.00", "{text}");
    assert_eq!(total[at("Count (ea)")], "2", "{text}");
    assert_eq!(total[at("Angle (\u{b0})")], "", "{text}");
    assert_eq!(total[at("Diameter (ft)")], "", "{text}");
    // Two counts made one after the other are two counts, alike as they look: each is one
    // row of type Count, however many annotations it is written as.
    let counts: Vec<&Vec<&str>> = lines.iter().filter(|l| l[at("Type")] == "Count").collect();
    assert_eq!(counts.len(), 2, "{text}");
    assert!(counts.iter().all(|c| c[at("Count (ea)")] == "1"), "{text}");
    let row = |value: &str, column: &str| {
        lines
            .iter()
            .any(|l| !l[at("Subject")].starts_with("Subtotal") && l[at(column)] == value)
    };
    assert!(row("90.0", "Angle (\u{b0})"), "{text}");
    assert!(row("5.00", "Radius (ft)") && row("10.00", "Diameter (ft)"), "{text}");
    assert!(row("10.00", "Diameter (ft)"), "{text}");
}

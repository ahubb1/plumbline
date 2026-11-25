//! Runs the built `plumb` binary for page editing, bookmarks, links, flatten and sets
//! (milestone M8) on pl-testkit's structure fixtures. `plumb` is a console program: nothing here
//! opens a window (the child is started with CREATE_NO_WINDOW as well).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use pl_testkit::{flatten_set, signed_stub, structure_main, structure_source, LinkKind};

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

/// A `plumb` command that can find PDFium (see tests/cli.rs).
fn plumb() -> Command {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_plumb"));
    cmd.env_remove("PLUMBLINE_PDFIUM_DLL");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    if cfg!(debug_assertions) {
        cmd.env("PLUMBLINE_PDFIUM_DLL", vendor_dll());
    } else {
        cmd.arg("--pdfium").arg(vendor_dll());
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

/// A folder with the structure fixtures: main.pdf (8 pages), source.pdf (4), signed.pdf,
/// flatten.pdf.
struct Files {
    dir: tempfile::TempDir,
}

impl Files {
    fn new() -> Files {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("main.pdf"), structure_main().0.bytes).unwrap();
        std::fs::write(dir.path().join("source.pdf"), structure_source().0.bytes).unwrap();
        std::fs::write(dir.path().join("signed.pdf"), signed_stub().bytes).unwrap();
        std::fs::write(dir.path().join("flatten.pdf"), flatten_set().0.bytes).unwrap();
        Files { dir }
    }

    fn path(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }
}

/// One line of `plumb pages <pdf>`.
#[derive(Debug, Clone, PartialEq)]
struct PageRow {
    label: Option<String>,
    rotate: i32,
    annotations: usize,
    content: String,
}

fn field<'a>(line: &'a str, key: &str) -> &'a str {
    let rest = line
        .split(&format!("{key} "))
        .nth(1)
        .unwrap_or_else(|| panic!("no {key} in {line}"));
    rest.split(',').next().unwrap().trim()
}

fn page_rows(listing: &str) -> Vec<PageRow> {
    listing
        .lines()
        .filter(|l| l.starts_with("page "))
        .map(|l| PageRow {
            label: l
                .split("label \"")
                .nth(1)
                .map(|r| r.split('"').next().unwrap().to_string()),
            rotate: field(l, "rotate").parse().unwrap(),
            annotations: field(l, "annotations").parse().unwrap(),
            content: field(l, "content").to_string(),
        })
        .collect()
}

fn list(pdf: &Path) -> Vec<PageRow> {
    page_rows(&ok(plumb().arg("pages").arg(pdf)))
}

fn page_text(pdf: &Path, page: usize) -> String {
    ok(plumb()
        .arg("text")
        .arg(pdf)
        .arg("--page")
        .arg(page.to_string()))
}

/// The fingerprint of a new blank page: one empty content stream.
const BLANK: &str = "af63bd4c8601b7df";

/// What a page of the result should be: its fingerprint and rotation.
#[derive(Debug, Clone, PartialEq)]
struct Model {
    content: String,
    rotate: i32,
}

fn model_of(rows: &[PageRow]) -> Vec<Model> {
    rows.iter()
        .map(|r| Model {
            content: r.content.clone(),
            rotate: r.rotate,
        })
        .collect()
}

#[test]
fn pages_lists_every_page_with_its_label_rotation_and_fingerprint() {
    let files = Files::new();
    let (_, truth) = structure_main();
    let rows = list(&files.path("main.pdf"));
    assert_eq!(rows.len(), 8);
    let labels: Vec<String> = rows.iter().map(|r| r.label.clone().unwrap()).collect();
    assert_eq!(labels, truth.labels);
    // The fixture's rotated pages: page 2 by 90, the inherited pages 5 and 6 by 180.
    let rotations: Vec<i32> = rows.iter().map(|r| r.rotate).collect();
    assert_eq!(rotations, vec![0, 90, 0, 0, 180, 180, 0, 0]);
    // Every page draws its own marker, so every fingerprint differs.
    let mut seen: Vec<&str> = rows.iter().map(|r| r.content.as_str()).collect();
    seen.sort_unstable();
    seen.dedup();
    assert_eq!(seen.len(), 8);
    // The marker really is on its page.
    assert!(page_text(&files.path("main.pdf"), 3).contains(&truth.markers[2]));
}

#[test]
fn page_operations_match_the_reference_list_by_fingerprint() {
    let files = Files::new();
    let main = model_of(&list(&files.path("main.pdf")));
    let source = model_of(&list(&files.path("source.pdf")));
    let out = files.path("out.pdf");
    let stdout = ok(plumb()
        .arg("pages")
        .arg(files.path("main.pdf"))
        .args(["delete", "2", "rotate", "1,3", "90", "move", "7", "to", "1"])
        .args(["insert-from", "3"])
        .arg(files.path("source.pdf"))
        .args([
            "pages=2-3",
            "insert-blank",
            "end",
            "size=letter",
            "rotate=-90",
        ])
        .args(["-o"])
        .arg(&out));
    assert!(stdout.contains("delete 2: 1 page removed"), "{stdout}");
    assert!(stdout.contains("wrote "), "{stdout}");
    assert!(stdout.contains(": 10 pages"), "{stdout}");

    // The same operations on a plain list.
    let mut m = main.clone();
    m.remove(1);
    for i in [0, 2] {
        m[i].rotate = (m[i].rotate + 90).rem_euclid(360);
    }
    let moved = m.remove(6);
    m.insert(0, moved);
    m.insert(2, source[1].clone());
    m.insert(3, source[2].clone());
    let blank = Model {
        content: BLANK.into(),
        rotate: 270,
    };
    m.push(blank);
    assert_eq!(model_of(&list(&out)), m);
    // The source is unchanged and the result's markers are where the model puts them.
    assert_eq!(model_of(&list(&files.path("main.pdf"))), main);
    let (_, truth) = structure_main();
    let (_, src_truth) = structure_source();
    assert!(page_text(&out, 1).contains(&truth.markers[7]));
    assert!(page_text(&out, 3).contains(&src_truth.markers[1]));
    assert!(page_text(&out, 2).contains(&truth.markers[0]));
}

/// xorshift64*, so a failure can be replayed from its seed.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn random_page_operation_sequences_match_the_reference_list() {
    let files = Files::new();
    let main = model_of(&list(&files.path("main.pdf")));
    let source = model_of(&list(&files.path("source.pdf")));
    let blank = Model {
        content: BLANK.into(),
        rotate: 0,
    };
    for seed in 1..=12u64 {
        let mut rng = Rng(seed.wrapping_mul(0x9e37_79b9_7f4a_7c15) | 1);
        let mut m = main.clone();
        let mut words: Vec<String> = Vec::new();
        for _ in 0..(3 + rng.below(4)) {
            let n = m.len();
            match rng.below(7) {
                0 if n > 1 => {
                    let p = rng.below(n);
                    m.remove(p);
                    words.extend(["delete".into(), (p + 1).to_string()]);
                }
                1 => {
                    let p = rng.below(n);
                    let d = [90, 180, 270, -90][rng.below(4)];
                    m[p].rotate = (m[p].rotate + d).rem_euclid(360);
                    words.extend(["rotate".into(), (p + 1).to_string(), d.to_string()]);
                }
                2 if n > 1 => {
                    let p = rng.below(n);
                    let to = rng.below(n);
                    let page = m.remove(p);
                    m.insert(to, page);
                    words.extend(["move".into(), (p + 1).to_string(), (to + 1).to_string()]);
                }
                3 => {
                    let at = rng.below(n + 1);
                    let s = rng.below(4);
                    m.insert(at, source[s].clone());
                    words.extend([
                        "insert-from".into(),
                        (at + 1).to_string(),
                        files.path("source.pdf").display().to_string(),
                        format!("pages={}", s + 1),
                    ]);
                }
                4 => {
                    let at = rng.below(n + 1);
                    m.insert(at, blank.clone());
                    words.extend([
                        "insert-blank".into(),
                        (at + 1).to_string(),
                        "size=a4".into(),
                    ]);
                }
                5 if n > 2 => {
                    // Keep two pages, backwards.
                    let a = rng.below(n - 1);
                    let kept = vec![m[a + 1].clone(), m[a].clone()];
                    m = kept;
                    words.extend(["extract".into(), format!("{}-{}", a + 2, a + 1)]);
                }
                _ => {
                    let at = rng.below(n);
                    let s = rng.below(4);
                    m[at] = source[s].clone();
                    words.extend([
                        "replace".into(),
                        (at + 1).to_string(),
                        files.path("source.pdf").display().to_string(),
                        format!("pages={}", s + 1),
                    ]);
                }
            }
        }
        let out = files.path(&format!("random-{seed}.pdf"));
        let r = run(plumb()
            .arg("pages")
            .arg(files.path("main.pdf"))
            .args(&words)
            .arg("-o")
            .arg(&out));
        assert_eq!(
            r.code, 0,
            "seed {seed}: {words:?}\n{}\n{}",
            r.stdout, r.stderr
        );
        assert_eq!(model_of(&list(&out)), m, "seed {seed}: {words:?}");
    }
}

#[test]
fn split_and_merge_write_the_right_pages() {
    let files = Files::new();
    let main = model_of(&list(&files.path("main.pdf")));
    let source = model_of(&list(&files.path("source.pdf")));
    let stdout = ok(plumb()
        .arg("pages")
        .arg(files.path("main.pdf"))
        .args(["split", "1-3,4-"])
        .arg("-o")
        .arg(files.path("part.pdf")));
    assert!(
        stdout.contains("part-1.pdf: pages 1-3 (3 pages)"),
        "{stdout}"
    );
    assert!(
        stdout.contains("part-2.pdf: pages 4-8 (5 pages)"),
        "{stdout}"
    );
    assert_eq!(model_of(&list(&files.path("part-1.pdf"))), main[..3]);
    assert_eq!(model_of(&list(&files.path("part-2.pdf"))), main[3..]);

    // Operations before a split apply first; {n} names the parts.
    ok(plumb()
        .arg("pages")
        .arg(files.path("main.pdf"))
        .args(["delete", "1-4", "split", "every=3"])
        .arg("-o")
        .arg(files.path("p{n}.pdf")));
    assert_eq!(model_of(&list(&files.path("p1.pdf"))), main[4..7]);
    assert_eq!(model_of(&list(&files.path("p2.pdf"))), main[7..]);

    let merged = files.path("merged.pdf");
    let stdout = ok(plumb()
        .arg("pages")
        .arg(files.path("main.pdf"))
        .arg("merge")
        .arg(files.path("source.pdf"))
        .arg("-o")
        .arg(&merged));
    assert!(
        stdout.contains("bookmarks added") || stdout.contains("bookmark added"),
        "{stdout}"
    );
    let mut both = main.clone();
    both.extend(source.iter().cloned());
    assert_eq!(model_of(&list(&merged)), both);
    // The merge brings the source's bookmarks, leading to its pages at their new numbers.
    let (_, src_truth) = structure_source();
    let tree = ok(plumb().arg("outline").arg(&merged));
    for item in &src_truth.outline {
        let line = tree
            .lines()
            .find(|l| l.contains(&format!("{:?}", item.title)))
            .unwrap_or_else(|| panic!("no {:?} in\n{tree}", item.title));
        if let Some(p) = item.page {
            assert!(line.contains(&format!(" page {} ", p + 9)), "{line}");
        }
    }
}

#[test]
fn outline_prints_the_tree_and_edits_append_to_the_file() {
    let files = Files::new();
    let (_, truth) = structure_main();
    let main = files.path("main.pdf");
    let tree = ok(plumb().arg("outline").arg(&main));
    let lines: Vec<&str> = tree.lines().collect();
    assert_eq!(lines.len(), truth.outline.len() + 1, "{tree}");
    assert_eq!(
        *lines.last().unwrap(),
        format!("{} bookmarks", truth.outline.len())
    );
    for (line, item) in lines.iter().zip(&truth.outline) {
        assert!(line.starts_with(&"  ".repeat(item.level)), "{line}");
        assert!(!line.starts_with(&"  ".repeat(item.level + 1)), "{line}");
        assert!(line.contains(&format!("{:?}", item.title)), "{line}");
        match item.page {
            Some(p) => assert!(line.contains(&format!(" page {} ", p + 1)), "{line}"),
            None => assert!(!line.contains(" page "), "{line}"),
        }
        if item.count < 0 {
            assert!(line.contains("closed"), "{line}");
        }
    }

    let out = files.path("bookmarks.pdf");
    let stdout = ok(plumb()
        .arg("outline")
        .arg(&main)
        .args(["add", "1", "Cover sheet", "3", "at=72,700"])
        .args(["rename", "2", "Renamed"])
        .args(["dest", "2", "8"])
        .args(["add", "1.1", "Note", "none"])
        .arg("-o")
        .arg(&out));
    assert!(stdout.contains("wrote "), "{stdout}");
    let tree = ok(plumb().arg("outline").arg(&out));
    let first: Vec<&str> = tree.lines().take(3).collect();
    assert!(
        first[0].starts_with("1 \"Cover sheet\" -> page 3 at 72,700"),
        "{tree}"
    );
    assert!(
        first[1].starts_with("  1.1 \"Note\" -> no destination"),
        "{tree}"
    );
    assert!(
        first[2].starts_with("2 \"Renamed\" -> page 8 fit"),
        "{tree}"
    );
    assert!(
        tree.ends_with(&format!("{} bookmarks\n", truth.outline.len() + 2)),
        "{tree}"
    );
    // An incremental update: the original bytes are an exact prefix.
    let before = std::fs::read(&main).unwrap();
    let after = std::fs::read(&out).unwrap();
    assert!(after.len() > before.len() && after[..before.len()] == before[..]);

    // A bad path changes nothing.
    let r = run(plumb()
        .arg("outline")
        .arg(&main)
        .args(["delete", "40"])
        .arg("-o")
        .arg(files.path("never.pdf")));
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert!(r.stderr.contains("no outline item"), "{}", r.stderr);
    assert!(!files.path("never.pdf").exists());
}

fn kind_word(kind: LinkKind) -> &'static str {
    match kind {
        LinkKind::GoTo => "goto",
        LinkKind::GoToNamed => "goto-named",
        LinkKind::Uri => "uri",
        LinkKind::GoToR => "gotor",
        LinkKind::Launch => "launch",
        LinkKind::Named => "named",
    }
}

#[test]
fn links_list_every_kind_with_resolved_targets_and_follow_moved_pages() {
    let files = Files::new();
    let (_, truth) = structure_main();
    let main = files.path("main.pdf");
    let listing = ok(plumb().arg("links").arg(&main));
    let lines: Vec<&str> = listing.lines().filter(|l| l.starts_with("page ")).collect();
    assert_eq!(lines.len(), truth.links.len(), "{listing}");
    for t in &truth.links {
        let r = t.rect;
        let head = format!(
            "page {}: [{} {} {} {}] ",
            t.page_index + 1,
            r[0],
            r[1],
            r[2],
            r[3]
        );
        let line = *lines
            .iter()
            .find(|l| l.starts_with(&head))
            .unwrap_or_else(|| {
                panic!(
                    "no {head} in
{listing}"
                )
            });
        let rest = &line[head.len()..];
        assert!(
            rest == kind_word(t.kind) || rest.starts_with(&format!("{} ", kind_word(t.kind))),
            "{line}"
        );
        match t.kind {
            LinkKind::GoTo | LinkKind::GoToNamed => {
                let page = t.target_page.unwrap();
                assert!(line.contains(&format!("-> page {} ", page + 1)), "{line}");
                if let Some(pt) = t.target_point {
                    assert!(line.contains(&format!("at {},{}", pt[0], pt[1])), "{line}");
                }
            }
            LinkKind::GoToR => {
                assert!(line.contains(&format!("{:?}", t.value)), "{line}");
                if let Some(p) = t.target_page {
                    assert!(line.contains(&format!("-> page {} ", p + 1)), "{line}");
                    assert!(line.ends_with("of that file"), "{line}");
                }
            }
            LinkKind::Uri | LinkKind::Launch => {
                assert!(line.contains(&format!("{:?}", t.value)), "{line}");
                assert!(!line.contains("->"), "{line}");
            }
            LinkKind::Named => {
                assert!(line.contains(&t.value), "{line}");
                // NextPage leads to the page after, when there is one.
                if t.value == "NextPage" && t.page_index + 1 < 8 {
                    assert!(
                        line.contains(&format!("-> page {} ", t.page_index + 2)),
                        "{line}"
                    );
                }
            }
        }
    }

    // Reverse the pages: every local link still leads to the same page, at its new number.
    let reversed = files.path("reversed.pdf");
    ok(plumb()
        .arg("pages")
        .arg(&main)
        .args(["extract", "8-1", "-o"])
        .arg(&reversed));
    let listing = ok(plumb().arg("links").arg(&reversed));
    for t in truth
        .links
        .iter()
        .filter(|t| matches!(t.kind, LinkKind::GoTo | LinkKind::GoToNamed))
    {
        let from = 8 - t.page_index;
        let to = 8 - t.target_page.unwrap();
        let found = listing.lines().any(|l| {
            l.starts_with(&format!("page {from}: ")) && l.contains(&format!("-> page {to} "))
        });
        assert!(found, "page {from} -> {to} missing in\n{listing}");
    }
    // Bookmarks follow too.
    let tree = ok(plumb().arg("outline").arg(&reversed));
    for item in truth.outline.iter().filter(|i| i.page.is_some()) {
        let line = tree
            .lines()
            .find(|l| l.contains(&format!("{:?}", item.title)))
            .unwrap();
        assert!(
            line.contains(&format!(" page {} ", 8 - item.page.unwrap())),
            "{line}"
        );
    }
}

#[test]
fn deleting_pages_reports_and_drops_what_led_to_them() {
    let files = Files::new();
    let (_, truth) = structure_main();
    let main = files.path("main.pdf");
    // Delete every page some local link leads to (but keep page 1).
    let mut targets: Vec<usize> = truth
        .links
        .iter()
        .filter(|t| matches!(t.kind, LinkKind::GoTo | LinkKind::GoToNamed))
        .filter_map(|t| t.target_page)
        .filter(|p| *p != 0)
        .collect();
    targets.sort_unstable();
    targets.dedup();
    let pages: Vec<String> = targets.iter().map(|p| (p + 1).to_string()).collect();
    let out = files.path("deleted.pdf");
    let stdout = ok(plumb()
        .arg("pages")
        .arg(&main)
        .args(["delete", &pages.join(",")])
        .arg("-o")
        .arg(&out));
    assert!(
        stdout.contains("link removed") || stdout.contains("links removed"),
        "{stdout}"
    );
    let listing = ok(plumb().arg("links").arg(&out));
    assert!(!listing.contains("broken"), "{listing}");
    // What is left of the local links leads only to pages that are still there.
    let remaining = 8 - targets.len();
    for l in listing.lines().filter(|l| l.contains("] goto")) {
        let page: usize = l
            .split("-> page ")
            .nth(1)
            .unwrap()
            .split(' ')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        assert!(page <= remaining, "{l}");
    }
}

#[test]
fn a_signed_file_is_refused_unless_signatures_may_be_invalidated() {
    let files = Files::new();
    let signed = files.path("signed.pdf");
    let out = files.path("unsigned.pdf");
    let before = std::fs::read(&signed).unwrap();
    let listing = ok(plumb().arg("pages").arg(&signed));
    assert!(listing.contains("signed: "), "{listing}");
    let r = run(plumb()
        .arg("pages")
        .arg(&signed)
        .args(["rotate", "1", "90", "-o"])
        .arg(&out));
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert!(r.stderr.contains("--invalidate-signatures"), "{}", r.stderr);
    assert!(!out.exists());
    let r = run(plumb().arg("flatten").arg(&signed).arg("-o").arg(&signed));
    assert_eq!(r.code, 2);
    assert!(r.stderr.contains("signed"), "{}", r.stderr);
    assert_eq!(
        std::fs::read(&signed).unwrap(),
        before,
        "refused saves leave it alone"
    );

    let stdout = ok(plumb()
        .arg("pages")
        .arg(&signed)
        .args(["rotate", "1", "90", "--invalidate-signatures", "-o"])
        .arg(&out));
    assert!(
        stdout.contains("warning: signature invalidated"),
        "{stdout}"
    );
    assert_eq!(list(&out)[0].rotate, 90);
    // A bookmark edit is appended, so it is allowed on a signed file.
    ok(plumb()
        .arg("outline")
        .arg(&signed)
        .args(["add", "1", "Signed page", "1", "-o"])
        .arg(files.path("signed-bookmark.pdf")));
}

fn png(path: &Path) -> image::RgbaImage {
    image::open(path).expect("readable PNG").to_rgba8()
}

fn render(pdf: &Path, page: usize, png_path: &Path) -> image::RgbaImage {
    ok(plumb()
        .arg("render")
        .arg(pdf)
        .args(["--page", &page.to_string(), "-o"])
        .arg(png_path));
    png(png_path)
}

/// Pixels that differ by more than `level` in any channel.
fn differing(a: &image::RgbaImage, b: &image::RgbaImage, level: u8) -> usize {
    assert_eq!(a.dimensions(), b.dimensions());
    a.pixels()
        .zip(b.pixels())
        .filter(|(p, q)| (0..3).any(|c| p.0[c].abs_diff(q.0[c]) > level))
        .count()
}

#[test]
fn flatten_burns_in_the_visible_markups_and_keeps_the_render() {
    let files = Files::new();
    let (_, truth) = flatten_set();
    let input = files.path("flatten.pdf");
    let out = files.path("flat.pdf");
    let before = list(&input);
    let stdout = ok(plumb().arg("flatten").arg(&input).arg("-o").arg(&out));
    let flattened: usize = truth.visible.iter().sum();
    assert!(
        stdout.contains(&format!("{flattened} annotations flattened")),
        "{stdout}"
    );
    let after = list(&out);
    for (i, (b, a)) in before.iter().zip(&after).enumerate() {
        assert_eq!(a.rotate, b.rotate);
        // What stays (hidden, no-view, link) stays; the visible markups and their popups go.
        assert!(a.annotations <= truth.stay[i], "page {}: {a:?}", i + 1);
        assert!(
            a.annotations + truth.visible[i] <= b.annotations,
            "page {}",
            i + 1
        );
        assert_ne!(a.content, b.content, "page {} got the appearances", i + 1);
    }
    // The pages look as they did. PDFium leaves out an annotation's /CA when it draws the
    // appearance (pl-pdf's open issue 3), and flatten follows the standard, so the half-opaque
    // square differs; everything else matches.
    for page in 1..=4 {
        let a = render(&input, page, &files.path(&format!("before-{page}.png")));
        let b = render(&out, page, &files.path(&format!("after-{page}.png")));
        let total = (a.width() * a.height()) as usize;
        let diff = differing(&a, &b, 64);
        assert!(
            diff * 20 < total,
            "page {page}: {diff} of {total} pixels differ"
        );
    }

    // Only page 2, and for print: the other pages keep their annotations.
    let out2 = files.path("flat2.pdf");
    ok(plumb()
        .arg("flatten")
        .arg(&input)
        .args(["--pages", "2", "--print", "-o"])
        .arg(&out2));
    let after2 = list(&out2);
    for i in [0, 2, 3] {
        assert_eq!(after2[i], before[i], "page {} untouched", i + 1);
    }
    assert!(after2[1].annotations < before[1].annotations);
}

#[test]
fn sets_are_created_listed_and_searched() {
    let files = Files::new();
    let (_, truth) = structure_main();
    let (_, src_truth) = structure_source();
    let set = files.path("plans.plset");
    let stdout = ok(plumb()
        .args(["set", "create"])
        .arg(&set)
        .arg(files.path("main.pdf"))
        .arg(files.path("source.pdf"))
        .args(["--name", "Test plans"]));
    assert!(stdout.contains("2 files"), "{stdout}");
    let json = std::fs::read_to_string(&set).unwrap();
    assert!(
        json.contains("\"main.pdf\"") && json.contains("\"source.pdf\""),
        "{json}"
    );
    let r = run(plumb().args(["set", "create"]).arg(&set));
    assert_eq!(r.code, 2, "an existing set is not overwritten");

    let stdout = ok(plumb()
        .args(["set", "add"])
        .arg(&set)
        .arg(files.path("flatten.pdf"))
        .arg(files.path("main.pdf")));
    assert!(stdout.contains("already in the set"), "{stdout}");
    assert!(stdout.contains("added 1"), "{stdout}");
    let listing = ok(plumb().args(["set", "list"]).arg(&set));
    assert!(listing.contains("set: Test plans"), "{listing}");
    assert!(listing.contains("1: main.pdf (8 pages)"), "{listing}");
    assert!(listing.contains("2: source.pdf (4 pages)"), "{listing}");
    assert!(listing.contains("3: flatten.pdf (4 pages)"), "{listing}");

    // Each marker is found in its own file and on its own page only.
    for (file, marker, page) in [
        ("main.pdf", &truth.markers[4], 5),
        ("source.pdf", &src_truth.markers[2], 3),
    ] {
        let hits = ok(plumb().args(["set", "search"]).arg(&set).arg(marker));
        let lines: Vec<&str> = hits.lines().filter(|l| l.contains(" page ")).collect();
        assert_eq!(lines.len(), 1, "{hits}");
        assert!(
            lines[0].starts_with(&format!("{file} page {page}: [")),
            "{hits}"
        );
        assert!(lines[0].ends_with(marker.as_str()), "{hits}");
        assert!(
            hits.ends_with(
                "1 hit in 1 of 3 files
"
            ),
            "{hits}"
        );
    }
    // A prefix every main page has: eight hits, all in main.pdf.
    let hits = ok(plumb().args(["set", "search"]).arg(&set).arg("PLM-A-"));
    assert!(hits.ends_with("8 hits in 1 of 3 files\n"), "{hits}");
    let r = run(plumb()
        .args(["set", "search"])
        .arg(&set)
        .arg("no such words"));
    assert_eq!(r.code, 1, "{}", r.stdout);

    // After the pages move, the search finds the marker at its new place.
    ok(plumb()
        .arg("pages")
        .arg(files.path("source.pdf"))
        .args(["move", "3", "1", "-o"])
        .arg(files.path("source.pdf")));
    let hits = ok(plumb()
        .args(["set", "search"])
        .arg(&set)
        .arg(&src_truth.markers[2]));
    assert!(hits.starts_with("source.pdf page 1: ["), "{hits}");
}

#[test]
fn a_command_file_mixes_markup_and_page_steps() {
    let files = Files::new();
    let (_, truth) = structure_main();
    let (_, src_truth) = structure_source();
    let work = files.path("work.pdf");
    std::fs::copy(files.path("main.pdf"), &work).unwrap();
    let steps = files.path("steps.json");
    let mut expected_text = vec![String::new(); 9];
    expected_text[0] = truth.markers[7].clone();
    expected_text[2] = src_truth.markers[0].clone();
    let commands = format!(
        r#"{{ "author": "Tester", "steps": [
          {{ "op": "open", "path": "work.pdf" }},
          {{ "op": "add", "id": "box", "kind": "Square", "page": 1, "rect": [100, 100, 200, 200] }},
          {{ "op": "save" }},
          {{ "op": "pages", "ops": [
              {{ "do": "delete", "pages": "2" }},
              {{ "do": "move", "pages": "last", "to": 1 }},
              {{ "do": "insert_from", "at": 3, "file": "source.pdf", "pages": 1 }},
              {{ "do": "insert_blank", "at": "end", "size": [612, 792] }} ] }},
          {{ "op": "expect", "count": {on_p2}, "page": 2 }},
          {{ "op": "add", "kind": "Circle", "page": 1, "rect": [50, 50, 90, 90] }},
          {{ "op": "save" }},
          {{ "op": "expect_pages", "count": 9, "text": {texts} }},
          {{ "op": "outline", "edits": [ {{ "do": "add", "path": "1", "title": "Start", "page": 2 }} ] }},
          {{ "op": "expect_outline", "titles": {titles}, "levels": [0, 0] }},
          {{ "op": "expect_links", "page": 2, "count": {links_p1} }},
          {{ "op": "split", "parts": "1-2,3-", "output": "piece.pdf" }},
          {{ "op": "expect_pages", "input": "piece-1.pdf", "count": 2 }},
          {{ "op": "flatten", "pages": "2", "output": "flat.pdf" }},
          {{ "op": "expect", "count": {on_p2}, "page": 2 }},
          {{ "op": "set_create", "set": "s.plset", "name": "S", "files": ["work.pdf"] }},
          {{ "op": "set_add", "set": "s.plset", "files": ["flat.pdf", "source.pdf"] }},
          {{ "op": "set_list", "set": "s.plset", "count": 3 }},
          {{ "op": "set_search", "set": "s.plset", "text": {marker},
            "expect": [ {{ "file": "work.pdf", "page": 1 }}, {{ "file": "flat.pdf", "page": 1 }} ] }}
        ] }}"#,
        texts = serde_json::to_string(&expected_text).unwrap(),
        titles = "[\"Start\", \"x\"]",
        on_p2 = list(&files.path("main.pdf"))[0].annotations + 1,
        links_p1 = truth.links.iter().filter(|l| l.page_index == 0).count(),
        marker = serde_json::to_string(&truth.markers[7]).unwrap(),
    );
    // The outline check names only the first two of many titles: it must fail, with its step
    // number, after everything before it ran.
    std::fs::write(&steps, &commands).unwrap();
    let r = run(plumb().arg("run").arg(&steps));
    assert_eq!(r.code, 2, "{}", r.stdout);
    assert!(
        r.stderr.contains("step 10 (expect_outline)"),
        "{}",
        r.stderr
    );
    assert!(r.stdout.contains("4: pages wrote "), "{}", r.stdout);
    assert!(r.stdout.contains("5: expect"), "{}", r.stdout);
    assert!(
        r.stdout.contains("8: expect_pages 9 pages as expected"),
        "{}",
        r.stdout
    );

    // With the right titles and levels it runs to the end.
    std::fs::copy(files.path("main.pdf"), &work).unwrap();
    // Deleting page 2 took the bookmark that led there with it.
    let kept: Vec<_> = truth.outline.iter().filter(|i| i.page != Some(1)).collect();
    let titles: Vec<String> = std::iter::once("Start".to_string())
        .chain(kept.iter().map(|i| i.title.clone()))
        .collect();
    let levels: Vec<usize> = std::iter::once(0)
        .chain(kept.iter().map(|i| i.level))
        .collect();
    let fixed = commands
        .replace(
            "[\"Start\", \"x\"]",
            &serde_json::to_string(&titles).unwrap(),
        )
        .replace(
            "\"levels\": [0, 0]",
            &format!("\"levels\": {}", serde_json::to_string(&levels).unwrap()),
        );
    std::fs::write(&steps, &fixed).unwrap();
    let out = ok(plumb().arg("run").arg(&steps));
    assert!(out.ends_with("19 steps done\n"), "{out}");
    assert!(out.contains("12: split 2 parts"), "{out}");
    assert!(out.contains("19: set_search 2 hits on 2 pages"), "{out}");
    // The page step kept the markup saved before it (now on page 2) and the one added after.
    let annots = ok(plumb().arg("annots").arg(&work));
    assert!(
        annots.lines().any(|l| l.starts_with("page 2: Square")),
        "{annots}"
    );
    assert!(
        annots.lines().any(|l| l.starts_with("page 1: Circle")),
        "{annots}"
    );

    // Unsaved changes stop a page step.
    std::fs::write(
        &steps,
        r#"{ "steps": [
            { "op": "open", "path": "work.pdf" },
            { "op": "add", "kind": "Square", "page": 1, "rect": [10, 10, 20, 20] },
            { "op": "pages", "ops": [ { "do": "rotate", "pages": "1", "degrees": 90 } ] }
        ] }"#,
    )
    .unwrap();
    let r = run(plumb().arg("run").arg(&steps));
    assert_eq!(r.code, 2);
    assert!(
        r.stderr
            .contains("step 3 (pages): the open document has unsaved changes"),
        "{}",
        r.stderr
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

/// A folder under the workspace's `target` for copies of the real sample.
fn sample_work_dir() -> tempfile::TempDir {
    let base = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("target")
        .join("tmp");
    std::fs::create_dir_all(&base).unwrap();
    tempfile::tempdir_in(std::path::absolute(base).unwrap()).unwrap()
}

/// Runs a command on the sample without ever printing what it wrote (the sample's text and
/// bookmarks are not to be shown).
fn quiet_ok(cmd: &mut Command, what: &str) -> String {
    let r = run(cmd);
    assert_eq!(r.code, 0, "{what} failed with exit code {}", r.code);
    r.stdout
}

#[test]
fn page_operations_and_set_search_on_the_real_sample() {
    let Some(sample) = sample_path() else {
        eprintln!("SAMPLE SKIPPED: samples/site-development-plans.pdf is not present");
        return;
    };
    let stamp = std::fs::metadata(&sample)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap();
    let work = sample_work_dir();
    let copy = work.path().join("plans.pdf");
    std::fs::copy(&sample, &copy).unwrap();
    let rows = page_rows(&quiet_ok(plumb().arg("pages").arg(&copy), "pages list"));
    let n = rows.len();
    assert!(n >= 2);
    let out = work.path().join("out.pdf");
    quiet_ok(
        plumb()
            .arg("pages")
            .arg(&copy)
            .args([
                "rotate", "1", "90", "move", "last", "to", "1", "delete", "2", "-o",
            ])
            .arg(&out),
        "pages edit",
    );
    let mut m = model_of(&rows);
    m[0].rotate = (m[0].rotate + 90).rem_euclid(360);
    let last = m.pop().unwrap();
    m.insert(0, last);
    m.remove(1);
    let after = model_of(&page_rows(&quiet_ok(
        plumb().arg("pages").arg(&out),
        "pages list",
    )));
    assert!(
        after == m,
        "the edited sample's pages do not match the reference list"
    );
    // Links and bookmarks are listed without error.
    quiet_ok(plumb().arg("links").arg(&out), "links");
    quiet_ok(plumb().arg("outline").arg(&out), "outline");

    // Set search: a word from one page of the copy is found on that page.
    let text = quiet_ok(plumb().arg("text").arg(&copy).args(["--page", "2"]), "text");
    let word = text
        .split(|c: char| !c.is_ascii_alphanumeric())
        .find(|w| w.len() >= 6 && w.chars().all(|c| c.is_ascii_alphabetic()))
        .map(str::to_string);
    if let Some(word) = word {
        let set = work.path().join("plans.plset");
        quiet_ok(
            plumb().args(["set", "create"]).arg(&set).arg(&copy),
            "set create",
        );
        let hits = quiet_ok(
            plumb().args(["set", "search"]).arg(&set).arg(&word),
            "set search",
        );
        assert!(
            hits.lines().any(|l| l.starts_with("plans.pdf page 2: [")),
            "the set search did not find the word on its page"
        );
    } else {
        eprintln!("SAMPLE NOTE: page 2 has no plain word to search for; set search not checked");
    }
    let now = std::fs::metadata(&sample)
        .map(|m| (m.len(), m.modified().ok()))
        .unwrap();
    assert_eq!(now, stamp, "the sample itself is never written");
}

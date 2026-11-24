//! Rasteriser goldens: each test drawing rendered small by tiny-skia and reduced to a coarse
//! text picture, compared with a stored copy under `tests/goldens/`.
//!
//! The pictures are deliberately coarse (a 60 x 40 grid, five ink levels, the mean of each 4 x 4
//! pixel cell) so they do not flip on a rounding difference, yet a shape that moves, changes
//! size, loses its ending or its dashes changes many cells. They are also readable in a diff.
//! Each golden starts with the drawing's exact bounds and its total ink, which catch what a
//! coarse picture cannot (the ending of a thin line).
//!
//! To accept new output after an intended change:
//!
//! ```text
//! PL_DRAW_BLESS=1 cargo test -p pl-draw --test goldens
//! ```

mod cases;

use std::path::PathBuf;

use pl_draw::kurbo::Affine;
use pl_draw::Rgb;

const COLS: u32 = 60;
const ROWS: u32 = 40;
const CELL: u32 = 4;
/// Ink levels from paper to solid.
const LEVELS: [char; 5] = [' ', '.', ':', 'x', '#'];

/// Renders the 240 x 160 unit test window at 1 px per unit on white and reduces it to text.
fn picture(list: &pl_draw::DrawList) -> String {
    let transform = Affine::new([1.0, 0.0, 0.0, -1.0, 0.0, 160.0]);
    let canvas =
        pl_draw::render_new(list, transform, COLS * CELL, ROWS * CELL, Some(Rgb::WHITE)).unwrap();
    let data = canvas.data();
    let width = (COLS * CELL) as usize;
    let mut out = String::new();
    for row in 0..ROWS {
        let mut line = String::new();
        for col in 0..COLS {
            // Ink is the distance from white, averaged over the cell and the three channels.
            let mut ink = 0u32;
            for y in 0..CELL {
                for x in 0..CELL {
                    let i = ((row * CELL + y) as usize * width + (col * CELL + x) as usize) * 4;
                    ink += (255 - data[i] as u32)
                        + (255 - data[i + 1] as u32)
                        + (255 - data[i + 2] as u32);
                }
            }
            let mean = ink as f64 / (CELL * CELL * 3) as f64 / 255.0;
            // Level boundaries sit away from the values flat fills produce.
            let level = match mean {
                m if m < 0.04 => 0,
                m if m < 0.22 => 1,
                m if m < 0.45 => 2,
                m if m < 0.70 => 3,
                _ => 4,
            };
            line.push(LEVELS[level]);
        }
        out.push_str(line.trim_end());
        out.push('\n');
    }
    // Two figures the coarse picture cannot show for small features such as a thin line's
    // ending: the exact bounds, and the total ink in pixel-equivalents.
    let b = list.bounds().expect("the drawing has bounds");
    let ink: f64 = data
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| (765 - p[0] as u32 - p[1] as u32 - p[2] as u32) as f64 / 765.0)
        .sum();
    format!(
        "bounds {:.2} {:.2} {:.2} {:.2}\nink {:.1}\n{out}",
        b.x0, b.y0, b.x1, b.y1, ink
    )
}

/// Splits a golden into its bounds line, its ink figure and its picture.
fn parts(golden: &str) -> (String, f64, String) {
    let mut lines = golden.lines();
    let bounds = lines.next().unwrap_or_default().to_owned();
    let ink = lines
        .next()
        .and_then(|l| l.strip_prefix("ink "))
        .and_then(|v| v.parse().ok())
        .unwrap_or(f64::NAN);
    let picture: Vec<&str> = lines.collect();
    (bounds, ink, picture.join("\n"))
}

/// Whether two goldens describe the same drawing: equal bounds, ink within 1 % (or 2 pixels),
/// and pictures that differ in at most [`MAX_CELL_DIFF`] cells.
fn same_drawing(a: &str, b: &str) -> Result<(), String> {
    let (ba, ia, pa) = parts(a);
    let (bb, ib, pb) = parts(b);
    if ba != bb {
        return Err(format!("{ba:?} vs {bb:?}"));
    }
    let slack = (0.01 * ia.abs().max(ib.abs())).max(2.0);
    if (ia - ib).abs() > slack || ia.is_nan() || ib.is_nan() {
        return Err(format!("ink {ia} vs {ib}"));
    }
    let n = cell_diff(&pa, &pb);
    if n > MAX_CELL_DIFF {
        return Err(format!("{n} cells differ"));
    }
    Ok(())
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("goldens")
        .join(format!("{name}.txt"))
}

/// How many cells may differ from the golden: cells whose mean ink sits right on a level
/// boundary can land either side on another CPU's rounding.
const MAX_CELL_DIFF: usize = 6;

fn cell_diff(a: &str, b: &str) -> usize {
    let grid = |s: &str| -> Vec<Vec<char>> {
        let mut rows: Vec<Vec<char>> = s.lines().map(|l| l.chars().collect()).collect();
        rows.resize(ROWS as usize, Vec::new());
        for r in &mut rows {
            r.resize(COLS as usize, ' ');
        }
        rows
    };
    let (ga, gb) = (grid(a), grid(b));
    ga.iter()
        .zip(&gb)
        .map(|(ra, rb)| ra.iter().zip(rb).filter(|(x, y)| x != y).count())
        .sum()
}

fn check_all(group: &str, cases: Vec<cases::Case>) {
    let bless = std::env::var_os("PL_DRAW_BLESS").is_some();
    let mut failures = Vec::new();
    let mut pictures = std::collections::HashMap::new();
    for case in &cases {
        let actual = picture(&case.list);
        assert!(
            parts(&actual).2.chars().any(|c| c != ' ' && c != '\n'),
            "{} draws nothing",
            case.name
        );
        let path = golden_path(&case.name);
        if bless {
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, &actual).unwrap();
        } else {
            match std::fs::read_to_string(&path) {
                Ok(expected) => {
                    let expected = expected.replace("\r\n", "\n");
                    if let Err(why) = same_drawing(&expected, &actual) {
                        failures.push(format!(
                            "{}: {why}, against {}\nexpected:\n{expected}\nactual:\n{actual}",
                            case.name,
                            path.display()
                        ));
                    }
                }
                Err(e) => failures.push(format!(
                    "{}: no golden at {} ({e})",
                    case.name,
                    path.display()
                )),
            }
        }
        pictures.insert(case.name.clone(), actual);
    }
    assert!(
        failures.is_empty(),
        "{group}: {} of {} goldens differ:\n{}",
        failures.len(),
        cases.len(),
        failures.join("\n")
    );
    assert!(!cases.is_empty());
}

#[test]
fn shape_goldens() {
    check_all("shapes", cases::shape_cases());
}

#[test]
fn stroke_style_goldens() {
    check_all("stroke styles", cases::stroke_style_cases());
}

#[test]
fn dash_goldens() {
    let mut cases = cases::dash_cases();
    let (dashed, solid) = cases::dashed_cloud_cases();
    cases.extend([dashed, solid]);
    check_all("dashes", cases);
}

#[test]
fn line_ending_goldens() {
    check_all("line endings", cases::ending_cases());
}

#[test]
fn opacity_goldens() {
    check_all("opacity", cases::opacity_cases());
}

#[test]
fn multiply_goldens() {
    check_all("multiply", cases::multiply_cases());
}

#[test]
fn group_and_image_goldens() {
    check_all("groups and images", cases::group_and_image_cases());
}

#[test]
fn text_goldens() {
    check_all("text", cases::text_cases());
}

/// The golden pictures can tell the test drawings apart: no two endings, dash patterns or
/// opacities reduce to the same picture, so a golden cannot pass for the wrong drawing.
#[test]
fn golden_pictures_distinguish_the_cases() {
    let mut all = cases::all_geometry_cases();
    all.extend(cases::text_cases());
    let pictures: Vec<(String, String)> = all
        .iter()
        .map(|c| (c.name.clone(), picture(&c.list)))
        .collect();
    // Pairs that are meant to look alike at this resolution on white paper.
    let alike = |a: &str, b: &str| {
        let pair = |x: &str, y: &str| (a == x && b == y) || (a == y && b == x);
        // Multiply over white paper is the same picture as Normal.
        (a.starts_with("multiply-") && b.starts_with("multiply-"))
            || pair("group-rotated", "group-rotated-multiply")
            || pair("image-opaque", "image-multiply")
            || pair("polygon-nonzero", "polygon-evenodd")
            // Thin strokes of different styles on the same outline.
            || (a.starts_with("le-None") && b.starts_with("le-None"))
            || pair("miter-limit-default", "miter-limit-low")
            || pair("join-miter", "join-bevel")
            || pair("join-miter", "join-round")
            || pair("join-round", "join-bevel")
            || pair("cap-butt", "cap-round")
            || pair("cap-butt", "cap-square")
            || pair("cap-round", "cap-square")
            || pair("text-Regular", "text-Italic")
            || pair("text-Bold", "text-BoldItalic")
    };
    let mut same = Vec::new();
    for (i, (na, pa)) in pictures.iter().enumerate() {
        for (nb, pb) in &pictures[i + 1..] {
            if same_drawing(pa, pb).is_ok() && !alike(na, nb) {
                same.push(format!("{na} / {nb}"));
            }
        }
    }
    assert!(same.is_empty(), "indistinguishable pictures: {same:?}");
}

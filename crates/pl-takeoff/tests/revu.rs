//! Takeoff rows from files saved by Bluebeam Revu (`samples/external/cyantakeoff-revu`, local
//! only and never committed; every test here skips when they are absent).

use std::path::{Path, PathBuf};

use pl_annot::AnnotSet;
use pl_pdf::PdfDocument;
use pl_takeoff::{ListContext, Table};

fn sample(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("external")
        .join("cyantakeoff-revu")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("REVU SAMPLE SKIPPED: samples/external/cyantakeoff-revu/{name} is not present");
        None
    }
}

/// Each Revu count (its items grouped with /IRT + /RT /Group) is one row with its item total,
/// and counts that only look alike are not merged: markup-space.pdf has a 15-item and a
/// 4-item group and five singles, markup-deep-spaces.pdf eight singles on page 1, two more and
/// a 3-item group on page 2.
#[test]
fn revu_count_groups_are_one_row_each() {
    for (name, rows) in [
        (
            "markup-space.pdf",
            vec![(0, 1), (0, 1), (0, 1), (0, 1), (0, 1), (0, 4), (0, 15)],
        ),
        (
            "markup-deep-spaces.pdf",
            vec![
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (0, 1),
                (1, 1),
                (1, 1),
                (1, 3),
            ],
        ),
    ] {
        let Some(path) = sample(name) else {
            continue;
        };
        let doc = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
        let set = AnnotSet::load(&doc);
        let table = Table::build(set.iter(), &ListContext::default());
        let mut got: Vec<(usize, u32)> = table
            .rows
            .iter()
            .filter(|r| r.kind_name == "Count")
            .map(|r| (r.page, r.count.unwrap() as u32))
            .collect();
        got.sort();
        assert_eq!(got, rows, "{name}");
        // Every item has a row: its group's.
        for a in set.iter().filter(|a| a.count_group().is_some()) {
            let row = table.row_of(&a.id).unwrap();
            assert_eq!(table.rows[row].id, a.count_group().unwrap(), "{name}");
        }
    }
}

//! The random undo test of `history.rs`, extended with the structural commands: a thousand
//! seeded sequences mix markup commands with page operations (insert blank, insert from this
//! document or another file, merge, delete, reorder, move, rotate, replace), bookmark and link
//! edits and flatten, through [`Document`] with real PDFium. Every state the history has been
//! through is reached again exactly by undo and redo: the pages by fingerprint and by their
//! display geometry, the annotations with their ties to the file, the outline. Refused
//! commands change nothing. Every command also goes through JSON, the journal's form.
//!
//! The starting documents are the structure fixture (rotated, UserUnit, CropBox and inherited
//! pages, labels, outline, links, fields, a layer) and the single-geometry fixtures (rotated,
//! UserUnit, CropBox).

use std::path::{Path, PathBuf};
use std::sync::Arc;

use pl_doc::pl_annot::{Annotation, Origin, PdfDate};
use pl_doc::pl_geom::{PageGeometry, Rect, Vec2};
use pl_doc::pl_pdf::SignaturePolicy;
use pl_doc::pl_render::PdfiumBackend;
use pl_doc::{
    BookmarkSpec, BookmarkTarget, Command, DestSpec, Document, FlattenScope, LinkData, LinkTarget,
    PageFingerprint, PageSource, Session, SessionOptions, StructCommand,
};

/// xorshift64*, as `history.rs` uses.
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n.max(1) as u64) as usize
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }

    fn coord(&mut self) -> f64 {
        self.below(1600) as f64 / 4.0
    }

    fn subset(&mut self, n: usize, max: usize) -> Vec<u32> {
        let mut all: Vec<u32> = (0..n as u32).collect();
        for i in (1..all.len()).rev() {
            all.swap(i, self.below(i + 1));
        }
        all.truncate(1 + self.below(max.min(n).max(1)));
        all.sort_unstable();
        all
    }
}

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

fn origin() -> Origin {
    Origin::new("Random", PdfDate::from_unix(1_800_000_000))
}

#[derive(Debug, PartialEq)]
struct State {
    prints: Vec<PageFingerprint>,
    geometry: Vec<PageGeometry>,
    annots: Vec<Annotation>,
    removed: Vec<Annotation>,
    outline: Vec<(Vec<usize>, String, Option<usize>)>,
}

fn state(doc: &mut Document) -> State {
    let geometry = (0..doc.page_count())
        .map(|p| *doc.page_geometry(p).unwrap())
        .collect();
    let editor = doc.annots().unwrap();
    let annots = editor.set().iter().cloned().collect();
    let mut removed: Vec<Annotation> = editor.set().removed().to_vec();
    removed.sort_by(|a, b| a.id.cmp(&b.id));
    State {
        prints: doc.page_fingerprints().unwrap(),
        geometry,
        annots,
        removed,
        outline: doc
            .outline_rows()
            .unwrap()
            .into_iter()
            .map(|r| (r.path, r.title, r.dest.map(|d| d.page_index)))
            .collect(),
    }
}

fn random_struct(rng: &mut Rng, doc: &mut Document, source: &Path) -> StructCommand {
    let n = doc.page_count() as usize;
    let page = |rng: &mut Rng| rng.below(n) as u32;
    let command = match rng.below(15) {
        0 | 1 => StructCommand::RotatePages {
            pages: rng.subset(n, 3),
            degrees: [90, 180, 270, -90][rng.below(4)],
        },
        2 => StructCommand::DeletePages {
            // Sometimes every page, which is refused.
            pages: if rng.chance(8) {
                (0..n as u32).collect()
            } else {
                rng.subset(n, 2)
            },
        },
        3 => {
            let pages = rng.subset(n, 2);
            // Sometimes one place too far, which is refused.
            let slack = usize::from(rng.chance(5));
            let to = rng.below(n + 1 - pages.len() + slack) as u32;
            StructCommand::MovePages { pages, to }
        }
        4 => {
            let mut order: Vec<u32> = (0..n as u32).collect();
            for i in (1..n).rev() {
                order.swap(i, rng.below(i + 1));
            }
            if rng.chance(5) {
                order.pop(); // not a permutation: refused
            }
            StructCommand::ReorderPages { order }
        }
        5 => StructCommand::InsertBlank {
            at: rng.below(n + 1) as u32,
            count: 1 + rng.below(2) as u32,
            media_box: rng
                .chance(50)
                .then(|| [0.0, 0.0, 300.0 + rng.coord(), 200.0 + rng.coord()]),
            rotate: [0, 90][rng.below(2)],
        },
        6 => StructCommand::InsertPages {
            at: rng.below(n + 1) as u32,
            source: PageSource::This,
            pages: rng.subset(n, 2),
        },
        7 => StructCommand::InsertPages {
            at: rng.below(n + 1) as u32,
            source: PageSource::File(source.to_path_buf()),
            pages: rng.subset(4, 2),
        },
        8 => StructCommand::ReplacePages {
            at: page(rng),
            source: PageSource::File(source.to_path_buf()),
            pages: vec![rng.below(4) as u32],
            keep_markups: rng.chance(50),
        },
        9 => StructCommand::InsertDocument {
            at: rng.below(n + 1) as u32,
            source: PageSource::File(source.to_path_buf()),
            import_outline: rng.chance(50),
        },
        10 => StructCommand::AddBookmark {
            parent: Vec::new(),
            index: 0,
            bookmark: BookmarkSpec {
                title: format!("Mark {}", rng.below(100)),
                target: if rng.chance(80) {
                    BookmarkTarget::Page(DestSpec::page(page(rng)))
                } else {
                    BookmarkTarget::None
                },
                color: None,
                bold: rng.chance(50),
                italic: false,
                open: false,
            },
        },
        11 => {
            let rows = doc.outline_rows().unwrap();
            let path = if rows.is_empty() || rng.chance(5) {
                vec![99]
            } else {
                rows[rng.below(rows.len())].path.clone()
            };
            match rng.below(4) {
                0 => StructCommand::RenameBookmark {
                    path,
                    title: format!("Renamed {}", rng.below(100)),
                },
                1 => StructCommand::DeleteBookmark { path },
                2 => StructCommand::SetBookmarkTarget {
                    path,
                    target: BookmarkTarget::Page(DestSpec::at(page(rng), 0.0, 700.0)),
                },
                _ => StructCommand::MoveBookmark {
                    from: path,
                    parent: Vec::new(),
                    index: 0,
                },
            }
        }
        12 => {
            let (x, y) = (rng.coord(), rng.coord());
            StructCommand::AddLink {
                page: page(rng),
                link: LinkData::new(
                    Rect::new(x, y, x + 20.0, y + 10.0),
                    if rng.chance(70) {
                        LinkTarget::Page(DestSpec::page(page(rng)))
                    } else {
                        LinkTarget::Uri("https://example.invalid/".into())
                    },
                ),
            }
        }
        13 => {
            let p = page(rng);
            let links = doc.page_links(p).unwrap();
            match links.iter().filter_map(|l| l.id.clone()).nth(rng.below(3)) {
                Some(id) => StructCommand::DeleteLink { id: id.0 },
                None => StructCommand::DeleteLink {
                    id: "no-such-link".into(),
                },
            }
        }
        _ => StructCommand::Flatten {
            scope: FlattenScope::Pages(vec![page(rng)]),
            print: rng.chance(30),
            include_form_fields: false,
        },
    };
    let json = serde_json::to_string(&command).unwrap();
    let back: StructCommand = serde_json::from_str(&json).unwrap();
    assert_eq!(back, command, "{json}");
    command
}

fn random_markup(rng: &mut Rng, doc: &mut Document) -> Command {
    let n = doc.page_count() as usize;
    let set = doc.annots().unwrap().set();
    let pick = (!set.is_empty()).then(|| set.iter().nth(rng.below(set.len())).unwrap().clone());
    let command = match (rng.below(4), pick) {
        (0, Some(a)) => Command::DeleteAnnotations(vec![a.id]),
        (1, Some(a)) => {
            let mut b = a.clone();
            b.translate(Vec2::new(rng.coord() / 8.0 - 25.0, 5.0));
            Command::ModifyAnnotations(vec![b])
        }
        _ => {
            let (x, y) = (rng.coord(), rng.coord());
            let rect = Rect::new(x, y, x + 10.0 + rng.coord() / 4.0, y + 10.0);
            let page = rng.below(n);
            Command::add(if rng.chance(50) {
                Annotation::square(page, rect, &origin())
            } else {
                Annotation::circle(page, rect, &origin())
            })
        }
    };
    // The journal's form (which carries no tie to the file) reads back as it was written.
    let json = serde_json::to_string(&command).unwrap();
    let back: Command = serde_json::from_str(&json).unwrap();
    assert_eq!(serde_json::to_string(&back).unwrap(), json);
    command
}

#[test]
fn a_thousand_random_sequences_with_structural_commands_undo_and_redo_exactly() {
    let backend = Arc::new(PdfiumBackend::new(Some(&vendor_dll())).unwrap());
    let dir = tempfile::tempdir().unwrap();
    let starts: Vec<PathBuf> = [
        ("main.pdf", pl_testkit::structure_main().0.bytes),
        ("rotated.pdf", pl_testkit::rotated_pages().bytes),
        ("unit.pdf", pl_testkit::user_unit().bytes),
        ("crop.pdf", pl_testkit::crop_box().bytes),
    ]
    .into_iter()
    .map(|(name, bytes)| {
        let p = dir.path().join(name);
        std::fs::write(&p, bytes).unwrap();
        p
    })
    .collect();
    let source = dir.path().join("source.pdf");
    std::fs::write(&source, pl_testkit::structure_source().0.bytes).unwrap();
    let originals: Vec<Vec<u8>> = starts.iter().map(|p| std::fs::read(p).unwrap()).collect();

    let (mut structural, mut markups, mut refused, mut undos, mut redos) = (0, 0, 0, 0, 0);
    for seed in 0..1000u64 {
        let mut rng = Rng(0x9e37_79b9_7f4a_7c15 ^ (seed + 1).wrapping_mul(0xff51_afd7_ed55_8ccd));
        let which = seed as usize % starts.len();
        let mut s = Session::with_options(backend.clone(), SessionOptions::complete());
        let id = s.open(&starts[which], None).unwrap();
        let doc = s.document_mut(id).unwrap();
        doc.load_annots().unwrap();
        let start = state(doc);
        // timeline[k] is the state k steps from the start.
        let mut timeline = vec![start];
        let mut depth = 0usize;

        for step in 0..4 + rng.below(9) {
            let what = format!("seed {seed} step {step}");
            match rng.below(10) {
                0 | 1 if doc.can_undo() => {
                    assert!(doc.undo().unwrap(), "{what}");
                    depth -= 1;
                    undos += 1;
                    assert_eq!(state(doc), timeline[depth], "{what}: undo");
                }
                2 if doc.can_redo() => {
                    assert!(doc.redo().unwrap(), "{what}");
                    depth += 1;
                    redos += 1;
                    assert_eq!(state(doc), timeline[depth], "{what}: redo");
                }
                3..=5 => {
                    let command = random_markup(&mut rng, doc);
                    let before = state(doc);
                    match doc.apply(command) {
                        Ok(out) if out.changed => {
                            markups += 1;
                            depth += 1;
                            timeline.truncate(depth);
                            timeline.push(state(doc));
                        }
                        Ok(_) => assert_eq!(state(doc), before, "{what}: a no-op changed"),
                        Err(_) => {
                            refused += 1;
                            assert_eq!(state(doc), before, "{what}: refused markup command");
                        }
                    }
                }
                _ => {
                    let command = random_struct(&mut rng, doc, &source);
                    let before = state(doc);
                    match doc.apply_structure(command.clone(), SignaturePolicy::Refuse) {
                        Ok(out) => {
                            structural += 1;
                            depth += 1;
                            timeline.truncate(depth);
                            let after = state(doc);
                            assert_eq!(
                                out.page_map.new_page_count as usize,
                                after.prints.len(),
                                "{what}: {command:?}"
                            );
                            assert_eq!(out.page_map.old_to_new.len(), before.prints.len());
                            // Pages the map says survived are the same pages (by content).
                            if !matches!(
                                command,
                                StructCommand::Flatten { .. } | StructCommand::ReplacePages { .. }
                            ) {
                                for (old, new) in out.page_map.old_to_new.iter().enumerate() {
                                    if let Some(new) = new {
                                        assert_eq!(
                                            after.prints[*new as usize].content,
                                            before.prints[old].content,
                                            "{what}: {command:?} page {old} -> {new}"
                                        );
                                    }
                                }
                            }
                            timeline.push(after);
                        }
                        Err(_) => {
                            refused += 1;
                            assert_eq!(state(doc), before, "{what}: refused {command:?}");
                        }
                    }
                }
            }
            assert!(
                doc.warnings().is_empty(),
                "seed {seed}: {:?}",
                doc.warnings()
            );
        }

        // Forward to the end, back to the very start, forward again.
        while doc.can_redo() {
            doc.redo().unwrap();
            depth += 1;
            redos += 1;
        }
        let end_depth = depth;
        assert_eq!(state(doc), timeline[depth], "seed {seed}: the end");
        while doc.can_undo() {
            doc.undo().unwrap();
            depth -= 1;
            undos += 1;
            assert_eq!(
                state(doc),
                timeline[depth],
                "seed {seed}: undo all, depth {depth}"
            );
        }
        assert_eq!(depth, 0, "seed {seed}");
        assert!(!doc.is_modified(), "seed {seed}");
        while doc.can_redo() {
            doc.redo().unwrap();
            depth += 1;
            redos += 1;
            assert_eq!(
                state(doc),
                timeline[depth],
                "seed {seed}: redo all, depth {depth}"
            );
        }
        assert_eq!(depth, end_depth, "seed {seed}");
        drop(s);
    }
    for (path, bytes) in starts.iter().zip(&originals) {
        assert_eq!(&std::fs::read(path).unwrap(), bytes, "no file was written");
    }
    println!(
        "{structural} structural, {markups} markup, {refused} refused, {undos} undo, {redos} redo"
    );
    assert!(structural > 2500, "{structural} structural commands");
    assert!(markups > 1500, "{markups} markup commands");
    assert!(refused > 100, "{refused} refused");
    assert!(undos > 4000 && redos > 4000, "{undos} undos, {redos} redos");
}

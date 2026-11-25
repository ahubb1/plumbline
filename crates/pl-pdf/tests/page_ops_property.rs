//! The M8 property test: random sequences of insert (blank, from another file, from the
//! original), delete, move, rotate, extract, merge and replace are applied both to a reference
//! list model (each page's source file, original index and rotation) and to the real fixture,
//! and the two are compared by page fingerprint (the marker each fixture page draws, plus
//! `/Rotate`) after every step. Outlines, named destinations and links must keep resolving to
//! the right pages; labels must stay with their pages; the form's field tree must list exactly
//! the widgets on the pages; and the result must survive the verified full-rewrite save (lopdf
//! and hayro-syntax) and open in PDFium with the right page text.
//!
//! The generator is the workspace's SplitMix64 (no extra crate), so a failure reproduces from
//! the seed it prints.

mod structure_util;

use std::collections::{BTreeSet, HashSet};

use pl_pdf::{
    BlankPage, LinkAction, Object, ObjectId, PdfDocument, ReplaceOptions, Restructure, SaveOptions,
    SignaturePolicy,
};
use pl_render::RenderBackend;
use pl_testkit::{structure_main, structure_source, StructureTruth};
use structure_util::{load, marker_in, page_content, temp_dir, Rng};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Src {
    A,
    B,
    Blank,
}

/// One page of the reference model.
#[derive(Debug, Clone, PartialEq)]
struct Fp {
    src: Src,
    index: usize,
    rotate: i32,
    /// This is the original page `index` of the main fixture, not a copy.
    original: bool,
    /// References (outline items, named destinations, links of original pages) to original
    /// page `k` lead here: the original itself, or what replaced it.
    ref_of: Option<usize>,
    /// The label the page carries; `None` shows its page number.
    label: Option<String>,
}

#[derive(Debug, Clone)]
enum Op {
    Blank(usize),
    InsertB(usize, Vec<usize>),
    InsertA(usize, Vec<usize>),
    Delete(Vec<usize>),
    Move(Vec<usize>, usize),
    Rotate(Vec<usize>, i32),
    Extract(Vec<usize>),
    MergeB,
    Replace(usize, Vec<usize>, bool),
}

struct World {
    a: PdfDocument,
    b: PdfDocument,
    ta: StructureTruth,
    tb: StructureTruth,
    a_ids: Vec<ObjectId>,
    a_rot: Vec<i32>,
}

fn world() -> World {
    let (fa, ta) = structure_main();
    let (fb, tb) = structure_source();
    let a = load(&fa.bytes);
    let b = load(&fb.bytes);
    let a_ids = a.page_ids().to_vec();
    let a_rot = a
        .pages()
        .unwrap()
        .iter()
        .map(|p| p.rotate.degrees())
        .collect();
    World {
        a,
        b,
        ta,
        tb,
        a_ids,
        a_rot,
    }
}

fn initial_model(w: &World) -> Vec<Fp> {
    (0..w.a.page_count())
        .map(|i| Fp {
            src: Src::A,
            index: i,
            rotate: w.a_rot[i],
            original: true,
            ref_of: Some(i),
            label: Some(w.ta.labels[i].clone()),
        })
        .collect()
}

fn gen_op(rng: &mut Rng, n: usize, b_count: usize, a_count: usize) -> Op {
    loop {
        let op = match rng.below(9) {
            0 => Op::Blank(rng.below(n + 1)),
            1 => {
                let mut pages = rng.subset(b_count, 3);
                rng.shuffle(&mut pages);
                Op::InsertB(rng.below(n + 1), pages)
            }
            2 => {
                let mut pages = rng.subset(a_count, 3);
                rng.shuffle(&mut pages);
                Op::InsertA(rng.below(n + 1), pages)
            }
            3 if n > 1 => Op::Delete(rng.subset(n, (n - 1).min(4))),
            4 => {
                let pages = rng.subset(n, 3);
                let to = rng.below(n - pages.len() + 1);
                Op::Move(pages, to)
            }
            5 => {
                let deg = [90, 180, 270, -90][rng.below(4)];
                Op::Rotate(rng.subset(n, 4), deg)
            }
            6 if n > 1 => {
                let mut pages = rng.subset(n, n.min(6));
                rng.shuffle(&mut pages);
                Op::Extract(pages)
            }
            7 if n < 30 => Op::MergeB,
            8 => {
                let mut pages = rng.subset(b_count, 2.min(n));
                rng.shuffle(&mut pages);
                let at = rng.below(n - pages.len() + 1);
                Op::Replace(at, pages, rng.below(2) == 0)
            }
            _ => continue,
        };
        return op;
    }
}

fn apply_model(w: &World, model: &mut Vec<Fp>, op: &Op) {
    let fresh = |src: Src, index: usize, rotate: i32, label: Option<String>| Fp {
        src,
        index,
        rotate,
        original: false,
        ref_of: None,
        label,
    };
    match op {
        Op::Blank(at) => {
            let rotate = model[(*at).min(model.len() - 1)].rotate;
            model.insert(*at, fresh(Src::Blank, 0, rotate, None));
        }
        Op::InsertB(at, pages) => {
            for (k, p) in pages.iter().enumerate() {
                model.insert(at + k, fresh(Src::B, *p, 0, None));
            }
        }
        Op::InsertA(at, pages) => {
            for (k, p) in pages.iter().enumerate() {
                model.insert(
                    at + k,
                    fresh(Src::A, *p, w.a_rot[*p], Some(w.ta.labels[*p].clone())),
                );
            }
        }
        Op::Delete(pages) => {
            for p in pages.iter().rev() {
                model.remove(*p);
            }
        }
        Op::Move(pages, to) => {
            let block: Vec<Fp> = pages.iter().map(|p| model[*p].clone()).collect();
            let mut rest: Vec<Fp> = model
                .iter()
                .enumerate()
                .filter(|(i, _)| !pages.contains(i))
                .map(|(_, f)| f.clone())
                .collect();
            for (k, f) in block.into_iter().enumerate() {
                rest.insert(to + k, f);
            }
            *model = rest;
        }
        Op::Rotate(pages, deg) => {
            for p in pages {
                model[*p].rotate = (model[*p].rotate + deg).rem_euclid(360);
            }
        }
        Op::Extract(pages) => {
            *model = pages.iter().map(|p| model[*p].clone()).collect();
        }
        Op::MergeB => {
            for p in 0..w.b.page_count() {
                model.push(fresh(Src::B, p, 0, None));
            }
        }
        Op::Replace(at, pages, _) => {
            for (k, p) in pages.iter().enumerate() {
                let old = &model[at + k];
                let new = Fp {
                    src: Src::B,
                    index: *p,
                    rotate: 0,
                    original: false,
                    ref_of: old.ref_of,
                    label: old.label.clone(),
                };
                model[at + k] = new;
            }
        }
    }
}

fn apply_real(w: &World, r: &mut Restructure, op: &Op) {
    let result = match op {
        Op::Blank(at) => r.insert_blank(*at, BlankPage::LikeNeighbour),
        Op::InsertB(at, pages) => r.insert_from(*at, &w.b, pages),
        Op::InsertA(at, pages) => r.insert_from(*at, &w.a, pages),
        Op::Delete(pages) => r.delete(pages),
        Op::Move(pages, to) => r.move_pages(pages, *to),
        Op::Rotate(pages, deg) => r.rotate(pages, *deg),
        Op::Extract(pages) => r.extract(pages),
        Op::MergeB => {
            let at = r.page_count();
            r.insert_document(at, &w.b, true)
        }
        Op::Replace(at, pages, keep) => r.replace(
            *at,
            &w.b,
            pages,
            ReplaceOptions {
                keep_markups: *keep,
            },
        ),
    };
    result.unwrap_or_else(|e| panic!("{op:?} failed: {e}"));
}

fn expected_marker(w: &World, f: &Fp) -> Option<String> {
    match f.src {
        Src::A => Some(w.ta.markers[f.index].clone()),
        Src::B => Some(w.tb.markers[f.index].clone()),
        Src::Blank => None,
    }
}

/// Every widget on a page, and every widget the field tree reaches.
fn widgets(doc: &PdfDocument) -> (HashSet<ObjectId>, HashSet<ObjectId>, Vec<String>) {
    let mut on_pages = HashSet::new();
    for p in 0..doc.page_count() {
        for a in doc.annotations(p).unwrap() {
            if a.subtype() == Some(b"Widget") {
                on_pages.insert(a.id.unwrap());
            }
        }
    }
    let mut in_tree = HashSet::new();
    let mut top_names = Vec::new();
    let Some(form) = doc
        .catalog()
        .unwrap()
        .get(b"AcroForm")
        .ok()
        .and_then(|f| doc.resolve(f))
    else {
        return (on_pages, in_tree, top_names);
    };
    let fields = doc
        .resolve(form.as_dict().unwrap().get(b"Fields").unwrap())
        .unwrap()
        .as_array()
        .unwrap();
    fn walk(doc: &PdfDocument, o: &Object, out: &mut HashSet<ObjectId>) {
        let Object::Reference(id) = o else { return };
        let Some(Object::Dictionary(d)) = doc.object(*id) else {
            panic!("field {id:?} is missing")
        };
        if d.get(b"Subtype").ok().and_then(|s| s.as_name().ok()) == Some(b"Widget".as_slice()) {
            out.insert(*id);
        }
        if let Ok(Object::Array(kids)) = d.get(b"Kids") {
            for k in kids {
                walk(doc, k, out);
            }
        }
    }
    for f in fields {
        let d = doc.resolve(f).unwrap().as_dict().unwrap();
        if let Ok(t) = d.get(b"T") {
            top_names.push(pl_pdf::decode_text(t).unwrap());
        }
        walk(doc, f, &mut in_tree);
    }
    (on_pages, in_tree, top_names)
}

fn check(w: &World, doc: &PdfDocument, model: &[Fp], ctx: &str) {
    assert_eq!(doc.page_count(), model.len(), "{ctx}: page count");
    let pages = doc.pages().unwrap();
    for (i, f) in model.iter().enumerate() {
        let marker = marker_in(&page_content(doc, i));
        assert_eq!(marker, expected_marker(w, f), "{ctx}: page {i} fingerprint");
        assert_eq!(
            pages[i].rotate.degrees(),
            f.rotate,
            "{ctx}: page {i} rotation"
        );
        assert_eq!(
            doc.page_ids()[i] == w.a_ids[f.index] && f.src == Src::A,
            f.original,
            "{ctx}: page {i} identity"
        );
    }
    // Labels travel with their pages.
    let labels: Vec<String> = doc
        .page_labels()
        .expect("labels")
        .iter()
        .map(|l| l.text())
        .collect();
    let want: Vec<String> = model
        .iter()
        .enumerate()
        .map(|(i, f)| f.label.clone().unwrap_or_else(|| (i + 1).to_string()))
        .collect();
    assert_eq!(labels, want, "{ctx}: labels");

    let where_ref = |k: usize| model.iter().position(|f| f.ref_of == Some(k));
    // Outline: leaves of the main fixture are there exactly when their page (or its
    // replacement) is; every item with a destination resolves to the expected page.
    let flat = doc.outline().flatten_with(doc);
    for (title, k) in [
        ("Cover", 0),
        ("Contents", 1),
        ("Sheet 5", 4),
        ("Sheet 6", 5),
        ("Plan \u{00DC}bersicht \u{03A9}", 6),
    ] {
        let item = flat.iter().find(|i| i.title == title);
        match where_ref(k) {
            Some(at) => {
                let item = item.unwrap_or_else(|| panic!("{ctx}: outline item {title} is missing"));
                assert_eq!(
                    item.dest.map(|d| d.page_index),
                    Some(at),
                    "{ctx}: {title} leads to page {at}"
                );
            }
            None => assert!(
                item.is_none(),
                "{ctx}: {title} leads to a deleted page but is still there"
            ),
        }
    }
    for item in &flat {
        if let Some(d) = item.dest {
            assert!(d.page_index < model.len(), "{ctx}: {} resolves", item.title);
        }
        if item.title == "B start" || item.title == "B three" {
            let d = item
                .dest
                .unwrap_or_else(|| panic!("{ctx}: {} has a destination", item.title));
            let want = if item.title == "B start" { 0 } else { 2 };
            assert_eq!(
                expected_marker(w, &model[d.page_index]),
                Some(w.tb.markers[want].clone()),
                "{ctx}: {} leads to a copy of source page {want}",
                item.title
            );
        }
    }
    assert!(
        flat.iter().any(|i| i.title == "Web"),
        "{ctx}: the URI item never goes"
    );
    // Named destinations.
    for (name, k) in &w.ta.named_dests {
        let resolved = doc.resolve_named_destination(name.as_bytes());
        assert_eq!(
            resolved.map(|r| r.page_index),
            where_ref(*k),
            "{ctx}: named destination {name}"
        );
    }
    // Links: every GoTo resolves; on original pages they lead where the truth says (or are
    // gone when that page is).
    for (i, f) in model.iter().enumerate() {
        let links = doc.links(i).unwrap();
        for l in &links {
            if let LinkAction::GoTo(d) = &l.action {
                assert!(
                    doc.resolve_link_dest(d).is_some(),
                    "{ctx}: a GoTo link on page {i} is broken: {d:?}"
                );
            }
        }
        if !f.original {
            continue;
        }
        for t in w.ta.links.iter().filter(|t| t.page_index == f.index) {
            let found = links
                .iter()
                .find(|l| (l.rect.x0, l.rect.y0) == (t.rect[0], t.rect[1]));
            let goto = matches!(
                t.kind,
                pl_testkit::LinkKind::GoTo | pl_testkit::LinkKind::GoToNamed
            );
            if !goto {
                assert!(
                    found.is_some(),
                    "{ctx}: non-GoTo link {:?} on page {i} stays",
                    t.kind
                );
                continue;
            }
            match where_ref(t.target_page.unwrap()) {
                Some(at) => {
                    let l =
                        found.unwrap_or_else(|| panic!("{ctx}: link {t:?} on page {i} is missing"));
                    let LinkAction::GoTo(d) = &l.action else {
                        panic!()
                    };
                    assert_eq!(
                        doc.resolve_link_dest(d).unwrap().page_index,
                        at,
                        "{ctx}: link {t:?}"
                    );
                }
                None => assert!(
                    found.is_none(),
                    "{ctx}: link {t:?} on page {i} leads to a deleted page but is still there"
                ),
            }
        }
    }
    // Form fields: the tree lists exactly the widgets on the pages; top-level names are unique.
    let (on_pages, in_tree, names) = widgets(doc);
    assert_eq!(
        on_pages, in_tree,
        "{ctx}: widgets on pages vs in the field tree"
    );
    let unique: BTreeSet<&String> = names.iter().collect();
    assert_eq!(
        unique.len(),
        names.len(),
        "{ctx}: duplicate top-level field names {names:?}"
    );
}

#[test]
fn random_page_operation_sequences_match_the_reference_model() {
    let w = world();
    let dir = temp_dir("property");
    let backend = structure_util::pdfium();
    const CASES: u64 = 200;
    let mut kinds_seen = [0usize; 9];
    for seed in 0..CASES {
        let mut rng = Rng::new(seed);
        let mut model = initial_model(&w);
        let mut r = w.a.restructure().unwrap();
        let steps = 1 + rng.below(6);
        let mut ops = Vec::new();
        for step in 0..steps {
            let op = gen_op(&mut rng, model.len(), w.b.page_count(), w.a.page_count());
            kinds_seen[match op {
                Op::Blank(_) => 0,
                Op::InsertB(..) => 1,
                Op::InsertA(..) => 2,
                Op::Delete(_) => 3,
                Op::Move(..) => 4,
                Op::Rotate(..) => 5,
                Op::Extract(_) => 6,
                Op::MergeB => 7,
                Op::Replace(..) => 8,
            }] += 1;
            ops.push(op.clone());
            apply_model(&w, &mut model, &op);
            apply_real(&w, &mut r, &op);
            let ctx = format!("seed {seed} step {step} after {ops:?}");
            check(&w, &r.snapshot().unwrap(), &model, &ctx);
        }
        // Save, verify and re-open.
        let path = dir.join(format!("case-{seed}.pdf"));
        let doc = r.finish().unwrap();
        doc.save_new(&path, &SaveOptions::default(), SignaturePolicy::Refuse)
            .unwrap_or_else(|e| panic!("seed {seed} {ops:?}: save failed: {e}"));
        let bytes = std::fs::read(&path).unwrap();
        let back = load(&bytes);
        let ctx = format!("seed {seed} after save, {ops:?}");
        check(&w, &back, &model, &ctx);
        if seed % 4 == 0 {
            let id = structure_util::open_bytes(&backend, bytes);
            assert_eq!(
                backend.page_count(id).unwrap() as usize,
                model.len(),
                "{ctx}: PDFium page count"
            );
            let p = rng.below(model.len());
            let text = backend.page_text(id, p as u32).unwrap().text();
            match expected_marker(&w, &model[p]) {
                Some(m) => assert!(
                    text.contains(&m),
                    "{ctx}: PDFium reads {m} on page {p}: {text:?}"
                ),
                None => assert!(
                    !text.contains("PLM-"),
                    "{ctx}: blank page {p} has no marker"
                ),
            }
            backend.close(id).unwrap();
        }
        let _ = std::fs::remove_file(&path);
    }
    assert!(
        kinds_seen.iter().all(|n| *n >= 3),
        "every kind of operation is exercised several times: {kinds_seen:?}"
    );
}

/// The model check itself catches a wrong page: a sanity test of the test.
#[test]
#[should_panic(expected = "fingerprint")]
fn the_check_notices_a_swapped_page() {
    let w = world();
    let mut model = initial_model(&w);
    let mut r = w.a.restructure().unwrap();
    r.move_pages(&[0], 1).unwrap();
    // Model not updated: pages 0 and 1 are swapped in the real document.
    model.truncate(8);
    check(&w, &r.finish().unwrap(), &model, "deliberate");
}

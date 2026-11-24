//! Popups, replies and review status: the structure written (`/Popup` and `/Parent`, `/IRT`
//! and `/RT`, `/State` and `/StateModel`) checked at object level, round-tripped, and shown
//! to paint nothing on the page; and the same structure written by another application, read
//! and preserved.

mod common;

use std::collections::HashMap;

use common::{Work, TARGETS};
use pl_annot::{
    AnnotId, AnnotKind, AnnotSet, Annotation, DrawList, Flags, Origin, PdfDate, ReviewState, Rgb,
};
use pl_geom::{Point, Rect};
use pl_pdf::{Dictionary, Object, ObjectId, PdfDocument, SaveOptions};

fn dict_of(doc: &PdfDocument, id: ObjectId) -> &Dictionary {
    doc.object(id).unwrap().as_dict().unwrap()
}

fn name(s: &str) -> Object {
    Object::Name(s.as_bytes().to_vec())
}

fn text(s: &str) -> Object {
    Object::string_literal(s)
}

fn string(dict: &Dictionary, key: &str) -> String {
    let Object::String(bytes, _) = dict.get(key.as_bytes()).unwrap() else {
        panic!("/{key} is a string");
    };
    pl_annot::decode_text(bytes)
}

fn at(author: &str, date: &str) -> Origin {
    Origin::new(author, PdfDate::parse(date).unwrap())
}

#[test]
fn popups_replies_and_status_are_standard_structure_on_every_page_layout() {
    let backend = common::backend();
    let mut failures = Vec::new();
    let ada = common::origin();
    let bo = at("Bo Reviewer", "D:20261124110000Z");
    let cy = at("Cy Engineer", "D:20261124120000Z");
    for target in &TARGETS {
        let label = target.name;
        let fixture = (target.fixture)();
        let mut work = Work::new(&fixture);
        let geometry = work.geometry(target.page);
        let crop = geometry.crop_box();
        let o = work.sample_origin(target.page);
        let before = work.doc().clone();
        let page_id = before.page_id(target.page).unwrap();

        let mut set = AnnotSet::load(&before);
        let mut square = Annotation::square(
            target.page,
            Rect::new(o.x + 20.0, o.y + 30.0, o.x + 140.0, o.y + 110.0),
            &ada,
        );
        square.style.width = 2.0;
        square.meta.contents = Some("Check this wall".into());
        let quad = [
            Point::new(o.x + 30.0, o.y + 130.0),
            Point::new(o.x + 200.0, o.y + 130.0),
            Point::new(o.x + 200.0, o.y + 146.0),
            Point::new(o.x + 30.0, o.y + 146.0),
        ];
        let highlight =
            Annotation::text_markup(AnnotKind::Highlight, target.page, vec![quad], &ada);
        let shown = DrawList::from_ops(
            square
                .draw_list()
                .ops
                .into_iter()
                .chain(highlight.draw_list().ops)
                .collect(),
        );
        let sq = set.add(square).unwrap();
        let hl = set.add(highlight).unwrap();
        let sq_popup = set.ensure_popup(&sq, crop, &ada).unwrap();
        let hl_popup = set.ensure_popup(&hl, crop, &ada).unwrap();
        let r1 = set.add_reply(&sq, "Is this the right wall?", &bo).unwrap();
        let r2 = set.add_reply(&r1, "Yes, type A2.", &ada).unwrap();
        let s1 = set
            .set_review_state(&sq, ReviewState::Accepted, &bo)
            .unwrap();
        let s2 = set
            .set_review_state(&sq, ReviewState::Completed, &cy)
            .unwrap();
        assert_eq!(set.len(), 8);
        let expected: Vec<Annotation> = set.iter().map(Annotation::detached).collect();

        let (plan, _) = work.save(&set);
        assert_eq!(plan.added.len(), 8, "{label}");
        let after = work.doc().clone();
        common::assert_untouched(&before, &after, &[page_id]);
        let objects: HashMap<&AnnotId, ObjectId> =
            plan.added.iter().map(|c| (&c.id, c.object)).collect();
        let reference = |id: &AnnotId| Object::Reference(objects[id]);
        let dict = |id: &AnnotId| dict_of(&after, objects[id]);

        // Popup <-> parent.
        for (markup, popup) in [(&sq, &sq_popup), (&hl, &hl_popup)] {
            assert_eq!(
                dict(markup).get(b"Popup").unwrap(),
                &reference(popup),
                "{label}"
            );
            let p = dict(popup);
            assert_eq!(p.get(b"Subtype").unwrap(), &name("Popup"));
            assert_eq!(p.get(b"Parent").unwrap(), &reference(markup), "{label}");
            assert_eq!(p.get(b"Open").unwrap(), &Object::Boolean(false));
            assert!(
                !p.has(b"AP"),
                "{label}: a viewer draws the popup window itself"
            );
            assert!(
                !p.has(b"T") && !p.has(b"C"),
                "{label}: a popup is not a markup"
            );
            let rect = pl_pdf::rect_from_object(p.get(b"Rect").unwrap()).unwrap();
            assert!(
                rect.width() > 50.0 && rect.height() > 30.0,
                "{label}: {rect:?}"
            );
            assert_eq!(crop.union(rect), crop, "{label}: the popup is on the page");
        }
        // Replies: Text annotations in reply to their parent.
        let reply = dict(&r1);
        assert_eq!(reply.get(b"Subtype").unwrap(), &name("Text"));
        assert_eq!(reply.get(b"IRT").unwrap(), &reference(&sq), "{label}");
        assert_eq!(reply.get(b"RT").unwrap(), &name("R"));
        assert_eq!(string(reply, "Contents"), "Is this the right wall?");
        assert_eq!(string(reply, "T"), "Bo Reviewer");
        assert_eq!(string(reply, "M"), "D:20261124110000Z");
        assert_eq!(reply.get(b"F").unwrap(), &Object::Integer(28));
        assert!(!reply.has(b"State") && !reply.has(b"StateModel"));
        assert_eq!(dict(&r2).get(b"IRT").unwrap(), &reference(&r1), "{label}");
        // Status: a hidden reply with /State and /StateModel, as Acrobat writes it.
        for (id, state, author) in [
            (&s1, "Accepted", "Bo Reviewer"),
            (&s2, "Completed", "Cy Engineer"),
        ] {
            let s = dict(id);
            assert_eq!(s.get(b"Subtype").unwrap(), &name("Text"));
            assert_eq!(s.get(b"IRT").unwrap(), &reference(&sq), "{label}");
            assert_eq!(string(s, "State"), state);
            assert_eq!(string(s, "StateModel"), "Review");
            assert_eq!(string(s, "T"), author);
            assert_eq!(string(s, "Contents"), format!("{state} set by {author}"));
            assert_eq!(
                s.get(b"F").unwrap(),
                &Object::Integer(30),
                "hidden, print, no zoom, no rotate"
            );
        }
        // A reply or status has an appearance that paints nothing, so no viewer invents an
        // icon for it.
        for id in [&r1, &r2, &s1, &s2] {
            let ap = dict(id).get(b"AP").unwrap().as_dict().unwrap();
            let n = ap.get(b"N").unwrap().as_reference().unwrap();
            let Some(Object::Stream(stream)) = after.object(n) else {
                panic!("{label}: /AP /N is a stream");
            };
            assert!(stream.content.is_empty(), "{label}: an empty appearance");
        }

        // Read back: the same eight, linked the same way.
        let loaded = AnnotSet::load(&after);
        assert!(!loaded.is_dirty(), "{label}");
        assert_eq!(loaded.len(), 8);
        for want in &expected {
            assert_eq!(&loaded.get(&want.id).unwrap().detached(), want, "{label}");
        }
        assert_eq!(loaded.review_state(&sq), ReviewState::Completed);
        assert_eq!(loaded.review_state(&hl), ReviewState::None);
        assert_eq!(loaded.review_history(&sq).len(), 2);
        let replies: Vec<&AnnotId> = loaded.replies(&sq).iter().map(|a| &a.id).collect();
        assert_eq!(replies, [&r1]);
        assert_eq!(loaded.thread(&sq).len(), 4);
        assert_eq!(loaded.get(&sq).unwrap().popup.as_ref(), Some(&sq_popup));
        assert_eq!(loaded.get(&sq_popup).unwrap().parent.as_ref(), Some(&sq));
        assert_eq!(
            common::pdfium_annotation_counts(
                &backend,
                pl_render::DocSource::Bytes(work.bytes()),
                &[target.page]
            ),
            [8]
        );

        // On the page PDFium shows the square and the highlight and nothing else.
        let r = common::render_pdf(&backend, work.bytes(), target.page, &geometry);
        let ours = common::raster_over(&shown, &geometry, &r.backdrop);
        let d = common::diff(&r.pdfium, &ours, 1, 40);
        if d.bad > 3 || d.mean > 0.6 {
            common::dump(&r.pdfium, &format!("thread-{label}-pdfium"));
            common::dump(&ours, &format!("thread-{label}-ours"));
            failures.push(format!("{label}: {d:?}"));
        }

        // An unrelated save rewrites none of them.
        let mut set = loaded;
        set.add(Annotation::line(
            target.page,
            Point::new(o.x + 10.0, o.y + 10.0),
            Point::new(o.x + 60.0, o.y + 20.0),
            &ada,
        ))
        .unwrap();
        work.save(&set);
        let last = work.doc().clone();
        common::assert_untouched(&after, &last, &[page_id]);
        let again = AnnotSet::load(&last);
        for want in &expected {
            assert_eq!(&again.get(&want.id).unwrap().detached(), want, "{label}");
        }
        // Removing the square removes its popup, replies and statuses, and nothing else.
        let mut set = again;
        let removed = set.remove(&sq);
        assert_eq!(removed.items.len(), 6, "{label}");
        let (plan, _) = work.save(&set);
        assert_eq!(plan.deleted.len(), 6);
        let left = AnnotSet::load(work.doc());
        assert_eq!(left.len(), 3, "highlight, its popup and the line");
        assert!(left.contains(&hl) && left.contains(&hl_popup));
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// A note with a popup, a reply and two status annotations, written as raw dictionaries the
/// way another application writes them (with keys of its own).
fn foreign_thread() -> (Work, [ObjectId; 5]) {
    let fixture = pl_testkit::plain();
    let mut work = Work::new(&fixture);
    let doc = work.doc().clone();
    let page = doc.page_id(0).unwrap();
    let mut edits = doc.edit();
    let ids: [ObjectId; 5] = std::array::from_fn(|_| edits.reserve_id());
    let [note, popup, reply, status_a, status_b] = ids;
    let common_keys = |subtype: &str, nm: &str, rect: [i64; 4]| {
        let mut d = Dictionary::new();
        d.set("Type", name("Annot"));
        d.set("Subtype", name(subtype));
        d.set("NM", text(nm));
        d.set(
            "Rect",
            Object::Array(rect.iter().map(|v| Object::Integer(*v)).collect()),
        );
        d.set("P", Object::Reference(page));
        d
    };
    let mut d = common_keys("Text", "foreign-note", [200, 500, 218, 518]);
    d.set("Contents", text("Original comment"));
    d.set("T", text("Reviewer One"));
    d.set("M", text("D:20260101090000Z"));
    d.set("F", Object::Integer(28));
    d.set(
        "C",
        Object::Array(vec![
            Object::Integer(1),
            Object::Integer(1),
            Object::Integer(0),
        ]),
    );
    d.set("Name", name("Comment"));
    d.set("Popup", Object::Reference(popup));
    d.set("VendorNote", Object::Integer(1));
    edits.set_object(note, Object::Dictionary(d));

    let mut d = common_keys("Popup", "foreign-popup", [230, 400, 410, 520]);
    d.set("Parent", Object::Reference(note));
    d.set("Open", Object::Boolean(true));
    edits.set_object(popup, Object::Dictionary(d));

    let mut d = common_keys("Text", "foreign-reply", [200, 500, 218, 518]);
    d.set("Contents", text("A reply"));
    d.set("T", text("Reviewer Two"));
    d.set("M", text("D:20260102090000Z"));
    d.set("IRT", Object::Reference(note));
    d.set("RT", name("R"));
    d.set("F", Object::Integer(28));
    d.set("VendorReply", text("keep me"));
    edits.set_object(reply, Object::Dictionary(d));

    for (id, nm, state, date) in [
        (
            status_a,
            "foreign-status-a",
            "Accepted",
            "D:20260103090000Z",
        ),
        (
            status_b,
            "foreign-status-b",
            "Rejected",
            "D:20260104090000Z",
        ),
    ] {
        let mut d = common_keys("Text", nm, [200, 500, 218, 518]);
        d.set("Contents", text(&format!("{state} set by Reviewer Two")));
        d.set("T", text("Reviewer Two"));
        d.set("M", text(date));
        d.set("IRT", Object::Reference(note));
        d.set("State", text(state));
        d.set("StateModel", text("Review"));
        d.set("F", Object::Integer(30));
        edits.set_object(id, Object::Dictionary(d));
    }
    for id in ids {
        edits.attach_annotation(0, id);
    }
    work.file
        .save_incremental(&edits, &SaveOptions::default())
        .unwrap();
    work.original = work.bytes();
    (work, ids)
}

#[test]
fn a_foreign_thread_is_read_and_preserved() {
    let (mut work, [note, popup, reply, status_a, status_b]) = foreign_thread();
    let before = work.doc().clone();
    let id = |s: &str| AnnotId::from(s);

    let mut set = AnnotSet::load(&before);
    assert_eq!(set.len(), 5);
    assert!(!set.is_dirty());
    let n = set.get(&id("foreign-note")).unwrap();
    assert_eq!(n.popup, Some(id("foreign-popup")));
    assert_eq!(n.icon.as_deref(), Some("Comment"));
    let p = set.get(&id("foreign-popup")).unwrap();
    assert_eq!((p.parent.clone(), p.open), (Some(id("foreign-note")), true));
    let r = set.get(&id("foreign-reply")).unwrap();
    assert_eq!(r.meta.reply_to, Some(id("foreign-note")));
    assert_eq!(r.meta.reply_type.as_deref(), Some("R"));
    assert!(r.is_reply() && r.review_state().is_none());
    let s = set.get(&id("foreign-status-b")).unwrap();
    assert_eq!(s.review_state(), Some(ReviewState::Rejected));
    assert!(s.meta.flags.has(Flags::HIDDEN) && !s.is_reply());
    assert_eq!(set.review_state(&id("foreign-note")), ReviewState::Rejected);
    assert_eq!(set.review_history(&id("foreign-note")).len(), 2);
    assert_eq!(set.replies(&id("foreign-note")).len(), 1);
    assert_eq!(
        set.ensure_popup(
            &id("foreign-note"),
            Rect::new(0.0, 0.0, 612.0, 792.0),
            &common::origin()
        ),
        Some(id("foreign-popup"))
    );
    assert!(!set.is_dirty(), "the popup it has is the popup it keeps");

    // Recolouring the note writes the note; the popup, the reply and the statuses are not
    // rewritten, and the note keeps its links and its vendor key.
    set.get_mut(&id("foreign-note")).unwrap().style.stroke = Some(Rgb::new(0.5, 0.75, 1.0));
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1);
    assert!(plan.added.is_empty() && plan.deleted.is_empty());
    let after = work.doc().clone();
    common::assert_untouched(&before, &after, &[note]);
    let d = dict_of(&after, note);
    assert_eq!(d.get(b"Popup").unwrap(), &Object::Reference(popup));
    assert_eq!(d.get(b"VendorNote").unwrap(), &Object::Integer(1));
    assert_eq!(
        dict_of(&after, reply).get(b"VendorReply").unwrap(),
        &text("keep me")
    );

    // A new status and a new reply go beside the old ones.
    let mut set = AnnotSet::load(&after);
    let cy = at("Cy Engineer", "D:20261124120000Z");
    let new_status = set
        .set_review_state(&id("foreign-note"), ReviewState::Completed, &cy)
        .unwrap();
    let new_reply = set.add_reply(&id("foreign-reply"), "Thanks", &cy).unwrap();
    let (plan, _) = work.save(&set);
    assert_eq!(plan.added.len(), 2);
    assert!(plan.modified.is_empty());
    let last = work.doc().clone();
    let page = last.page_id(0).unwrap();
    common::assert_untouched(&after, &last, &[page]);
    let added: HashMap<&AnnotId, ObjectId> = plan.added.iter().map(|c| (&c.id, c.object)).collect();
    assert_eq!(
        dict_of(&last, added[&new_status]).get(b"IRT").unwrap(),
        &Object::Reference(note)
    );
    assert_eq!(
        dict_of(&last, added[&new_reply]).get(b"IRT").unwrap(),
        &Object::Reference(reply)
    );
    let loaded = AnnotSet::load(&last);
    assert_eq!(
        loaded.review_state(&id("foreign-note")),
        ReviewState::Completed
    );
    assert_eq!(loaded.review_history(&id("foreign-note")).len(), 3);
    assert_eq!(loaded.thread(&id("foreign-note")).len(), 5);
    for object in [status_a, status_b] {
        assert_eq!(last.object(object), before.object(object));
    }

    // Deleting the note takes the whole thread out of the page.
    let mut set = loaded;
    assert_eq!(set.remove(&id("foreign-note")).items.len(), 7);
    work.save(&set);
    assert!(AnnotSet::load(work.doc()).is_empty());
}

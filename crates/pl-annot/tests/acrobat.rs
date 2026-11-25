//! Annotations saved by Adobe Acrobat (the pdf.js test files in `samples/external/pdfjs`,
//! local only and never committed; every test skips a file that is absent).
//!
//! * Each file reads, and a save that changes nothing writes nothing.
//! * A FreeText of each file is made editable, its text changed and the file saved: only that
//!   annotation's object is written again (every other object keeps its bytes and its place),
//!   and its rich text is the new text in plain XHTML paragraphs.
//! * What Acrobat's own FreeText says about `/DA`, `/DS` and `/RD` is recorded as found.

mod common;

use std::path::{Path, PathBuf};

use common::Work;
use pl_annot::{AnnotKind, AnnotSet, Annotation, Rgb};
use pl_pdf::{Object, ObjectId, PdfDocument};

const FILES: [&str; 5] = [
    "freetexts.pdf",
    "rotated_freetexts.pdf",
    "highlight_popup.pdf",
    "rotated_ink.pdf",
    "stamps.pdf",
];

fn sample(name: &str) -> Option<PathBuf> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("samples")
        .join("external")
        .join("pdfjs")
        .join(name);
    if path.is_file() {
        Some(path)
    } else {
        eprintln!("ACROBAT SAMPLE SKIPPED: samples/external/pdfjs/{name} is not present");
        None
    }
}

fn text_of(doc: &PdfDocument, a: &Annotation, key: &str) -> Option<String> {
    let d = doc.object(a.object_id()?)?.as_dict().ok()?;
    match d.get(key.as_bytes()).ok()? {
        Object::String(bytes, _) => Some(pl_annot::decode_text(bytes)),
        Object::Reference(id) => match doc.object(*id)? {
            Object::String(bytes, _) => Some(pl_annot::decode_text(bytes)),
            Object::Stream(s) => Some(String::from_utf8_lossy(&s.content).into_owned()),
            _ => None,
        },
        _ => None,
    }
}

#[test]
fn acrobat_files_read_and_an_unchanged_save_writes_nothing() {
    for name in FILES {
        let Some(path) = sample(name) else { continue };
        let doc = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
        let set = AnnotSet::load(&doc);
        assert!(!set.is_empty(), "{name}");
        assert!(!set.is_dirty(), "{name}");
        assert!(set.plan_save(&doc).unwrap().is_empty(), "{name}");
    }
}

/// Changes the text of the annotation at `index` of `name` (a FreeText made editable, or the
/// comment of another markup), saves, and checks what was written.
fn edit_one(name: &str, path: &Path, index: usize) {
    const NEW: &str = "Edited by Plumbline\nsecond line";
    let mut work = Work::from_bytes(name, std::fs::read(path).unwrap());
    let before = work.doc().clone();
    let mut set = AnnotSet::load(&before);
    let id = set.iter().nth(index).unwrap().id.clone();
    let had_rc = set.get(&id).unwrap().original_dict().unwrap().has(b"RC");
    let a = set.get_mut(&id).unwrap();
    let free_text = a.kind == AnnotKind::FreeText;
    if free_text {
        assert!(a.make_editable(), "{name} {id}");
        assert!(a.is_modelled());
    }
    a.meta.contents = Some(NEW.to_string());
    let (plan, _) = work.save(&set);
    assert_eq!(plan.modified.len(), 1, "{name} {id}");
    assert!(plan.added.is_empty() && plan.deleted.is_empty());
    let object: ObjectId = plan.modified[0].object;
    let after = work.doc().clone();
    // Every other object of the file is the same object at the same offset.
    common::assert_untouched(&before, &after, &[object]);

    let back = AnnotSet::load(&after);
    assert!(!back.is_dirty(), "{name} {id}");
    let got = back.get(&id).unwrap();
    assert_eq!(got.meta.contents.as_deref(), Some(NEW));
    // The rich text says the same as /Contents, a paragraph a line, in the XHTML body
    // Acrobat reads; a markup without rich text is not given any.
    let rc = text_of(&after, got, "RC");
    if free_text || had_rc {
        let rc = rc.unwrap_or_else(|| panic!("{name} {id}: /RC"));
        assert!(rc.starts_with("<?xml"), "{rc}");
        assert!(
            rc.contains("<body xmlns=\"http://www.w3.org/1999/xhtml\""),
            "{rc}"
        );
        assert!(
            rc.contains("<p>Edited by Plumbline</p><p>second line</p>"),
            "{rc}"
        );
        assert!(rc.ends_with("</body>"), "{rc}");
    } else {
        assert!(rc.is_none());
    }
    if free_text {
        // Ours now: a full appearance and the keys that describe it.
        assert!(got.free_text.is_some(), "{name} {id}");
        for key in ["DA", "DS", "AP"] {
            assert!(
                got.original_dict().unwrap().has(key.as_bytes()),
                "{name} {key}"
            );
        }
    }
}

#[test]
fn an_edited_acrobat_free_text_is_the_only_object_written() {
    for name in ["freetexts.pdf", "rotated_freetexts.pdf"] {
        let Some(path) = sample(name) else { continue };
        let doc = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
        let set = AnnotSet::load(&doc);
        let free_texts: Vec<usize> = set
            .iter()
            .enumerate()
            .filter(|(_, a)| a.kind == AnnotKind::FreeText)
            .map(|(i, _)| i)
            .collect();
        assert!(!free_texts.is_empty(), "{name}");
        for index in free_texts {
            edit_one(name, &path, index);
        }
    }
}

#[test]
fn an_edited_acrobat_comment_is_the_only_object_written() {
    for name in ["highlight_popup.pdf", "rotated_ink.pdf", "stamps.pdf"] {
        let Some(path) = sample(name) else { continue };
        let doc = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
        let set = AnnotSet::load(&doc);
        let first = set.iter().position(|a| a.kind != AnnotKind::Popup).unwrap();
        edit_one(name, &path, first);
    }
}

/// What the files show, recorded rather than assumed:
///
/// * The one FreeText Acrobat itself wrote in `freetexts.pdf` (a typewriter, rich text
///   `xfa:APIVersion="Acrobat:23.1.0"`) has `/DA (0 G 0 g ... /Cour 14 Tf)` and
///   `/DS (... color:#6BD929)`, and its appearance paints the text in #6BD929: the `/DA`
///   colours are not the text colour, which Acrobat takes from `/DS` (and `/RC`). It has no
///   border (`/BS /W 0`) and no `/RD`.
/// * No FreeText in `freetexts.pdf` or `rotated_freetexts.pdf` has `/RD`, so the order of
///   its four numbers cannot be confirmed from these files.
#[test]
fn acrobat_free_text_da_ds_and_rd_as_found() {
    for name in ["freetexts.pdf", "rotated_freetexts.pdf"] {
        let Some(path) = sample(name) else { continue };
        let doc = PdfDocument::from_bytes(std::fs::read(&path).unwrap(), None).unwrap();
        let set = AnnotSet::load(&doc);
        for a in set.iter().filter(|a| a.kind == AnnotKind::FreeText) {
            assert!(!a.original_dict().unwrap().has(b"RD"), "{name} {}", a.id);
        }
        if name != "freetexts.pdf" {
            continue;
        }
        let acrobat: Vec<&Annotation> = set
            .iter()
            .filter(|a| {
                a.kind == AnnotKind::FreeText
                    && text_of(&doc, a, "RC").is_some_and(|rc| rc.contains("Acrobat:23"))
            })
            .collect();
        assert_eq!(acrobat.len(), 1);
        let a = acrobat[0];
        let da = text_of(&doc, a, "DA").unwrap();
        assert!(da.starts_with("0 G 0 g "), "{da}");
        assert!(text_of(&doc, a, "DS").unwrap().contains("color:#6BD929"));
        // Read as Acrobat reads it: the text in the /DS colour, no border.
        let mut editable = a.clone();
        assert!(editable.make_editable());
        let t = editable.free_text.as_ref().unwrap();
        assert_eq!(
            t.color.to_u8(),
            Rgb::new(
                0x6B as f32 / 255.0,
                0xD9 as f32 / 255.0,
                0x29 as f32 / 255.0
            )
            .to_u8()
        );
        assert_eq!(editable.style.stroke, None);
    }
}

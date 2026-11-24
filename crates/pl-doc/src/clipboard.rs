//! Copy, cut and paste of annotations as plain serialisable data.
//!
//! A [`Clipboard`] holds copies of annotations with nothing of the document they came from, so
//! it can be pasted on another page or into another document, and its JSON
//! ([`Clipboard::to_json`]) can sit on the system clipboard under a private format.

use pl_annot::{AnnotKind, Annotation, Origin};
use pl_geom::{Point, Rect, Vec2};
use serde::{Deserialize, Serialize};

use crate::annot_data::AnnotData;

/// Where pasted annotations go on the target page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum PastePlacement {
    /// At the coordinates they were copied from.
    InPlace,
    /// Moved by this much from where they were copied (page units).
    Offset(Vec2),
    /// So that the centre of their common bounding box is at this point.
    CenteredAt(Point),
}

/// Copied annotations.
#[derive(Clone, Debug, PartialEq, Default, Serialize, Deserialize)]
pub struct Clipboard {
    /// The copies, in the stacking order they had.
    pub items: Vec<AnnotData>,
    /// How many annotations of the request could not be copied: popup windows (they follow
    /// their markup) and what this version cannot create (another application's text box or
    /// stamp, unknown kinds). Our own text boxes, stamps and pictures are copied whole; a
    /// picture is shared, not duplicated, by the copies in this process.
    #[serde(default)]
    pub skipped: usize,
}

impl Clipboard {
    /// Whether an annotation can be copied: not a popup, and of a kind that can be created.
    pub fn can_hold(a: &Annotation) -> bool {
        a.kind != AnnotKind::Popup && a.is_modelled()
    }

    /// Copies `annotations`. Popups and kinds that cannot be created are left out and counted
    /// in [`Clipboard::skipped`].
    pub fn from_annotations<'a>(
        annotations: impl IntoIterator<Item = &'a Annotation>,
    ) -> Clipboard {
        let mut clip = Clipboard::default();
        for a in annotations {
            if !Clipboard::can_hold(a) {
                clip.skipped += 1;
            } else {
                clip.items.push(AnnotData::from(a));
            }
        }
        clip
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string(self).expect("clipboard data is plain values")
    }

    pub fn from_json(json: &str) -> Result<Clipboard, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The box around the copied geometry (points and corners, not stroke widths).
    pub fn bounds(&self) -> Option<Rect> {
        self.items
            .iter()
            .map(|d| d.geometry.to_geometry().bounds())
            .reduce(|a, b| a.union(b))
    }

    /// New annotations for `page` made from the copies: new ids, no popup or reply links,
    /// author and dates from `origin`, moved as `placement` says. Everything else (geometry,
    /// style, text, private data) is as copied.
    pub fn instantiate(
        &self,
        page: usize,
        placement: PastePlacement,
        origin: &Origin,
    ) -> Vec<Annotation> {
        let by = match placement {
            PastePlacement::InPlace => Vec2::ZERO,
            PastePlacement::Offset(by) => by,
            PastePlacement::CenteredAt(at) => self
                .bounds()
                .map_or(Vec2::ZERO, |bounds| at - bounds.center()),
        };
        self.items
            .iter()
            .map(|data| {
                let mut a = data.to_annotation().duplicate(origin);
                a.page = page;
                if by != Vec2::ZERO {
                    a.translate(by);
                }
                a
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pl_annot::{Geometry, PdfDate};

    fn origin() -> Origin {
        Origin::new("Ada", PdfDate::from_unix(1_700_000_000))
    }

    #[test]
    fn text_boxes_stamps_and_pictures_are_copied_whole() {
        use pl_annot::{StampContent, StampSpec};
        use std::sync::Arc;
        let o = origin();
        let mut note = Annotation::free_text(1, Rect::new(40.0, 500.0, 220.0, 560.0), "Falls", &o);
        note.set_rotation(90);
        let callout = Annotation::callout(
            1,
            Rect::new(300.0, 300.0, 420.0, 340.0),
            Point::new(250.0, 410.0),
            "Kerb",
            &o,
        );
        let stamp = Annotation::stamp(
            1,
            Rect::new(100.0, 100.0, 260.0, 160.0),
            StampSpec::approved(),
            &o,
        );
        let pixels: Vec<u8> = (0..16 * 8 * 4u32).map(|i| (i * 7 % 251) as u8).collect();
        let picture = Arc::new(pl_annot::RgbaImage::new(16, 8, pixels).unwrap());
        let image =
            Annotation::image_stamp(1, Rect::new(10.0, 10.0, 70.0, 40.0), picture.clone(), &o);
        let sources = [&note, &callout, &stamp, &image];
        let clip = Clipboard::from_annotations(sources);
        assert_eq!((clip.len(), clip.skipped), (4, 0));
        // Through JSON, as on the system clipboard, and straight from memory.
        for clip in [Clipboard::from_json(&clip.to_json()).unwrap(), clip] {
            let by = Vec2::new(12.0, -8.0);
            let pasted = clip.instantiate(2, PastePlacement::Offset(by), &o);
            assert_eq!(pasted.len(), 4);
            for (p, source) in pasted.iter().zip(sources) {
                let mut expected = source.detached();
                expected.translate(by);
                expected.id = p.id.clone();
                expected.page = 2;
                assert_eq!(*p, expected, "{}", source.kind.pdf_name());
                assert!(p.is_modelled());
            }
            // The callout's leader moved with its box.
            let leader = &pasted[1].free_text.as_ref().unwrap().callout;
            assert_eq!(leader[0], Point::new(262.0, 402.0));
            // The picture is the same picture, not a second copy of its pixels.
            let Some(StampContent::Image(pasted_picture)) =
                pasted[3].stamp.as_ref().map(|s| &s.content)
            else {
                panic!("the picture stamp lost its picture")
            };
            assert!(Arc::ptr_eq(pasted_picture, &picture));
        }
    }

    #[test]
    fn copies_paste_equal_apart_from_id_and_page() {
        let o = origin();
        let mut note = Annotation::note(0, Rect::new(0.0, 0.0, 18.0, 18.0), "n", &o);
        let popup = Annotation::popup_for(&mut note, Rect::new(30.0, 0.0, 130.0, 60.0), &o);
        let square = Annotation::square(0, Rect::new(100.0, 100.0, 200.0, 160.0), &o);
        let free = Annotation::new(
            AnnotKind::FreeText,
            0,
            Geometry::Rect(Rect::new(0.0, 0.0, 9.0, 9.0)),
            &o,
        );
        let clip = Clipboard::from_annotations([&note, &popup, &square, &free]);
        assert_eq!((clip.len(), clip.skipped), (2, 2));
        let clip = Clipboard::from_json(&clip.to_json()).unwrap();

        let pasted = clip.instantiate(3, PastePlacement::InPlace, &o);
        assert_eq!(pasted.len(), 2);
        for (p, source) in pasted.iter().zip([&note, &square]) {
            assert_ne!(p.id, source.id);
            let mut expected = source.detached();
            expected.id = p.id.clone();
            expected.page = 3;
            expected.popup = None;
            assert_eq!(*p, expected);
        }

        let moved = clip.instantiate(0, PastePlacement::Offset(Vec2::new(10.0, -5.0)), &o);
        assert_eq!(
            moved[1].geometry,
            Geometry::Rect(Rect::new(110.0, 95.0, 210.0, 155.0))
        );
        // The common box is 0,0 .. 200,160: its centre goes to the given point.
        let centred = clip.instantiate(0, PastePlacement::CenteredAt(Point::new(300.0, 300.0)), &o);
        assert_eq!(
            centred[1].geometry,
            Geometry::Rect(Rect::new(300.0, 320.0, 400.0, 380.0))
        );
        assert!(Clipboard::default().bounds().is_none());
        assert!(Clipboard::from_json("{}").is_err());
    }
}

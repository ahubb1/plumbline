//! In-memory counterparts of the full-rewrite save, for a caller that keeps a document being
//! restructured in memory (pl-doc's undoable page operations): apply an [`EditSet`] without
//! writing a file, and serialise a document to bytes exactly as the full save would write it.

use std::collections::BTreeMap;

use crate::document::PdfDocument;
use crate::edit::{EditSet, ResolvedEdits};
use crate::error::{PdfError, Result};
use crate::save::prepare_full;

impl PdfDocument {
    /// A new document with `edits` applied, as a full save would apply them, but in memory.
    /// `edits` must have been started from this document ([`PdfDocument::edit`]). Objects the
    /// edits leave unreferenced stay in the object graph until the document is serialised
    /// ([`PdfDocument::to_bytes`]) or saved, which prune them.
    pub fn with_edits(&self, edits: &EditSet) -> Result<PdfDocument> {
        let resolved = edits.resolve(&self.doc, &self.pages, self.state)?;
        let mut doc = self.doc.clone();
        for (id, object) in resolved.objects {
            doc.objects.insert(id, object);
        }
        doc.max_id = doc.max_id.max(resolved.max_id);
        PdfDocument::derived(self.bytes.clone(), doc)
    }

    /// The document as one revision, serialised the way the full save writes it (unused
    /// objects pruned, object and cross-reference streams kept if the document had them, an
    /// encrypted document encrypted again with its own keys). Reading the bytes back with
    /// [`PdfDocument::from_bytes`] gives the document a file saved from this one would hold.
    pub fn to_bytes(&self) -> Result<Vec<u8>> {
        let none = ResolvedEdits {
            objects: BTreeMap::new(),
            annotation_counts: Vec::new(),
            max_id: 0,
        };
        let (mut doc, options) = prepare_full(self.doc.clone(), none)?;
        let mut out = Vec::with_capacity(self.bytes.len() + 4096);
        doc.save_with_options(&mut out, options)
            .map_err(PdfError::Write)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lopdf::{dictionary, Object};

    fn fixture() -> PdfDocument {
        PdfDocument::from_bytes(pl_testkit::plain().bytes, None).unwrap()
    }

    #[test]
    fn bytes_read_back_as_the_same_pages() {
        let doc = fixture();
        let bytes = doc.to_bytes().unwrap();
        let back = PdfDocument::from_bytes(bytes, None).unwrap();
        assert_eq!(back.page_count(), doc.page_count());
        assert_eq!(back.pages().unwrap(), doc.pages().unwrap());
    }

    #[test]
    fn edits_apply_in_memory_and_the_original_is_untouched() {
        let doc = fixture();
        let before = doc.annotation_counts();
        let mut edits = doc.edit();
        edits.add_annotation(
            0,
            dictionary! {
                "Type" => Object::Name(b"Annot".to_vec()),
                "Subtype" => Object::Name(b"Square".to_vec()),
                "Rect" => Object::Array(vec![10.into(), 10.into(), 50.into(), 50.into()]),
            },
        );
        let edited = doc.with_edits(&edits).unwrap();
        assert_eq!(edited.annotation_counts()[0], before[0] + 1);
        assert_eq!(doc.annotation_counts(), before);
        // A second application of the same set is refused: it belongs to `doc`'s state.
        assert!(matches!(
            edited.with_edits(&edits),
            Err(PdfError::StaleEditSet)
        ));
        let back = PdfDocument::from_bytes(edited.to_bytes().unwrap(), None).unwrap();
        assert_eq!(back.annotation_counts(), edited.annotation_counts());
    }
}

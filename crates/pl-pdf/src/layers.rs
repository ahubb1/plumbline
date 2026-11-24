//! Optional-content layers: listing them, and the layer-override builder.
//!
//! PDFium has no call that switches an optional-content group, so a layer is toggled for
//! display by re-opening the render mirror from the original bytes followed by an in-memory
//! incremental update whose catalog carries a new `/OCProperties /D`. This module builds that
//! update. Nothing here writes to the file.

use lopdf::{Dictionary, IncrementalDocument, Object, ObjectId};

use crate::document::PdfDocument;
use crate::error::{PdfError, Result};

/// One optional-content group listed in the catalog's `/OCProperties /OCGs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LayerInfo {
    pub id: ObjectId,
    /// `/Name`, lossily decoded.
    pub name: String,
    /// Whether the default configuration `/D` shows the layer.
    pub default_visible: bool,
}

impl PdfDocument {
    /// The document's layers in `/OCGs` order; empty when it has none.
    pub fn layers(&self) -> Vec<LayerInfo> {
        let Some(ocp) = self.oc_properties() else {
            return Vec::new();
        };
        let default = ocp
            .get(b"D")
            .ok()
            .and_then(|o| self.resolve(o))
            .and_then(|o| o.as_dict().ok());
        let base_off = default
            .and_then(|d| d.get(b"BaseState").ok())
            .and_then(|o| o.as_name().ok())
            .is_some_and(|n| n == b"OFF");
        let listed = |key: &[u8]| -> Vec<ObjectId> {
            default
                .and_then(|d| d.get(key).ok())
                .and_then(|o| self.resolve(o))
                .and_then(|o| o.as_array().ok())
                .map(|a| a.iter().filter_map(|o| o.as_reference().ok()).collect())
                .unwrap_or_default()
        };
        let (on, off) = (listed(b"ON"), listed(b"OFF"));
        let groups = ocp
            .get(b"OCGs")
            .ok()
            .and_then(|o| self.resolve(o))
            .and_then(|o| o.as_array().ok());
        let Some(groups) = groups else {
            return Vec::new();
        };
        groups
            .iter()
            .filter_map(|o| o.as_reference().ok())
            .map(|id| {
                let name = self
                    .doc
                    .get_dictionary(id)
                    .ok()
                    .and_then(|d| d.get(b"Name").ok())
                    .and_then(|o| o.as_str().ok())
                    .map(decode_layer_name)
                    .unwrap_or_default();
                let default_visible = if base_off {
                    on.contains(&id)
                } else {
                    !off.contains(&id)
                };
                LayerInfo {
                    id,
                    name,
                    default_visible,
                }
            })
            .collect()
    }

    fn oc_properties(&self) -> Option<&Dictionary> {
        let catalog = self.doc.catalog().ok()?;
        let ocp = self.resolve(catalog.get(b"OCProperties").ok()?)?;
        ocp.as_dict().ok()
    }

    /// The bytes to present after the file's own bytes so that exactly the layers in `off` are
    /// hidden and every other layer is shown: an incremental update that rewrites the catalog
    /// with a new default configuration (`/BaseState /ON`, `/OFF [...]`). The rest of the
    /// existing `/D` (order, names, locked groups) is kept.
    ///
    /// The result is only the appended tail; `bytes()` followed by it is a valid PDF. Pass it
    /// as the `appended` part of a render-mirror open. It is never written to the file.
    ///
    /// Cost: the parsed document and its bytes are cloned for the writer, so on a very large
    /// file a toggle briefly needs a second copy of both.
    pub fn layer_override_tail(&self, off: &[ObjectId]) -> Result<Vec<u8>> {
        if self.doc.xref_recovery.is_some() || self.doc.xref_start == 0 {
            return Err(PdfError::IncrementalUnsupported(
                "its cross-reference data was damaged and had to be repaired on load".into(),
            ));
        }
        let root = self
            .doc
            .trailer
            .get(b"Root")
            .and_then(Object::as_reference)
            .map_err(PdfError::Parse)?;
        let Some(ocp) = self.oc_properties() else {
            return Err(PdfError::NoLayers);
        };
        let known: Vec<ObjectId> = self.layers().iter().map(|l| l.id).collect();
        if let Some(stranger) = off.iter().find(|id| !known.contains(id)) {
            return Err(PdfError::ObjectNotFound(*stranger));
        }

        let mut config = ocp
            .get(b"D")
            .ok()
            .and_then(|o| self.resolve(o))
            .and_then(|o| o.as_dict().ok())
            .cloned()
            .unwrap_or_default();
        config.set("BaseState", Object::Name(b"ON".to_vec()));
        config.remove(b"ON");
        config.set(
            "OFF",
            Object::Array(off.iter().map(|id| Object::Reference(*id)).collect()),
        );
        let mut new_ocp = ocp.clone();
        new_ocp.set("D", Object::Dictionary(config));

        let original_len = self.bytes.len();
        let mut inc = IncrementalDocument::create_from(self.bytes.clone(), self.doc.clone());
        for key in crate::save::STALE_TRAILER_KEYS {
            inc.new_document.trailer.remove(key);
        }
        inc.opt_clone_object_to_new_document(root)
            .map_err(PdfError::Parse)?;
        inc.new_document
            .get_object_mut(root)
            .and_then(Object::as_dict_mut)
            .map_err(PdfError::Parse)?
            .set("OCProperties", Object::Dictionary(new_ocp));
        let mut out = Vec::with_capacity(original_len + 4096);
        inc.save_to(&mut out).map_err(PdfError::Write)?;
        if out.len() <= original_len || out[..original_len] != self.bytes[..] {
            return Err(PdfError::Write(std::io::Error::other(
                "the layer override is not append-only",
            )));
        }
        out.drain(..original_len);
        Ok(out)
    }
}

/// Decodes a layer's /Name text string: UTF-16BE (or little-endian) with a byte-order mark,
/// UTF-8 with a BOM, otherwise UTF-8 if valid, else one byte per character.
fn decode_layer_name(bytes: &[u8]) -> String {
    let utf16 = |rest: &[u8], be: bool| {
        let units: Vec<u16> = rest
            .chunks(2)
            .map(|c| {
                let pair = [c[0], *c.get(1).unwrap_or(&0)];
                if be { u16::from_be_bytes(pair) } else { u16::from_le_bytes(pair) }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    if let Some(rest) = bytes.strip_prefix(&[0xFE, 0xFF]) {
        return utf16(rest, true);
    }
    if let Some(rest) = bytes.strip_prefix(&[0xFF, 0xFE]) {
        return utf16(rest, false);
    }
    let bytes = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
    match std::str::from_utf8(bytes) {
        Ok(s) => s.to_owned(),
        Err(_) => bytes.iter().map(|&b| b as char).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::verify::{verify_bytes, Expected};

    #[test]
    fn layer_names_in_utf16_are_decoded() {
        assert_eq!(decode_layer_name(&[0xFE, 0xFF, 0x00, 0x4C, 0x00, 0x32, 0x00, 0xE9]), "L2\u{e9}");
        assert_eq!(decode_layer_name(&[0xFF, 0xFE, 0x4C, 0x00]), "L");
        assert_eq!(decode_layer_name(b"Walls"), "Walls");
        assert_eq!(decode_layer_name(&[0x4E, 0xE9]), "N\u{e9}");
    }

    #[test]
    fn layers_are_listed_with_their_default_state() {
        let fx = pl_testkit::layers();
        let doc = PdfDocument::from_bytes(fx.bytes.clone(), None).unwrap();
        let layers = doc.layers();
        assert_eq!(layers.len(), fx.manifest.layers.len());
        for (found, truth) in layers.iter().zip(&fx.manifest.layers) {
            assert_eq!(found.name, truth.name);
            assert_eq!(found.default_visible, truth.default_visible);
        }
        let plain = PdfDocument::from_bytes(pl_testkit::plain().bytes, None).unwrap();
        assert!(plain.layers().is_empty());
        assert!(matches!(
            plain.layer_override_tail(&[]),
            Err(PdfError::NoLayers)
        ));
    }

    #[test]
    fn the_override_tail_is_a_valid_append_that_changes_only_the_default_state() {
        let fx = pl_testkit::layers();
        let doc = PdfDocument::from_bytes(fx.bytes.clone(), None).unwrap();
        let layers = doc.layers();
        let tail = doc.layer_override_tail(&[layers[0].id]).unwrap();
        assert!(!tail.is_empty());

        let mut whole = fx.bytes.clone();
        whole.extend_from_slice(&tail);
        let expected = Expected {
            page_count: doc.page_count(),
            annotation_counts: doc.annotation_counts(),
        };
        // The full verification of an incremental save applies: prefix, xref as written,
        // /Prev chain, both parsers.
        let (_, after) = verify_bytes(whole, None, &expected, Some(&fx.bytes)).unwrap();
        let now = after.layers();
        assert!(!now[0].default_visible);
        assert!(now[1..].iter().all(|l| l.default_visible));
        assert_eq!(now[0].name, layers[0].name);

        // Everything on again, and every layer off.
        let mut whole = fx.bytes.clone();
        whole.extend_from_slice(&doc.layer_override_tail(&[]).unwrap());
        let after = PdfDocument::from_bytes(whole, None).unwrap();
        assert!(after.layers().iter().all(|l| l.default_visible));
        let all: Vec<ObjectId> = layers.iter().map(|l| l.id).collect();
        let mut whole = fx.bytes.clone();
        whole.extend_from_slice(&doc.layer_override_tail(&all).unwrap());
        let after = PdfDocument::from_bytes(whole, None).unwrap();
        assert!(after.layers().iter().all(|l| !l.default_visible));

        // An id that is not a layer is refused.
        assert!(matches!(
            doc.layer_override_tail(&[(9999, 0)]),
            Err(PdfError::ObjectNotFound((9999, 0)))
        ));
    }
}

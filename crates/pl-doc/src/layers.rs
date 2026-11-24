//! Layers (optional-content groups): listing them and switching them for display.
//!
//! PDFium has no call that switches a layer, so a toggle reopens the render mirror from the
//! file's bytes followed by an in-memory update that overrides the default configuration
//! (pl-pdf's `layer_override_tail`). Nothing is written to the file: layer visibility is a
//! view setting of this session. The annotations the store has hidden are hidden again in the
//! new mirror, and every page gets a new revision, so no stale tile is shown.

use pl_pdf::{ObjectId, PdfDocument};

use crate::error::{DocError, Result};
use crate::session::{Document, Origin};

/// One layer of a document and whether it is shown.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Layer {
    pub id: ObjectId,
    /// The layer's name; may be empty.
    pub name: String,
    /// Whether the file's own default configuration shows it.
    pub default_visible: bool,
    /// Whether this session shows it.
    pub visible: bool,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct LayerState {
    layers: Vec<Layer>,
}

impl LayerState {
    fn at_default(&self) -> bool {
        self.layers.iter().all(|l| l.visible == l.default_visible)
    }

    fn off(&self) -> Vec<ObjectId> {
        self.layers
            .iter()
            .filter(|l| !l.visible)
            .map(|l| l.id)
            .collect()
    }
}

impl Document {
    /// Takes a layer list read by a load job, unless the layers are known already.
    pub(crate) fn install_layers(&mut self, infos: Vec<pl_pdf::LayerInfo>) {
        if self.layers.is_none() {
            self.layers = Some(LayerState {
                layers: infos
                    .into_iter()
                    .map(|l| Layer {
                        id: l.id,
                        name: l.name,
                        default_visible: l.default_visible,
                        visible: l.default_visible,
                    })
                    .collect(),
            });
        }
    }

    /// The layers this session has switched off, when its choices differ from the file's own
    /// default configuration.
    pub(crate) fn layers_switched_off(&self) -> Option<Vec<ObjectId>> {
        self.layers
            .as_ref()
            .filter(|s| !s.at_default())
            .map(LayerState::off)
    }

    /// Forgets this session's layer choices (the display shows the file's own).
    pub(crate) fn reset_layer_choices(&mut self) {
        if let Some(state) = self.layers.as_mut() {
            for layer in &mut state.layers {
                layer.visible = layer.default_visible;
            }
        }
    }

    /// Runs `f` on the parsed file. A file that was not parsed before is released again.
    fn with_parsed<T>(&mut self, f: impl FnOnce(&PdfDocument) -> Result<T>) -> Result<T> {
        if let Origin::Memory { bytes, .. } = &self.origin {
            let doc = PdfDocument::from_bytes(bytes.to_vec(), self.password.as_deref())?;
            return f(&doc);
        }
        let was_loaded = self.pdf_is_loaded();
        let file = self.pdf()?;
        let result = match file.document() {
            Ok(doc) => f(doc),
            Err(e) => Err(e.into()),
        };
        if !was_loaded {
            file.unload();
        }
        result
    }

    /// The layers if they have been read ([`layers`](Document::layers)). Never waits.
    pub fn cached_layers(&self) -> Option<&[Layer]> {
        self.layers.as_ref().map(|s| s.layers.as_slice())
    }

    /// The document's layers in the file's order; empty when it has none. The first call
    /// parses the file with lopdf on the calling thread (as
    /// [`load_annots`](Document::load_annots) does).
    pub fn layers(&mut self) -> Result<&[Layer]> {
        if self.layers.is_none() {
            // A load on a worker thread reads the layers too: wait for it, do not parse twice.
            self.settle_load()?;
        }
        if self.layers.is_none() {
            let layers = self.with_parsed(|doc| {
                Ok(doc
                    .layers()
                    .into_iter()
                    .map(|l| Layer {
                        id: l.id,
                        name: l.name,
                        default_visible: l.default_visible,
                        visible: l.default_visible,
                    })
                    .collect())
            })?;
            self.layers = Some(LayerState { layers });
        }
        Ok(self.cached_layers().expect("just set"))
    }

    /// Shows or hides one layer in this session's display. Returns whether anything changed.
    /// The render mirror is reopened (see the module documentation): every page's revision
    /// goes up and [`render_id`](Document::render_id) changes. The file is not touched and
    /// the document does not count as modified.
    pub fn set_layer_visible(&mut self, id: ObjectId, visible: bool) -> Result<bool> {
        self.set_layers_visible(&[(id, visible)])
    }

    /// As [`set_layer_visible`](Document::set_layer_visible) for several layers with one
    /// reopen of the mirror.
    pub fn set_layers_visible(&mut self, changes: &[(ObjectId, bool)]) -> Result<bool> {
        self.refuse_while_saving()?;
        self.layers()?;
        let state = self.layers.as_mut().expect("loaded above");
        let before = state.layers.clone();
        for (id, visible) in changes {
            let layer = state
                .layers
                .iter_mut()
                .find(|l| l.id == *id)
                .ok_or(DocError::Pdf(pl_pdf::PdfError::ObjectNotFound(*id)))?;
            layer.visible = *visible;
        }
        if state.layers == before {
            return Ok(false);
        }
        if let Err(e) = self.apply_layer_override() {
            // The mirror is as it was (a failed open leaves the old one in place).
            self.layers = Some(LayerState { layers: before });
            return Err(e);
        }
        Ok(true)
    }

    /// Shows every layer as the file's default configuration has it.
    pub fn reset_layers(&mut self) -> Result<bool> {
        let changes: Vec<(ObjectId, bool)> = self
            .layers()?
            .iter()
            .map(|l| (l.id, l.default_visible))
            .collect();
        self.set_layers_visible(&changes)
    }

    fn apply_layer_override(&mut self) -> Result<()> {
        let Some(state) = self.layers.as_ref() else {
            return Ok(());
        };
        let tail = if state.at_default() {
            Vec::new()
        } else {
            let off = state.off();
            self.with_parsed(|doc| Ok(doc.layer_override_tail(&off)?))?
        };
        self.reopen_render(tail)
    }
}

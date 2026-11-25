//! Digital signature detection, so a caller can warn before a save that would invalidate one.
//!
//! A signature covers the file's bytes as signed (`/ByteRange`). An incremental save appends
//! after them and leaves the signature verifiable (subject to `/DocMDP` permissions); a full
//! rewrite, which every structural edit needs, changes those bytes and breaks every signature.

use std::collections::BTreeSet;

use lopdf::{decode_text_string, Dictionary, Document, Object, ObjectId};

use crate::dest::deref;
use crate::document::{number, PdfDocument};

/// What the document holds in the way of signatures.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SignatureInfo {
    /// Fully qualified names of signature fields that carry a signature (`/FT /Sig` with a
    /// `/V` signature dictionary).
    pub signed_fields: Vec<String>,
    /// Signature fields without a value (places to sign; a rewrite does not harm them).
    pub empty_signature_fields: Vec<String>,
    /// The catalog's `/Perms` has `/DocMDP` (a certification signature).
    pub doc_mdp: bool,
    /// The catalog's `/Perms` has `/UR3` or `/UR` (usage rights signature).
    pub usage_rights: bool,
    /// `/AcroForm /SigFlags` (bit 1: signatures exist, bit 2: append only).
    pub sig_flags: i64,
}

impl SignatureInfo {
    /// True when a full rewrite would invalidate at least one signature.
    pub fn is_signed(&self) -> bool {
        !self.signed_fields.is_empty() || self.doc_mdp || self.usage_rights
    }

    /// Names to show in a warning: the signed fields, then the permission signatures.
    pub fn descriptions(&self) -> Vec<String> {
        let mut out = self.signed_fields.clone();
        if self.doc_mdp {
            out.push("certification signature (DocMDP)".into());
        }
        if self.usage_rights {
            out.push("usage rights signature".into());
        }
        out
    }
}

fn field_name(doc: &Document, d: &Dictionary) -> Option<String> {
    d.get(b"T")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| decode_text_string(o).ok())
}

pub(crate) fn detect(doc: &Document) -> SignatureInfo {
    let mut info = SignatureInfo::default();
    let Ok(catalog) = doc.catalog() else {
        return info;
    };
    if let Some(Object::Dictionary(perms)) = catalog.get(b"Perms").ok().and_then(|o| deref(doc, o))
    {
        info.doc_mdp = perms.has(b"DocMDP");
        info.usage_rights = perms.has(b"UR3") || perms.has(b"UR");
    }
    let Some(Object::Dictionary(form)) = catalog.get(b"AcroForm").ok().and_then(|o| deref(doc, o))
    else {
        return info;
    };
    info.sig_flags = form
        .get(b"SigFlags")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(number)
        .map_or(0, |v| v as i64);
    let Some(Object::Array(fields)) = form.get(b"Fields").ok().and_then(|o| deref(doc, o)) else {
        return info;
    };
    let mut seen = BTreeSet::new();
    for f in fields {
        walk(doc, f, None, None, &mut info, &mut seen, 0);
    }
    info
}

fn walk(
    doc: &Document,
    node: &Object,
    parent_name: Option<&str>,
    inherited_ft: Option<&[u8]>,
    info: &mut SignatureInfo,
    seen: &mut BTreeSet<ObjectId>,
    depth: usize,
) {
    if depth > 32 {
        return;
    }
    if let Object::Reference(id) = node {
        if !seen.insert(*id) {
            return;
        }
    }
    let Some(Object::Dictionary(d)) = deref(doc, node) else {
        return;
    };
    let own = field_name(doc, d);
    let name = match (parent_name, &own) {
        (Some(p), Some(o)) => Some(format!("{p}.{o}")),
        (None, Some(o)) => Some(o.clone()),
        (Some(p), None) => Some(p.to_string()),
        (None, None) => None,
    };
    let ft = d
        .get(b"FT")
        .ok()
        .and_then(|o| deref(doc, o))
        .and_then(|o| o.as_name().ok())
        .or(inherited_ft);
    let kids: Vec<&Object> = match d.get(b"Kids").ok().and_then(|o| deref(doc, o)) {
        Some(Object::Array(k)) => k.iter().collect(),
        _ => Vec::new(),
    };
    // A kid that has a /T is a field of its own; kids without are this field's widgets.
    let field_kids: Vec<&Object> = kids
        .iter()
        .copied()
        .filter(|k| {
            deref(doc, k)
                .and_then(|o| o.as_dict().ok())
                .is_some_and(|kd| kd.has(b"T"))
        })
        .collect();
    if field_kids.is_empty() && ft == Some(b"Sig".as_slice()) {
        let label = name.clone().unwrap_or_else(|| "(unnamed signature)".into());
        let signed = d
            .get(b"V")
            .ok()
            .and_then(|o| deref(doc, o))
            .is_some_and(|v| matches!(v, Object::Dictionary(_)));
        if signed {
            info.signed_fields.push(label);
        } else {
            info.empty_signature_fields.push(label);
        }
        return;
    }
    for k in field_kids {
        walk(doc, k, name.as_deref(), ft, info, seen, depth + 1);
    }
}

impl PdfDocument {
    /// Signatures in the document. Check [`SignatureInfo::is_signed`] before a full rewrite.
    pub fn signatures(&self) -> SignatureInfo {
        detect(&self.doc)
    }
}

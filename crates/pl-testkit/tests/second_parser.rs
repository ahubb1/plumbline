//! Cross-check with hayro-syntax, a parser that shares no code with lopdf
//! (which wrote the files): page counts, boxes, rotation and decoded content.

use hayro_syntax::page::Rotation;
use hayro_syntax::Pdf;
use pl_testkit::{all_fixtures_with, encrypted_token, Options, OWNER_PASSWORD};

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

#[test]
fn hayro_syntax_reads_every_fixture() {
    // Full-size dense sheet, small raster (its pixels are checked in the lopdf tests).
    let options = Options { raster_px: [255, 330], ..Options::default() };
    for fx in all_fixtures_with(&options) {
        let m = &fx.manifest;
        let pdf = Pdf::new(fx.bytes.clone()).unwrap_or_else(|e| panic!("{}: hayro-syntax failed: {e:?}", m.name));
        let pages = pdf.pages();
        assert_eq!(pages.len(), m.page_count(), "{}: page count", m.name);
        for (i, (page, truth)) in pages.iter().zip(&m.pages).enumerate() {
            let ctx = format!("{} page {}", m.name, i + 1);
            let mb = page.media_box();
            assert_eq!([mb.x0, mb.y0, mb.x1, mb.y1], truth.media_box, "{ctx}: MediaBox");
            let cb = page.crop_box();
            assert_eq!([cb.x0, cb.y0, cb.x1, cb.y1], truth.visible_box(), "{ctx}: CropBox");
            let rotate = match page.rotation() {
                Rotation::None => 0,
                Rotation::Horizontal => 90,
                Rotation::Flipped => 180,
                Rotation::FlippedHorizontal => 270,
            };
            assert_eq!(rotate, truth.rotate, "{ctx}: Rotate");

            let content = page.page_stream().unwrap_or_else(|| panic!("{ctx}: no content"));
            for t in &truth.texts {
                let shown = format!("({}) Tj", t.text.replace('(', "\\(").replace(')', "\\)"));
                assert!(contains(content, shown.as_bytes()), "{ctx}: text {:?} missing", t.text);
            }
            let strokes = content.windows(4).filter(|w| w == b" l S").count();
            assert_eq!(strokes, truth.segment_count, "{ctx}: stroked segments");
        }
        // The loop above already decrypted the encrypted fixtures with the empty
        // user password through hayro's own security-handler code. hayro-syntax
        // 0.9.0 implements the owner-password path only for revisions 5 and 6,
        // so the owner password is cross-checked for the AES-256 file alone
        // (lopdf checks it for RC4 in the other test file).
        if m.encryption.as_ref().is_some_and(|e| e.revision >= 5) {
            let owner = Pdf::new_with_password(fx.bytes.clone(), OWNER_PASSWORD)
                .unwrap_or_else(|e| panic!("{}: owner password: {e:?}", m.name));
            let content = owner.pages()[1].page_stream().unwrap();
            assert!(contains(content, encrypted_token(1).as_bytes()), "{}", m.name);
        }
    }
}

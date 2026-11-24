//! Test-only helpers shared by the unit tests.

use pl_render::{PageText, TextChar};

/// Builds page text from lines of `(string, baseline origin, font size)`. Each character is
/// `0.5 * size` wide and spans `-0.2 * size ..= 0.8 * size` around the baseline; lines are
/// separated by a generated `\r\n` without boxes, as PDFium does.
pub fn synthetic_text(page: u32, lines: &[(&str, [f64; 2], f64)]) -> PageText {
    let mut chars = Vec::new();
    for (li, (s, origin, size)) in lines.iter().enumerate() {
        if li > 0 {
            for ch in ['\r', '\n'] {
                chars.push(TextChar {
                    ch,
                    bbox: [0.0; 4],
                    origin: [0.0; 2],
                    font_size: *size as f32,
                    generated: true,
                });
            }
        }
        for (k, ch) in s.chars().enumerate() {
            let x = origin[0] + k as f64 * 0.5 * size;
            chars.push(TextChar {
                ch,
                bbox: [
                    x,
                    origin[1] - 0.2 * size,
                    x + 0.5 * size,
                    origin[1] + 0.8 * size,
                ],
                origin: [x, origin[1]],
                font_size: *size as f32,
                generated: false,
            });
        }
    }
    PageText { page, chars }
}

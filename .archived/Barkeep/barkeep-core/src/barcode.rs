use crate::card::{BarcodeType, Card};
use image::{GrayImage, Luma};
use rxing::{MultiFormatReader, MultiFormatWriter, Reader, Writer};
use std::path::{Path, PathBuf};

// -----------------------------------------------------------------------
// NOTE ON CONFIDENCE: originally written with no working `rxing` checkout
// to compile against, so the exact method names below were a best-effort
// idiomatic guess at the API rather than something actually verified.
// Since then, the two riskiest, most structurally-uncertain calls --
// BinaryBitmap::new(...) and Luma8LuminanceSource::new(...), the two
// most likely to have picked up a wrapper type (Rc<RefCell<...>>,
// Box::new(...)) across versions -- were checked directly against the
// real docs.rs pages for rxing 0.5.20, the exact version `rxing = "0.5"`
// in Cargo.toml resolves to, not just "latest" or an old README example.
// Both matched this file's own code exactly. RXingResult::getText()/
// getBarcodeFormat() were also confirmed stable, appearing identically
// in the crate's own README example across every published version from
// 0.2.12 through the current latest. BitMatrix::width()/height()/get(),
// HybridBinarizer::new(...), and MultiFormatWriter::encode(...) weren't
// individually re-verified the same way -- they follow the same
// consistent generics-based API (introduced well before the pinned 0.5
// line, at v0.4.0) already confirmed twice above, so the remaining risk
// there is low, but not eliminated the same way. If `cargo build` still
// errors out here, it's most likely one of these three.
// -----------------------------------------------------------------------

/// Tried a reduced quiet-zone margin here via encode hints at one point
/// (MARGIN, reduced to 2) -- worked, but confirmed inconsistent across
/// formats in practice (matching the known upstream ZXing limitation
/// that Aztec/DataMatrix writers don't honor MARGIN the way QR_CODE
/// does) and rolled back by request rather than kept as a partial,
/// format-dependent fix.
fn matrix_to_png(matrix: &rxing::common::BitMatrix, path: &Path) -> Option<()> {
    let width = matrix.width();
    let height = matrix.height();
    let mut img = GrayImage::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let on = matrix.get(x, y);
            img.put_pixel(x, y, Luma([if on { 0u8 } else { 255u8 }]));
        }
    }
    img.save(path).ok()
}

/// Target render canvas per format. This is what actually controls how
/// much of the available window space a barcode fills, since GTK's
/// content-fit=Contain just scales the source image to fit its box while
/// preserving *this* aspect ratio -- it can't invent proportions the
/// source image doesn't already have.
///
/// Linear (1D) formats have no inherent height -- the bar height isn't
/// part of the encoded data, so there's no "correct" proportion to
/// preserve -- so these get a tall canvas, letting them grow to fill
/// available vertical space rather than staying artificially squat.
/// 2D formats (QR/Aztec/Data Matrix) have a real square module grid, so
/// those get an equal width/height canvas to guarantee no distortion
/// regardless of how the box is shaped. PDF417 is the one 2D-ish format
/// that's naturally wide rather than square, so it gets a wide canvas
/// matching that.
/// 1D/linear formats render at 1px native height, not a tall canvas --
/// confirmed necessary, not just a nice-to-have: a height_request floor
/// on the display side (tried first) didn't fix reported fuzziness on a
/// height-squished window, and the actual cause is almost certainly
/// downscale/minification blur -- shrinking a *tall* source image to
/// fit a short widget blends multiple source rows together the same way
/// shrinking any image does. A 1D barcode has no vertical detail at all
/// (every row is identical -- the bars run the full height uniformly),
/// so there's nothing lost by only ever *upscaling* from a single
/// source row instead: stretching a 1px-tall source to any height just
/// reproduces that one row exactly, with nothing to blend against.
/// Bonus: also cheaper to render and store. 2D formats keep a real
/// square canvas (a genuine module grid, not simulated).
fn target_dimensions(barcode_type: BarcodeType) -> (i32, i32) {
    use BarcodeType::*;
    match barcode_type {
        QrCode | Aztec | DataMatrix => (700, 700),
        Pdf417 => (700, 260),
        Code39 | Code93 | Code128 | Codabar | Itf | Ean8 | Ean13 | UpcA | UpcE => (500, 1),
        NoCode => (500, 220), // unused -- NoCode never reaches the renderer
    }
}

/// Same idea as target_dimensions, but for the small add/edit dialog
/// preview specifically -- 1D formats there should read as a short,
/// wide strip spanning the dialog's width (like a real barcode label),
/// not the tall canvas target_dimensions uses for the main card detail
/// view. Also 1px native height now, for the same
/// only-ever-upscale-never-downscale reasoning as target_dimensions
/// above. 2D formats stay square; PDF417 stays wide -- neither of those
/// needed a separate preview shape.
fn preview_target_dimensions(barcode_type: BarcodeType) -> (i32, i32) {
    use BarcodeType::*;
    match barcode_type {
        Code39 | Code93 | Code128 | Codabar | Itf | Ean8 | Ean13 | UpcA | UpcE => (700, 1),
        other => target_dimensions(other),
    }
}

/// Renders a card's code to a PNG on disk (GTK loads PNG natively via
/// gdk-pixbuf, no extra SVG/rsvg dependency needed). Returns `None` for
/// `NoCode` cards or cards with no stored value -- the UI shows a
/// plain "no code yet" placeholder in that case (see show_card_detail).
pub fn render_png(card: &Card) -> Option<PathBuf> {
    let format = card.barcode_type.to_rxing()?;
    if card.code_value.is_empty() {
        return None;
    }
    let (w, h) = target_dimensions(card.barcode_type);
    let writer = MultiFormatWriter::default();
    let matrix = writer.encode(&card.code_value, &format, w, h).ok()?;

    let mut path = std::env::temp_dir();
    path.push(format!("barkeep-{}.png", card.id));
    matrix_to_png(&matrix, &path)?;
    Some(path)
}

/// Same as `render_png`, but for the live preview in the add/edit dialog,
/// where we don't have a saved `Card` yet -- just a candidate value + type.
/// `cache_key` just needs to be unique enough to avoid clobbering another
/// open dialog's preview file; the dialog's own transient lifetime is fine.
pub fn render_preview_png(value: &str, barcode_type: BarcodeType, cache_key: &str) -> Option<PathBuf> {
    let format = barcode_type.to_rxing()?;
    if value.is_empty() {
        return None;
    }
    let (w, h) = preview_target_dimensions(barcode_type);
    let writer = MultiFormatWriter::default();
    let matrix = writer.encode(value, &format, w, h).ok()?;

    let mut path = std::env::temp_dir();
    path.push(format!("barkeep-preview-{}.png", cache_key));
    matrix_to_png(&matrix, &path)?;
    Some(path)
}

/// Decodes a barcode/QR from an arbitrary image file (photo of a physical
/// card, screenshot of a digital pass, etc.) -- backs the "Scan from
/// Image" add-card flow. Returns the decoded text and its format if found.
pub fn decode_image(path: &Path) -> Option<(String, BarcodeType)> {
    let img = image::open(path).ok()?.to_luma8();
    let (width, height) = img.dimensions();

    let source = rxing::Luma8LuminanceSource::new(img.into_raw(), width, height);
    let mut bitmap = rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(source));

    let mut reader = MultiFormatReader::default();
    let result = reader.decode(&mut bitmap).ok()?;

    let text = result.getText().to_string();
    let format = BarcodeType::from_rxing(result.getBarcodeFormat().to_owned());
    Some((text, format))
}

fn try_decode_luma(data: &[u8], width: u32, height: u32) -> Option<(String, BarcodeType)> {
    let source = rxing::Luma8LuminanceSource::new(data.to_vec(), width, height);
    let mut bitmap = rxing::BinaryBitmap::new(rxing::common::HybridBinarizer::new(source));
    let mut reader = MultiFormatReader::default();
    let result = reader.decode(&mut bitmap).ok()?;
    let text = result.getText().to_string();
    let format = BarcodeType::from_rxing(result.getBarcodeFormat().to_owned());
    Some((text, format))
}

fn rotate_90_cw(data: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = vec![0u8; w * h];
    for y in 0..h {
        for x in 0..w {
            out[x * h + (h - 1 - y)] = data[y * w + x];
        }
    }
    out
}

/// Decodes a barcode/QR from a single raw grayscale frame, e.g. one pulled
/// off a live camera feed. Tries the frame as-is, then rotated 90/180/270,
/// since the camera sensor's physical mounting angle (fixed, independent
/// of how the phone is held) isn't known/corrected upstream -- whichever
/// orientation the person naturally holds the phone in should work
/// without them needing to guess the sensor's mounting angle themselves.
pub fn decode_luma(data: Vec<u8>, width: u32, height: u32) -> Option<(String, BarcodeType)> {
    if let Some(found) = try_decode_luma(&data, width, height) {
        return Some(found);
    }
    let r90 = rotate_90_cw(&data, width, height);
    if let Some(found) = try_decode_luma(&r90, height, width) {
        return Some(found);
    }
    let r180 = rotate_90_cw(&r90, height, width);
    if let Some(found) = try_decode_luma(&r180, width, height) {
        return Some(found);
    }
    let r270 = rotate_90_cw(&r180, width, height);
    try_decode_luma(&r270, height, width)
}

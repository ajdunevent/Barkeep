use crate::card::{BarcodeType, Card};
use image::{GrayImage, Luma};
use rxing::{MultiFormatReader, MultiFormatWriter, Reader, Writer};
use std::path::{Path, PathBuf};

/// A reduced quiet-zone margin (MARGIN, 2) was tried via encode hints
/// but was inconsistent across formats (Aztec/DataMatrix writers don't
/// honor MARGIN the way QR_CODE does), so it was rolled back.
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

/// Target render canvas per format. This controls how much of the
/// available window space a barcode fills, since GTK's
/// content-fit=Contain scales the source image to fit its box while
/// preserving this aspect ratio.
///
/// Linear (1D) formats have no inherent height (bar height isn't part
/// of the encoded data), so they get a tall canvas to fill available
/// vertical space. 2D formats (QR/Aztec/Data Matrix) get an equal
/// width/height canvas so no distortion happens regardless of box
/// shape. PDF417 is naturally wide rather than square, so it gets a
/// wide canvas.
///
/// 1D formats render at 1px native height rather than a tall canvas. A
/// height_request floor on the display side was tried first and didn't
/// fix reported fuzziness on a height-squished window; the actual
/// cause is downscale/minification blur from shrinking a tall source
/// image. A 1D barcode has no vertical detail (every row is
/// identical), so upscaling from a single source row loses nothing.
fn target_dimensions(barcode_type: BarcodeType) -> (i32, i32) {
    use BarcodeType::*;
    match barcode_type {
        QrCode | Aztec | DataMatrix => (700, 700),
        Pdf417 => (700, 260),
        Code39 | Code93 | Code128 | Codabar | Itf | Ean8 | Ean13 | UpcA | UpcE => (500, 1),
        NoCode => (500, 220), // unused: NoCode never reaches the renderer
    }
}

/// Same idea as target_dimensions, for the add/edit dialog preview: 1D
/// formats read as a short, wide strip spanning the dialog's width
/// instead of the tall canvas target_dimensions uses. Also 1px native
/// height, same reasoning as target_dimensions. 2D formats and PDF417
/// keep their normal shape.
fn preview_target_dimensions(barcode_type: BarcodeType) -> (i32, i32) {
    use BarcodeType::*;
    match barcode_type {
        Code39 | Code93 | Code128 | Codabar | Itf | Ean8 | Ean13 | UpcA | UpcE => (700, 1),
        other => target_dimensions(other),
    }
}

/// Renders a card's code to a PNG on disk (GTK loads PNG natively via
/// gdk-pixbuf, no extra SVG/rsvg dependency needed). Returns `None` for
/// `NoCode` cards or cards with no stored value.
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

/// Same as `render_png`, for the live preview in the add/edit dialog,
/// where there's no saved `Card` yet, just a candidate value + type.
/// `cache_key` only needs to be unique enough to avoid clobbering
/// another open dialog's preview file.
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

/// Decodes a barcode/QR from an arbitrary image file (photo of a
/// physical card, screenshot of a digital pass), backing the "Scan
/// from Image" add-card flow.
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

/// Decodes a barcode/QR from a single raw grayscale frame, e.g. from a
/// live camera feed. Tries the frame as-is, then rotated 90/180/270,
/// since the camera sensor's mounting angle isn't known upstream.
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

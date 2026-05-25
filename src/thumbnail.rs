//! Thumbnail generation for CLI uploads.
//!
//! Generates a JPEG thumbnail from image file bytes using a quality-cascade
//! strategy: try progressively smaller dimensions and lower JPEG quality
//! until the output fits within the server's encrypted-thumbnail size cap
//! (128 KB including AES-256-GCM overhead).
//!
//! Uses the `image` crate for decoding (JPEG, PNG, GIF, WebP, TIFF) and
//! JPEG encoding with quality control. The `image` crate's built-in WebP
//! encoder only supports lossless mode, so JPEG is used instead for
//! controllable file sizes.

use image::{DynamicImage, GenericImageView};
use std::io::Cursor;

/// Quality cascade: (max_longest_edge, jpeg_quality 1-100).
/// Tries each step in order; the first output that fits under TARGET_BYTES
/// wins. If none reach TARGET_BYTES, the first that fits under MAX_BYTES
/// is used as a fallback.
const LADDER: &[(u32, u8)] = &[
    (768, 82),
    (768, 74),
    (768, 66),
    (768, 58),
    (768, 50),
    (640, 58),
    (512, 56),
    (384, 54),
];

/// Ideal target: 100 KB plaintext.
const TARGET_BYTES: usize = 100 * 1024;

/// Hard ceiling: server accepts 128 KB encrypted (medium variant).
/// AES-256-GCM adds 28 bytes (12-byte nonce + 16-byte tag), so the
/// plaintext must stay under 128*1024 - 28.
const MAX_BYTES: usize = 128 * 1024 - 28;

/// Result of thumbnail generation.
#[allow(dead_code)]
pub struct ThumbnailResult {
    /// JPEG-encoded thumbnail bytes.
    pub data: Vec<u8>,
    /// Thumbnail width in pixels.
    pub width: u32,
    /// Thumbnail height in pixels.
    pub height: u32,
}

/// Generate a JPEG thumbnail from raw file bytes.
///
/// Returns `None` for non-image MIME types or if decoding/encoding fails.
pub fn generate_from_file(file_bytes: &[u8], mime: Option<&str>) -> Option<ThumbnailResult> {
    let is_image = mime.map(|m| m.starts_with("image/")).unwrap_or(false);
    if !is_image {
        return None;
    }

    let img = image::load_from_memory(file_bytes).ok()?;
    generate_from_image(&img)
}

fn generate_from_image(img: &DynamicImage) -> Option<ThumbnailResult> {
    let (src_w, src_h) = img.dimensions();
    let mut fallback: Option<ThumbnailResult> = None;

    for &(max_dim, quality) in LADDER {
        let (w, h) = fit_dimensions(src_w, src_h, max_dim);
        let resized = if w < src_w || h < src_h {
            img.resize_exact(w, h, image::imageops::FilterType::Lanczos3)
        } else {
            img.clone()
        };

        let encoded = match encode_jpeg(&resized, quality) {
            Some(bytes) => bytes,
            None => continue,
        };

        if encoded.len() <= TARGET_BYTES {
            return Some(ThumbnailResult {
                data: encoded,
                width: w,
                height: h,
            });
        }

        if encoded.len() <= MAX_BYTES && fallback.is_none() {
            fallback = Some(ThumbnailResult {
                data: encoded,
                width: w,
                height: h,
            });
        }
    }

    fallback
}

/// Scale (src_w, src_h) so the longest edge is at most `max_dim`, preserving
/// aspect ratio. Returns the original dimensions if already small enough.
fn fit_dimensions(src_w: u32, src_h: u32, max_dim: u32) -> (u32, u32) {
    let longest = src_w.max(src_h);
    if longest <= max_dim {
        return (src_w, src_h);
    }
    let scale = max_dim as f64 / longest as f64;
    let w = (src_w as f64 * scale).round().max(1.0) as u32;
    let h = (src_h as f64 * scale).round().max(1.0) as u32;
    (w, h)
}

/// Encode a `DynamicImage` as JPEG with the given quality (1-100).
fn encode_jpeg(img: &DynamicImage, quality: u8) -> Option<Vec<u8>> {
    let rgb = img.to_rgb8();
    let mut buf = Cursor::new(Vec::new());
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    encoder
        .encode(
            rgb.as_raw(),
            rgb.width(),
            rgb.height(),
            image::ExtendedColorType::Rgb8,
        )
        .ok()?;
    Some(buf.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fit_dimensions_no_change_when_small() {
        assert_eq!(fit_dimensions(400, 300, 768), (400, 300));
    }

    #[test]
    fn fit_dimensions_scales_landscape() {
        let (w, h) = fit_dimensions(2000, 1000, 768);
        assert_eq!(w, 768);
        assert_eq!(h, 384);
    }

    #[test]
    fn fit_dimensions_scales_portrait() {
        let (w, h) = fit_dimensions(1000, 2000, 768);
        assert_eq!(w, 384);
        assert_eq!(h, 768);
    }

    #[test]
    fn fit_dimensions_square() {
        let (w, h) = fit_dimensions(1500, 1500, 768);
        assert_eq!(w, 768);
        assert_eq!(h, 768);
    }

    #[test]
    fn non_image_mime_returns_none() {
        assert!(generate_from_file(b"not an image", Some("text/plain")).is_none());
    }

    #[test]
    fn no_mime_returns_none() {
        assert!(generate_from_file(b"data", None).is_none());
    }

    #[test]
    fn corrupt_image_returns_none() {
        assert!(generate_from_file(b"not valid image data", Some("image/jpeg")).is_none());
    }

    #[test]
    fn generates_thumbnail_from_synthetic_image() {
        // Create a small synthetic JPEG in memory
        let img = image::RgbImage::from_fn(100, 100, |x, y| {
            image::Rgb([(x % 256) as u8, (y % 256) as u8, 128])
        });
        let mut jpeg_bytes = Cursor::new(Vec::new());
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, 90);
        encoder
            .encode(img.as_raw(), 100, 100, image::ExtendedColorType::Rgb8)
            .unwrap();

        let result = generate_from_file(jpeg_bytes.get_ref(), Some("image/jpeg"));
        assert!(result.is_some());
        let thumb = result.unwrap();
        assert!(thumb.data.len() <= MAX_BYTES);
        assert!(thumb.width <= 768);
        assert!(thumb.height <= 768);
    }

    #[test]
    fn thumbnail_respects_max_bytes() {
        // Create a large-ish synthetic image
        let img = image::RgbImage::from_fn(2000, 1500, |x, y| {
            image::Rgb([
                ((x * 7 + y * 13) % 256) as u8,
                ((x * 11 + y * 3) % 256) as u8,
                ((x * 5 + y * 17) % 256) as u8,
            ])
        });
        let mut jpeg_bytes = Cursor::new(Vec::new());
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, 95);
        encoder
            .encode(img.as_raw(), 2000, 1500, image::ExtendedColorType::Rgb8)
            .unwrap();

        let result = generate_from_file(jpeg_bytes.get_ref(), Some("image/jpeg"));
        assert!(result.is_some());
        let thumb = result.unwrap();
        assert!(
            thumb.data.len() <= MAX_BYTES,
            "thumbnail {} bytes exceeds max {}",
            thumb.data.len(),
            MAX_BYTES
        );
    }
}

//! Thumbnail generation for CLI uploads.
//!
//! Decodes image files using the `image` crate (JPEG, PNG, GIF, WebP, TIFF),
//! converts to raw RGBA bytes, then delegates to
//! `beebeeb_core::thumbnail::generate_thumbnail` for lossy WebP encoding via
//! libwebp. This ensures the CLI produces the same thumbnail format as all
//! other Beebeeb clients (iOS, Android, desktop).

use image::GenericImageView;

/// Result of thumbnail generation.
#[allow(dead_code)]
pub struct ThumbnailResult {
    /// WebP-encoded thumbnail bytes.
    pub data: Vec<u8>,
    /// Thumbnail width in pixels.
    pub width: u32,
    /// Thumbnail height in pixels.
    pub height: u32,
}

/// Generate a WebP thumbnail from raw file bytes.
///
/// Returns `None` for non-image MIME types, if decoding fails, or if the core
/// encoder cannot produce output within the byte budget.
pub fn generate_from_file(file_bytes: &[u8], mime: Option<&str>) -> Option<ThumbnailResult> {
    let is_image = mime.map(|m| m.starts_with("image/")).unwrap_or(false);
    if !is_image {
        return None;
    }

    // Decode using `image` — handles JPEG, PNG, GIF, WebP, TIFF.
    let img = image::load_from_memory(file_bytes).ok()?;
    let (width, height) = img.dimensions();

    // Convert to raw RGBA bytes that the core expects.
    let rgba = img.into_rgba8().into_raw();

    // Delegate to the canonical core implementation for lossy WebP encoding.
    let config = beebeeb_core::thumbnail::ThumbnailConfig::medium();
    let output = beebeeb_core::thumbnail::generate_thumbnail(&rgba, width, height, &config).ok()?;

    Some(ThumbnailResult {
        data: output.data,
        width: output.width,
        height: output.height,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Hard ceiling from `ThumbnailConfig::medium()` — must stay in sync.
    const MAX_BYTES: usize = 128 * 1024 - 28;

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
        // Create a small synthetic JPEG in memory.
        let img = image::RgbImage::from_fn(100, 100, |x, y| image::Rgb([(x % 256) as u8, (y % 256) as u8, 128]));
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
    fn thumbnail_output_is_webp() {
        // Verify the output starts with the RIFF header (WebP magic bytes).
        let img = image::RgbImage::from_fn(200, 150, |x, y| {
            image::Rgb([
                ((x * 7 + y * 13) % 256) as u8,
                ((x * 3 + y * 11) % 256) as u8,
                ((x * 5 + y * 17) % 256) as u8,
            ])
        });
        let mut jpeg_bytes = Cursor::new(Vec::new());
        let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg_bytes, 90);
        encoder
            .encode(img.as_raw(), 200, 150, image::ExtendedColorType::Rgb8)
            .unwrap();

        let result = generate_from_file(jpeg_bytes.get_ref(), Some("image/jpeg"));
        assert!(result.is_some());
        let thumb = result.unwrap();
        assert!(
            thumb.data.len() >= 4 && &thumb.data[..4] == b"RIFF",
            "output should start with RIFF (WebP) header, got {:?}",
            &thumb.data[..4.min(thumb.data.len())]
        );
    }

    #[test]
    fn thumbnail_respects_max_bytes() {
        // Create a large-ish synthetic image.
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

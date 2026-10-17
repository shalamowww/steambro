//! Turns a source image into the file Steam shows in a slot: crop to the slot's shape, shrink to
//! the slot's size, and encode.

use std::io::Cursor;

use image::{DynamicImage, GenericImageView, ImageFormat, imageops::FilterType};

use super::{Anchor, Crop, Slot};

pub struct Rendered {
    pub bytes: Vec<u8>,
    /// File extension without the dot: `png` or `jpg`.
    pub ext: &'static str,
}

/// Icons larger than this are shrunk; Steam shows them small.
const ICON_SIZE: u32 = 256;

/// `crop` applies to slots with a shape; `None` means the default crop, placed by `anchor`.
pub fn render(source: &[u8], slot: Slot, crop: Option<Crop>, anchor: Anchor) -> crate::Result<Rendered> {
    let format = image::guess_format(source)?;
    let image = image::load_from_memory_with_format(source, format)?;
    let (width, height) = image.dimensions();
    let Some(shape) = slot.size() else {
        // A logo needs its transparency, and Steam reads icons as PNG reliably.
        if format == ImageFormat::Png && (slot == Slot::Logo || width.max(height) <= ICON_SIZE) {
            return Ok(Rendered { bytes: source.to_vec(), ext: "png" });
        }
        let image = if slot == Slot::Icon && width.max(height) > ICON_SIZE {
            image.resize(ICON_SIZE, ICON_SIZE, FilterType::Lanczos3)
        } else {
            image
        };
        return encode(&image, ImageFormat::Png);
    };

    let (x, y, w, h) = crop.unwrap_or_else(|| Crop::default_for((width, height), shape, anchor)).pixels((width, height));
    let whole = (x, y, w, h) == (0, 0, width, height);
    // Keep the original file when nothing changes, to avoid a second lossy encode.
    if whole && w <= shape.0 && matches!(format, ImageFormat::Png | ImageFormat::Jpeg) {
        let ext = if format == ImageFormat::Png { "png" } else { "jpg" };
        return Ok(Rendered { bytes: source.to_vec(), ext });
    }
    let mut out = image.crop_imm(x, y, w, h);
    if w > shape.0 {
        let new_h = (u64::from(h) * u64::from(shape.0) / u64::from(w)).max(1) as u32;
        out = out.resize_exact(shape.0, new_h, FilterType::Lanczos3);
    }
    if out.color().has_alpha() && format == ImageFormat::Png { encode(&out, ImageFormat::Png) } else { encode(&out, ImageFormat::Jpeg) }
}

fn encode(image: &DynamicImage, format: ImageFormat) -> crate::Result<Rendered> {
    let mut bytes = Vec::new();
    let ext = match format {
        ImageFormat::Jpeg => {
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, 90);
            DynamicImage::ImageRgb8(image.to_rgb8()).write_with_encoder(encoder)?;
            "jpg"
        }
        _ => {
            image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
            "png"
        }
    };
    Ok(Rendered { bytes, ext })
}

/// The MIME type of an image, for serving it to the UI.
pub fn mime(bytes: &[u8]) -> &'static str {
    match image::guess_format(bytes) {
        Ok(ImageFormat::Png) => "image/png",
        Ok(ImageFormat::Jpeg) => "image/jpeg",
        Ok(ImageFormat::WebP) => "image/webp",
        _ => "application/octet-stream",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(width: u32, height: u32) -> Vec<u8> {
        let image = DynamicImage::new_rgb8(width, height);
        let mut bytes = Vec::new();
        image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png).unwrap();
        bytes
    }

    #[test]
    fn header_from_portrait_is_cropped_to_shape() {
        let out = render(&png(600, 800), Slot::Hero, None, Anchor::Center).unwrap();
        let image = image::load_from_memory(&out.bytes).unwrap();
        assert_eq!(image.dimensions(), (600, 194));
        assert_eq!(out.ext, "jpg");
    }

    #[test]
    fn matching_cover_is_kept_as_is() {
        let source = png(600, 900);
        let out = render(&source, Slot::Cover, None, Anchor::Center).unwrap();
        assert_eq!(out.bytes, source);
    }

    #[test]
    fn big_cover_is_shrunk_to_slot_size() {
        let out = render(&png(1200, 1800), Slot::Cover, None, Anchor::Center).unwrap();
        assert_eq!(image::load_from_memory(&out.bytes).unwrap().dimensions(), (600, 900));
    }
}

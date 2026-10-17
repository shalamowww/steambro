//! Steam library artwork: the slots, where their images come from, and how an image is cut to fit
//! a slot.

pub mod cache;
pub mod http;
pub mod render;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

/// The artwork slots of a game in Steam's library.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Slot {
    /// Portrait capsule in the library grid.
    Cover,
    /// The banner at the top of the game's page. Steam calls it the hero.
    Hero,
    /// Transparent title logo that Steam shows over the header.
    Logo,
    /// Landscape capsule, used by "recent games" and some views.
    Wide,
    /// Small icon in lists. Steam reads it from the shortcut, not from the grid folder name.
    Icon,
}

impl Slot {
    pub const ALL: [Slot; 5] = [Slot::Cover, Slot::Hero, Slot::Logo, Slot::Wide, Slot::Icon];

    /// The slot by the name the window uses for it, like `hero`.
    pub fn from_name(name: &str) -> Option<Slot> {
        serde_json::from_value(serde_json::Value::String(name.to_string())).ok()
    }

    pub fn label(self) -> &'static str {
        match self {
            Slot::Cover => "cover",
            Slot::Hero => "header",
            Slot::Logo => "logo",
            Slot::Wide => "wide cover",
            Slot::Icon => "icon",
        }
    }

    /// The size Steam shows the slot at. An image for a slot with a size is cropped to its shape;
    /// logos and icons are used whole.
    pub fn size(self) -> Option<(u32, u32)> {
        match self {
            Slot::Cover => Some((600, 900)),
            Slot::Hero => Some((3840, 1240)),
            Slot::Wide => Some((920, 430)),
            Slot::Logo | Slot::Icon => None,
        }
    }

    /// The file name, without extension, that Steam looks for in `userdata\<account>\config\grid`.
    /// Steam has no name for icons; steambro's own name is set in the shortcut instead.
    pub fn grid_stem(self, app_id: u32) -> String {
        match self {
            Slot::Cover => format!("{app_id}p"),
            Slot::Hero => format!("{app_id}_hero"),
            Slot::Logo => format!("{app_id}_logo"),
            Slot::Wide => format!("{app_id}"),
            Slot::Icon => format!("{app_id}_icon"),
        }
    }
}

/// Where an artwork image comes from before steambro keeps its own copy.
#[derive(Clone, Debug)]
pub enum Origin {
    File(PathBuf),
    Url(String),
    /// The icon of an exe, as a PNG.
    ExeIcon(PathBuf),
}

impl Origin {
    pub fn fetch(&self) -> crate::Result<Vec<u8>> {
        match self {
            Origin::File(path) => Ok(std::fs::read(path)?),
            Origin::Url(url) => http::get(url),
            Origin::ExeIcon(exe) => exe_icon(exe),
        }
    }
}

/// The exe's icon at 256 pixels, as Explorer shows it, encoded as a PNG.
fn exe_icon(exe: &std::path::Path) -> crate::Result<Vec<u8>> {
    use windows::Win32::Foundation::SIZE;
    use windows::Win32::Graphics::Gdi::{
        BI_RGB, BITMAP, BITMAPINFO, BITMAPINFOHEADER, CreateCompatibleDC, DIB_RGB_COLORS, DeleteDC, DeleteObject, GetDIBits, GetObjectW, HGDIOBJ,
    };
    use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
    use windows::Win32::UI::Shell::{IShellItemImageFactory, SHCreateItemFromParsingName, SIIGBF_ICONONLY};
    use windows::core::HSTRING;
    unsafe {
        let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
        let item: IShellItemImageFactory = SHCreateItemFromParsingName(&HSTRING::from(exe.as_os_str()), None)?;
        let bitmap = item.GetImage(SIZE { cx: 256, cy: 256 }, SIIGBF_ICONONLY)?;
        let mut size = BITMAP::default();
        GetObjectW(HGDIOBJ(bitmap.0), size_of::<BITMAP>() as i32, Some((&raw mut size).cast()));
        let (width, height) = (size.bmWidth, size.bmHeight);
        let mut info = BITMAPINFO {
            // A negative height asks for the rows from the top down.
            bmiHeader: BITMAPINFOHEADER { biSize: size_of::<BITMAPINFOHEADER>() as u32, biWidth: width, biHeight: -height, biPlanes: 1, biBitCount: 32, biCompression: BI_RGB.0, ..Default::default() },
            ..Default::default()
        };
        let mut pixels = vec![0u8; (width * height * 4) as usize];
        let dc = CreateCompatibleDC(None);
        let rows = GetDIBits(dc, bitmap, 0, height as u32, Some(pixels.as_mut_ptr().cast()), &mut info, DIB_RGB_COLORS);
        let _ = DeleteDC(dc);
        let _ = DeleteObject(HGDIOBJ(bitmap.0));
        if rows == 0 {
            return Err(format!("couldn't read the icon of {}", exe.display()).into());
        }
        // Windows gives blue, green, red, alpha.
        for pixel in pixels.as_chunks_mut::<4>().0 {
            pixel.swap(0, 2);
        }
        let image = image::RgbaImage::from_raw(width as u32, height as u32, pixels).ok_or("the icon has an odd size")?;
        let mut png = std::io::Cursor::new(Vec::new());
        image.write_to(&mut png, image::ImageFormat::Png)?;
        Ok(png.into_inner())
    }
}

/// Which part of an image the default crop keeps when it has to cut it.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Anchor {
    Center,
    /// Keep the top edge: for art with the title near the top.
    Top,
}

/// The part of the source image a slot shows, as fractions of the source's width and height.
#[derive(Clone, Copy, PartialEq, Debug, Serialize, Deserialize)]
pub struct Crop {
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
}

impl Crop {
    /// The default crop: the largest area with the slot's shape, placed by `anchor`. The UI
    /// computes the same default (`defaultCrop` in ui/app.js).
    pub fn default_for(source: (u32, u32), shape: (u32, u32), anchor: Anchor) -> Self {
        let source_ratio = f64::from(source.0) / f64::from(source.1);
        let ratio = f64::from(shape.0) / f64::from(shape.1);
        if source_ratio > ratio {
            let w = ratio / source_ratio;
            Crop { x: (1.0 - w) / 2.0, y: 0.0, w, h: 1.0 }
        } else {
            let h = source_ratio / ratio;
            let y = if anchor == Anchor::Top { 0.0 } else { (1.0 - h) / 2.0 };
            Crop { x: 0.0, y, w: 1.0, h }
        }
    }

    /// The crop in source pixels, clamped to the image: `(x, y, width, height)`.
    pub fn pixels(self, source: (u32, u32)) -> (u32, u32, u32, u32) {
        let (sw, sh) = (f64::from(source.0), f64::from(source.1));
        let w = (self.w.clamp(0.0, 1.0) * sw).round().max(1.0).min(sw);
        let h = (self.h.clamp(0.0, 1.0) * sh).round().max(1.0).min(sh);
        let x = (self.x * sw).round().clamp(0.0, sw - w);
        let y = (self.y * sh).round().clamp(0.0, sh - h);
        (x as u32, y as u32, w as u32, h as u32)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn centered_crop_of_portrait_for_header_is_a_middle_band() {
        let crop = Crop::default_for((600, 800), (3840, 1240), Anchor::Center);
        assert_eq!(crop.w, 1.0);
        let (x, y, w, h) = crop.pixels((600, 800));
        assert_eq!((x, w), (0, 600));
        assert_eq!(h, 194); // 600 * 1240 / 3840
        assert_eq!(y, 303);
    }

    #[test]
    fn centered_crop_of_matching_shape_is_whole_image() {
        let crop = Crop::default_for((1440, 2160), (600, 900), Anchor::Center);
        assert_eq!(crop.pixels((1440, 2160)), (0, 0, 1440, 2160));
    }

    #[test]
    fn top_anchor_keeps_the_top_edge() {
        let crop = Crop::default_for((1920, 1080), (920, 430), Anchor::Top);
        assert_eq!(crop.pixels((1920, 1080)), (0, 0, 1920, 897));
    }
}

//! Logos: the game's title on a transparent background, which Steam shows over the header and above
//! the menu in Big Picture's game overlay. Cuts a logo out of a store's titled art, and trims the
//! empty margins of a store's logo.

use std::collections::VecDeque;
use std::io::Cursor;

use image::{DynamicImage, GenericImageView, ImageFormat, RgbImage, RgbaImage, imageops::FilterType};

/// Changes when the cut-out or the trim changes, so steambro makes the logos again (`sync::refresh`).
pub const VERSION: u32 = 2;

/// The title of `titled` cut out as a transparent PNG. `titled` is a picture with the game's title
/// painted on it and `plain` the same picture without it, as the Microsoft Store has them for most
/// games (TitledHeroArt and SuperHeroArt). The logo is where the two differ, in the top part of the
/// picture. `None` when the two aren't the same picture or no clear title shows, so that a doubtful
/// cut never becomes a logo.
pub fn cut_out(titled: &[u8], plain: &[u8]) -> crate::Result<Option<Vec<u8>>> {
    let titled = image::load_from_memory(titled)?.to_rgb8();
    let mut plain = image::load_from_memory(plain)?;
    if plain.dimensions() != titled.dimensions() {
        plain = plain.resize_exact(titled.width(), titled.height(), FilterType::Triangle);
    }
    match cut(&titled, &plain.to_rgb8()) {
        Some(logo) => Ok(Some(png(&DynamicImage::ImageRgba8(logo))?)),
        None => Ok(None),
    }
}

/// The logo without its empty transparent margins, with a thin edge left, so that it fills the box
/// Steam shows it in. A logo without transparency, or with little margin, comes back unchanged.
pub fn trim(bytes: Vec<u8>) -> Vec<u8> {
    let Ok(image) = image::load_from_memory(&bytes) else { return bytes };
    if !image.color().has_alpha() {
        return bytes;
    }
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let mut bounds: Option<(u32, u32, u32, u32)> = None;
    for (x, y, pixel) in rgba.enumerate_pixels() {
        if pixel[3] > 8 {
            let (x0, y0, x1, y1) = bounds.unwrap_or((x, y, x, y));
            bounds = Some((x0.min(x), y0.min(y), x1.max(x), y1.max(y)));
        }
    }
    let Some((x0, y0, x1, y1)) = bounds else { return bytes };
    let pad = ((x1 - x0).max(y1 - y0) / 100).max(2);
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(width - 1), (y1 + pad).min(height - 1));
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    if u64::from(w) * u64::from(h) * 100 >= u64::from(width) * u64::from(height) * 95 {
        return bytes;
    }
    png(&image.crop_imm(x0, y0, w, h)).unwrap_or(bytes)
}

fn png(image: &DynamicImage) -> crate::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    image.write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)?;
    Ok(bytes)
}

/// The share of pixels that must be about the same in both pictures: less means they are different
/// pictures, like a titled art with another scene.
const MIN_SAME: f64 = 0.6;
/// Titles sit in the top part of the Store's titled art; changes below it are the picture's own.
const TITLE_AREA: f64 = 0.45;
/// The largest share of pixels below the title area that may change clearly. Clean pairs measured
/// 0 to 9 % (ASTRONEER, High On Life 2, Solarpunk, Minecraft); Civilization VI, whose characters
/// moved between its two pictures, 21 %.
const MAX_CHANGE_BELOW: f64 = 0.12;

fn cut(titled: &RgbImage, plain: &RgbImage) -> Option<RgbaImage> {
    let (width, height) = (titled.width() as usize, titled.height() as usize);
    // How much each pixel changed: the largest change of its three colors, averaged over 3 × 3
    // pixels so that JPEG noise doesn't count.
    let change: Vec<f32> = titled
        .pixels()
        .zip(plain.pixels())
        .map(|(a, b)| (0..3).map(|c| a[c].abs_diff(b[c])).max().unwrap_or(0) as f32)
        .collect();
    let change = box_blur(&change, width, height);
    let same = change.iter().filter(|c| **c < 24.0).count() as f64 / change.len().max(1) as f64;
    if same < MIN_SAME {
        return None;
    }
    let mut mask: Vec<bool> = change.iter().map(|c| *c > 40.0).collect();
    // Below the title the pictures should match. Where they don't, the scene itself changed, as
    // when characters moved, and the changes near the title would be cut out with it.
    let title_rows = (height as f64 * TITLE_AREA) as usize;
    let below = &mask[title_rows * width..];
    if below.iter().filter(|m| **m).count() as f64 > below.len() as f64 * MAX_CHANGE_BELOW {
        return None;
    }
    close(&mut mask, width, height, 3);
    fill_holes(&mut mask, &change, width, height);
    // The row just below the title area shows whether a title goes on past it.
    let past = mask[title_rows * width..(title_rows + 1).min(height) * width].to_vec();
    mask[title_rows * width..].fill(false);

    // The title is the biggest changed area and the pieces near it, like the letters of a subtitle,
    // each kept whole.
    let (labels, sizes) = label(&mask, width, height);
    let biggest = (1..sizes.len()).max_by_key(|&i| sizes[i])?;
    let mut rows = vec![(usize::MAX, 0usize); sizes.len()];
    for (i, &l) in labels.iter().enumerate().filter(|(_, l)| **l > 0) {
        let y = i / width;
        rows[l] = (rows[l].0.min(y), rows[l].1.max(y));
    }
    let (top, bottom) = rows[biggest];
    let reach = 80 * height / 1080;
    let keep_label: Vec<bool> =
        (0..sizes.len()).map(|l| sizes[l] > 0 && sizes[l] * 50 >= sizes[biggest] && rows[l].0 <= bottom + reach && rows[l].1 + reach >= top).collect();
    let keep: Vec<bool> = labels.iter().map(|&l| keep_label[l]).collect();
    // A title that goes on below the title area would be cut in half.
    if title_rows > 0 && past.iter().enumerate().any(|(x, &p)| p && keep[(title_rows - 1) * width + x]) {
        return None;
    }

    let (mut x0, mut y0, mut x1, mut y1) = (width, height, 0, 0);
    let mut kept = 0usize;
    for (i, _) in keep.iter().enumerate().filter(|(_, k)| **k) {
        let (x, y) = (i % width, i / width);
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
        kept += 1;
    }
    if kept == 0 {
        return None;
    }
    let (w, h) = (x1 - x0 + 1, y1 - y0 + 1);
    // A title is a band of some size, not a speck and not the whole width, and the cut is mostly
    // letters where it covers anything.
    if w * 25 < width || h * 50 < height || w * 10 > width * 9 || kept * 100 < w * h * 8 {
        return None;
    }

    // Soft edges: the kept area blurred a little becomes the transparency.
    let alpha = gaussian_blur(&keep.iter().map(|&k| if k { 1.0 } else { 0.0 }).collect::<Vec<f32>>(), width, height, 0.8);
    let pad = 2;
    let (x0, y0) = (x0.saturating_sub(pad), y0.saturating_sub(pad));
    let (x1, y1) = ((x1 + pad).min(width - 1), (y1 + pad).min(height - 1));
    let mut logo = RgbaImage::new((x1 - x0 + 1) as u32, (y1 - y0 + 1) as u32);
    for (x, y, pixel) in logo.enumerate_pixels_mut() {
        let (sx, sy) = (x0 + x as usize, y0 + y as usize);
        let color = titled.get_pixel(sx as u32, sy as u32);
        let a = (alpha[sy * width + sx] * 255.0).round().clamp(0.0, 255.0) as u8;
        *pixel = image::Rgba([color[0], color[1], color[2], a]);
    }
    Some(logo)
}

/// The average of each pixel's 3 × 3 neighborhood; the edge pixels repeat beyond the edges.
fn box_blur(values: &[f32], width: usize, height: usize) -> Vec<f32> {
    let at = |x: isize, y: isize| values[y.clamp(0, height as isize - 1) as usize * width + x.clamp(0, width as isize - 1) as usize];
    (0..width * height)
        .map(|i| {
            let (x, y) = ((i % width) as isize, (i / width) as isize);
            let mut sum = 0.0;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    sum += at(x + dx, y + dy);
                }
            }
            sum / 9.0
        })
        .collect()
}

/// A Gaussian blur, done along rows and then columns; the picture is mirrored beyond its edges.
fn gaussian_blur(values: &[f32], width: usize, height: usize, sigma: f32) -> Vec<f32> {
    let radius = (4.0 * sigma + 0.5) as isize;
    let weights: Vec<f32> = (-radius..=radius).map(|d| (-((d * d) as f32) / (2.0 * sigma * sigma)).exp()).collect();
    let total: f32 = weights.iter().sum();
    let mirror = |i: isize, n: usize| {
        let n = n as isize;
        let i = if i < 0 { -i - 1 } else if i >= n { 2 * n - i - 1 } else { i };
        i.clamp(0, n - 1) as usize
    };
    let mut rows = vec![0.0; values.len()];
    for y in 0..height {
        for x in 0..width {
            let sum: f32 = weights.iter().zip(-radius..=radius).map(|(w, d)| w * values[y * width + mirror(x as isize + d, width)]).sum();
            rows[y * width + x] = sum / total;
        }
    }
    let mut out = vec![0.0; values.len()];
    for y in 0..height {
        for x in 0..width {
            let sum: f32 = weights.iter().zip(-radius..=radius).map(|(w, d)| w * rows[mirror(y as isize + d, height) * width + x]).sum();
            out[y * width + x] = sum / total;
        }
    }
    out
}

/// Closes gaps of up to `steps` pixels: grows the mask by `steps` pixels and shrinks it back. Each
/// step reaches the four neighbors; nothing lies beyond the edges.
fn close(mask: &mut Vec<bool>, width: usize, height: usize, steps: usize) {
    for _ in 0..steps {
        *mask = neighbors(mask, width, height, true);
    }
    for _ in 0..steps {
        *mask = neighbors(mask, width, height, false);
    }
}

/// One step of growing (`grow`) or shrinking the mask over the four neighbors.
fn neighbors(mask: &[bool], width: usize, height: usize, grow: bool) -> Vec<bool> {
    let at = |x: isize, y: isize| x >= 0 && y >= 0 && (x as usize) < width && (y as usize) < height && mask[y as usize * width + x as usize];
    (0..width * height)
        .map(|i| {
            let (x, y) = ((i % width) as isize, (i / width) as isize);
            let around = [at(x, y), at(x - 1, y), at(x + 1, y), at(x, y - 1), at(x, y + 1)];
            if grow { around.iter().any(|v| *v) } else { around.iter().all(|v| *v) }
        })
        .collect()
}

/// Fills the holes of the mask that belong to the title: the parts outside it that the edge of the
/// picture can't reach, where the picture changed on average, or that are a few pixels small. The
/// inside of a letter like O shows the scene unchanged, so it stays out and becomes transparent.
fn fill_holes(mask: &mut [bool], change: &[f32], width: usize, height: usize) {
    let mut outside = vec![false; mask.len()];
    let mut queue = VecDeque::new();
    for i in 0..mask.len() {
        let (x, y) = (i % width, i / width);
        if (x == 0 || y == 0 || x == width - 1 || y == height - 1) && !mask[i] {
            outside[i] = true;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        for next in four(i, width, height) {
            if !mask[next] && !outside[next] {
                outside[next] = true;
                queue.push_back(next);
            }
        }
    }
    // Each hole on its own: the areas neither in the mask nor reached from the edge.
    let mut seen = outside;
    for start in 0..mask.len() {
        if mask[start] || seen[start] {
            continue;
        }
        let mut hole = vec![start];
        seen[start] = true;
        let mut at = 0;
        while at < hole.len() {
            for next in four(hole[at], width, height) {
                if !mask[next] && !seen[next] {
                    seen[next] = true;
                    hole.push(next);
                }
            }
            at += 1;
        }
        let mean = hole.iter().map(|&i| change[i]).sum::<f32>() / hole.len() as f32;
        if mean >= 20.0 || hole.len() <= 64 {
            for i in hole {
                mask[i] = true;
            }
        }
    }
}

/// Numbers the connected areas of the mask, from 1, over the four neighbors. Returns each pixel's
/// number (0 outside the mask) and each number's size in pixels (`sizes[0]` is 0).
fn label(mask: &[bool], width: usize, height: usize) -> (Vec<usize>, Vec<usize>) {
    let mut labels = vec![0usize; mask.len()];
    let mut sizes = vec![0usize];
    let mut queue = VecDeque::new();
    for start in 0..mask.len() {
        if !mask[start] || labels[start] != 0 {
            continue;
        }
        let number = sizes.len();
        sizes.push(0);
        labels[start] = number;
        queue.push_back(start);
        while let Some(i) = queue.pop_front() {
            sizes[number] += 1;
            for next in four(i, width, height) {
                if mask[next] && labels[next] == 0 {
                    labels[next] = number;
                    queue.push_back(next);
                }
            }
        }
    }
    (labels, sizes)
}

fn four(i: usize, width: usize, height: usize) -> impl Iterator<Item = usize> {
    let (x, y) = (i % width, i / width);
    [(x > 0).then(|| i - 1), (x + 1 < width).then(|| i + 1), (y > 0).then(|| i - width), (y + 1 < height).then(|| i + width)].into_iter().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A picture: a gradient, so that every part differs from its neighbors a little.
    fn scene(width: u32, height: u32) -> RgbImage {
        RgbImage::from_fn(width, height, |x, y| image::Rgb([(x * 255 / width) as u8, (y * 255 / height) as u8, 90]))
    }

    fn encode(image: RgbImage) -> Vec<u8> {
        png(&DynamicImage::ImageRgb8(image)).unwrap()
    }

    #[test]
    fn the_title_painted_on_the_picture_becomes_the_logo() {
        let plain = scene(480, 270);
        let mut titled = plain.clone();
        // Two words of "title" near the top, white.
        for (x0, x1) in [(120, 220), (240, 360)] {
            for x in x0..x1 {
                for y in 40..80 {
                    titled.put_pixel(x, y, image::Rgb([255, 255, 255]));
                }
            }
        }
        let logo = cut_out(&encode(titled), &encode(plain)).unwrap().expect("a logo");
        let logo = image::load_from_memory(&logo).unwrap().to_rgba8();
        let (w, h) = logo.dimensions();
        assert!((240..=250).contains(&w) && (40..=50).contains(&h), "{w} × {h}");
        assert_eq!(logo.get_pixel(w / 4, h / 2)[3], 255, "a letter is solid");
        assert_eq!(logo.get_pixel(0, 0)[3], 0, "the corner is clear");
    }

    #[test]
    fn different_pictures_or_no_title_give_no_logo() {
        let plain = scene(480, 270);
        assert!(cut_out(&encode(plain.clone()), &encode(plain.clone())).unwrap().is_none(), "nothing differs");
        let other = RgbImage::from_fn(480, 270, |x, y| image::Rgb([(y % 256) as u8, (x % 256) as u8, 200]));
        assert!(cut_out(&encode(other), &encode(plain.clone())).unwrap().is_none(), "another picture");
        let mut low = plain.clone();
        for x in 100..380 {
            for y in 200..240 {
                low.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
        assert!(cut_out(&encode(low), &encode(plain)).unwrap().is_none(), "a change at the bottom isn't a title");
    }

    #[test]
    fn the_inside_of_a_letter_stays_clear() {
        let plain = scene(480, 270);
        let mut titled = plain.clone();
        // An O: a white ring, with the scene unchanged inside it.
        for x in 200..280u32 {
            for y in 20..100u32 {
                let d = ((x as f64 - 240.0).powi(2) + (y as f64 - 60.0).powi(2)).sqrt();
                if (15.0..=30.0).contains(&d) {
                    titled.put_pixel(x, y, image::Rgb([255, 255, 255]));
                }
            }
        }
        let logo = cut_out(&encode(titled), &encode(plain)).unwrap().expect("a logo");
        let logo = image::load_from_memory(&logo).unwrap().to_rgba8();
        let (w, h) = logo.dimensions();
        assert_eq!(logo.get_pixel(w / 2, h / 2)[3], 0, "the inside of the O is clear");
        assert_eq!(logo.get_pixel(w / 2, 4)[3], 255, "the ring is solid");
    }

    #[test]
    fn a_title_that_goes_past_the_title_area_gives_no_logo() {
        let plain = scene(480, 270);
        let mut titled = plain.clone();
        // From 37 % to 52 % of the height: the title area ends at 45 %.
        for x in 140..340 {
            for y in 100..140 {
                titled.put_pixel(x, y, image::Rgb([255, 255, 255]));
            }
        }
        assert!(cut_out(&encode(titled), &encode(plain)).unwrap().is_none());
    }

    #[test]
    fn trim_takes_off_the_empty_margins() {
        let mut logo = RgbaImage::new(400, 200);
        for x in 100..300 {
            for y in 80..120 {
                logo.put_pixel(x, y, image::Rgba([255, 0, 0, 255]));
            }
        }
        let trimmed = trim(png(&DynamicImage::ImageRgba8(logo)).unwrap());
        assert_eq!(image::load_from_memory(&trimmed).unwrap().dimensions(), (204, 44));
        let full = png(&DynamicImage::ImageRgba8(RgbaImage::from_pixel(50, 20, image::Rgba([0, 0, 0, 255])))).unwrap();
        assert_eq!(trim(full.clone()), full, "nothing to trim");
    }
}

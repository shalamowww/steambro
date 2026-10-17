//! Artwork for Xbox games from the Microsoft Store's public display catalog. The catalog finds a
//! game by its package family name, so games without a Store ID in their config work too.

use serde_json::Value;

use crate::art::{Anchor, Origin, Slot, http};

struct Image {
    purpose: String,
    width: u64,
    height: u64,
    uri: String,
}

/// The Store image for each slot. Empty when the catalog doesn't know the package.
pub fn art(package_family: &str) -> crate::Result<Vec<(Slot, Origin)>> {
    let url = format!(
        "https://displaycatalog.mp.microsoft.com/v7.0/products/lookup?alternateId=PackageFamilyName&value={package_family}&market=US&languages=en-US&fieldsTemplate=details"
    );
    let json: Value = serde_json::from_slice(&http::get(&url)?)?;
    let images: Vec<Image> = json["Products"][0]["LocalizedProperties"][0]["Images"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|i| {
            Some(Image {
                purpose: i["ImagePurpose"].as_str()?.to_string(),
                width: i["Width"].as_u64()?,
                height: i["Height"].as_u64()?,
                uri: i["Uri"].as_str()?.to_string(),
            })
        })
        .collect();
    Ok(pick(&images))
}

/// The Store puts the title near the top of its titled art, so the default crop of the wide cover
/// keeps the top. Its other images are cropped around the center.
pub fn anchor(slot: Slot) -> Anchor {
    if slot == Slot::Wide { Anchor::Top } else { Anchor::Center }
}

fn pick(images: &[Image]) -> Vec<(Slot, Origin)> {
    let first = |purposes: &[&str]| purposes.iter().find_map(|p| images.iter().find(|i| i.purpose == *p));
    let biggest = |purpose: &str| images.iter().filter(|i| i.purpose == purpose).max_by_key(|i| i.width * i.height);
    // The Store has no transparent title logo, so the logo slot stays empty and Steam shows the
    // game's name over the header instead. Its "Logo" is a square tile, which suits the icon.
    let choices = [
        (Slot::Cover, first(&["Poster", "BrandedKeyArt", "BoxArt"]), 600),
        (Slot::Hero, first(&["SuperHeroArt"]).or_else(|| biggest("Screenshot")), 3840),
        (Slot::Wide, first(&["TitledHeroArt", "SuperHeroArt"]), 1920),
        (Slot::Icon, first(&["Logo", "BoxArt"]), 256),
    ];
    choices
        .into_iter()
        .filter_map(|(slot, image, width)| {
            // Store URIs have no scheme, and the image server resizes on request (`w`).
            Some((slot, Origin::Url(format!("https:{}?w={width}", image?.uri))))
        })
        .collect()
}

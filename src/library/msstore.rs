//! Artwork from the Microsoft Store's public display catalog, for Xbox games and for the Battle.net
//! games that the Store sells too. The catalog finds a game by its package family name, so games
//! without a Store ID in their config work too, or by its Store ID.

use serde_json::Value;

use crate::art::{Anchor, Origin, Slot, http};

struct Image {
    purpose: String,
    width: u64,
    height: u64,
    uri: String,
}

/// The Store image for each slot of the package's game. Empty when the catalog doesn't know it.
pub fn by_family(package_family: &str) -> crate::Result<Vec<(Slot, Origin)>> {
    let json = get(&format!(
        "https://displaycatalog.mp.microsoft.com/v7.0/products/lookup?alternateId=PackageFamilyName&value={package_family}&market=US&languages=en-US&fieldsTemplate=details"
    ))?;
    Ok(pick(&images(&json["Products"][0])))
}

/// The Store image for each slot of the product with this Store ID, like `9PBDLPVVCGVJ`. Empty
/// when the catalog doesn't know it.
pub fn by_id(store_id: &str) -> crate::Result<Vec<(Slot, Origin)>> {
    if !store_id.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err(format!("{store_id} isn't a Microsoft Store ID").into());
    }
    let json = get(&format!("https://displaycatalog.mp.microsoft.com/v7.0/products?bigIds={store_id}&market=US&languages=en-US"))?;
    Ok(pick(&images(&json["Products"][0])))
}

/// The Store puts the title near the top of its titled art, so the default crop of the wide cover
/// keeps the top. Its other images are cropped around the center.
pub fn anchor(slot: Slot) -> Anchor {
    if slot == Slot::Wide { Anchor::Top } else { Anchor::Center }
}

fn get(url: &str) -> crate::Result<Value> {
    Ok(serde_json::from_slice(&http::get(url)?)?)
}

fn images(product: &Value) -> Vec<Image> {
    product["LocalizedProperties"][0]["Images"]
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
        .collect()
}

fn pick(images: &[Image]) -> Vec<(Slot, Origin)> {
    let first = |purposes: &[&str]| purposes.iter().find_map(|p| images.iter().find(|i| i.purpose == *p));
    let biggest = |purpose: &str| images.iter().filter(|i| i.purpose == purpose).max_by_key(|i| i.width * i.height);
    // Store URIs have no scheme, and the image server resizes on request (`w`).
    let link = |image: &Image, width: u32| format!("https:{}?w={width}", image.uri);
    let plain = first(&["SuperHeroArt"]);
    let titled = first(&["TitledHeroArt"]);
    let mut origins: Vec<(Slot, Origin)> = [
        (Slot::Cover, first(&["Poster", "BrandedKeyArt", "BoxArt"]), 600),
        (Slot::Hero, plain.or_else(|| biggest("Screenshot")), 3840),
        (Slot::Wide, titled.or(plain), 1920),
        // The Store's "Logo" is a square tile, which suits the icon.
        (Slot::Icon, first(&["Logo", "BoxArt"]), 256),
    ]
    .into_iter()
    .filter_map(|(slot, image, width)| Some((slot, Origin::Url(link(image?, width)))))
    .collect();
    // The Store has no transparent title logo, but its titled hero art is the plain one with the
    // title painted on: the difference is the logo.
    if let (Some(titled), Some(plain)) = (titled, plain) {
        origins.push((Slot::Logo, Origin::CutOut { titled: link(titled, 1920), plain: link(plain, 1920) }));
    }
    origins
}



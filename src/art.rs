// Album art: the playing song's cover (and its color, which tints the window), and the small
// covers on the rows of a list. The player fetches them; the window turns them into images.

use crate::*;

/// A cover as fetched: where from, the file, and the color it's mostly made of (sRGB, 0 to 1).
#[derive(Clone)]
pub struct Cover {
    pub url: String,
    pub bytes: Arc<Vec<u8>>,
    pub tint: [f32; 3],
}

/// The playing song's cover.
#[derive(Default)]
pub struct Art {
    pub url: Option<String>,    // the cover wanted
    pub cover: Option<Cover>,   // the one on show; the last song's until the new one arrives
}

/// Spotify's 64 px copy of a 640 px album cover (they differ only in a size code), for rows; an
/// artist's portrait comes no smaller than 160 px.
pub fn small(url: &str) -> String {
    url.replacen("ab67616d0000b273", "ab67616d00004851", 1).replacen("ab6761610000e5eb", "ab6761610000f178", 1)
}

/// The 300 px copy, for the header over a list (320 px for a portrait).
pub fn medium(url: &str) -> String {
    url.replacen("ab67616d0000b273", "ab67616d00001e02", 1).replacen("ab6761610000e5eb", "ab67616100005174", 1)
}

pub async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    Some(reqwest::get(url).await.ok()?.error_for_status().ok()?.bytes().await.ok()?.to_vec())
}

/// Whether this is a headless render (`SPOT_RUNE="--snap …"`): it runs on a virtual clock that
/// won't wait for the network, so art is fetched before the frame instead.
pub fn headless() -> bool {
    rune::tooling::headless_run().is_some()
}

/// Fetch now, blocking: for headless renders only.
pub fn fetch_blocking(rt: &tokio::runtime::Handle, url: &str) -> Option<Vec<u8>> {
    let (rt, url) = (rt.clone(), url.to_string());
    std::thread::spawn(move || rt.block_on(fetch_bytes(&url))).join().ok().flatten()
}

async fn fetch(url: &str) -> Option<Cover> {
    let bytes = fetch_bytes(url).await?;
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let tint = tint(&image::load_from_memory(&bytes).ok()?);
        Some(Cover { url, bytes: Arc::new(bytes), tint })
    })
    .await
    .ok()?
}

/// The color a cover is mostly made of: the heaviest of 12 hue buckets, weighting each pixel by
/// how colorful it is, so a red sleeve on a black cover gives red, not dark grey. Covers with no
/// real color (black and white photos) give their plain average.
pub fn tint(image: &image::DynamicImage) -> [f32; 3] {
    let small = image.thumbnail(32, 32).to_rgb8();
    let mut buckets = [([0f32; 3], 0f32); 12];
    let mut all = [0f32; 3];
    let n = (small.width() * small.height()).max(1) as f32;
    for p in small.pixels() {
        let [r, g, b] = p.0.map(|c| c as f32 / 255.0);
        all = [all[0] + r, all[1] + g, all[2] + b];
        let (max, min) = (r.max(g).max(b), r.min(g).min(b));
        let chroma = max - min;
        if chroma < 0.08 || max < 0.12 {
            continue;
        }
        let hue = if max == r {
            ((g - b) / chroma).rem_euclid(6.0)
        } else if max == g {
            (b - r) / chroma + 2.0
        } else {
            (r - g) / chroma + 4.0
        };
        let w = chroma * max;
        let bucket = &mut buckets[((hue * 2.0) as usize).min(11)];
        bucket.0 = [bucket.0[0] + r * w, bucket.0[1] + g * w, bucket.0[2] + b * w];
        bucket.1 += w;
    }
    let (sum, w) = buckets.iter().copied().fold(([0.0; 3], 0.0), |a, b| if b.1 > a.1 { b } else { a });
    // under a few percent of the cover's area worth of color: call it colorless
    if w < n * 0.02 {
        return all.map(|c| c / n);
    }
    sum.map(|c| c / w)
}

impl App {
    /// Start fetching the playing song's cover, if that's changed.
    pub fn sync_art(&mut self) {
        let url = self.now().and_then(|t| t.cover.clone());
        if url == self.art.url {
            return;
        }
        self.art.url = url.clone();
        let Some(url) = url else {
            self.art.cover = None;
            return;
        };
        if headless() {
            let rt = self.rt.handle().clone();
            self.art.cover = std::thread::spawn(move || rt.block_on(fetch(&url))).join().ok().flatten();
            return;
        }
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let cover = fetch(&url).await;
            if cover.is_none() {
                log::debug!("no album art from {url}");
            }
            let _ = tx.send(Msg::Art(url, cover));
        });
    }

    /// A cover arrived. It's dropped if the song has moved on.
    pub fn on_art(&mut self, url: String, cover: Option<Cover>) {
        if self.art.url.as_ref() == Some(&url) && cover.is_some() {
            self.art.cover = cover;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_red_sleeve_on_black_is_red() {
        let mut img = image::RgbImage::from_pixel(64, 64, image::Rgb([5, 5, 5]));
        for y in 20..40 {
            for x in 20..40 {
                img.put_pixel(x, y, image::Rgb([200, 30, 40]));
            }
        }
        let [r, g, b] = tint(&image::DynamicImage::ImageRgb8(img));
        assert!(r > 0.6 && g < 0.2 && b < 0.25, "{r} {g} {b}");
    }

    #[test]
    fn grey_covers_give_their_average() {
        let img = image::RgbImage::from_pixel(8, 8, image::Rgb([128, 128, 128]));
        let [r, g, b] = tint(&image::DynamicImage::ImageRgb8(img));
        assert!((r - 0.5).abs() < 0.01 && r == g && g == b);
    }

    #[test]
    fn small_covers_swap_the_size_code() {
        assert_eq!(small("https://i.scdn.co/image/ab67616d0000b273abc"), "https://i.scdn.co/image/ab67616d00004851abc");
        assert_eq!(small("https://i.scdn.co/image/ab6765630000ba8aabc"), "https://i.scdn.co/image/ab6765630000ba8aabc");
        assert_eq!(small("https://i.scdn.co/image/ab6761610000e5ebabc"), "https://i.scdn.co/image/ab6761610000f178abc");
    }
}

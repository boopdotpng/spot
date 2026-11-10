// The playing song's album art, in the player strip. Covers are kept on disk once fetched. The player has no terminal of its own, so
// the client asks the terminal what it can draw and passes that along when it attaches; the art
// lives only as long as that terminal does (a new terminal needs its own copy anyway).

use ratatui_image::{FilterType, Resize, picker::{Picker, ProtocolType, cap_parser::QueryStdioOptions}, protocol::Protocol};

use crate::*;

const ROWS: u16 = 3; // the player strip's height

#[derive(Default)]
pub struct Art {
    pub url: Option<String>, // the cover wanted
    pub image: Option<Protocol>, // the one on show; the last song's until the new one arrives
}

/// Client side: what the terminal draws, and its cell size in pixels, as the `attach` arguments.
/// Call it on the alternate screen. Inside tmux, kitty graphics would need passthrough the player
/// can't see from here, so that's half blocks.
pub fn query_terminal() -> [String; 2] {
    // tmux doesn't answer the query, and a query left waiting would go on to eat the first key
    // and take the terminal out of raw mode: don't ask
    if std::env::var_os("TMUX").is_some() {
        return ["halfblocks".into(), "10x20".into()];
    }
    let picker = Picker::from_query_stdio_with_options(QueryStdioOptions { timeout: Duration::from_millis(500), ..Default::default() });
    let Ok(picker) = picker else { return ["halfblocks".into(), "10x20".into()] };
    let name = match picker.protocol_type() {
        _ if picker.tmux_detected() => "halfblocks",
        ProtocolType::Kitty => "kitty",
        ProtocolType::Sixel => "sixel",
        ProtocolType::Iterm2 => "iterm2",
        ProtocolType::Halfblocks => "halfblocks",
    };
    let size = picker.font_size();
    [name.into(), format!("{}x{}", size.width, size.height)]
}

/// Player side: a picker for what the client said.
pub fn picker(protocol: &str, cell: &str) -> Picker {
    let cell = cell.split_once('x').and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
    let (w, h) = cell.filter(|&(w, h): &(u16, u16)| w > 0 && h > 0).unwrap_or((10, 20));
    // deprecated for apps that have a terminal to ask; ours answered in the client
    #[allow(deprecated)]
    let mut p = Picker::from_fontsize(ratatui_image::FontSize::new(w, h));
    p.set_protocol_type(match protocol {
        "kitty" => ProtocolType::Kitty,
        "sixel" => ProtocolType::Sixel,
        "iterm2" => ProtocolType::Iterm2,
        _ => ProtocolType::Halfblocks,
    });
    p
}

/// How wide the art is: square, in cells that usually aren't.
fn columns(p: &Picker) -> u16 {
    let cell = p.font_size();
    (ROWS as f32 * cell.height as f32 / cell.width as f32).round() as u16
}

/// Kitty draws to the pixel, so the art can fall short of its cells: the player strip's top is
/// set off by the rule above it, and this keeps the bottom off the status line. Half blocks and
/// sixels get it as it is, with no transparency to count on.
fn inset(image: image::DynamicImage, picker: &Picker, size: ratatui::layout::Size) -> image::DynamicImage {
    if picker.protocol_type() != ProtocolType::Kitty {
        return image;
    }
    let cell = picker.font_size();
    let (w, h) = ((size.width * cell.width) as u32, (size.height * cell.height) as u32);
    let side = (h - h / 10).min(w);
    let mut canvas = image::RgbaImage::new(w, h);
    image::imageops::overlay(&mut canvas, &image.resize_to_fill(side, side, FilterType::Lanczos3).to_rgba8(), 0, 0);
    canvas.into()
}

/// The 300 px copy, for the header over a list (320 px for a portrait, and a playlist's
/// four-album mosaic from the sidebar's 60 px one).
pub fn medium(url: &str) -> String {
    url.replacen("ab67616d0000b273", "ab67616d00001e02", 1).replacen("ab6761610000e5eb", "ab67616100005174", 1)
        .replacen("mosaic.scdn.co/60/", "mosaic.scdn.co/300/", 1)
}

/// Where an image is kept on disk: its url, flattened to a file name. Spotify's image urls name
/// the picture and its size, so a cached file never goes stale.
fn disk_path(url: &str) -> PathBuf {
    let name: String = url.split_once("://").map_or(url, |(_, rest)| rest).chars().map(|c| if c.is_ascii_alphanumeric() || c == '.' || c == '-' { c } else { '_' }).collect();
    cache_dir().join("art").join(name)
}

/// An image fetched before, from disk.
fn cached(url: &str) -> Option<Vec<u8>> {
    fs::read(disk_path(url)).ok().filter(|b| !b.is_empty())
}

/// From disk if it's been fetched before, else from Spotify, kept on disk for next time.
async fn fetch_bytes(url: &str) -> Option<Vec<u8>> {
    if let Some(bytes) = cached(url) {
        return Some(bytes);
    }
    let bytes = reqwest::get(url).await.ok()?.error_for_status().ok()?.bytes().await.ok()?.to_vec();
    let path = disk_path(url);
    let _ = fs::create_dir_all(path.parent().expect("in the art directory"));
    // through a temp file, so a crash or a second fetch can't leave half an image
    let tmp = path.with_extension(format!("tmp{:08x}", rand::random::<u32>()));
    if fs::write(&tmp, &bytes).is_ok() && fs::rename(&tmp, &path).is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Some(bytes)
}

async fn fetch(url: &str, picker: Picker) -> Option<Protocol> {
    let bytes = fetch_bytes(&medium(url)).await?;
    let size = ratatui::layout::Size::new(columns(&picker), ROWS);
    let encode = move || {
        let image = inset(image::load_from_memory(&bytes).ok()?, &picker, size);
        picker.new_protocol(image, size, Resize::Fit(Some(FilterType::Lanczos3))).ok()
    };
    tokio::task::spawn_blocking(encode).await.ok()?
}

impl App {
    /// Start fetching the playing song's cover, if that's changed and there's a terminal to show it.
    pub fn sync_art(&mut self) {
        let Some(picker) = self.terminal.as_ref().map(terminal::Display::picker) else { return };
        let url = self.now().and_then(|t| t.cover.clone());
        if url == self.art.url {
            return;
        }
        self.art.url = url.clone();
        let Some(url) = url else {
            self.art.image = None;
            return;
        };
        let tx = self.tx.clone();
        self.rt.spawn(async move {
            let image = fetch(&url, picker).await;
            if image.is_none() {
                log::debug!("no album art from {url}");
            }
            let _ = tx.send(Msg::Art(url, image));
        });
    }

    /// A cover arrived. It's dropped if the song has moved on, or the terminal it was made for has.
    pub fn on_art(&mut self, url: String, image: Option<Protocol>) {
        if self.art.url.as_ref() == Some(&url) {
            self.art.image = image;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn medium_mosaics_are_300() {
        assert_eq!(medium("https://mosaic.scdn.co/60/ab67616d00001e02aab67616d00001e02b"), "https://mosaic.scdn.co/300/ab67616d00001e02aab67616d00001e02b");
    }

    #[test]
    fn the_player_gets_what_the_client_saw() {
        let p = picker("kitty", "16x34");
        assert_eq!(p.protocol_type(), ProtocolType::Kitty);
        assert_eq!(columns(&p), 6);
    }

    #[test]
    fn anything_else_is_half_blocks() {
        for (protocol, cell) in [("nonsense", "0x0"), ("halfblocks", "x")] {
            let p = picker(protocol, cell);
            assert_eq!(p.protocol_type(), ProtocolType::Halfblocks);
            assert_eq!(columns(&p), 6);
        }
    }

    #[test]
    fn kitty_art_stops_short_of_its_last_row() {
        let opaque = image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(64, 64, image::Rgba([9, 9, 9, 255])));
        let size = ratatui::layout::Size::new(6, 3);
        let art = inset(opaque.clone(), &picker("kitty", "16x31"), size).to_rgba8();
        assert_eq!(art.dimensions(), (96, 93));
        assert_eq!(art.get_pixel(0, 0)[3], 255);
        assert_eq!(art.get_pixel(0, 83)[3], 255);
        assert_eq!(art.get_pixel(0, 84)[3], 0);
        assert_eq!(inset(opaque, &picker("halfblocks", "16x31"), size).width(), 64);
    }

    #[test]
    fn kitty_art_is_sent_once_and_only_placed_after_that() {
        use ratatui::{buffer::Buffer, layout::Rect, widgets::Widget};
        let image = image::DynamicImage::new_rgb8(64, 64);
        let p = picker("kitty", "16x34");
        let proto = p.new_protocol(image, ratatui::layout::Size::new(6, 3), Resize::Fit(None)).unwrap();
        let area = Rect::new(0, 0, 6, 3);
        let frame = || {
            let mut buf = Buffer::empty(area);
            ratatui_image::Image::new(&proto).render(area, &mut buf);
            buf.content().iter().map(|c| c.symbol().to_string()).collect::<String>()
        };
        assert!(frame().contains("\x1b_G"));
        let second = frame();
        assert!(!second.contains("\x1b_G"));
        assert!(second.contains('\u{10EEEE}'));
    }
}

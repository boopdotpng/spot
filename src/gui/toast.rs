// The popup: what's playing, for a moment at the top of the screen, when a media key skipped (or,
// with popup = "track" in config.toml, whenever a song starts) and spot's window isn't in front.

use rune::{Color, Cx, El, SurfaceFit, SystemMaterial, TextStyle, col, el, icon, row, text};

use super::{Now, icons};
use crate::*;

#[derive(Default)]
pub struct Toast {
    pub now: Option<Now>,
    pub shows: u64,
}

pub fn view(t: &Toast, cx: &mut Cx<Toast>) -> El {
    let th = theme(cx.system_dark());
    let Some(n) = &t.now else { return el().key("toast") };
    let art = el().key("art").size(44.0, 44.0).radius(9.0).clip_rounded(9.0).fill(th.text.alpha(0.1));
    let art = match &n.cover {
        Some(i) => art.pixels_fit(i.pixels(), SurfaceFit::Fill),
        None => art.center().child(icon(&icons::NOTE, 18.0).color(th.dim)),
    };
    let pill = row().key("pill").h(64.0).max_w(440.0).pl(10.0).pr(22.0).gap(12.0).center_y().radius(32.0).continuous_corners()
        .system_material(SystemMaterial::Glass)
        .fill(th.bg.alpha(if th.dark { 0.6 } else { 0.55 }))
        .border(1.0, th.text.alpha(0.1))
        .child(art)
        .child(col().key(format!("words-{}", n.uri)).shrink().min_w(0.0).gap(2.0)
            .transition(rune::Transition::blur_replace())
            .child(text(n.name.clone()).font(TextStyle::new(14.0, 650.0)).color(th.text).truncate())
            .child(text(n.artists.clone()).font(TextStyle::new(12.0, 450.0)).color(th.sub).truncate()))
        .child(icon(if n.playing { &icons::PLAY } else { &icons::PAUSE }, 13.0).color(th.accent));
    let _ = Color::TRANSPARENT;
    row().key("toast").w_full().h_full().justify_center().items_start().pt(4.0).child(pill)
}

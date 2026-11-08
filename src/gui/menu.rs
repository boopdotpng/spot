// spot's own menus: what a right click (or a row's "more") opens, drawn in the window instead of
// as the system's menu, so it's in the window's colors and moves as the rest of it does. One is
// open at a time; a pick runs its action, and a click outside or esc closes it. The arrow keys
// and return work in it, as in the system's.

use std::rc::Rc;

use rune::{Align, Color, Cursor, El, PointerPress, Side, Spring, SpringPair, Transition, Tx, col, el, icon, row, text};

use super::Ui;
use super::view::BODY;
use crate::Theme;

/// Where a menu opens: under an element (by its key, which has to be the only one so keyed), or
/// at a point in the window (a right click's).
pub enum At {
    Under(String),
    Point(f32, f32),
}

impl From<PointerPress> for At {
    fn from(p: PointerPress) -> At {
        At::Point(p.x, p.y)
    }
}

struct Item {
    id: &'static str,
    label: String,
    icon: Option<&'static rune::Icon>,
    enabled: bool,
}

type Pick = Rc<dyn Fn(&mut Ui, &'static str, &mut Tx)>;

/// A menu: its items (None for a separator), what a pick does, and where it opens.
pub struct Menu {
    items: Vec<Option<Item>>,
    at: Option<At>,
    pick: Option<Pick>,
    id: u64, // each one opened is new, so it enters (and the last one leaves) on its own
}

impl Menu {
    pub fn new() -> Menu {
        Menu { items: vec![], at: None, pick: None, id: 0 }
    }
    pub fn item(mut self, id: &'static str, label: impl Into<String>, icon: Option<&'static rune::Icon>) -> Menu {
        self.items.push(Some(Item { id, label: label.into(), icon, enabled: true }));
        self
    }
    /// The last item, greyed out and not to be picked unless `on`.
    pub fn enabled(mut self, on: bool) -> Menu {
        if let Some(Some(i)) = self.items.last_mut() {
            i.enabled = on;
        }
        self
    }
    pub fn separator(mut self) -> Menu {
        self.items.push(None);
        self
    }
}

/// Open `m` at `at`; picking item `id` runs `pick(ui, id, tx)`.
pub fn open(ui: &mut Ui, mut m: Menu, at: impl Into<At>, pick: impl Fn(&mut Ui, &'static str, &mut Tx) + 'static) {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    m.at = Some(at.into());
    m.pick = Some(Rc::new(pick));
    m.id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    ui.menu = Some(m);
}

const W: f32 = 228.0;

/// The open menu, if there is one, added on top of the window's `root` (last, so it draws over
/// everything and can anchor to anything).
pub fn view(ui: &Ui, th: &Theme, mut root: El) -> El {
    let Some(m) = &ui.menu else { return root };
    let (anchor, side, align) = match &m.at {
        Some(At::Under(key)) => (key.clone(), Side::Bottom, Align::End),
        &Some(At::Point(x, y)) => {
            // a point to hang it from: just under and right of the pointer, as macOS puts one
            root = root.child(el().key("menu-at").absolute(x, y).size(1.0, 1.0));
            ("menu-at".to_string(), Side::Bottom, Align::Start)
        }
        None => return root,
    };
    let id = m.id;
    let mut list = col().key("menu-items").w_full().gap(1.0).role("menu").focus_group();
    for (i, it) in m.items.iter().enumerate() {
        let Some(it) = it else {
            list = list.child(el().key(format!("sep-{i}")).h(1.0).mx(10.0).my(4.0).fill(th.line));
            continue;
        };
        let c = if it.enabled { th.text } else { th.dim.alpha(0.7) };
        let mut r = row().key(format!("mi-{}", it.id)).w_full().h(30.0).px(10.0).gap(10.0).center_y().radius(7.0)
            .role("menuitem").label(it.label.clone())
            .child(match it.icon {
                Some(ic) => icon(ic, 15.0).key("ic").color(if it.enabled { th.sub } else { c }),
                None => el().key("ic").size(15.0, 15.0),
            })
            .child(text(it.label.clone()).font(BODY).color(c).truncate().shrink());
        if it.enabled {
            let pick = it.id;
            // the highlight under the pointer (or the arrow keys) takes the playing cover's color
            r = r.focusable(true).focus_highlight(th.accent.alpha(if th.dark { 0.24 } else { 0.16 })).cursor(Cursor::Pointer)
                .on_press_tx(move |ui: &mut Ui, tx| {
                    let Some(m) = ui.menu.take() else { return };
                    if m.id == id && let Some(f) = m.pick {
                        f(ui, pick, tx);
                    }
                    ui.after(tx);
                });
        } else {
            r = r.focusable(false);
        }
        list = list.child(r);
    }
    // quicker than the window's own motion: a menu is in the way until it's gone
    let pop = Transition::scale(0.94).combined(Transition::offset(0.0, -4.0))
        .springs(SpringPair::new(Spring::new(0.26, 0.18), Spring::new(0.12, 0.0)));
    let card = col().key(format!("menu-{id}")).anchored(anchor, side, align, 4.0).w(W).padding(5.0)
        .radius(11.0).continuous_corners().fill(th.raised).border(1.0, th.text.alpha(if th.dark { 0.12 } else { 0.1 }))
        .shadow(10.0, 32.0, Color::BLACK.alpha(if th.dark { 0.5 } else { 0.18 }))
        .modal(true, |ui: &mut Ui| ui.menu = None)
        .transition(pop)
        .child(list);
    root.child(card)
}

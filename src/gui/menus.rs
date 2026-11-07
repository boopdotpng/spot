// The menu bar, while the window shows (spot has none as a menu-bar-only app). Rune's standard
// menus give Edit and Window; these add the app's own: Settings and Quit in the spot menu, a
// Playback menu, and Help. Each item does what its key does in the window.

use rune::{MenuItem, Shortcut};

use super::Ui;

const SETTINGS: &str = "Settings…";
const QUIT: &str = "Quit spot";

fn key(s: &str) -> Shortcut {
    s.parse().expect("a valid shortcut")
}

/// A Playback item: the player does `$f`, then the window follows. No key equivalents here: the
/// window's keys for these are bare letters, which a menu would take from typing.
macro_rules! playback {
    ($title:expr, |$ui:ident| $f:expr) => {
        MenuItem::new($title, |$ui: &mut Ui, tx| {
            $ui.app.status.clear();
            $f;
            $ui.after(tx);
        })
        .menu("Playback")
    };
}

/// Every item, in menu order. An item with a key lets the window's own key handling see it first
/// (`keys_first`), so the key works the same with the menu as without it.
pub fn items() -> Vec<MenuItem<Ui>> {
    vec![
        MenuItem::new(SETTINGS, |ui: &mut Ui, tx| {
            ui.app.help = false;
            ui.prefs.show();
            ui.open_window();
            ui.after(tx);
        })
        .key_equivalent(key("cmd+,"))
        .keys_first(),
        // spot's own quit, which saves where it was; AppKit's Quit would just end the process
        MenuItem::new(QUIT, |ui: &mut Ui, tx| {
            ui.app.quit = true;
            ui.after(tx);
        })
        .key_equivalent(key("cmd+q"))
        .keys_first(),
        playback!("Play/Pause", |ui| ui.app.toggle()),
        playback!("Next", |ui| ui.app.skip_soon(1)),
        playback!("Previous", |ui| ui.app.prev()),
        MenuItem::separator().menu("Playback"),
        playback!("Shuffle", |ui| ui.app.toggle_shuffle()),
        playback!("Repeat", |ui| ui.app.cycle_repeat()),
        MenuItem::separator().menu("Playback"),
        playback!("Volume Up", |ui| ui.app.set_volume(3277)),
        playback!("Volume Down", |ui| ui.app.set_volume(-3277)),
        MenuItem::separator().menu("Playback"),
        playback!("Show Queue", |ui| ui.app.show_queue()),
        MenuItem::new("Quick Search", |ui: &mut Ui, _| ui.toggle_panel())
            .menu("Playback")
            .key_equivalent(key("cmd+o"))
            .keys_first(),
        MenuItem::new("Keyboard Shortcuts", |ui: &mut Ui, tx| {
            ui.prefs.open = false;
            ui.app.help = true;
            ui.open_window();
            ui.after(tx);
        })
        .menu("Help"),
    ]
}

/// Put the app menu in the usual order once AppKit has filled it (when the Dock icon first
/// shows): About, Settings, Services and Hide, then Quit. Rune puts the app's own items at the
/// top, and AppKit adds a Quit of its own, which would end the process without saving the resume
/// point: that one goes. Cheap, and does nothing once tidy; run whenever the window takes focus.
#[cfg(target_os = "macos")]
pub fn tidy() {
    use objc2::sel;
    use objc2_app_kit::{NSApplication, NSMenu, NSMenuItem};
    use objc2_foundation::MainThreadMarker;

    let Some(mtm) = MainThreadMarker::new() else { return };
    let app = NSApplication::sharedApplication(mtm);
    // SAFETY: main thread; every menu and item is retained while used.
    unsafe {
        let Some(root) = app.mainMenu() else { return };
        let Some(menu) = root.itemAtIndex(0).and_then(|i| i.submenu()) else { return };
        let items = |m: &NSMenu| (0..m.numberOfItems()).filter_map(|i| m.itemAtIndex(i)).collect::<Vec<_>>();
        let find = |m: &NSMenu, f: &dyn Fn(&NSMenuItem) -> bool| items(m).iter().position(|i| f(i)).map(|i| i as isize);
        let titled = |t: &'static str| move |i: &NSMenuItem| i.title().to_string() == t;

        // the bar: spot, Edit, Playback, Window, Help (Rune adds menus in whatever order they come)
        let mut at = 1;
        for t in ["Edit", "Playback", "Window", "Help"] {
            if let Some(i) = find(&root, &titled(t)) {
                if i != at {
                    let top = root.itemAtIndex(i).expect("found it");
                    root.removeItem(&top);
                    root.insertItem_atIndex(&top, at);
                }
                at += 1;
            }
        }
        if app.helpMenu().is_none()
            && let Some(help) = find(&root, &titled("Help")).and_then(|i| root.itemAtIndex(i)).and_then(|i| i.submenu())
        {
            app.setHelpMenu(Some(&help));
        }

        for i in items(&menu) {
            if i.action() == Some(sel!(terminate:)) {
                menu.removeItem(&i);
            }
        }
        // before AppKit's items are in, there's nothing to order (and the bar isn't shown)
        let Some(about) = find(&menu, &|i| i.action() == Some(sel!(orderFrontStandardAboutPanel:))) else { return };
        if let Some(at) = find(&menu, &titled(SETTINGS))
            && at != about + 2
        {
            let settings = menu.itemAtIndex(at).expect("found it");
            menu.removeItem(&settings);
            let about = find(&menu, &|i| i.action() == Some(sel!(orderFrontStandardAboutPanel:))).expect("still there");
            // About, a separator, Settings, a separator
            let mut at = about + 1;
            if !menu.itemAtIndex(at).is_some_and(|i| i.isSeparatorItem()) {
                menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), at);
            }
            at += 1;
            menu.insertItem_atIndex(&settings, at);
            if !menu.itemAtIndex(at + 1).is_some_and(|i| i.isSeparatorItem()) {
                menu.insertItem_atIndex(&NSMenuItem::separatorItem(mtm), at + 1);
            }
        }
        let n = menu.numberOfItems();
        if let Some(at) = find(&menu, &titled(QUIT))
            && at != n - 1
        {
            let quit = menu.itemAtIndex(at).expect("found it");
            menu.removeItem(&quit);
            if !menu.itemAtIndex(menu.numberOfItems() - 1).is_some_and(|i| i.isSeparatorItem()) {
                menu.addItem(&NSMenuItem::separatorItem(mtm));
            }
            menu.addItem(&quit);
        }
    }
}

#[cfg(not(target_os = "macos"))]
pub fn tidy() {}

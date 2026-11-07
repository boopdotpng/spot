// The quick search panel's place among windows and Spaces, as Spotlight does it (and spot's old
// Swift launcher did): it belongs to the Space it opened on, so a desktop swipe takes it away with
// that desktop and it closes, rather than riding along to the new one. It opens where it was last
// dragged to. Rune doesn't expose the panel's window, so this finds it the moment it takes the
// keyboard (it's the key window then), and moves it itself when the card is dragged.

use std::fs;

use rune::AppHandle;

use super::panel::Panel;
use crate::cache_dir;

/// The panel's collection behavior: moves to the active Space when shown, on that one only, over
/// full-screen apps, and not in Mission Control. (Rune's launcher default joins every Space,
/// which keeps it on screen through a swipe.)
pub const BEHAVIOR: usize = (1 << 1) | (1 << 3) | (1 << 8); // MoveToActiveSpace | Transient | FullScreenAuxiliary

fn saved_path() -> std::path::PathBuf {
    cache_dir().join("panel-position")
}

/// Where it opens: the window's top-left, in top-down global points, if it was dragged somewhere
/// that's still on a screen.
pub fn home() -> Option<(f32, f32)> {
    let s = fs::read_to_string(saved_path()).ok()?;
    let mut it = s.split_whitespace().map(|v| v.parse::<f32>());
    let (x, y) = (it.next()?.ok()?, it.next()?.ok()?);
    imp::on_screen(x, y).then_some((x, y))
}

fn save(x: f32, y: f32) {
    let _ = fs::write(saved_path(), format!("{x} {y}\n"));
}

#[cfg(target_os = "macos")]
mod imp {
    use std::{cell::{Cell, RefCell}, ptr::NonNull};

    use block2::RcBlock;
    use objc2::rc::Retained;
    use objc2_app_kit::{NSApplication, NSEvent, NSScreen, NSWindow, NSWorkspace, NSWorkspaceActiveSpaceDidChangeNotification};
    use objc2_foundation::{MainThreadMarker, NSNotification, NSPoint};
    use rune::{AppHandle, Phase};

    use super::Panel;

    thread_local! {
        static WINDOW: RefCell<Option<Retained<NSWindow>>> = const { RefCell::new(None) };
        /// Where the user last dragged it to, while open: saved when it closes.
        static DRAGGED: Cell<Option<(f32, f32)>> = const { Cell::new(None) };
        /// A drag in progress: where the pointer and the window's origin were at the press.
        static GRAB: Cell<Option<(NSPoint, NSPoint)>> = const { Cell::new(None) };
    }

    /// The window's top-left in top-down global points (AppKit's go bottom-up from the primary screen).
    fn top_left(mtm: MainThreadMarker, w: &NSWindow) -> Option<(f32, f32)> {
        let primary = NSScreen::screens(mtm).iter().next()?.frame().size.height;
        let f = w.frame();
        Some((f.origin.x as f32, (primary - f.origin.y - f.size.height) as f32))
    }

    pub fn on_screen(x: f32, y: f32) -> bool {
        let Some(mtm) = MainThreadMarker::new() else { return false };
        let screens = NSScreen::screens(mtm);
        let Some(primary) = screens.iter().next().map(|s| s.frame().size.height) else { return false };
        // a point well inside the bar, not the transparent margin around it
        let p = NSPoint::new(f64::from(x) + 80.0, primary - f64::from(y) - 50.0);
        screens.iter().any(|s| {
            let f = s.visibleFrame();
            p.x >= f.origin.x && p.x < f.origin.x + f.size.width && p.y >= f.origin.y && p.y < f.origin.y + f.size.height
        })
    }

    /// The panel took the keyboard, so it's the key window.
    pub fn focused() {
        let Some(mtm) = MainThreadMarker::new() else { return };
        if let Some(w) = NSApplication::sharedApplication(mtm).keyWindow() {
            WINDOW.with(|slot| *slot.borrow_mut() = Some(w));
        }
    }

    /// The card is being dragged: the window follows the pointer. (Rune's `window_drag` would
    /// move winit's own window, which the panel has taken the view from.) In screen points, as
    /// the pointer's place in the window changes while the window moves.
    pub fn drag(phase: Phase) {
        let Some(mtm) = MainThreadMarker::new() else { return };
        WINDOW.with(|slot| {
            let slot = slot.borrow();
            let Some(w) = slot.as_ref() else { return };
            // SAFETY: a class property read, on the main thread
            let at = unsafe { NSEvent::mouseLocation() };
            match phase {
                Phase::Down => GRAB.set(Some((at, w.frame().origin))),
                Phase::Move => {
                    let Some((from, origin)) = GRAB.get() else { return };
                    // SAFETY: the panel's own window, on the main thread
                    unsafe { w.setFrameOrigin(NSPoint::new(origin.x + at.x - from.x, origin.y + at.y - from.y)) };
                    DRAGGED.set(top_left(mtm, w));
                }
                Phase::Up => GRAB.set(None),
            }
        });
    }

    /// It's closing: if it was dragged, open there next time.
    pub fn closing() {
        if let Some((x, y)) = DRAGGED.take() {
            super::save(x, y);
        }
    }

    /// Close the panel the moment the desktop changes (a swipe, Mission Control, ⌃→).
    pub fn watch_spaces(panel: AppHandle<Panel>) {
        let block = RcBlock::new(move |_: NonNull<NSNotification>| {
            if panel.is_visible() {
                closing();
                panel.hide();
            }
        });
        // SAFETY: main thread (at start); the observer lives as long as the process, so its
        // token is kept for good.
        unsafe {
            let center = NSWorkspace::sharedWorkspace().notificationCenter();
            let token = center.addObserverForName_object_queue_usingBlock(
                Some(NSWorkspaceActiveSpaceDidChangeNotification),
                None,
                None,
                &block,
            );
            std::mem::forget(token);
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod imp {
    use rune::AppHandle;

    use super::Panel;

    pub fn on_screen(_: f32, _: f32) -> bool {
        false
    }
    pub fn focused() {}
    pub fn drag(_: rune::Phase) {}
    pub fn closing() {}
    pub fn watch_spaces(_: AppHandle<Panel>) {}
}

pub use imp::{closing, drag, focused};

pub fn watch_spaces(panel: AppHandle<Panel>) {
    imp::watch_spaces(panel)
}

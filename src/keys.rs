// Keyboard: what each key does. The window turns its key events into `Key`s; typing in the
// search and find fields arrives as their text (set_search, set_find), the keys that aren't
// typing (enter, esc, tab, arrows) as `Key`s.

use crate::*;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Esc,
    Tab,
    BackTab,
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
    Home,
    End,
    Backspace,
}

impl App {
    /// The find field's text changed.
    pub fn set_find(&mut self, text: &str) {
        self.focus = Focus::Tracks;
        self.find = Some(text.to_string());
        self.refilter();
        self.tr_state.select(Some(0));
    }

    /// The find field's keys that aren't typing. True when it took the key.
    pub fn on_find_key(&mut self, k: Key) -> bool {
        match k {
            Key::Esc => self.close_find(),
            Key::Enter => {
                // play the match, but queue the whole list so playback continues through the playlist
                if let Some(i) = self.selected_track() {
                    self.close_find();
                    self.activate(i);
                }
            }
            Key::Down => self.move_sel(1),
            Key::Up => self.move_sel(-1),
            Key::PageDown => self.move_sel(15),
            Key::PageUp => self.move_sel(-15),
            _ => return false,
        }
        true
    }

    /// The search field's text changed: results come as you type.
    pub fn set_search(&mut self, text: &str) {
        if self.input.as_deref() == Some(text) {
            return;
        }
        self.input = Some(text.to_string());
        self.search_soon();
    }

    /// The search field's keys that aren't typing: enter keeps the results, esc goes back to
    /// what was open. True when it took the key.
    pub fn on_search_key(&mut self, k: Key) -> bool {
        match k {
            Key::Enter => {
                self.search_at = None;
                self.run_search();
                self.input = None;
                self.live_search = false;
                self.focus = Focus::Tracks;
            }
            Key::Esc => {
                self.input = None;
                self.search_at = None;
                self.end_live_search();
            }
            Key::Tab => self.cycle_search_kind(false),
            Key::BackTab => self.cycle_search_kind(true),
            // the results' cursor: by line on a page of cards, else by row
            Key::Up if self.live_search => {
                if self.grid_move(0, -1).is_none() {
                    self.move_sel(-1);
                }
            }
            Key::Down if self.live_search => {
                if self.grid_move(0, 1).is_none() {
                    self.move_sel(1);
                }
            }
            _ => return false,
        }
        true
    }

    pub fn cycle_search_kind(&mut self, back: bool) {
        const ORDER: [Option<Kind>; 5] = [None, Some(Kind::Tracks), Some(Kind::Albums), Some(Kind::Artists), Some(Kind::Shows)];
        let at = ORDER.iter().position(|&k| k == self.search_kind).unwrap_or(0);
        self.search_kind = ORDER[(at + if back { ORDER.len() - 1 } else { 1 }) % ORDER.len()];
        self.search_at = Some(Instant::now()); // nothing being typed to wait for
    }

    /// A key while no field has the keyboard. `cmd` is ⌘ (or ⌃) held. True when it did something.
    pub fn on_key(&mut self, k: Key, cmd: bool) -> bool {
        if self.help {
            // any key closes the key help
            self.help = false;
            return true;
        }
        self.status.clear();
        match k {
            _ if cmd => return false,
            Key::Char('d') if self.dated() => self.set_sort(self.sort.next()),
            // cards: left and right go card to card, up and down line to line
            Key::Left | Key::Char('h') if self.focus == Focus::Tracks && self.grid_move(-1, 0) == Some(true) => {}
            Key::Right | Key::Char('l') if self.focus == Focus::Tracks && self.grid_move(1, 0).is_some() => {}
            Key::Down | Key::Char('j') if self.focus == Focus::Tracks && self.grid_move(0, 1).is_some() => {}
            Key::Up | Key::Char('k') if self.focus == Focus::Tracks && self.grid_move(0, -1).is_some() => {}
            Key::Char('P') if self.view.artist.is_some() && self.view.info.is_some() => self.play_artist(),
            Key::Tab | Key::BackTab | Key::Left | Key::Right | Key::Char('h') | Key::Char('l') => {
                self.focus = match (&self.focus, k) {
                    (_, Key::Left | Key::Char('h')) => Focus::Playlists,
                    (_, Key::Right | Key::Char('l')) => Focus::Tracks,
                    (Focus::Playlists, _) => Focus::Tracks,
                    (Focus::Tracks, _) => Focus::Playlists,
                }
            }
            Key::Char(' ') => self.toggle(),
            Key::Char('s') => self.toggle_shuffle(),
            Key::Char('r') => self.cycle_repeat(),
            Key::Char('P') => self.play_from_view(None),
            Key::Char('a') => self.goto_album(),
            Key::Char('A') => self.goto_artist(),
            Key::Char('m') => self.toggle_podcasts(),
            Key::Char('v') => self.toggle_artist_mode(),
            Key::Char('f') => self.cycle_releases(),
            Key::Esc | Key::Backspace => self.back(),
            Key::Char('n') => self.skip_soon(1),
            Key::Char('p') => self.prev(),
            Key::Char('.') => self.seek_by(SeekDirection::Forward, Duration::from_secs(10)),
            Key::Char(',') => self.seek_by(SeekDirection::Backward, Duration::from_secs(10)),
            Key::Char('+') | Key::Char('=') => self.set_volume(3277),
            Key::Char('-') => self.set_volume(-3277),
            Key::Char('o') => self.show_queue(),
            Key::Char('e') => self.enqueue_selected(),
            Key::Char('Q') => self.open_queue(),
            Key::Char('x') => self.dequeue_selected(),
            Key::Enter => self.enter(),
            Key::Down | Key::Char('j') => self.move_sel(1),
            Key::Up | Key::Char('k') => self.move_sel(-1),
            Key::PageDown => self.move_sel(15),
            Key::PageUp => self.move_sel(-15),
            Key::Char('g') | Key::Home => self.move_sel(isize::MIN / 2),
            Key::Char('G') | Key::End => self.move_sel(isize::MAX / 2),
            Key::Char('M') => self.move_to_middle(),
            _ => return false,
        }
        true
    }

    /// Enter (or a double click): open the sidebar's list, or play / open the row.
    pub fn enter(&mut self) {
        match self.focus {
            Focus::Playlists if self.podcasts => {
                if let Some(l) = self.sh_state.selected().and_then(|i| self.shows.get(i)).cloned() {
                    self.open_podcast_row(l);
                    self.focus = Focus::Tracks;
                }
            }
            Focus::Playlists => {
                if let Some(i) = self.pl_state.selected() {
                    self.open_playlist(i);
                    self.focus = Focus::Tracks;
                }
            }
            Focus::Tracks => {
                if let Some(i) = self.selected_track() {
                    self.activate(i);
                }
            }
        }
    }
}

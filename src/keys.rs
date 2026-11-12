// Keyboard: reading keys from the terminal, and what each one does. Typing in the search and
// find prompts edits their text (set_search, set_find); the keys that aren't typing (enter, esc,
// tab, arrows) go to on_search_key / on_find_key, and the rest to on_key.

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

/// A key from the terminal, and whether ctrl was held.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct Press {
    pub key: Key,
    pub ctrl: bool,
}

const DETACHED: &str = "detached; `spot` brings it back";

impl App {
    /// A key from the terminal: the prompts' typing, keys only the TUI has, then on_key.
    pub fn on_press(&mut self, Press { key: k, ctrl }: Press) {
        // wherever you are: like closing the terminal, not like q
        if ctrl && k == Key::Char('c') {
            return self.detach_terminal_saying(DETACHED);
        }
        if let Some(mut text) = self.find.clone() {
            match k {
                Key::Char('n') if ctrl => self.move_sel(1),
                Key::Char('p') if ctrl => self.move_sel(-1),
                Key::Backspace => {
                    text.pop();
                    self.set_find(&text);
                }
                Key::Char(c) if !ctrl => {
                    text.push(c);
                    self.set_find(&text);
                }
                _ => {
                    self.on_find_key(k);
                }
            }
            return;
        }
        if let Some(mut text) = self.input.clone() {
            match k {
                Key::Backspace => {
                    text.pop();
                    self.set_search(&text);
                }
                Key::Char(c) if !ctrl => {
                    text.push(c);
                    self.set_search(&text);
                }
                _ => {
                    self.on_search_key(k);
                }
            }
            return;
        }
        if self.help {
            // any key closes the key help
            self.help = false;
            return;
        }
        self.status.clear();
        match k {
            Key::Char('q') if !ctrl => self.quit = true,
            Key::Char('d') if !ctrl => self.detach_terminal_saying(DETACHED),
            Key::Char('/') if !ctrl => self.input = Some(String::new()),
            Key::Char('?') => self.help = true,
            Key::Char('t') if ctrl => self.flash(format!("theme: {}", cycle_family())),
            Key::Char('f') if ctrl => self.set_find(""),
            Key::Char('d') if ctrl => self.move_sel(15),
            Key::Char('u') if ctrl => self.move_sel(-15),
            Key::Char('[') => self.back(),
            Key::Char(']') => self.forward(),
            Key::Char('*') if self.focus == Focus::Playlists && !self.podcasts => {
                if let Some(i) = self.pl_state.selected() {
                    self.toggle_pin(i);
                }
            }
            _ if ctrl => {}
            _ => {
                self.on_key(k, false);
            }
        }
    }

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

    /// The search field's keys that aren't typing: enter plays the top result, tab goes into the
    /// results, shift+tab changes what's searched for, esc goes back to
    /// what was open. True when it took the key.
    pub fn on_search_key(&mut self, k: Key) -> bool {
        match k {
            Key::Enter => {
                let typed = self.input.as_deref().is_some_and(|q| !q.trim().is_empty());
                self.search_at = None;
                self.run_search();
                self.input = None;
                self.live_search = false;
                self.focus = Focus::Tracks;
                // enter plays what the cursor's on (the top result, unless it was moved), now or
                // once the results are in
                if typed && self.view.search_query().is_some() {
                    self.act_on_load = Some(self.view.id);
                    if !self.view.loading {
                        self.act_on_found();
                    }
                }
            }
            Key::Esc => {
                self.input = None;
                self.search_at = None;
                self.end_live_search();
            }
            // into the results, playing nothing: the cursor's theirs from here (tab again goes
            // on through a page's sections)
            Key::Tab => {
                self.search_at = None;
                self.run_search();
                self.input = None;
                self.live_search = false;
                self.focus = Focus::Tracks;
            }
            Key::BackTab => self.cycle_search_kind(false),
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

    /// Enter on a search: play the highlighted song, or the album from its first (opened too,
    /// and played once its songs are in); an artist is only opened. A search for only albums,
    /// artists or podcasts just lists them.
    pub fn act_on_found(&mut self) {
        self.act_on_load = None;
        if self.view.kind != Kind::Tracks {
            return;
        }
        let Some(i) = self.selected_track() else { return };
        let from = self.view.id;
        self.activate(i);
        if self.view.id != from && self.view.key.starts_with("spotify:album:") {
            if self.view.tracks.is_empty() {
                self.act_on_load = Some(self.view.id);
            } else {
                self.activate(0);
            }
        }
    }

    pub fn cycle_search_kind(&mut self, back: bool) {
        const ORDER: [Option<Kind>; 5] = [None, Some(Kind::Tracks), Some(Kind::Albums), Some(Kind::Artists), Some(Kind::Shows)];
        let at = ORDER.iter().position(|&k| k == self.search_kind).unwrap_or(0);
        self.search_kind = ORDER[(at + if back { ORDER.len() - 1 } else { 1 }) % ORDER.len()];
        self.search_at = Some(Instant::now()); // nothing being typed to wait for
    }

    /// A key while no prompt has the keyboard. `cmd`: a modifier was held. True when it did something.
    pub fn on_key(&mut self, k: Key, cmd: bool) -> bool {
        if self.help {
            // any key closes the key help
            self.help = false;
            return true;
        }
        match k {
            _ if cmd => return false,
            Key::Char('S') if self.dated() => self.set_sort(self.sort.next()),
            Key::Char('S') => self.flash("S sorts playlists by date added"),
            // cards: left and right go card to card, up and down line to line
            Key::Left | Key::Char('h') if self.focus == Focus::Tracks && self.grid_move(-1, 0) == Some(true) => {}
            Key::Right | Key::Char('l') if self.focus == Focus::Tracks && self.grid_move(1, 0).is_some() => {}
            Key::Down | Key::Char('j') if self.focus == Focus::Tracks && self.grid_move(0, 1).is_some() => {}
            Key::Up | Key::Char('k') if self.focus == Focus::Tracks && self.grid_move(0, -1).is_some() => {}
            // a search's page or an artist's: tab goes section to section (left still reaches the sidebar)
            Key::Tab | Key::BackTab if self.focus == Focus::Tracks && self.view.info.is_some() && self.section_jump(k == Key::BackTab) => {}
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

    /// Enter: open the sidebar's list, or play / open the row.
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

/// Parse raw terminal input. Crossterm's reader can't be used: it treats the terminal's color
/// scheme report (`CSI ? 997 ; 1|2 n`) as an unfinished sequence and swallows the keys after it.
pub fn parse_input(b: &[u8]) -> Vec<Msg> {
    let key = |key| Msg::Key(Press { key, ctrl: false });
    let ctrl = |c: u8| Msg::Key(Press { key: Key::Char(c as char), ctrl: true });
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // a lone ESC at the end of a read is the Esc key; otherwise it starts a sequence
            0x1b if i + 1 == b.len() => {
                out.push(key(Key::Esc));
                i += 1;
            }
            0x1b => match b[i + 1] {
                b'[' => {
                    let start = i + 2;
                    let Some(len) = b[start..].iter().position(|c| (0x40..=0x7e).contains(c)) else { break };
                    let (params, fin) = (&b[start..start + len], b[start + len]);
                    i = start + len + 1;
                    let first = params.split(|&c| c == b';').next().unwrap_or(b"");
                    out.extend(match (fin, first) {
                        (b'A', _) => Some(key(Key::Up)),
                        (b'B', _) => Some(key(Key::Down)),
                        (b'C', _) => Some(key(Key::Right)),
                        (b'D', _) => Some(key(Key::Left)),
                        (b'H', _) | (b'~', b"1" | b"7") => Some(key(Key::Home)),
                        (b'F', _) | (b'~', b"4" | b"8") => Some(key(Key::End)),
                        (b'~', b"5") => Some(key(Key::PageUp)),
                        (b'~', b"6") => Some(key(Key::PageDown)),
                        (b'Z', _) => Some(key(Key::BackTab)),
                        (b'n', _) if params == b"?997;1" || params == b"?997;2" => {
                            Some(Msg::Scheme(params == b"?997;2"))
                        }
                        _ => None,
                    });
                }
                b'O' if i + 2 < b.len() => {
                    out.extend(match b[i + 2] {
                        b'A' => Some(key(Key::Up)),
                        b'B' => Some(key(Key::Down)),
                        b'C' => Some(key(Key::Right)),
                        b'D' => Some(key(Key::Left)),
                        b'H' => Some(key(Key::Home)),
                        b'F' => Some(key(Key::End)),
                        _ => None,
                    });
                    i += 3;
                }
                // OSC: the background color we ask for when the scheme changes; skip anything else
                b']' => {
                    i += 2;
                    let start = i;
                    while i < b.len() && b[i] != 0x07 && !(b[i] == 0x1b && b.get(i + 1) == Some(&b'\\')) {
                        i += 1;
                    }
                    out.extend(parse_bg_reply(&b[start..i]).map(Msg::Background));
                    i += if b.get(i) == Some(&0x1b) { 2 } else { 1 };
                }
                // Alt+key: unused, drop both bytes
                _ => i += 2,
            },
            b'\r' | b'\n' => {
                out.push(key(Key::Enter));
                i += 1;
            }
            b'\t' => {
                out.push(key(Key::Tab));
                i += 1;
            }
            0x7f | 0x08 => {
                out.push(key(Key::Backspace));
                i += 1;
            }
            c @ 0x01..=0x1a => {
                out.push(ctrl(b'a' + c - 1));
                i += 1;
            }
            c if c < 0x20 => i += 1,
            c => {
                let len = match c {
                    0xf0.. => 4,
                    0xe0.. => 3,
                    0xc0.. => 2,
                    _ => 1,
                };
                if let Some(ch) = b.get(i..i + len).and_then(|s| std::str::from_utf8(s).ok()).and_then(|s| s.chars().next()) {
                    out.push(key(Key::Char(ch)));
                }
                i += len;
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn keys(b: &[u8]) -> Vec<(Key, bool)> {
        parse_input(b).into_iter().filter_map(|m| match m {
            Msg::Key(p) => Some((p.key, p.ctrl)),
            _ => None,
        }).collect()
    }

    #[test]
    fn reads_keys_arrows_and_ctrl() {
        assert_eq!(keys(b"j\x1b[A\x06\r\t\x7f"), [(Key::Char('j'), false), (Key::Up, false), (Key::Char('f'), true), (Key::Enter, false), (Key::Tab, false), (Key::Backspace, false)]);
        assert_eq!(keys(b"\x1b"), [(Key::Esc, false)]);
        assert_eq!(keys("é".as_bytes()), [(Key::Char('é'), false)]);
    }

    #[test]
    fn color_scheme_reports_dont_eat_keys() {
        let m = parse_input(b"\x1b[?997;2nq");
        assert!(matches!(m[0], Msg::Scheme(true)));
        assert!(matches!(m[1], Msg::Key(Press { key: Key::Char('q'), ctrl: false })));
    }
}

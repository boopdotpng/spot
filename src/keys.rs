// Keyboard: reading keys from the terminal, and what each one does.

use crate::*;

impl App {
    pub fn on_find_key(&mut self, k: KeyEvent) {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Esc => self.close_find(),
            KeyCode::Enter => {
                // play the match, but queue the whole list so playback continues through the playlist
                if let Some(i) = self.selected_track() {
                    self.close_find();
                    self.activate(i);
                }
            }
            KeyCode::Down => self.move_sel(1),
            KeyCode::Up => self.move_sel(-1),
            KeyCode::Char('n') if ctrl => self.move_sel(1),
            KeyCode::Char('p') if ctrl => self.move_sel(-1),
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Backspace => {
                if let Some(f) = &mut self.find {
                    f.pop();
                }
                self.refilter();
                self.tr_state.select(Some(0));
            }
            KeyCode::Char(c) if !ctrl => {
                if let Some(f) = &mut self.find {
                    f.push(c);
                }
                self.refilter();
                self.tr_state.select(Some(0));
            }
            _ => {}
        }
    }

    pub fn on_key(&mut self, k: KeyEvent) {
        if self.help {
            // any key closes the key help
            self.help = false;
            return;
        }
        if self.find.is_some() {
            return self.on_find_key(k);
        }
        if let Some(buf) = &mut self.input {
            match k.code {
                KeyCode::Enter => {
                    let q = buf.trim().to_string();
                    self.input = None;
                    if !q.is_empty() {
                        self.open_search(q);
                    }
                }
                KeyCode::Esc => self.input = None,
                KeyCode::Tab => self.search_kind = self.search_kind.next(),
                KeyCode::Backspace => {
                    buf.pop();
                }
                KeyCode::Char(c) => buf.push(c),
                _ => {}
            }
            return;
        }
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        match k.code {
            KeyCode::Char('q') => self.quit = true,
            KeyCode::Char('c') if ctrl => self.quit = true,
            KeyCode::Char('/') => self.input = Some(String::new()),
            KeyCode::Char('?') => self.help = !self.help,
            KeyCode::Char('t') if ctrl => {
                self.note = Some((format!("theme: {}", cycle_family()), Instant::now() + Duration::from_millis(1500)))
            }
            KeyCode::Char('f') if ctrl => {
                self.focus = Focus::Tracks;
                self.find = Some(String::new());
                self.tr_state.select(Some(0));
            }
            KeyCode::Tab | KeyCode::BackTab | KeyCode::Left | KeyCode::Right | KeyCode::Char('h') | KeyCode::Char('l') => {
                self.focus = match (&self.focus, k.code) {
                    (_, KeyCode::Left | KeyCode::Char('h')) => Focus::Playlists,
                    (_, KeyCode::Right | KeyCode::Char('l')) => Focus::Tracks,
                    (Focus::Playlists, _) => Focus::Tracks,
                    (Focus::Tracks, _) => Focus::Playlists,
                }
            }
            KeyCode::Char(' ') => self.toggle(),
            KeyCode::Char('s') => self.toggle_shuffle(),
            KeyCode::Char('r') => self.cycle_repeat(),
            KeyCode::Char('P') => self.play_from_view(None),
            KeyCode::Char('a') => self.goto_album(),
            KeyCode::Char('A') => self.goto_artist(),
            KeyCode::Char('v') => self.toggle_artist_mode(),
            KeyCode::Char('f') => self.cycle_releases(),
            KeyCode::Esc | KeyCode::Backspace => self.back(),
            KeyCode::Char('n') => self.skip_soon(1),
            KeyCode::Char('p') => self.prev(),
            KeyCode::Char('.') => self.seek_by(SeekDirection::Forward, Duration::from_secs(10)),
            KeyCode::Char(',') => self.seek_by(SeekDirection::Backward, Duration::from_secs(10)),
            KeyCode::Char('+') | KeyCode::Char('=') => self.set_volume(3277),
            KeyCode::Char('-') => self.set_volume(-3277),
            KeyCode::Char('o') => self.show_queue(),
            KeyCode::Char('e') => self.enqueue_selected(),
            KeyCode::Char('Q') => self.open_queue(),
            KeyCode::Char('x') => self.dequeue_selected(),
            KeyCode::Enter => match self.focus {
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
            },
            KeyCode::Down | KeyCode::Char('j') => self.move_sel(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_sel(-1),
            KeyCode::PageDown | KeyCode::Char('d') if k.code == KeyCode::PageDown || ctrl => self.move_sel(15),
            KeyCode::PageUp | KeyCode::Char('u') if k.code == KeyCode::PageUp || ctrl => self.move_sel(-15),
            KeyCode::Char('g') | KeyCode::Home => self.move_sel(isize::MIN / 2),
            KeyCode::Char('G') | KeyCode::End => self.move_sel(isize::MAX / 2),
            KeyCode::Char('M') => self.move_to_middle(),
            _ => {}
        }
    }
}

// ---------- main ----------

// ratatui hides the cursor; we exit via process::exit so Terminal's Drop never re-shows it.
/// Reads keys from stdin. Crossterm's reader can't be used: it treats the terminal's color
/// scheme report (`CSI ? 997 ; 1|2 n`) as an unfinished sequence and swallows the keys after it.
pub fn read_input(tx: mpsc::Sender<Msg>) {
    use std::io::Read;
    let mut stdin = std::io::stdin().lock();
    let mut buf = [0u8; 1024];
    loop {
        let n = match stdin.read(&mut buf) {
            Ok(0) | Err(_) => return,
            Ok(n) => n,
        };
        for m in parse_input(&buf[..n]) {
            if tx.send(m).is_err() {
                return;
            }
        }
    }
}

pub fn parse_input(b: &[u8]) -> Vec<Msg> {
    let key = |code| Msg::Key(KeyEvent::new(code, KeyModifiers::NONE));
    let ctrl = |c: u8| Msg::Key(KeyEvent::new(KeyCode::Char(c as char), KeyModifiers::CONTROL));
    let mut out = vec![];
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            // a lone ESC at the end of a read is the Esc key; otherwise it starts a sequence
            0x1b if i + 1 == b.len() => {
                out.push(key(KeyCode::Esc));
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
                        (b'A', _) => Some(key(KeyCode::Up)),
                        (b'B', _) => Some(key(KeyCode::Down)),
                        (b'C', _) => Some(key(KeyCode::Right)),
                        (b'D', _) => Some(key(KeyCode::Left)),
                        (b'H', _) | (b'~', b"1" | b"7") => Some(key(KeyCode::Home)),
                        (b'F', _) | (b'~', b"4" | b"8") => Some(key(KeyCode::End)),
                        (b'~', b"5") => Some(key(KeyCode::PageUp)),
                        (b'~', b"6") => Some(key(KeyCode::PageDown)),
                        (b'Z', _) => Some(Msg::Key(KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT))),
                        (b'n', _) if params == b"?997;1" || params == b"?997;2" => {
                            Some(Msg::Scheme(params == b"?997;2"))
                        }
                        _ => None,
                    });
                }
                b'O' if i + 2 < b.len() => {
                    out.extend(match b[i + 2] {
                        b'A' => Some(key(KeyCode::Up)),
                        b'B' => Some(key(KeyCode::Down)),
                        b'C' => Some(key(KeyCode::Right)),
                        b'D' => Some(key(KeyCode::Left)),
                        b'H' => Some(key(KeyCode::Home)),
                        b'F' => Some(key(KeyCode::End)),
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
                out.push(key(KeyCode::Enter));
                i += 1;
            }
            b'\t' => {
                out.push(key(KeyCode::Tab));
                i += 1;
            }
            0x7f | 0x08 => {
                out.push(key(KeyCode::Backspace));
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
                    out.push(key(KeyCode::Char(ch)));
                }
                i += len;
            }
        }
    }
    out
}

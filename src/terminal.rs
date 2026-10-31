// A terminal is a disposable client of the background player. Only terminal output crosses
// this connection; closing it never stops audio or discards the player's state.
use std::{
    io::{self, BufRead, BufReader, Read, Write},
    net::Shutdown,
    os::unix::net::UnixStream,
    sync::{Mutex, atomic::{AtomicU64, Ordering::Relaxed}},
};

use ratatui::{Terminal, TerminalOptions, Viewport, backend::CrosstermBackend, layout::Rect};
use crate::*;

type Screen = Terminal<CrosstermBackend<UnixStream>>;

/// The player's last words before it closes a terminal: an OSC (which terminals ignore) carrying
/// what the client prints once the terminal is restored.
const FAREWELL: &[u8] = b"\x1b]spot;";

pub struct Attachment {
    id: u64,
    out: UnixStream,
    area: Rect,
    light: bool,
    bg: u32,
}

pub struct Display {
    id: u64,
    screen: Screen,
    conn: UnixStream,
}

impl Drop for Display {
    fn drop(&mut self) {
        let _ = self.conn.shutdown(Shutdown::Both);
    }
}

pub enum Event {
    Input(Vec<u8>),
    Resize(Rect),
    Closed,
}

fn area(w: u16, h: u16) -> Rect {
    Rect::new(0, 0, w.clamp(1, 1000), h.clamp(1, 1000))
}

/// A cleared screen of `area`. Clears and resizes go through here rather than ratatui's, which ask
/// the real terminal where the cursor is, and the background player has no terminal to ask.
fn screen(mut out: UnixStream, area: Rect) -> io::Result<Screen> {
    out.write_all(b"\x1b[H\x1b[2J")?;
    Terminal::with_options(CrosstermBackend::new(out), TerminalOptions { viewport: Viewport::Fixed(area) })
}

/// Called by the socket thread after the text handshake. Upstream frames are K + length +
/// raw input, or R + width + height (all integers big endian). Downstream is terminal output.
pub fn serve(mut conn: UnixStream, args: &[String], tx: mpsc::Sender<Msg>) {
    let parsed = || -> Option<_> {
        if args.len() != 6 || args[1] != "1" { return None; }
        Some((area(args[2].parse().ok()?, args[3].parse().ok()?), args[4] == "1", args[5].parse().ok()?))
    };
    let Some((area, light, bg)) = parsed() else {
        let _ = writeln!(conn, "2\nunsupported attach protocol; update spot and restart the player");
        return;
    };
    let Ok(out) = conn.try_clone() else { return };
    // A terminal that stops reading must never hold up playback indefinitely.
    let _ = out.set_write_timeout(Some(Duration::from_millis(200)));
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let id = NEXT.fetch_add(1, Relaxed);
    if tx.send(Msg::Attach(Attachment { id, out, area, light, bg })).is_err() { return; }
    while let Ok(event) = read_event(&mut conn) {
        if tx.send(Msg::Terminal { id, event }).is_err() { return; }
    }
    let _ = tx.send(Msg::Terminal { id, event: Event::Closed });
}

fn read_event(input: &mut impl Read) -> io::Result<Event> {
    let mut kind = [0];
    input.read_exact(&mut kind)?;
    match kind[0] {
        b'K' => {
            let mut len = [0; 2];
            input.read_exact(&mut len)?;
            let len = u16::from_be_bytes(len) as usize;
            if len > 1024 { return Err(io::Error::other("input frame too large")); }
            let mut bytes = vec![0; len];
            input.read_exact(&mut bytes)?;
            Ok(Event::Input(bytes))
        }
        b'R' => {
            let mut size = [0; 4];
            input.read_exact(&mut size)?;
            Ok(Event::Resize(area(u16::from_be_bytes([size[0], size[1]]), u16::from_be_bytes([size[2], size[3]]))))
        }
        _ => Err(io::Error::other("invalid terminal frame")),
    }
}

impl App {
    pub fn attach_terminal(&mut self, mut a: Attachment) {
        // the newest terminal wins: dropping the old one disconnects it, and its client restores that terminal
        self.detach_terminal_saying("detached: spot opened in another terminal");
        if a.out.write_all(b"0\n").is_err() { return; }
        let Ok(conn) = a.out.try_clone() else { return };
        match screen(a.out, a.area) {
            Ok(screen) => {
                LIGHT.store(a.light, Relaxed);
                BG.store(a.bg, Relaxed);
                self.terminal = Some(Display { id: a.id, screen, conn });
            }
            Err(e) => {
                let _ = conn.shutdown(Shutdown::Both);
                log::warn!("attach: {e}");
            }
        }
    }

    pub fn detach_terminal(&mut self) {
        self.terminal = None;
    }

    /// Detach, leaving `note` printed in that terminal.
    pub fn detach_terminal_saying(&mut self, note: &str) {
        if let Some(t) = &mut self.terminal {
            let out = t.screen.backend_mut();
            let _ = out.write_all(FAREWELL).and_then(|_| write!(out, "{note}\x07")).and_then(|_| out.flush());
        }
        self.detach_terminal();
    }

    pub fn terminal_event(&mut self, id: u64, event: Event) {
        if self.terminal.as_ref().is_none_or(|t| t.id != id) { return; }
        match event {
            Event::Closed => self.detach_terminal(),
            Event::Input(bytes) => {
                for msg in parse_input(&bytes) {
                    self.on_msg(msg);
                    if self.terminal.is_none() || self.quit { break; }
                }
            }
            Event::Resize(area) => {
                let Some(t) = &mut self.terminal else { return };
                match t.conn.try_clone().and_then(|out| screen(out, area)) {
                    Ok(screen) => t.screen = screen,
                    Err(_) => self.detach_terminal(),
                }
            }
        }
    }

    pub fn query_background(&mut self) {
        if let Some(t) = &mut self.terminal {
            let _ = t.screen.backend_mut().write_all(b"\x1b]11;?\x1b\\");
            let _ = t.screen.backend_mut().flush();
        }
    }

    pub fn draw_terminal(&mut self) {
        let Some(mut t) = self.terminal.take() else { return };
        match t.screen.draw(|f| draw(f, self)) {
            Ok(_) => self.terminal = Some(t),
            Err(e) => log::debug!("terminal detached: {e}"),
        }
    }
}

/// Plain `spot` starts the player if necessary and then attaches. First-run browser approvals
/// happen here with a terminal, before starting the detached process.
pub fn launch() -> i32 {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("spot: the TUI needs a terminal; use `spot start` for background playback");
        return 2;
    }
    if UnixStream::connect(sock_path()).is_err() {
        if let Err(e) = run(true) {
            eprintln!("spot: {e}");
            return 1;
        }
        let code = cli::start(false);
        if code != 0 { return code; }
    }
    client()
}

/// Restore the terminal even if the server disconnects or a local write fails.
struct Restore;
impl Drop for Restore {
    fn drop(&mut self) {
        let _ = io::stdout().write_all(b"\x1b[?2031l\x1b[?25h\x1b[?1049l");
        let _ = io::stdout().flush();
        let _ = ratatui::crossterm::terminal::disable_raw_mode();
    }
}

pub fn client() -> i32 {
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!("spot: attach needs a terminal");
        return 2;
    }
    let conn = match UnixStream::connect(sock_path()) {
        Ok(conn) => conn,
        Err(e) if matches!(e.kind(), io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused) => {
            eprintln!("spot isn't running; use `spot` or `spot start` first");
            return 3;
        }
        Err(e) => { eprintln!("spot: {e}"); return 1; }
    };
    match attach(conn) {
        Ok(note) => {
            if let Some(note) = note { println!("spot: {note}"); }
            0
        }
        Err(e) => { eprintln!("spot: {e}"); 1 }
    }
}

fn attach(mut conn: UnixStream) -> Result<Option<String>, String> {
    let _ = conn.set_read_timeout(Some(Duration::from_secs(10)));
    probe_terminal();
    let (w, h) = ratatui::crossterm::terminal::size().map_err(|e| e.to_string())?;
    writeln!(conn, "attach\t1\t{w}\t{h}\t{}\t{}", u8::from(LIGHT.load(Relaxed)), BG.load(Relaxed)).map_err(|e| e.to_string())?;
    let mut output = BufReader::new(conn.try_clone().map_err(|e| e.to_string())?);
    let mut reply = String::new();
    output.read_line(&mut reply).map_err(|e| e.to_string())?;
    if reply.trim() != "0" {
        reply.clear();
        output.read_line(&mut reply).map_err(|e| e.to_string())?;
        return Err(if reply.is_empty() { "player doesn't support attach; restart it after upgrading".into() } else { reply.trim().into() });
    }
    let _ = conn.set_read_timeout(None);
    ratatui::crossterm::terminal::enable_raw_mode().map_err(|e| e.to_string())?;
    let _restore = Restore;
    io::stdout().write_all(b"\x1b[?1049h\x1b[?25l").map_err(|e| e.to_string())?;
    if std::env::var_os("SPOT_THEME").is_none() {
        io::stdout().write_all(b"\x1b[?2031h\x1b[?996n").map_err(|e| e.to_string())?;
    }
    io::stdout().flush().map_err(|e| e.to_string())?;
    let input = Arc::new(Mutex::new(conn));
    let keys = input.clone();
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        let mut buf = [0u8; 1024];
        while let Ok(n) = stdin.read(&mut buf) {
            if n == 0 { break; }
            let mut conn = keys.lock().unwrap_or_else(|e| e.into_inner());
            if conn.write_all(b"K").and_then(|_| conn.write_all(&(n as u16).to_be_bytes())).and_then(|_| conn.write_all(&buf[..n])).is_err() { break; }
        }
        let _ = keys.lock().unwrap_or_else(|e| e.into_inner()).shutdown(Shutdown::Both);
    });
    std::thread::spawn(move || {
        let mut last = (w, h);
        loop {
            std::thread::sleep(Duration::from_millis(200));
            let Ok(now) = ratatui::crossterm::terminal::size() else { continue };
            if now == last { continue; }
            last = now;
            let mut conn = input.lock().unwrap_or_else(|e| e.into_inner());
            if conn.write_all(b"R").and_then(|_| conn.write_all(&now.0.to_be_bytes())).and_then(|_| conn.write_all(&now.1.to_be_bytes())).is_err() { break; }
        }
    });
    let mut buf = [0; 16 * 1024];
    let mut stdout = io::stdout().lock();
    let mut tail = Vec::new();  // the end of the output, where a farewell would be
    loop {
        let n = output.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 { break; }
        stdout.write_all(&buf[..n]).and_then(|_| stdout.flush()).map_err(|e| e.to_string())?;
        tail.extend_from_slice(&buf[..n]);
        tail.drain(..tail.len().saturating_sub(256));
    }
    Ok(farewell(&tail))
}

fn farewell(tail: &[u8]) -> Option<String> {
    let start = tail.windows(FAREWELL.len()).rposition(|w| w == FAREWELL)? + FAREWELL.len();
    let note = tail[start..].strip_suffix(b"\x07")?;
    Some(String::from_utf8_lossy(note).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn farewell_is_the_last_note() {
        assert_eq!(farewell(b"\x1b[2Jscreen\x1b]spot;detached\x07").as_deref(), Some("detached"));
        assert_eq!(farewell(b"\x1b[2Jscreen"), None);
        assert_eq!(farewell(b"\x1b]spot;cut off"), None);
    }

    #[test]
    fn protocol_rejects_truncated_and_oversized_input() {
        for bytes in [b"K\x00\x02x".as_slice(), b"K\x04\x01", b"R\x00", b"?"] {
            assert!(read_event(&mut &bytes[..]).is_err());
        }
    }

    #[test]
    fn terminal_connection_renders_resizes_and_disconnects_independently() {
        // Attach twice to the same message loop: disconnecting a display does not end it.
        let (tx, rx) = mpsc::channel();
        for _ in 0..2 {
            let (mut client, server) = UnixStream::pair().unwrap();
            client.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            let tx = tx.clone();
            let thread = std::thread::spawn(move || {
                serve(server, &["attach", "1", "80", "24", "0", "0"].map(String::from), tx);
            });
            let Msg::Attach(a) = rx.recv_timeout(Duration::from_secs(2)).unwrap() else { panic!("expected attach") };
            let conn = a.out.try_clone().unwrap();
            let mut display = Display { id: a.id, screen: screen(a.out, a.area).unwrap(), conn };
            display.screen.draw(|f| f.render_widget(Paragraph::new("attached"), f.area())).unwrap();
            let mut bytes = [0; 4096];
            let n = client.read(&mut bytes).unwrap();
            assert!(String::from_utf8_lossy(&bytes[..n]).contains("attached"));
            client.write_all(b"K\x00\x01dR\x00\x64\x00\x28").unwrap();
            let Msg::Terminal { id, event: Event::Input(input) } = rx.recv_timeout(Duration::from_secs(2)).unwrap() else { panic!("expected input") };
            assert_eq!(id, display.id);
            assert_eq!(input, b"d");
            let Msg::Terminal { event: Event::Resize(area), .. } = rx.recv_timeout(Duration::from_secs(2)).unwrap() else { panic!("expected resize") };
            display.screen = screen(display.conn.try_clone().unwrap(), area).unwrap();
            assert_eq!(display.screen.get_frame().area(), Rect::new(0, 0, 100, 40));
            drop(display);
            assert!(matches!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), Msg::Terminal { event: Event::Closed, .. }));
            thread.join().unwrap();
        }
    }
}

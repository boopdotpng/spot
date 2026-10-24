// Drawing the window.

use crate::*;

pub fn mmss(ms: u64) -> String {
    format!("{}:{:02}", ms / 60000, ms / 1000 % 60)
}

/// Truncate to at most `max` terminal columns, ending in "…" when cut.
pub fn fit(s: &str, max: usize) -> String {
    use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};
    if s.width() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::new();
    let mut w = 0;
    for c in s.chars() {
        let cw = c.width().unwrap_or(0);
        if w + cw > max - 1 {
            break;
        }
        out.push(c);
        w += cw;
    }
    out.trim_end().to_string() + "…"
}

/// 10435 -> "10,435"
pub fn num(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

pub fn plural(n: usize, what: &str) -> String {
    format!("{} {what}{}", num(n), if n == 1 { "" } else { "s" })
}

/// Total length of a list: "42 min", "3 h 5 min", "712 h".
pub fn hours(ms: u64) -> String {
    let m = (ms + 30_000) / 60_000;
    match (m / 60, m % 60) {
        (0, m) => format!("{m} min"),
        (h, _) if h >= 10 => format!("{} h", num(h as usize)),
        (h, m) => format!("{h} h {m} min"),
    }
}

/// `w` cells of a thick filled rule followed by a thin empty one, optionally with a knob.
pub fn meter(w: usize, ratio: f64, knob: bool, th: &Theme) -> Vec<Span<'static>> {
    let filled = ((ratio.clamp(0.0, 1.0) * w as f64).round() as usize).min(w);
    let (filled, knob) = if knob && w > 0 { (filled.min(w - 1), 1) } else { (filled, 0) };
    vec![
        Span::styled("━".repeat(filled), Style::new().fg(th.accent)),
        Span::styled("●".repeat(knob), Style::new().fg(th.accent)),
        Span::styled("─".repeat(w - filled - knob), Style::new().fg(th.dim)),
    ]
}

/// Key hints for the status line, most useful first; trimmed from the end when space runs out.
pub fn hints(app: &App) -> Vec<(&'static str, &'static str)> {
    let mut h = vec![];
    match app.focus {
        Focus::Playlists => {
            h.push(("⏎", "open"));
            h.push(("/", "search"));
            h.push(("space", "pause"));
            h.push(("s", "shuffle"));
            h.push(("r", "repeat"));
        }
        Focus::Tracks => {
            h.push(("⏎", if app.view.kind == Kind::Tracks { "play" } else { "open" }));
            if app.view.key == QUEUE_KEY {
                h.push(("x", "remove"));
            } else if app.view.kind == Kind::Tracks {
                h.push(("e", "queue"));
            }
            h.push(("/", "search"));
            h.push(("^f", "find"));
            if app.view.artist.is_some() {
                h.push(("v", if app.artist_songs { "releases" } else { "songs" }));
                if app.view.kind == Kind::Albums {
                    h.push(("f", ["all", "albums", "singles"][app.view.filter.next() as usize]));
                }
            }
            if !app.history.is_empty() {
                h.push(("esc", "back"));
            }
            h.push(("a", "album"));
            h.push(("A", "artist"));
            h.push(("s", "shuffle"));
        }
    }
    h.push(("?", "keys"));
    h
}

/// The `?` key block (nano-style, at the bottom): every key in as many columns as fit.
pub fn help_lines(width: usize, th: &Theme) -> Vec<Line<'static>> {
    const KEYS: &[(&str, &str)] = &[
        ("/", "search"),
        ("ctrl+f", "find in list"),
        ("enter", "play / open"),
        ("space", "pause"),
        ("n  p", "next / previous"),
        (",  .", "seek 10s"),
        ("+  -", "volume"),
        ("s", "shuffle"),
        ("r", "repeat"),
        ("P", "play whole list"),
        ("o", "go to playing"),
        ("e", "add to queue"),
        ("Q", "show queue"),
        ("x", "queue: remove song"),
        ("a  A", "album / artist"),
        ("v", "artist: songs / releases"),
        ("f", "releases: all / albums / singles"),
        ("esc", "back"),
        ("tab", "switch pane"),
        ("ctrl+t", "theme"),
        ("g  M  G", "top / middle / bottom"),
        ("q", "quit"),
    ];
    const COL: usize = 36;
    let cols = (width.saturating_sub(1) / COL).clamp(1, 4);
    let rows = KEYS.len().div_ceil(cols);
    let mut out = vec![Line::from(vec![
        Span::styled(" keys", Style::new().fg(th.text).bold()),
        Span::styled("   any key closes", Style::new().fg(th.dim)),
    ])];
    for r in 0..rows {
        let mut spans = vec![Span::raw(" ")];
        for c in 0..cols {
            if let Some((k, d)) = KEYS.get(c * rows + r) {
                spans.push(Span::styled(format!("{k:>7}  "), Style::new().fg(th.accent).bold()));
                spans.push(Span::styled(format!("{d:<COL$}", COL = COL - 9), Style::new().fg(th.sub)));
            }
        }
        out.push(Line::from(spans));
    }
    out
}

pub fn draw(f: &mut Frame, app: &mut App) {
    use ratatui::{layout::Rect, widgets::HighlightSpacing};
    let th = theme();
    let width = |spans: &[Span]| spans.iter().map(|s| s.width()).sum::<usize>();
    let dim = Style::new().fg(th.dim);
    let body = f.area();

    // A terminal-app frame: two panes split by a thin rule, the player under a rule across the
    // whole width, and a vim-style status line at the bottom carrying the search / find prompts,
    // notices and key hints. `?` swaps the status line for a nano-style key block.
    let help = app.help.then(|| help_lines(body.width as usize, th));
    let bottom_h = help.as_ref().map_or(1, |l| l.len() as u16);
    let [top, rule, player, bottom] =
        Layout::vertical([Constraint::Min(3), Constraint::Length(1), Constraint::Length(3), Constraint::Length(bottom_h)]).areas(body);
    let side_w = (body.width * 26 / 100).clamp(18, 36).min(body.width / 2);
    let [left, sep, right] =
        Layout::horizontal([Constraint::Length(side_w), Constraint::Length(1), Constraint::Fill(1)]).areas(top);
    f.render_widget(Paragraph::new(vec![Line::from("│"); sep.height as usize]).style(dim), sep);
    let join = (sep.x - rule.x) as usize;
    f.render_widget(
        Paragraph::new(Line::from(format!("{}┴{}", "─".repeat(join), "─".repeat((rule.width as usize).saturating_sub(join + 1))))).style(dim),
        rule,
    );
    let line_at = |r: Rect, dy: u16| Rect { y: r.y + dy, height: 1.min(r.height.saturating_sub(dy)), ..r };
    let title_style =
        |focused: bool| if focused { Style::new().fg(th.accent).bold() } else { Style::new().fg(th.sub).bold() };
    // Selection: the theme's highlight softened into the terminal background, plus an accent bar
    // at the left edge while the pane has focus.
    let hl = |focused: bool| {
        let bg = match (bg_hex(), rgb_hex(th.hl)) {
            (Some(s), Some(h)) => hex_rgb(mix(s, h, if focused { 0.75 } else { 0.4 })),
            _ if focused => th.hl,
            _ => th.deep,
        };
        let st = Style::new().bg(bg);
        if focused { st.bold() } else { st }
    };
    let bar_sym = |focused: bool| Span::styled(if focused { "┃" } else { " " }, Style::new().fg(th.accent));
    let pl_focus = app.focus == Focus::Playlists;
    let mark = |playing: bool| if playing { "▶" } else { "‖" };

    // sidebar: title, blank, playlists (the playing one carries the play mark)
    f.render_widget(Paragraph::new(Span::styled("    playlists", title_style(pl_focus))), line_at(left, 0));
    let playing_key = app.queue.as_ref().map(|q| q.key.as_str());
    let name_w = (left.width as usize).saturating_sub(5);
    let items: Vec<ListItem> = app
        .playlists
        .iter()
        .map(|p| {
            let playing = Some(p.key().as_str()) == playing_key;
            let style = if playing {
                Style::new().fg(th.accent).bold()
            } else if p.id.is_none() {
                Style::new().fg(th.pink)
            } else {
                Style::new().fg(th.text)
            };
            let m = if playing { mark(app.playing) } else { " " };
            ListItem::new(format!(" {m} {}", fit(&p.name, name_w))).style(style)
        })
        .collect();
    let list_area = Rect { y: left.y + 2, height: left.height.saturating_sub(2), width: left.width.saturating_sub(1), ..left };
    let pl_list = List::new(items)
        .highlight_style(hl(pl_focus))
        .highlight_symbol(bar_sym(pl_focus))
        .highlight_spacing(HighlightSpacing::Always);
    f.render_stateful_widget(pl_list, list_area, &mut app.pl_state);

    // the open list: title · meta, column header, numbered rows, scrollbar
    let tracks = app.view.kind == Kind::Tracks;
    // by uri: a list refreshed from the cache can have shifted under the queue's copy
    let playing_uri = app.queue.as_ref().filter(|q| q.key == app.view.key && app.queued.is_none()).and_then(|q| q.tracks.get(q.idx())).map(|t| t.uri.as_str());
    let shown = app.shown();
    let total = shown.len();
    let meta = if app.view.loading {
        "loading…".into()
    } else if app.view.key.is_empty() {
        String::new()
    } else if tracks {
        let ms = app.view.tracks.iter().map(|t| t.ms).sum();
        format!("{} · {}", plural(app.view.len(), "song"), hours(ms))
    } else {
        let what = match app.view.kind {
            Kind::Artists => "artist",
            _ if app.view.artist.is_some() => match app.view.filter {
                Releases::All => "release",
                Releases::Albums => "album",
                Releases::Singles => "single",
            },
            _ => "album",
        };
        plural(app.view.len(), what)
    };
    let inner = Rect { x: right.x + 1, width: right.width.saturating_sub(3), ..right }; // pad, scrollbar, margin
    let title_row = line_at(inner, 0);
    let title_w = (title_row.width as usize).saturating_sub(Span::raw(&meta).width() + 4);
    f.render_widget(Paragraph::new(Span::styled(format!(" {}", fit(&app.view.title, title_w)), title_style(!pl_focus))), title_row);
    f.render_widget(Paragraph::new(Line::from(Span::styled(meta, dim)).right_aligned()), title_row);

    // Build only the rows that fit: ratatui would otherwise lay out every row of a 10k-song list each frame.
    let table_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let height = table_area.height.saturating_sub(1) as usize;
    let sel = app.tr_state.selected().map(|s| s.min(total.saturating_sub(1)));
    let mut off = app.tr_state.offset().min(total.saturating_sub(height));
    // multi-disc albums get a "disc n" line above each disc's first song, set off by a blank
    // line unless it's at the top; they take lines but aren't rows you can select
    let discs = tracks && app.view.key.starts_with("spotify:album:") && app.view.tracks.iter().any(|t| t.disc > 1);
    let disc_of = |r: usize| shown.get(r).map_or(0, |i| app.view.tracks[i].disc);
    let starts_disc = |r: usize| discs && (r == 0 || disc_of(r) != disc_of(r - 1));
    // lines above row r's song when the window starts at `off`
    let above = |r: usize, off: usize| if !starts_disc(r) { 0 } else if r == off { 1 } else { 2 };
    let lines_to = |off: usize, s: usize| (off..=s).map(|r| 1 + above(r, off)).sum::<usize>();
    if let Some(s) = sel {
        if s < off {
            off = s;
        }
        while off < s && lines_to(off, s) > height {
            off += 1;
        }
    }
    let num_w = num(app.view.len().max(1)).len() as u16;
    let wide = table_area.width >= 90; // room for an album column
    let (cols, labels): (Vec<Constraint>, &[&str]) = match (tracks, wide) {
        (true, true) => (
            vec![Constraint::Length(num_w), Constraint::Fill(4), Constraint::Fill(3), Constraint::Fill(3), Constraint::Length(5)],
            &["", "title", "artist", "album", "time"],
        ),
        (true, false) => (
            vec![Constraint::Length(num_w), Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(5)],
            &["", "title", "artist", "time"],
        ),
        (false, _) => (vec![Constraint::Length(num_w), Constraint::Fill(3), Constraint::Fill(2)], &["", "name", "details"]),
    };
    const GAP: u16 = 2;
    // the selection bar takes the first cell
    let cols_area = Rect { x: table_area.x + 1, width: table_area.width.saturating_sub(1), ..table_area };
    let w: Vec<usize> = Layout::horizontal(cols.clone()).spacing(GAP).split(cols_area).iter().map(|r| r.width as usize).collect();
    let cell = |s: &str, k: usize, style: Style| Cell::from(fit(s, w[k])).style(style);
    let numcell = |i: usize, playing: bool, style: Style| {
        let s = if playing { mark(app.playing).to_string() } else { num(i + 1) };
        Cell::from(Line::from(s).right_aligned()).style(style)
    };
    let row = |i: usize| {
        if !tracks {
            let l = &app.view.links[i];
            return Row::new([
                numcell(i, false, dim),
                cell(&l.name, 1, Style::new().fg(th.text)),
                cell(&l.sub, 2, Style::new().fg(th.sub)),
            ]);
        }
        let t = &app.view.tracks[i];
        let playing = Some(t.uri.as_str()) == playing_uri;
        let [n, name, artist, album, time] = if playing {
            let a = Style::new().fg(th.accent);
            [a.bold(), a.bold(), a, a, a]
        } else {
            [th.dim, th.text, th.sub, th.dim, th.dim].map(|c| Style::new().fg(c))
        };
        let mut cells = vec![numcell(i, playing, n), cell(&t.name, 1, name), cell(&t.artists, 2, artist)];
        if wide {
            cells.push(cell(&t.album, 3, album));
        }
        cells.push(Cell::from(Line::from(mmss(t.ms)).right_aligned()).style(time));
        Row::new(cells)
    };
    let disc_style = Style::new().fg(th.accent2).bold();
    let mut rows: Vec<Row> = Vec::with_capacity(height);
    let mut sel_line = None;
    for r in off..total {
        let Some(i) = shown.get(r) else { break };
        // a header needs its first song under it
        if rows.len() + 1 + above(r, off) > height {
            break;
        }
        if above(r, off) == 2 {
            rows.push(Row::new([""]));
        }
        if above(r, off) > 0 {
            rows.push(Row::new([Cell::from(""), Cell::from(format!("disc {}", disc_of(r)))]).style(disc_style));
        }
        if sel == Some(r) {
            sel_line = Some(rows.len());
        }
        rows.push(row(i));
    }
    let header = Row::new(labels.iter().map(|&l| {
        let line = Line::from(l);
        Cell::from(if l == "time" { line.right_aligned() } else { line })
    }))
    .style(dim);
    let table = Table::new(rows, cols)
        .header(header)
        .column_spacing(GAP)
        .row_highlight_style(hl(!pl_focus))
        .highlight_symbol(Line::from(bar_sym(!pl_focus)))
        .highlight_spacing(HighlightSpacing::Always);
    let mut window = TableState::default().with_selected(sel_line);
    f.render_stateful_widget(table, table_area, &mut window);
    *app.tr_state.offset_mut() = off;
    if total > height {
        use ratatui::widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState};
        let area = Rect { x: table_area.right() + 1, y: table_area.y + 1, width: 1, height: height as u16 };
        let mut st = ScrollbarState::new(total - height + 1).position(off).viewport_content_length(height);
        // thumb only: a track line would read as a divider
        let sb = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_symbol(None)
            .thumb_symbol("┃")
            .thumb_style(dim);
        f.render_stateful_widget(sb, area, &mut st);
    }

    // player: mark + title with modes and where it's playing from, artist · album with what's next,
    // then the progress bar
    let pad = Rect { x: player.x + 1, width: player.width.saturating_sub(2), ..player };
    let [now, sub, bar] = Layout::vertical([Constraint::Length(1); 3]).areas(pad);
    let gold = Style::new().fg(th.gold).bold();
    let sep_span = || Span::styled("  ·  ", dim);
    let cur = app.now();
    // where it's playing from
    let ctx = if app.queued.is_some() { Some("Queue") } else { app.queue.as_ref().map(|q| q.name.as_str()) };
    let (pos, len) = cur.map_or((0, 0), |t| (app.position().min(t.ms), t.ms));
    let mut top_right: Vec<Span> = vec![];
    let add = |top_right: &mut Vec<Span<'static>>, span: Span<'static>| {
        if !top_right.is_empty() {
            top_right.push(sep_span());
        }
        top_right.push(span);
    };
    if app.shuffle {
        add(&mut top_right, Span::styled("shuffle", gold));
    }
    match app.repeat {
        Repeat::Off => {}
        Repeat::All => add(&mut top_right, Span::styled("repeat", gold)),
        Repeat::One => add(&mut top_right, Span::styled("repeat 1", gold)),
    }
    if let Some(name) = ctx.filter(|_| cur.is_some()) {
        let ctx_w = Span::raw(name).width();
        if (now.width as usize) >= ctx_w + width(&top_right) + 32 {
            add(&mut top_right, Span::styled(name.to_string(), Style::new().fg(th.accent2)));
        }
    }
    let avail = (now.width as usize).saturating_sub(5 + width(&top_right));
    let top_left = match cur {
        Some(t) => vec![
            Span::styled(format!("{} ", mark(app.playing)), Style::new().fg(th.accent).bold()),
            Span::styled(fit(&t.name, avail), Style::new().fg(th.text).bold()),
        ],
        None => vec![Span::styled(fit("nothing playing", avail + 3), dim)],
    };
    f.render_widget(Paragraph::new(Line::from(top_left)), now);
    f.render_widget(Paragraph::new(Line::from(top_right).right_aligned()), now);
    if let Some(t) = cur {
        // up next (when the queue isn't on its last song; repeat one plays this again)
        let next = (app.repeat != Repeat::One).then(|| app.up_next()).flatten();
        let full = (sub.width as usize).saturating_sub(2);
        let next_spans = next.map(|n| {
            let room = (full / 2).saturating_sub(8);
            let name = fit(&n.name, room);
            let mut v = vec![Span::styled("next  ", dim), Span::styled(name.clone(), Style::new().fg(th.sub))];
            let rest = room.saturating_sub(Span::raw(&name).width() + 3);
            if rest >= 6 {
                v.push(Span::styled(" · ", dim));
                v.push(Span::styled(fit(&n.artists, rest), dim));
            }
            v
        });
        let show_next = next_spans.as_ref().is_some_and(|n| full >= width(n) + 28);
        let avail = if show_next { full - width(next_spans.as_ref().unwrap()) - 3 } else { full };
        let artist = fit(&t.artists, avail);
        let mut spans = vec![Span::raw("  "), Span::styled(artist.clone(), Style::new().fg(th.sub))];
        let rest = avail.saturating_sub(Span::raw(&artist).width() + 3);
        if !t.album.is_empty() && rest >= 6 {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(fit(&t.album, rest), dim));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), sub);
        if show_next {
            f.render_widget(Paragraph::new(Line::from(next_spans.unwrap()).right_aligned()), sub);
        }
    }
    let (lt, rt) = if cur.is_some() { (mmss(pos), mmss(len)) } else { (String::new(), String::new()) };
    let (lt, rt) = (Span::styled(format!("{lt:>4} "), dim), Span::styled(format!(" {rt}"), dim));
    let track_w = (bar.width as usize).saturating_sub(lt.width() + rt.width());
    let ratio = if len > 0 { pos as f64 / len as f64 } else { 0.0 };
    let mut spans = vec![lt];
    spans.extend(meter(track_w, ratio, cur.is_some(), th));
    spans.push(rt);
    f.render_widget(Paragraph::new(Line::from(spans)), bar);

    // bottom: on a faint tint of the terminal background (needs truecolor)
    if let Some(p) = panel(th) {
        f.render_widget(Block::new().style(Style::new().bg(hex_rgb(p))), bottom);
    }
    if let Some(lines) = help {
        f.render_widget(Paragraph::new(lines), bottom);
        return;
    }
    // status line: prompt (search / find) > error > notice > key hints; position and volume at the right
    let bw = bottom.width as usize;
    let (left_side, right_side): (Vec<Span>, Vec<Span>) = if let Some(buf) = &app.input {
        let kinds = [Kind::Tracks, Kind::Albums, Kind::Artists].map(|k| {
            let st = if k == app.search_kind { gold } else { dim };
            Span::styled(k.label(), st)
        });
        let [a, b, c] = kinds;
        let right = vec![a, Span::styled(" · ", dim), b, Span::styled(" · ", dim), c, Span::styled("   tab ", dim)];
        (prompt(" /", th.accent, buf, bw.saturating_sub(width(&right) + 2), th), right)
    } else if let Some(buf) = &app.find {
        let right = vec![Span::styled(format!("{} of {} ", num(total), num(app.view.len())), dim)];
        (prompt(" find", th.gold, buf, bw.saturating_sub(width(&right) + 2), th), right)
    } else {
        let mut right = vec![];
        if app.sess.borrow().is_none() {
            right.push(Span::styled("connecting…   ", dim));
        }
        if app.focus == Focus::Tracks && total > 0 {
            right.push(Span::styled(format!("{}/{}   ", num(sel.map_or(0, |s| s + 1)), num(total)), dim));
        }
        let pct = app.vol_pct();
        if app.vol_until.is_some_and(|u| Instant::now() < u) {
            right.push(Span::styled("vol ", dim));
            right.extend(meter(10, pct as f64 / 100.0, false, th));
            right.push(Span::styled(format!(" {pct:>3}% "), Style::new().fg(th.text)));
        } else {
            right.push(Span::styled(format!("vol {pct}% "), dim));
        }
        let avail = bw.saturating_sub(width(&right) + 2);
        let left = if !app.status.is_empty() {
            vec![Span::styled(format!(" {}", fit(&app.status, avail)), Style::new().fg(th.err).bold())]
        } else if let Some((n, _)) = &app.note {
            vec![Span::styled(format!(" {}", fit(n, avail)), Style::new().fg(th.accent))]
        } else {
            let mut v = vec![];
            let mut used = 0;
            for (k, d) in hints(app) {
                let need = k.chars().count() + d.len() + 3;
                if used + need > avail {
                    break;
                }
                v.push(Span::styled(format!(" {k}"), Style::new().fg(th.sub).bold()));
                v.push(Span::styled(format!(" {d} "), dim));
                used += need;
            }
            v
        };
        (left, right)
    };
    f.render_widget(Paragraph::new(Line::from(left_side)), bottom);
    f.render_widget(Paragraph::new(Line::from(right_side).right_aligned()), bottom);
}

/// A one-line text prompt: colored label, the end of the text (so the cursor stays in view), a caret.
pub fn prompt(label: &str, color: Color, buf: &str, avail: usize, th: &Theme) -> Vec<Span<'static>> {
    let avail = avail.saturating_sub(label.len() + 3);
    let text = buf.chars().rev().scan(0, |w, c| {
        *w += unicode_width::UnicodeWidthChar::width(c).unwrap_or(0);
        (*w <= avail).then_some(c)
    });
    let text: String = text.collect::<Vec<_>>().into_iter().rev().collect();
    vec![
        Span::styled(format!("{label} "), Style::new().fg(color).bold()),
        Span::styled(text, Style::new().fg(th.text)),
        Span::styled("▏", Style::new().fg(color)),
    ]
}

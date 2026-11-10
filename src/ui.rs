// Drawing the TUI: the sidebar, the open list (or an artist's overview), the player and the
// status line.

use crate::*;

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
        ("Q", "up next"),
        ("x", "queue: remove song"),
        ("a  A", "album / artist"),
        ("v", "artist: overview/songs/albums"),
        ("f", "releases: all/albums/singles"),
        ("P", "artist: shuffle all songs"),
        ("S", "playlist: sort by date added"),
        ("m", "playlists / podcasts"),
        ("*", "pin playlist to the top"),
        ("esc  [", "back"),
        ("]", "forward"),
        ("tab", "switch pane"),
        ("ctrl+t", "theme"),
        ("g  M  G", "top / middle / bottom"),
        ("q", "quit"),
        ("d  ctrl+c", "detach (keep playing)"),
    ];
    const COL: usize = 40;
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

/// Keep the selection visible, examining at most one screen of songs even after a big jump.
fn table_offset(off: usize, selected: usize, height: usize, starts_disc: impl Fn(usize) -> bool) -> usize {
    let mut off = off.min(selected).max(selected.saturating_sub(height.saturating_sub(1)));
    let mut lines: usize = (off..=selected)
        .map(|r| 1 + if starts_disc(r) { if r == off { 1 } else { 2 } } else { 0 })
        .sum();
    while off < selected && lines > height {
        lines -= 1 + usize::from(starts_disc(off));
        off += 1;
        // A disc header at the top no longer needs a blank line above it.
        lines -= usize::from(starts_disc(off));
    }
    off
}


pub fn draw(f: &mut Frame, app: &mut App) {
    use ratatui::{layout::Rect, widgets::HighlightSpacing};
    let th = theme();
    let width = |spans: &[Span]| spans.iter().map(|s| s.width()).sum::<usize>();
    let dim = Style::new().fg(th.dim);
    let body = f.area();

    // A terminal-app frame: two panes split by a thin rule, the player under a rule across the
    // whole width, and a vim-style status line at the bottom carrying the search / find prompts
    // and notices. `?` swaps the status line for a nano-style key block.
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

    // sidebar: title, blank, playlists or podcasts (the playing one carries the play mark)
    let side_title = if app.podcasts { "    podcasts" } else { "    playlists" };
    f.render_widget(Paragraph::new(Span::styled(side_title, title_style(pl_focus))), line_at(left, 0));
    let playing_key = app.queue.as_ref().map(|q| q.key.as_str());
    let name_w = (left.width as usize).saturating_sub(5);
    let item = |key: &str, name: &str, special: bool| {
        let playing = Some(key) == playing_key;
        let style = if playing {
            Style::new().fg(th.accent).bold()
        } else if special {
            Style::new().fg(th.pink)
        } else {
            Style::new().fg(th.text)
        };
        let m = if playing { mark(app.playing) } else { " " };
        ListItem::new(format!(" {m} {}", fit(name, name_w))).style(style)
    };
    let (items, sel): (Vec<ListItem>, _) = if app.podcasts {
        (app.shows.iter().map(|l| item(&l.uri, &l.name, !is_show(&l.uri))).collect(), app.sh_state.selected())
    } else {
        (app.playlists.iter().map(|p| item(&p.key(), &p.name, p.id.is_none())).collect(), app.pl_state.selected())
    };
    let list_area = Rect { y: left.y + 2, height: left.height.saturating_sub(2), width: left.width.saturating_sub(1), ..left };
    let pl_list = List::new(items)
        .highlight_style(hl(pl_focus))
        .highlight_symbol(bar_sym(pl_focus))
        .highlight_spacing(HighlightSpacing::Always);
    let mut side = ListState::default().with_selected(sel).with_offset(app.side_off);
    f.render_stateful_widget(pl_list, list_area, &mut side);
    app.side_off = side.offset();

    // the open list: title · meta, column header, numbered rows, scrollbar
    let inner = Rect { x: right.x + 1, width: right.width.saturating_sub(3), ..right }; // pad, scrollbar, margin
    // an artist's overview or a search of everything is laid out in cards, as many to a line as fit
    let overview = app.view.info.is_some();
    app.grid_cols.set(if overview { ((inner.width as usize + CARD_GAP) / (CARD_W + CARD_GAP)).clamp(1, 6) } else { 1 });
    let tracks = app.view.kind == Kind::Tracks;
    // by uri: a list refreshed from the cache can have shifted under the queue's copy
    let playing_uri = app.queue.as_ref().filter(|q| q.key == app.view.key && app.queued.is_none()).and_then(|q| q.tracks.get(q.idx())).map(|t| t.uri.as_str());
    let shown = app.shown();
    let total = shown.len();
    let meta = if app.view.loading {
        "loading…".into()
    } else if app.view.key.is_empty() || overview {
        String::new()
    } else if tracks {
        let ms = app.view.tracks.iter().map(|t| t.ms).sum();
        let mut m = format!("{} · {}", plural(app.view.len(), "song"), hours(ms));
        if let Some(by) = &app.view.by {
            m = format!("{} · {m}", by.names);
        }
        if app.dated() && app.sort != Sort::List {
            m += if app.sort == Sort::Newest { " · newest added first" } else { " · oldest added first" };
        }
        m
    } else {
        let what = match app.view.kind {
            Kind::Artists => "artist",
            Kind::Shows => "podcast",
            _ if app.view.artist.is_some() => match app.view.filter {
                Releases::All => "release",
                Releases::Albums => "album",
                Releases::Singles => "single",
            },
            _ => "album",
        };
        plural(app.view.len(), what)
    };
    let title_row = line_at(inner, 0);
    let title_w = (title_row.width as usize).saturating_sub(Span::raw(&meta).width() + 4);
    f.render_widget(Paragraph::new(Span::styled(format!(" {}", fit(&app.view.title, title_w)), title_style(!pl_focus))), title_row);
    f.render_widget(Paragraph::new(Line::from(Span::styled(meta, dim)).right_aligned()), title_row);

    let table_area = Rect { y: inner.y + 1, height: inner.height.saturating_sub(1), ..inner };
    let sel = app.tr_state.selected().map(|s| s.min(total.saturating_sub(1)));
    'list: {
        if overview {
            draw_overview(f, app, table_area, !pl_focus, playing_uri.map(String::from));
            break 'list;
        }
        // Build only the rows that fit: ratatui would otherwise lay out every row of a 10k-song list each frame.
        let height = table_area.height.saturating_sub(1) as usize;
        let mut off = app.tr_off.min(total.saturating_sub(height));
        // multi-disc albums get a "disc n" line above each disc's first song, set off by a blank
        // line unless it's at the top; they take lines but aren't rows you can select
        // up next likewise, around the queue and the list after it
        let up_next = app.view.key == QUEUE_KEY;
        let queued = app.upnext.len();
        let discs = up_next || tracks && app.view.key.starts_with("spotify:album:") && app.view.tracks.iter().any(|t| t.disc > 1);
        let disc_of = |r: usize| shown.get(r).map_or(0, |i| if up_next { usize::from(i >= queued) } else { app.view.tracks[i].disc as usize });
        let disc_title = |r: usize| match (up_next, disc_of(r), &app.queue) {
            (false, disc, _) => format!("disc {disc}"),
            (true, 0, _) => "queue".into(),
            (true, _, Some(q)) => format!("then {}", q.name),
            (true, _, None) => "then".into(),
        };
        let starts_disc = |r: usize| discs && (r == 0 || disc_of(r) != disc_of(r - 1));
        // lines above row r's song when the window starts at `off`
        let above = |r: usize, off: usize| if !starts_disc(r) { 0 } else if r == off { 1 } else { 2 };
        if let Some(s) = sel {
            off = table_offset(off, s, height, starts_disc);
        }
        // only albums are numbered; elsewhere the column holds just the playing mark
        let numbered = tracks && app.view.key.starts_with("spotify:album:");
        let num_w = if numbered { num(app.view.len().max(1)).len() as u16 } else { 1 };
        let wide = table_area.width >= 90; // room for an album column
        // episodes: the show, its date, and what's left of it
        let episodes = app.view.tracks.first().is_some_and(Track::is_episode);
        let time_w = if episodes { 11 } else { 5 };
        let by_added = app.dated() && app.sort != Sort::List;
        let today = now_secs();
        let (cols, labels): (Vec<Constraint>, &[&str]) = match (tracks, wide) {
            (true, true) => (
                vec![Constraint::Length(num_w), Constraint::Fill(4), Constraint::Fill(3), Constraint::Fill(3), Constraint::Length(time_w)],
                // sorted by date added, that's the column instead of the album
            if episodes { &["", "title", "show", "date", "time"] } else if by_added { &["", "title", "artist", "added", "time"] } else { &["", "title", "artist", "album", "time"] },
            ),
            (true, false) => (
                vec![Constraint::Length(num_w), Constraint::Fill(3), Constraint::Fill(2), Constraint::Length(time_w)],
                if episodes { &["", "title", "show", "time"] } else { &["", "title", "artist", "time"] },
            ),
            (false, _) => (vec![Constraint::Length(num_w), Constraint::Fill(3), Constraint::Fill(2)], &["", "name", "details"]),
        };
        const GAP: u16 = 2;
        // the selection bar takes the first cell
        let cols_area = Rect { x: table_area.x + 1, width: table_area.width.saturating_sub(1), ..table_area };
        let w: Vec<usize> = Layout::horizontal(cols.clone()).spacing(GAP).split(cols_area).iter().map(|r| r.width as usize).collect();
        let cell = |s: &str, k: usize, style: Style| Cell::from(fit(s, w[k])).style(style);
        let numcell = |i: usize, playing: bool, style: Style| {
            let s = if playing { mark(app.playing).to_string() } else if numbered { num(i + 1) } else { String::new() };
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
            let (time_text, heard) = if t.is_episode() { episode_time(t.ms, app.progress.get(&t.uri).copied()) } else { (mmss(t.ms), false) };
            let [n, name, artist, album, time] = if playing {
                let a = Style::new().fg(th.accent);
                [a.bold(), a.bold(), a, a, a]
            } else if heard {
                [Style::new().fg(th.dim); 5]
            } else {
                [th.dim, th.text, th.sub, th.dim, th.dim].map(|c| Style::new().fg(c))
            };
            // explicit songs get an outlined E (U+1F134) after the title, like Spotify's label
            let title = if t.explicit {
                let badge = Style::new().fg(if playing { th.accent } else { th.dim });
                let line = Line::from(vec![Span::raw(fit(&t.name, w[1].saturating_sub(2))), Span::raw(" "), Span::styled("\u{1F134}", badge)]);
                Cell::from(line).style(name)
            } else {
                cell(&t.name, 1, name)
            };
            let mut cells = vec![numcell(i, playing, n), title, cell(&t.artists, 2, artist)];
            if wide {
                cells.push(if by_added { cell(&added(t.added, today), 3, album) } else { cell(&t.album, 3, album) });
            }
            cells.push(Cell::from(Line::from(time_text).right_aligned()).style(time));
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
                rows.push(Row::new([Cell::from(""), Cell::from(disc_title(r))]).style(disc_style));
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
        app.tr_off = off;
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
    }

    // player: mark + title with modes and where it's playing from, artist · album,
    // then the progress bar
    let mut pad = Rect { x: player.x + 1, width: player.width.saturating_sub(2), ..player };
    // album art at the left, while there's room for the text beside it
    if let Some(art) = app.art.image.as_ref().filter(|_| app.now().is_some() && pad.width >= 50) {
        let w = art.size().width;
        f.render_widget(ratatui_image::Image::new(art), Rect { width: w, ..pad });
        pad.x += w + 1;
        pad.width -= w + 1;
    }
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
        let avail = (sub.width as usize).saturating_sub(2);
        let artist = fit(&t.artists, avail);
        let mut spans = vec![Span::raw("  "), Span::styled(artist.clone(), Style::new().fg(th.sub))];
        let rest = avail.saturating_sub(Span::raw(&artist).width() + 3);
        if !t.album.is_empty() && rest >= 6 {
            spans.push(Span::styled(" · ", dim));
            spans.push(Span::styled(fit(&t.album, rest), dim));
        }
        f.render_widget(Paragraph::new(Line::from(spans)), sub);
    }
    let (lt, rt) = if cur.is_some() { (mmss(pos), mmss(len)) } else { (String::new(), String::new()) };
    let (lt, rt) = (Span::styled(format!("{lt:>4} "), dim), Span::styled(format!(" {rt}"), dim));
    let track_w = (bar.width as usize).saturating_sub(lt.width() + rt.width());
    let ratio = if len > 0 { pos as f64 / len as f64 } else { 0.0 };
    let mut spans = vec![lt];
    spans.extend(meter(track_w, ratio, cur.is_some(), th));
    spans.push(rt);
    f.render_widget(Paragraph::new(Line::from(spans)), bar);

    // bottom: on a faint tint of the terminal background (needs truecolor), when it has something to say
    let quiet = !app.help && app.input.is_none() && app.find.is_none() && app.status.is_empty() && app.note.is_none();
    if let Some(p) = panel(th).filter(|_| !quiet) {
        f.render_widget(Block::new().style(Style::new().bg(hex_rgb(p))), bottom);
    }
    if let Some(lines) = help {
        f.render_widget(Paragraph::new(lines), bottom);
        return;
    }
    // status line: prompt (search / find) > error > notice > what's next; position and volume at the right
    let bw = bottom.width as usize;
    let (left_side, right_side): (Vec<Span>, Vec<Span>) = if let Some(buf) = &app.input {
        let mut right = vec![];
        for k in [None, Some(Kind::Tracks), Some(Kind::Albums), Some(Kind::Artists), Some(Kind::Shows)] {
            if !right.is_empty() {
                right.push(Span::styled(" · ", dim));
            }
            right.push(Span::styled(search_label(k), if k == app.search_kind { gold } else { dim }));
        }
        right.push(Span::styled("   tab ", dim));
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
        } else if let Some(n) = cur.filter(|_| app.repeat != Repeat::One).and_then(|_| app.up_next()) {
            // up next (when the queue isn't on its last song; repeat one plays this again)
            let room = avail.saturating_sub(7);
            let name = fit(&n.name, room);
            let mut v = vec![Span::styled(" next  ", dim), Span::styled(name.clone(), Style::new().fg(th.sub))];
            let rest = room.saturating_sub(Span::raw(&name).width() + 3);
            if rest >= 6 {
                v.push(Span::styled(" · ", dim));
                v.push(Span::styled(fit(&n.artists, rest), dim));
            }
            v
        } else {
            vec![]
        };
        (left, right)
    };
    f.render_widget(Paragraph::new(Line::from(left_side)), bottom);
    f.render_widget(Paragraph::new(Line::from(right_side).right_aligned()), bottom);
}

/// A card's width on an overview, in cells, and the gap between cards.
const CARD_W: usize = 30;
const CARD_GAP: usize = 3;

/// An artist's overview or a search of everything: sections of songs and cards, laid out by
/// `App::blocks` (the cursor walks them with `grid_step`), scrolled to keep the cursor in view.
fn draw_overview(f: &mut Frame, app: &mut App, area: ratatui::layout::Rect, focused: bool, playing_uri: Option<String>) {
    use unicode_width::UnicodeWidthStr;
    let th = theme();
    let dim = Style::new().fg(th.dim);
    let width = area.width.saturating_sub(1) as usize; // the cursor's bar takes the first cell
    let sel = app.tr_state.selected();
    let all = app.view.is_search_all();
    let section_title = |s: Option<Section>| match s {
        None if all => "songs".to_string(),
        None => "popular".into(),
        Some(Section::Top) => "top result".into(),
        Some(Section::Releases) if all => "albums".into(),
        Some(Section::Releases) => app.view.filter.label().into(),
        Some(Section::Appears) => "appears on".into(),
        Some(Section::Related) if all => "artists".into(),
        Some(Section::Related) => "fans also like".into(),
    };
    let sel_bg = {
        let st = Style::new().bg(match (bg_hex(), rgb_hex(th.hl)) {
            (Some(s), Some(h)) => hex_rgb(mix(s, h, if focused { 0.75 } else { 0.4 })),
            _ if focused => th.hl,
            _ => th.deep,
        });
        if focused { st.bold() } else { st }
    };
    // one stop as `w` cells of spans
    let stop = |i: usize, section: Option<Section>, w: usize| -> Vec<Span<'static>> {
        let pad = |mut spans: Vec<Span<'static>>| {
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            spans.push(Span::raw(" ".repeat(w.saturating_sub(used))));
            spans
        };
        if section.is_none() {
            let Some(t) = app.view.tracks.get(i) else { return pad(vec![]) };
            let playing = playing_uri.as_deref() == Some(t.uri.as_str());
            let n = if playing { format!("{:>2} ", if app.playing { "▶" } else { "‖" }) } else { format!("{:>2} ", i + 1) };
            let time = format!(" {}", mmss(t.ms));
            let room = w.saturating_sub(n.width() + time.width());
            let name = fit(&t.name, room);
            let rest = room.saturating_sub(name.width() + 3);
            let mut spans = vec![
                Span::styled(n, if playing { Style::new().fg(th.accent).bold() } else { dim }),
                Span::styled(name, Style::new().fg(if playing { th.accent } else { th.text })),
            ];
            if rest >= 6 {
                spans.push(Span::styled(" · ", dim));
                spans.push(Span::styled(fit(&t.artists, rest), Style::new().fg(th.sub)));
            }
            let used: usize = spans.iter().map(|s| s.content.width()).sum();
            spans.push(Span::raw(" ".repeat(w.saturating_sub(used + time.width()))));
            spans.push(Span::styled(time, dim));
            return spans;
        }
        let Some((_, l)) = app.card_at(i) else { return pad(vec![]) };
        let name = fit(&l.name, w);
        let rest = w.saturating_sub(name.width() + 2);
        let mut spans = vec![Span::styled(name, Style::new().fg(th.text))];
        if rest >= 6 && !l.sub.is_empty() {
            spans.push(Span::raw("  "));
            spans.push(Span::styled(fit(&l.sub, rest), Style::new().fg(th.sub)));
        }
        pad(spans)
    };
    let mut lines: Vec<Line> = vec![];
    let mut sel_line = 0;
    for (bi, b) in app.blocks().into_iter().enumerate() {
        // a search's first section is also set off from the title above it
        if bi > 0 || all {
            lines.push(Line::default());
        }
        lines.push(Line::from(Span::styled(format!(" {}", section_title(b.section)), Style::new().fg(th.accent2).bold())));
        let rows = b.len.div_ceil(b.cols);
        let cell_w = (width.saturating_sub(CARD_GAP * (b.cols - 1)) / b.cols).max(1);
        for line in 0..rows {
            let mut spans = vec![];
            for col in 0..b.cols {
                let off = if b.down { col * rows + line } else { line * b.cols + col };
                if off >= b.len {
                    break;
                }
                let i = b.start + off;
                let here = sel == Some(i);
                spans.push(Span::styled(if col == 0 { if here && focused { "┃" } else { " " } } else { "" }, Style::new().fg(th.accent)));
                if col > 0 {
                    spans.push(Span::raw(" ".repeat(CARD_GAP)));
                }
                let cell = stop(i, b.section, cell_w);
                spans.extend(if here { cell.into_iter().map(|s| s.patch_style(sel_bg)).collect() } else { cell });
                if here {
                    sel_line = lines.len();
                }
            }
            lines.push(Line::from(spans));
        }
    }
    if lines.is_empty() {
        let what = if app.view.loading { "loading…" } else { "nothing here" };
        lines.push(Line::from(Span::styled(format!(" {what}"), dim)));
    }
    // keep the cursor's line in view, with its section's title when that fits
    let height = area.height as usize;
    let mut off = app.tr_off.min(lines.len().saturating_sub(height));
    if sel_line <= off {
        let titled = sel_line >= 1 && lines[sel_line - 1].spans.len() == 1;
        off = sel_line - usize::from(titled);
        // not scrolled by only the blank line above the first section
        if off == 1 && lines[0].spans.is_empty() {
            off = 0;
        }
    }
    if sel_line >= off + height {
        off = sel_line + 1 - height;
    }
    app.tr_off = off;
    let shown: Vec<Line> = lines.into_iter().skip(off).take(height).collect();
    f.render_widget(Paragraph::new(shown), area);
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

#[cfg(test)]
mod tests {
    use super::table_offset;

    #[test]
    fn scrolling_matches_original_layout_with_and_without_disc_headers() {
        for discs in [false, true] {
            let starts_disc = |r: usize| discs && r.is_multiple_of(5);
            for height in 0..15 {
                for selected in 0..40 {
                    for old in 0..40 {
                        let mut expected = old.min(selected);
                        while expected < selected {
                            let lines: usize = (expected..=selected)
                                .map(|r| 1 + if starts_disc(r) { if r == expected { 1 } else { 2 } } else { 0 })
                                .sum();
                            if lines <= height { break; }
                            expected += 1;
                        }
                        assert_eq!(table_offset(old, selected, height, starts_disc), expected);
                    }
                }
            }
        }
    }

    #[test]
    fn jumping_to_end_only_examines_one_screen() {
        let calls = std::cell::Cell::new(0);
        let off = table_offset(0, 100_000, 30, |_| {
            calls.set(calls.get() + 1);
            false
        });
        assert_eq!(off, 99_971);
        assert!(calls.get() <= 30);
    }
}

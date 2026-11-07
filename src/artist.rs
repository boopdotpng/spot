// Artist pages: the overview (popular songs, then the discography and more as cards), every
// song, and the discography as a grid; `v` cycles them. Cards sit in a grid the keyboard walks
// by line and by card, as many to a line as the window fits (the window says how many).

use crate::*;

/// Popular songs the overview lists.
const TOP: usize = 10;

/// A run of the cursor's stops laid out together: the popular songs (one to a line) or a
/// section of cards (`cols` to a line).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Block {
    pub section: Option<Section>,
    pub start: usize,
    pub len: usize,
    pub cols: usize,
    pub down: bool, // filled down the columns (the popular songs, in rank order), not across the lines
}

impl View {
    /// The artist's overview is open (not their songs or releases).
    pub fn overview(&self) -> Option<&ArtistInfo> {
        self.info.as_deref()
    }

    /// Which artist page this is, if it's one.
    pub fn artist_mode(&self) -> Option<ArtistMode> {
        self.artist.as_ref()?;
        Some(match (self.info.is_some(), self.kind) {
            (true, _) => ArtistMode::Overview,
            (false, Kind::Tracks) => ArtistMode::Songs,
            _ => ArtistMode::Releases,
        })
    }

    /// The artist's discography as a grid of covers.
    pub fn is_grid(&self) -> bool {
        self.artist_mode() == Some(ArtistMode::Releases)
    }

    /// What the queue's called when playing from here: popular songs aren't the artist's list.
    pub fn queue_key(&self) -> String {
        if self.info.is_some() { format!("top:{}", self.key) } else { self.key.clone() }
    }

    pub fn queue_name(&self) -> String {
        if self.info.is_some() { format!("{} · popular", self.title) } else { self.title.clone() }
    }

    /// The overview's cards in `s`: as many as its lines hold (two for the discography, one
    /// for the rest), at `cols` to a line.
    pub fn cards(&self, s: Section, cols: usize) -> Vec<&Link> {
        let Some(info) = self.overview() else { return vec![] };
        let cols = cols.max(1);
        match s {
            Section::Releases => info.releases.iter().filter(|l| self.filter.keeps(l)).take(cols * 2).collect(),
            Section::Appears => info.appears.iter().take(cols).collect(),
            Section::Related => info.related.iter().take(cols).collect(),
        }
    }

    /// How the cursor's stops are laid out: the overview's songs and sections, or the
    /// discography's one grid of `shown` cards. Empty for lists.
    pub fn blocks(&self, cols: usize, shown: usize) -> Vec<Block> {
        let cols = cols.max(1);
        if self.info.is_some() {
            let mut out = vec![Block { section: None, start: 0, len: self.tracks.len(), cols: top_cols(cols), down: true }];
            for s in [Section::Releases, Section::Appears, Section::Related] {
                let start = out.last().map_or(0, |b| b.start + b.len);
                out.push(Block { section: Some(s), start, len: self.cards(s, cols).len(), cols, down: false });
            }
            out.retain(|b| b.len > 0);
            return out;
        }
        if self.is_grid() {
            return vec![Block { section: None, start: 0, len: shown, cols, down: false }];
        }
        vec![]
    }

    /// The overview's card at stop `i`, and its section.
    pub fn card_at(&self, cols: usize, i: usize) -> Option<(Section, &Link)> {
        let b = self.blocks(cols, 0).into_iter().find(|b| i >= b.start && i < b.start + b.len)?;
        let s = b.section?;
        Some((s, *self.cards(s, cols).get(i - b.start)?))
    }

    /// List the popular songs.
    pub fn set_top(&mut self) {
        let Some(info) = &self.info else { return };
        let n = info.top.len().min(TOP);
        self.tracks = Arc::new(info.top[..n].to_vec());
    }
}

/// The popular songs' columns: two, where the pane fits four cards to a line.
pub fn top_cols(cols: usize) -> usize {
    if cols >= 4 { 2 } else { 1 }
}

/// Where the cursor goes from stop `cur`: a line up or down (on into the next section at its
/// ends, staying in its column), a card left or right. In a block filled down its columns, up
/// and down go in its order (over the top of the next column) and left and right across.
/// None: left off a line's first card.
pub fn grid_step(bs: &[Block], cur: usize, dx: isize, dy: isize) -> Option<usize> {
    let Some(bi) = bs.iter().position(|b| cur >= b.start && cur < b.start + b.len) else { return Some(0) };
    let b = bs[bi];
    let off = cur - b.start;
    let lines = b.len.div_ceil(b.cols);
    let (line, col) = if b.down { (off % lines, off / lines) } else { (off / b.cols, off % b.cols) };
    // the next section's first line, or the last of the one before, near this column
    let next = |col: usize| bs.get(bi + 1).map(|n| n.start + if n.cols > 1 && !n.down { col.min(n.len - 1) } else { 0 });
    let prev = |col: usize| {
        let p = bs[bi.checked_sub(1)?];
        let last = p.start + (p.len.div_ceil(p.cols) - 1) * p.cols;
        Some(if p.cols > 1 && !p.down { (last + col).min(p.start + p.len - 1) } else { p.start + p.len - 1 })
    };
    Some(match (b.down, dx.signum(), dy.signum()) {
        (true, _, 1) => if off + 1 < b.len { cur + 1 } else { next(0).unwrap_or(cur) },
        (true, _, -1) => if off > 0 { cur - 1 } else { prev(col).unwrap_or(cur) },
        (true, -1, _) => if col > 0 { cur - lines } else { return None },
        (true, _, _) => if col + 1 < b.cols && off + lines < b.len { cur + lines } else { cur },
        (false, _, 1) => if line + 1 < lines { b.start + (off + b.cols).min(b.len - 1) } else { next(col).unwrap_or(cur) },
        (false, _, -1) => if line > 0 { cur - b.cols } else { prev(col).unwrap_or(cur) },
        (false, -1, _) => if b.cols > 1 && col > 0 { cur - 1 } else { return None },
        (false, _, _) => if b.cols > 1 && col + 1 < b.cols && off + 1 < b.len { cur + 1 } else { cur },
    })
}

impl App {
    /// An artist's page in `mode`. The overview and the releases show from the cache at once and
    /// refresh in the background.
    pub fn artist_view(&mut self, uri: String, name: String, mode: ArtistMode) -> View {
        let (title, kind) = match mode {
            ArtistMode::Overview => (name.clone(), Kind::Tracks),
            ArtistMode::Songs => (format!("{name} · all songs"), Kind::Tracks),
            ArtistMode::Releases => (format!("{name} · {}", self.releases.label()), Kind::Albums),
        };
        let mut v = self.new_view(uri.clone(), title, kind);
        v.artist = Some(name);
        v.filter = self.releases;
        let cached = read_artist(&uri).map(Arc::new);
        v.portrait = cached.as_ref().and_then(|c| c.portrait.clone());
        let (s, tx, id) = (self.sess.subscribe(), self.tx.clone(), v.id);
        match mode {
            ArtistMode::Overview => {
                v.loading = cached.is_none();
                v.info = Some(cached.clone().unwrap_or_default());
                v.set_top();
                self.rt.spawn(artist_overview(s, tx, id, uri, cached));
            }
            ArtistMode::Songs => {
                v.tracks = Arc::new(read_tracks(&tracks_path(&uri_key(&uri))));
                v.loading = v.tracks.is_empty();
                self.rt.spawn(load_list(self.api.clone(), s, tx, id, uri, v.tracks.clone()));
            }
            ArtistMode::Releases => {
                let cached = read_links(&releases_path(&uri));
                v.loading = cached.is_empty();
                v.links = cached.iter().filter(|l| v.filter.keeps(l)).cloned().collect();
                v.releases = cached.clone();
                self.rt.spawn(artist_releases(s, tx, id, uri, cached));
            }
        }
        v
    }

    /// An artist opens on their overview.
    pub fn open_artist(&mut self, uri: String, name: String) {
        let v = self.artist_view(uri, name, ArtistMode::Overview);
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    /// Switch the open artist page to `mode`: in place (`v`), or as a page esc comes back from
    /// (the overview's "all songs" and "show all").
    pub fn open_artist_mode(&mut self, mode: ArtistMode, back: bool) {
        let Some(name) = self.view.artist.clone() else { return };
        let v = self.artist_view(self.view.key.clone(), name, mode);
        self.show(v, back);
        self.focus = Focus::Tracks;
    }

    /// `v` on an artist page: overview, every song, the discography, and around.
    pub fn toggle_artist_mode(&mut self) {
        let Some(mode) = self.view.artist_mode() else {
            self.flash("v works on artist pages");
            return;
        };
        self.open_artist_mode(mode.next(), false);
    }

    /// `f` on the overview or the discography: all releases, albums, or singles (remembered).
    pub fn cycle_releases(&mut self) {
        let mode = self.view.artist_mode();
        if !matches!(mode, Some(ArtistMode::Overview | ArtistMode::Releases)) {
            let msg = if mode.is_some() { "f works on releases (v)" } else { "f works on artist pages" };
            self.flash(msg);
            return;
        }
        self.set_releases(self.releases.next());
    }

    pub fn set_releases(&mut self, f: Releases) {
        let overview = self.view.info.is_some();
        // the cursor stays on its release, if that's still listed
        let keep = if overview { self.card_at(self.tr_state.selected().unwrap_or(0)).map(|(_, l)| l.uri) } else { self.selected_track().and_then(|i| self.view.links.get(i)).map(|l| l.uri.clone()) };
        self.releases = f;
        let v = &mut self.view;
        v.filter = f;
        if !overview {
            let name = v.artist.clone().unwrap_or_default();
            v.title = format!("{name} · {}", f.label());
            v.links = v.releases.iter().filter(|l| f.keeps(l)).cloned().collect();
        }
        self.find = None;
        self.matches.clear();
        let sel = match keep {
            Some(u) if overview => (0..self.overview_len()).find(|&i| self.card_at(i).is_some_and(|(_, l)| l.uri == u)),
            Some(u) => self.view.links.iter().position(|l| l.uri == u),
            None => None,
        };
        let sel = sel.or(self.tr_state.selected()).map(|s| s.min(self.shown().len().saturating_sub(1)));
        self.tr_state.select(sel);
    }

    pub fn blocks(&self) -> Vec<Block> {
        // only the discography's count comes from what's listed: the overview's is what shown() asks for here
        let shown = if self.view.is_grid() { self.shown().len() } else { 0 };
        self.view.blocks(self.grid_cols.get(), shown)
    }

    pub fn overview_len(&self) -> usize {
        self.blocks().last().map_or(0, |b| b.start + b.len)
    }

    pub fn card_at(&self, i: usize) -> Option<(Section, Link)> {
        self.view.card_at(self.grid_cols.get(), i).map(|(s, l)| (s, l.clone()))
    }

    /// Open the overview's card at stop `i`: a release, or another artist.
    pub fn open_card(&mut self, i: usize) {
        let Some((s, l)) = self.card_at(i) else { return };
        match s {
            Section::Related => self.open_artist(l.uri, l.name),
            Section::Releases => {
                let name = self.view.artist.clone().unwrap_or_default();
                let by = By { names: name.clone(), artist: Some((self.view.key.clone(), name)) };
                self.open_album(l.uri, l.name, by, None);
            }
            Section::Appears => {
                let names = l.sub.rsplit_once("· ").map_or(l.sub.as_str(), |(_, by)| by).to_string();
                self.open_album(l.uri, l.name, By { names, artist: None }, None);
            }
        }
    }

    /// Move the cursor on a page of cards (see `grid_step`). None if the page isn't laid out in
    /// cards; Some(false) for left off a line's first card (the sidebar's next, as on a list).
    pub fn grid_move(&mut self, dx: isize, dy: isize) -> Option<bool> {
        let bs = self.blocks();
        if bs.is_empty() {
            return None;
        }
        let to = grid_step(&bs, self.tr_state.selected().unwrap_or(0), dx, dy);
        if let Some(to) = to {
            self.tr_state.select(Some(to));
        }
        Some(to.is_some())
    }

    /// The artist page's play: every song they have, shuffled. At once from the cache (and
    /// refreshed for next time), else from their songs page once it's loaded.
    pub fn play_artist(&mut self) {
        let Some(name) = self.view.artist.clone() else { return };
        let uri = self.view.key.clone();
        if !self.shuffle {
            self.toggle_shuffle();
        }
        let cached = Arc::new(read_tracks(&tracks_path(&uri_key(&uri))));
        if cached.is_empty() {
            self.open_artist_mode(ArtistMode::Songs, true);
            self.shuffle_on_load = Some(self.view.id);
            return;
        }
        let q = Queue { key: uri.clone(), name: format!("{name} · all songs"), tracks: cached.clone(), order: vec![], pos: 0, sorted: vec![] };
        self.play_queue(q, None);
        let (api, sess, tx) = (self.api.clone(), self.sess.subscribe(), self.tx.clone());
        // no view waits on this one: it only brings the cache up to date
        self.rt.spawn(async move { fetch_list(api, sess, &uri, &cached, &tx, u64::MAX).await });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn link(i: usize) -> Link {
        Link { uri: format!("spotify:album:{i}"), name: format!("r{i}"), sub: String::new(), single: false, image: None }
    }

    fn track(i: usize) -> Track {
        parse_line(&format!("spotify:track:{i}\tt{i}\ta\t1\tal\t\t1000\t0\t0\t1\t0\t0")).unwrap()
    }

    /// An overview of 3 popular songs, 7 releases (the third a single) and 2 appearances.
    fn page() -> View {
        let mut releases: Vec<Link> = (0..7).map(link).collect();
        releases[2].single = true;
        let info = ArtistInfo { top: (0..3).map(track).collect(), releases, appears: (10..12).map(link).collect(), ..Default::default() };
        let mut v = View::new(1, "spotify:artist:x".into(), "X".into(), Kind::Tracks);
        v.artist = Some("X".into());
        v.info = Some(Arc::new(info));
        v.set_top();
        v
    }

    #[test]
    fn the_overview_lays_out_songs_then_two_lines_of_releases_then_one_of_the_rest() {
        let v = page();
        let b: Vec<(usize, usize, usize)> = v.blocks(3, 0).iter().map(|b| (b.start, b.len, b.cols)).collect();
        assert_eq!(b, [(0, 3, 1), (3, 6, 3), (9, 2, 3)]);
        assert_eq!(v.card_at(3, 4).map(|(s, l)| (s, l.name.as_str())), Some((Section::Releases, "r1")));
        assert_eq!(v.card_at(3, 10).map(|(s, l)| (s, l.name.as_str())), Some((Section::Appears, "r11")));
        assert!(v.card_at(3, 1).is_none());
    }

    #[test]
    fn the_releases_filter_picks_the_overviews_cards() {
        let mut v = page();
        v.filter = Releases::Singles;
        assert_eq!(v.cards(Section::Releases, 3).iter().map(|l| l.name.as_str()).collect::<Vec<_>>(), ["r2"]);
    }

    #[test]
    fn the_cursor_walks_the_grid_by_line_and_by_card() {
        let bs = page().blocks(3, 0);
        let mut at = 0;
        let mut go = |dx, dy| {
            at = grid_step(&bs, at, dx, dy).unwrap();
            at
        };
        assert_eq!(go(0, 1), 1);
        assert_eq!(go(0, 1), 2);
        assert_eq!(go(0, 1), 3); // off the songs onto the first card
        assert_eq!(go(1, 0), 4);
        assert_eq!(go(1, 0), 5);
        assert_eq!(go(1, 0), 5); // the line's last card
        assert_eq!(go(0, 1), 8); // a line down
        assert_eq!(go(0, 1), 10); // into the next section, as far right as it goes
        assert_eq!(go(0, 1), 10);
        assert_eq!(go(0, -1), 7); // back up, to the same column
        assert_eq!(go(0, -1), 4);
        assert_eq!(go(0, -1), 2); // up off the cards onto the last song
        assert_eq!(grid_step(&bs, 2, -1, 0), None); // left of a song: the sidebar's
        assert_eq!(grid_step(&bs, 6, -1, 0), None); // left of a line's first card
    }

    #[test]
    fn popular_songs_go_two_to_a_line_in_a_wide_pane() {
        let v = page();
        let b: Vec<(usize, usize, usize)> = v.blocks(4, 0).iter().map(|b| (b.start, b.len, b.cols)).collect();
        assert_eq!(b, [(0, 3, 2), (3, 7, 4), (10, 2, 4)]);
        // 3 songs down two columns: 0 and 1 on the left, 2 on the right
        let bs = v.blocks(4, 0);
        assert_eq!(grid_step(&bs, 0, 0, 1), Some(1));
        assert_eq!(grid_step(&bs, 1, 0, 1), Some(2)); // over the top of the next column
        assert_eq!(grid_step(&bs, 0, 1, 0), Some(2)); // across
        assert_eq!(grid_step(&bs, 1, 1, 0), Some(1)); // nothing across
        assert_eq!(grid_step(&bs, 2, -1, 0), Some(0));
        assert_eq!(grid_step(&bs, 1, -1, 0), None); // the sidebar's
        assert_eq!(grid_step(&bs, 2, 0, 1), Some(3)); // the last song, on to the first card
        assert_eq!(grid_step(&bs, 3, 0, -1), Some(2)); // and back
    }

    #[test]
    fn a_short_last_line_is_reached_from_any_column() {
        let bs = [Block { section: None, start: 0, len: 5, cols: 3, down: false }];
        assert_eq!(grid_step(&bs, 2, 0, 1), Some(4));
        assert_eq!(grid_step(&bs, 4, 0, -1), Some(1));
    }
}

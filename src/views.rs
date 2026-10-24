// What the right pane shows: opening lists, albums and artists, going back, the cursor and ctrl+f.

use crate::*;

impl App {
    pub fn new_view(&mut self, key: String, title: String, kind: Kind) -> View {
        self.view_seq += 1;
        View::new(self.view_seq, key, title, kind)
    }

    /// Put `v` in the right pane. `back` keeps the current view for esc to return to.
    pub fn show(&mut self, v: View, back: bool) {
        let sel = self.selected_track().unwrap_or(0);
        self.find = None;
        let old = std::mem::replace(&mut self.view, v);
        if back && !old.key.is_empty() {
            self.history.push((old, sel));
            if self.history.len() > 50 {
                self.history.remove(0);
            }
        }
        self.tr_state = TableState::default().with_selected(0);
    }

    pub fn back(&mut self) {
        let Some((v, sel)) = self.history.pop() else { return };
        self.view = v;
        self.find = None;
        self.tr_state = TableState::default().with_selected(sel);
        self.focus = Focus::Tracks;
    }

    /// The view a message is for: the open one, or one waiting in the history (still loading).
    pub fn view_mut(&mut self, id: u64) -> Option<&mut View> {
        if self.view.id == id {
            return Some(&mut self.view);
        }
        self.history.iter_mut().map(|(v, _)| v).find(|v| v.id == id)
    }

    pub fn session(&self) -> Option<Session> {
        self.sess.borrow().clone()
    }

    /// Opens instantly from the disk cache (or the playing queue), then refreshes in the background.
    pub fn open_playlist(&mut self, i: usize) {
        let Some(pl) = self.playlists.get(i).cloned() else { return };
        let key = pl.key();
        let mut v = self.new_view(key.clone(), pl.name.clone(), Kind::Tracks);
        v.tracks = match &self.queue {
            Some(q) if q.key == key => q.tracks.clone(),
            _ => Arc::new(read_tracks(&tracks_path(&key))),
        };
        v.loading = v.tracks.is_empty();
        let cached = v.tracks.clone();
        self.rt.spawn(load_tracks(self.api.clone(), self.sess.subscribe(), self.tx.clone(), v.id, pl, cached));
        self.show(v, true);
    }

    pub fn open_search(&mut self, q: String) {
        let kind = self.search_kind;
        let title = format!("Search {}: {q}", kind.label());
        let v = self.new_view(format!("search:{}:{q}", kind.label()), title, kind);
        self.rt.spawn(search(self.api.clone(), self.tx.clone(), v.id, q, kind));
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    pub fn open_album(&mut self, uri: String, title: String, want: Option<String>) {
        let mut v = self.new_view(uri.clone(), title, Kind::Tracks);
        v.want = want;
        v.tracks = Arc::new(read_tracks(&tracks_path(&uri_key(&uri))));
        v.loading = v.tracks.is_empty();
        let cached = v.tracks.clone();
        self.rt.spawn(link_tracks(self.sess.subscribe(), self.tx.clone(), v.id, uri, cached));
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    /// Artist page in the current mode: every song (newest release first), or albums & singles.
    pub fn artist_view(&mut self, uri: String, name: String) -> View {
        let (title, kind) = if self.artist_songs {
            (format!("{name} · all songs"), Kind::Tracks)
        } else {
            (format!("{name} · {}", self.releases.label()), Kind::Albums)
        };
        let mut v = self.new_view(uri.clone(), title, kind);
        v.artist = Some(name);
        v.filter = self.releases;
        let (s, tx, id) = (self.sess.subscribe(), self.tx.clone(), v.id);
        if self.artist_songs {
            v.tracks = Arc::new(read_tracks(&tracks_path(&uri_key(&uri))));
            v.loading = v.tracks.is_empty();
            self.rt.spawn(link_tracks(s, tx, id, uri, v.tracks.clone()));
        } else {
            let cached = read_links(&releases_path(&uri));
            v.loading = cached.is_empty();
            v.links = cached.iter().filter(|l| v.filter.keeps(l)).cloned().collect();
            v.releases = cached.clone();
            self.rt.spawn(artist_releases(s, tx, id, uri, cached));
        }
        v
    }

    pub fn open_artist(&mut self, uri: String, name: String) {
        let v = self.artist_view(uri, name);
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    /// `v` on an artist page: switch between all songs and albums & singles (remembered).
    pub fn toggle_artist_mode(&mut self) {
        let Some(name) = self.view.artist.clone() else {
            self.note = Some(("v works on artist pages".into(), Instant::now() + Duration::from_millis(1500)));
            return;
        };
        self.artist_songs = !self.artist_songs;
        let v = self.artist_view(self.view.key.clone(), name);
        self.show(v, false);
    }

    /// `f` on an artist's releases: cycle all / albums / singles (remembered).
    pub fn cycle_releases(&mut self) {
        let Some(name) = self.view.artist.clone().filter(|_| self.view.kind == Kind::Albums) else {
            let msg = if self.view.artist.is_some() { "f works on releases (v)" } else { "f works on artist pages" };
            self.note = Some((msg.into(), Instant::now() + Duration::from_millis(1500)));
            return;
        };
        let keep = self.selected_track().and_then(|i| self.view.links.get(i)).map(|l| l.uri.clone());
        self.releases = self.releases.next();
        let v = &mut self.view;
        v.filter = self.releases;
        v.title = format!("{name} · {}", v.filter.label());
        v.links = v.releases.iter().filter(|l| v.filter.keeps(l)).cloned().collect();
        let sel = keep.and_then(|u| v.links.iter().position(|l| l.uri == u)).unwrap_or(0);
        self.find = None;
        self.matches.clear();
        self.tr_state = TableState::default().with_selected(sel);
    }

    /// Open an album or artist from a search result or an artist's releases.
    pub fn open_link(&mut self, i: usize) {
        let Some(l) = self.view.links.get(i).cloned() else { return };
        if self.view.kind == Kind::Artists {
            return self.open_artist(l.uri, l.name);
        }
        let by = self.view.artist.as_ref().unwrap_or(&l.sub);
        self.open_album(l.uri.clone(), format!("{} — {by}", l.name), None);
    }

    /// What goto album/artist acts on: the highlighted song, else the one playing.
    pub fn target_track(&self) -> Option<Track> {
        if self.focus == Focus::Tracks
            && self.view.kind == Kind::Tracks
            && let Some(t) = self.selected_track().and_then(|i| self.view.tracks.get(i))
        {
            return Some(t.clone());
        }
        self.now().cloned()
    }

    pub fn goto_album(&mut self) {
        let Some(t) = self.target_track() else { return };
        let Some(uri) = id_uri("album", t.album_id) else { return };
        self.open_album(uri, format!("{} — {}", t.album, t.artists), Some(t.uri));
    }

    pub fn goto_artist(&mut self) {
        let Some(t) = self.target_track() else { return };
        let Some(uri) = id_uri("artist", t.artist_id) else { return };
        self.open_artist(uri, t.artists[..t.lead].to_string());
    }

    /// Enter on a row: play a track (queueing the whole list) or open an album/artist.
    pub fn activate(&mut self, i: usize) {
        if self.view.kind == Kind::Tracks { self.play_from_view(Some(i)) } else { self.open_link(i) }
    }
}

impl App {
    /// Rows currently listed (all of them unless filtering).
    pub fn shown(&self) -> Shown<'_> {
        match self.find.as_deref() {
            Some(q) if !q.is_empty() => Shown::Matches(&self.matches),
            _ => Shown::All(self.view.len()),
        }
    }

    /// Recompute the ctrl+f matches; call when the query or the open list changes.
    pub fn refilter(&mut self) {
        self.matches.clear();
        let Some(q) = self.find.as_deref().filter(|q| !q.is_empty()) else { return };
        let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
        let view = &self.view;
        self.matches.extend((0..view.len()).filter(|&i| {
            let hay = view.haystack(i);
            words.iter().all(|w| hay.contains(w.as_str()))
        }));
    }

    /// Original track index under the cursor.
    pub fn selected_track(&self) -> Option<usize> {
        self.shown().get(self.tr_state.selected()?)
    }

    pub fn close_find(&mut self) {
        let orig = self.selected_track();
        self.find = None;
        self.tr_state.select(orig.or(Some(0)));
    }
}

impl App {
    pub fn move_sel(&mut self, d: isize) {
        let (len, cur) = match self.focus {
            Focus::Playlists => (self.playlists.len(), self.pl_state.selected()),
            Focus::Tracks => (self.shown().len(), self.tr_state.selected()),
        };
        if len == 0 {
            return;
        }
        let to = (cur.unwrap_or(0) as isize).saturating_add(d);
        // single steps wrap around the ends; bigger jumps (page, top/bottom) stop at them
        let n = if d.abs() == 1 { to.rem_euclid(len as isize) } else { to.clamp(0, len as isize - 1) } as usize;
        match self.focus {
            Focus::Playlists => self.pl_state.select(Some(n)),
            Focus::Tracks => self.tr_state.select(Some(n)),
        }
    }

    pub fn move_to_middle(&mut self) {
        let (len, cur) = match self.focus {
            Focus::Playlists => (self.playlists.len(), self.pl_state.selected()),
            Focus::Tracks => (self.shown().len(), self.tr_state.selected()),
        };
        self.move_sel((len / 2) as isize - cur.unwrap_or(0) as isize);
    }
}

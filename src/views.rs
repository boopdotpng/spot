// What the right pane shows: opening lists, albums and artists, going back, the cursor and ctrl+f.

use crate::*;

/// How long typing has to stop for before searching; the launcher's wait.
const SEARCH_SETTLE: Duration = Duration::from_millis(140);

fn search_key(kind: Option<Kind>, q: &str) -> String {
    format!("search:{}:{q}", search_label(kind))
}

/// The songs, albums and artists a search of everything shows: the top result, five songs, and
/// a line each of artists and albums (as many as Spotify gives one request).
fn found(q: &str, v: &Value) -> (Option<Hit>, ArtistInfo) {
    let m = ctl::mixed(q, v);
    let hit = m.top.and_then(|(kind, i)| match kind {
        "track" => parse_track(i).map(|t| Hit { link: song_link(&t), song: Some(t) }),
        _ => parse_link(kind, i).map(|link| Hit { link, song: None }),
    });
    let info = ArtistInfo {
        top: m.tracks.into_iter().filter_map(parse_track).take(5).collect(),
        releases: m.albums.into_iter().filter_map(|i| parse_link("album", i)).collect(),
        related: m.artists.into_iter().filter_map(|i| parse_link("artist", i)).collect(),
        ..Default::default()
    };
    (hit, info)
}

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
            // going somewhere new leaves nothing to go forward to, as in a browser
            self.ahead.clear();
        }
        self.refilter();
        self.tr_state = Sel::with(0);
    }

    pub fn back(&mut self) {
        let Some((v, sel)) = self.history.pop() else { return };
        let here = self.selected_track().unwrap_or(0);
        let old = std::mem::replace(&mut self.view, v);
        self.ahead.push((old, here));
        self.returned_to(sel);
    }

    /// Forward again, to the view `back` left (⌘], or a swipe to the left).
    pub fn forward(&mut self) {
        let Some((v, sel)) = self.ahead.pop() else { return };
        let here = self.selected_track().unwrap_or(0);
        let old = std::mem::replace(&mut self.view, v);
        if !old.key.is_empty() {
            self.history.push((old, here));
        }
        self.returned_to(sel);
    }

    /// A view from the history is open again: its cursor back on the song it was on.
    fn returned_to(&mut self, sel: usize) {
        self.find = None;
        self.refilter();
        self.tr_state = Sel::with(self.row_of(sel).unwrap_or(0));
        self.focus = Focus::Tracks;
    }

    /// The view a message is for: the open one, or one waiting in the history (still loading).
    pub fn view_mut(&mut self, id: u64) -> Option<&mut View> {
        if self.view.id == id {
            return Some(&mut self.view);
        }
        self.history.iter_mut().chain(self.ahead.iter_mut()).map(|(v, _)| v).find(|v| v.id == id)
    }

    pub fn session(&self) -> Option<Session> {
        self.sess.borrow().clone()
    }

    /// The list with view key `key` as last seen: the playing queue's copy, else the disk cache's.
    fn cached_list(&self, key: &str) -> Arc<Vec<Track>> {
        match &self.queue {
            Some(q) if q.key == key => q.tracks.clone(),
            _ => Arc::new(read_tracks(&tracks_path(&list_key(key)))),
        }
    }

    /// Show `v` with `cached` at once, refreshed from `uri` in the background.
    fn show_list(&mut self, mut v: View, uri: String, cached: Arc<Vec<Track>>) {
        v.tracks = cached.clone();
        v.loading = cached.is_empty();
        if uri.starts_with("spotify:album:") {
            v.about = read_about(&uri);
        }
        self.rt.spawn(load_list(self.api.clone(), self.sess.subscribe(), self.tx.clone(), v.id, uri, cached));
        self.show(v, true);
    }

    /// Opens instantly from the disk cache (or the playing queue), then refreshes in the background.
    pub fn open_playlist(&mut self, i: usize) {
        let Some(pl) = self.playlists.get(i).cloned() else { return };
        let cached = self.cached_list(&pl.key());
        let v = self.new_view(pl.key(), pl.name.clone(), Kind::Tracks);
        self.show_list(v, pl.uri(), cached);
    }

    /// A show's episodes, newest first; `want` is an episode to land on.
    pub fn open_show(&mut self, uri: String, name: String, want: Option<String>) {
        let mut v = self.new_view(uri.clone(), name, Kind::Tracks);
        v.want = want;
        let cached = self.cached_list(&uri);
        self.show_list(v, uri, cached);
        self.focus = Focus::Tracks;
    }

    /// Enter on the podcast sidebar: a pinned list, or a followed show.
    pub fn open_podcast_row(&mut self, l: Link) {
        match l.uri.as_str() {
            CONTINUE_KEY => self.open_continuing(l.name),
            NEW_KEY => self.open_new_episodes(l.name),
            _ => self.open_show(l.uri, l.name, None),
        }
    }

    /// Show songs already at hand (nothing to fetch), with the keyboard on them.
    pub fn show_ready(&mut self, key: String, title: String, tracks: Arc<Vec<Track>>, back: bool) {
        let mut v = self.new_view(key, title, Kind::Tracks);
        v.tracks = tracks;
        v.loading = false;
        self.show(v, back);
        self.focus = Focus::Tracks;
    }

    /// `Continue listening`: episodes part way through, last played first.
    fn open_continuing(&mut self, title: String) {
        self.show_ready(CONTINUE_KEY.into(), title, Arc::new(self.continuing.clone()), true);
    }

    /// `New episodes`: instantly from the cache, less what's been heard since, then refreshed.
    fn open_new_episodes(&mut self, title: String) {
        let mut v = self.new_view(NEW_KEY.into(), title, Kind::Tracks);
        let heard: HashSet<String> = self.progress.iter().filter(|(_, ms)| **ms == HEARD).map(|(u, _)| u.clone()).collect();
        let mut cached = read_tracks(&tracks_path("new_episodes"));
        cached.retain(|t| !heard.contains(&t.uri));
        v.tracks = Arc::new(cached);
        v.loading = v.tracks.is_empty();
        let shows: Vec<Link> = self.shows.iter().filter(|s| is_show(&s.uri)).cloned().collect();
        self.rt.spawn(load_new_episodes(self.api.clone(), shows, heard, self.tx.clone(), v.id, v.tracks.clone()));
        self.show(v, true);
        self.focus = Focus::Tracks;
    }

    /// `m`: the sidebar lists podcasts instead of playlists, or back.
    pub fn toggle_podcasts(&mut self) {
        self.podcasts = !self.podcasts;
        self.focus = Focus::Playlists;
    }

    /// Search for what's in the prompt, unless those are already the results on show.
    pub fn run_search(&mut self) {
        let Some(q) = self.input.as_deref().map(str::trim).map(String::from) else {
            // a chip on a search's results: the same words, for something else, in its place
            if let Some(q) = self.view.search_query().map(String::from)
                && self.view.key != search_key(self.search_kind, &q)
            {
                self.open_search(q, false);
            }
            return;
        };
        if q.is_empty() {
            return self.end_live_search();
        }
        if !(self.live_search && self.view.key == search_key(self.search_kind, &q)) {
            self.open_search(q, !self.live_search);
            self.live_search = true;
        }
    }

    /// Search once typing has stopped for a moment.
    pub fn search_soon(&mut self) {
        self.search_at = Some(Instant::now() + SEARCH_SETTLE);
    }

    /// Back to what was open before the prompt's results.
    pub fn end_live_search(&mut self) {
        if let Some(task) = self.search_task.take() {
            task.abort();
        }
        if std::mem::take(&mut self.live_search) {
            self.back();
        }
    }

    /// `back` keeps the current view for esc; otherwise the results replace it. A search still
    /// paging is stopped: each page is a request, and nobody's waiting for those results now.
    pub fn open_search(&mut self, q: String, back: bool) {
        if let Some(task) = self.search_task.take() {
            task.abort();
        }
        let Some(kind) = self.search_kind else {
            let mut v = self.new_view(search_key(None, &q), format!("Search: {q}"), Kind::Tracks);
            v.info = Some(Arc::default());
            let (api, tx, view) = (self.api.clone(), self.tx.clone(), v.id);
            let task = self.rt.spawn(async move {
                let found = match search_all(&api, &q, 10).await {
                    Ok(v) => found(&q, &v),
                    Err(e) => {
                        let _ = tx.send(Msg::Error(e));
                        (None, ArtistInfo::default())
                    }
                };
                let _ = tx.send(Msg::Found { view, found: Box::new(found) });
            });
            self.search_task = Some(task.abort_handle());
            self.show(v, back);
            self.focus = Focus::Tracks;
            return;
        };
        let title = format!("Search {}: {q}", kind.label());
        let v = self.new_view(search_key(Some(kind), &q), title, kind);
        let (api, tx, view) = (self.api.clone(), self.tx.clone(), v.id);
        let task = self.rt.spawn(async move {
            let res = search(&api, &q, kind.ty(), 50, |items, done| {
                let _ = tx.send(match kind {
                    Kind::Tracks => Msg::Tracks { view, tracks: items.iter().filter_map(parse_track).collect(), done },
                    _ => Msg::Links { view, links: items.iter().filter_map(|i| parse_link(kind.ty(), i)).collect(), done },
                });
            })
            .await;
            if let Err(e) = res {
                let _ = tx.send(Msg::Error(e));
                let _ = tx.send(Msg::Tracks { view, tracks: vec![], done: true });
            }
        });
        self.search_task = Some(task.abort_handle());
        self.show(v, back);
        self.focus = Focus::Tracks;
    }

    /// An album, titled by its name; `by` shows under it (the artist goes to their page).
    pub fn open_album(&mut self, uri: String, name: String, mut by: By, want: Option<String>) {
        let mut v = self.new_view(uri.clone(), name, Kind::Tracks);
        v.want = want;
        let cached = Arc::new(read_tracks(&tracks_path(&uri_key(&uri))));
        by.find_artist(&cached);
        v.by = Some(by);
        self.show_list(v, uri, cached);
        self.focus = Focus::Tracks;
    }

    /// Open an album or artist from a search result or an artist's releases.
    pub fn open_link(&mut self, i: usize) {
        let Some(l) = self.view.links.get(i).cloned() else { return };
        match self.view.kind {
            Kind::Artists => return self.open_artist(l.uri, l.name),
            Kind::Shows => return self.open_show(l.uri, l.name, None),
            _ => {}
        }
        // an artist's releases are theirs; a search result's line starts with whose it is
        let by = match &self.view.artist {
            Some(name) => By::one(self.view.key.clone(), name.clone()),
            None => By { names: l.sub.split(" · ").next().unwrap_or_default().to_string(), artists: Vec::new() },
        };
        self.open_album(l.uri.clone(), l.name.clone(), by, None);
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
        if t.is_episode() {
            return self.goto_show(t);
        }
        let Some(uri) = id_uri("album", t.album_id) else { return };
        self.open_album(uri, t.album.clone(), By::of(&t), Some(t.uri));
    }

    pub fn goto_artist(&mut self) {
        let Some(t) = self.target_track() else { return };
        if t.is_episode() {
            return self.goto_show(t);
        }
        let Some(uri) = id_uri("artist", t.artist_id) else { return };
        self.open_artist(uri, t.artists[..t.lead].to_string());
    }

    fn goto_show(&mut self, t: Track) {
        let Some(uri) = id_uri("show", t.album_id) else { return };
        self.open_show(uri, t.artists, Some(t.uri));
    }

    /// Enter on a row: play a track (queueing the whole list) or open an album/artist.
    pub fn activate(&mut self, i: usize) {
        if self.view.info.is_some() && i >= self.view.tracks.len() {
            return self.open_card(i);
        }
        if self.view.kind == Kind::Tracks { self.play_from_view(Some(i)) } else { self.open_link(i) }
    }
}

impl App {
    /// Rows currently listed (all of them unless filtering).
    pub fn shown(&self) -> Shown<'_> {
        match self.find.as_deref() {
            Some(q) if !q.is_empty() => Shown::Matches(&self.matches),
            _ if !self.view.order.is_empty() => Shown::Matches(&self.view.order),
            _ if self.view.info.is_some() => Shown::All(self.overview_len()),
            _ => Shown::All(self.view.len()),
        }
    }

    /// The row a view index is listed at, if it's listed.
    pub fn row_of(&self, i: usize) -> Option<usize> {
        match self.shown() {
            Shown::All(n) => (i < n).then_some(i),
            Shown::Matches(m) => m.iter().position(|&j| j == i),
        }
    }

    /// The open list is a playlist (or Liked Songs): its songs have dates added, and it sorts.
    pub fn dated(&self) -> bool {
        let v = &self.view;
        v.kind == Kind::Tracks && (v.key == "liked" || self.playlists.iter().any(|p| p.id.as_deref() == Some(v.key.as_str())))
    }

    /// Recompute the sorted order and the ctrl+f matches; call when the query, the sort or the
    /// open list changes.
    pub fn refilter(&mut self) {
        self.view.order = if self.dated() { self.sort.order(&self.view.tracks) } else { vec![] };
        self.matches.clear();
        let Some(q) = self.find.as_deref().filter(|q| !q.is_empty()) else { return };
        let words: Vec<String> = q.to_lowercase().split_whitespace().map(String::from).collect();
        let view = &self.view;
        let rows: Box<dyn Iterator<Item = usize>> = if view.order.is_empty() { Box::new(0..view.len()) } else { Box::new(view.order.iter().copied()) };
        self.matches.extend(rows.filter(|&i| {
            let hay = view.haystack(i);
            words.iter().all(|w| hay.contains(w.as_str()))
        }));
    }

    /// Sort playlists as listed, newest added first, or oldest first; the cursor stays on its song.
    pub fn set_sort(&mut self, s: Sort) {
        let cur = self.selected_track();
        self.sort = s;
        let _ = fs::write(cache_dir().join("sort"), s.name());
        self.refilter();
        self.tr_state.select(cur.and_then(|i| self.row_of(i)).or(Some(0)));
        let what = match s {
            Sort::List => "as listed",
            Sort::Newest => "newest added first",
            Sort::Oldest => "oldest added first",
        };
        self.flash(format!("sorted {what}"));
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
    /// How many rows the focused pane lists, and the cursor's.
    fn focused_rows(&self) -> (usize, Option<usize>) {
        match self.focus {
            Focus::Playlists if self.podcasts => (self.shows.len(), self.sh_state.selected()),
            Focus::Playlists => (self.playlists.len(), self.pl_state.selected()),
            Focus::Tracks => (self.shown().len(), self.tr_state.selected()),
        }
    }

    pub fn move_sel(&mut self, d: isize) {
        let (len, cur) = self.focused_rows();
        if len == 0 {
            return;
        }
        let to = (cur.unwrap_or(0) as isize).saturating_add(d);
        // single steps wrap around the ends; bigger jumps (page, top/bottom) stop at them
        let n = if d.abs() == 1 { to.rem_euclid(len as isize) } else { to.clamp(0, len as isize - 1) } as usize;
        match self.focus {
            Focus::Playlists if self.podcasts => self.sh_state.select(Some(n)),
            Focus::Playlists => self.pl_state.select(Some(n)),
            Focus::Tracks => self.tr_state.select(Some(n)),
        }
    }

    pub fn move_to_middle(&mut self) {
        let (len, cur) = self.focused_rows();
        self.move_sel((len / 2) as isize - cur.unwrap_or(0) as isize);
    }
}

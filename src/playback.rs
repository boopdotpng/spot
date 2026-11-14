// Playing: the list and the queue, skipping, pause / resume fades, seeking, volume, and the
// player's events.

use crate::*;

/// What to remember of an episode of `ms` left at `pos`: nothing just after its start, HEARD
/// in its last minute (outros and credits, which nobody needs to come back for).
fn progress_after(pos: u64, ms: u64) -> Option<u64> {
    match pos {
        0..5000 => None,
        p if p + 60_000 >= ms => Some(HEARD),
        p => Some(p),
    }
}

impl App {
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if let Some(q) = &mut self.queue {
            let cur = q.idx();
            q.reorder(self.shuffle, Some(cur));
        }
        self.queue_changed();
    }

    /// Reconnect a session that died (sleep, network change). Still connecting: nothing to do.
    pub fn ensure_session(&mut self) {
        if !self.session().is_some_and(|s| s.is_invalid()) {
            return;
        }
        log::info!("session is invalid, reconnecting");
        self.status = "reconnecting…".into();
        match self.rt.block_on(connect(&self.cache, None)) {
            Ok(s) => {
                log::info!("reconnected");
                self.player.set_session(s.clone());
                self.sess.send_replace(Some(s));
                self.status.clear();
            }
            Err(e) => {
                log::error!("reconnect failed: {e}");
                self.status = e;
            }
        }
    }

    pub fn on_connected(&mut self, s: Session) {
        log::info!("audio session connected");
        self.player.set_session(s.clone());
        self.sess.send_replace(Some(s));
        if self.status.starts_with("approve the audio session") {
            self.status.clear();
        }
        // a song picked while connecting starts now. The one restored from last time waits for
        // play: loading it asks Spotify for its audio key, and those are rate-limited
        if self.want_play {
            self.load_cur();
        }
    }

    /// Load `cur` into the player at the current position, playing if that's wanted.
    pub fn load_cur(&mut self) {
        if self.loaded || self.session().is_none() || self.load_at.is_some_and(|t| Instant::now() < t) {
            return;
        }
        self.load_at = None;
        let Some(uri) = self.cur.clone() else { return };
        log::debug!("load {uri:?} play {} at {}ms", self.want_play, self.pos_ms);
        self.player.load(uri, self.want_play, self.pos_ms as u32);
        self.loaded = true;
    }

    pub fn set_cur(&mut self, t: &Track, pos_ms: u64) {
        let Ok(uri) = SpotifyUri::from_uri(&t.uri) else { return };
        self.end_listen();
        self.episode = t.is_episode().then(|| t.clone());
        // podcasts don't scrobble
        if self.lastfm.is_some() && !t.is_episode() {
            self.listen = Some(Listen { track: t.clone(), started: 0, listened: Duration::ZERO, since: None, announced: false });
        }
        if let Some(m) = &mut self.media {
            m.set_metadata(Metadata {
                title: &t.name,
                artist: &t.artists,
                album: &t.album,
                cover_url: t.cover.as_deref(),
                duration: Duration::from_millis(t.ms),
            });
        }
        self.cur = Some(uri);
        self.loaded = false;
        self.pos_ms = pos_ms;
        self.pos_at = None;
        self.plays += 1;
        self.heard = false;
        self.finished = false;
    }

    /// Remember where the playing episode has got to.
    pub fn save_progress(&mut self) {
        let Some(t) = self.episode.clone() else { return };
        let Some(at) = progress_after(self.position(), t.ms) else { return };
        if self.progress.insert(t.uri.clone(), at) == Some(at) {
            return;
        }
        save_progress(&self.progress);
        self.continuing.retain(|c| c.uri != t.uri);
        if at != HEARD {
            self.continuing.insert(0, t);
        }
        save_tracks(&continuing_path(), &self.continuing);
    }

    /// Scrobble the play that just ended, if it counts.
    pub fn end_listen(&mut self) {
        if let Some(l) = self.listen.take()
            && l.counts()
            && let Some(fm) = &self.lastfm
        {
            fm.queue(&l);
            let fm = fm.clone();
            self.rt.spawn(async move { fm.drain().await });
        }
    }

    pub fn pause_listen(&mut self) {
        if let Some(l) = &mut self.listen
            && let Some(s) = l.since.take()
        {
            l.listened += s.elapsed();
        }
    }

    /// The song playing (or paused, or about to load): a queued one, else the list's.
    pub fn now(&self) -> Option<&Track> {
        self.queued.as_ref().or_else(|| self.queue.as_ref().and_then(|q| q.tracks.get(q.idx())))
    }

    pub fn play_current(&mut self) {
        let Some(t) = self.now().cloned() else { return };
        log::info!("play {} ({} - {})", t.uri, t.name, t.artists);
        self.ensure_session();
        self.reset_fade();
        self.save_progress();
        // an episode picks up where it was left; one heard to the end starts over
        let at = self.progress.get(&t.uri).copied().filter(|_| t.is_episode()).filter(|&ms| ms != HEARD).unwrap_or(0);
        self.set_cur(&t, at);
        self.load_cur();
        self.queue_changed();
    }

    /// Put last session's queue back, paused on the song that was playing.
    pub fn restore(&mut self, last: Last) {
        self.shuffle = last.shuffle;
        self.repeat = last.repeat;
        let t = last.queue.tracks[last.queue.idx()].clone();
        // the list it carries on with is recently played too (new installs start with it)
        if let Some(l) = self.recent_link(&last.queue).filter(|l| !self.recent.iter().any(|r| r.uri == l.uri)) {
            self.remember(l);
        }
        self.queue = Some(last.queue);
        self.set_cur(&t, last.pos_ms);
        self.sync_media();
    }

    pub fn save_last(&mut self) {
        self.save_progress();
        match &self.queue {
            Some(q) => save_last(q, self.position(), self.shuffle, self.repeat),
            None => {
                let _ = fs::remove_file(last_path());
            }
        }
        // the queue: first line says whether its first song is the one playing
        let path = cache_dir().join("upnext");
        if self.queued.is_none() && self.upnext.is_empty() {
            let _ = fs::remove_file(path);
        } else {
            let mut s = format!("{}\n", self.queued.is_some() as u8);
            push_tracks(&mut s, self.queued.iter().chain(&self.upnext));
            write_atomic(&path, &s);
        }
    }

    /// Put last session's queue back; a queued song that was playing resumes instead of the list's.
    pub fn restore_upnext(&mut self) {
        let Ok(s) = fs::read_to_string(cache_dir().join("upnext")) else { return };
        let mut lines = s.lines();
        let playing = lines.next() == Some("1");
        self.upnext = lines.filter_map(parse_line).collect();
        if playing && !self.upnext.is_empty() {
            let t = self.upnext.remove(0);
            self.set_cur(&t, if self.queue.is_some() { self.pos_ms } else { 0 });
            self.queued = Some(t);
            self.sync_media();
        }
    }

    pub fn sync_media(&mut self) {
        let at = Duration::from_millis(self.position());
        let state = match (&self.cur, self.playing) {
            (None, _) => Playback::Stopped,
            (_, true) => Playback::Playing(at),
            (_, false) => Playback::Paused(at),
        };
        if let Some(m) = &mut self.media {
            m.set_playback(state);
        }
    }

    pub fn toggle(&mut self) {
        if self.want_play {
            self.want_play = false;
            if self.loaded {
                self.fade(false);
            }
        } else if self.cur.is_some() {
            self.ensure_session();
            if self.loaded {
                self.want_play = true;
                self.fade(true);
            } else {
                // still connecting: on_connected starts it
                self.reset_fade();
                self.load_cur();
            }
        }
    }

    /// Ramp volume down then pause (250ms), or resume and ramp back up (400ms).
    pub fn fade(&self, up: bool) {
        use std::sync::atomic::Ordering::SeqCst;
        let id = self.fade_gen.fetch_add(1, SeqCst) + 1;
        let (fade_gen, mixer, player, vol) = (self.fade_gen.clone(), self.mixer.clone(), self.player.clone(), self.vol);
        if up {
            player.play();
        }
        self.rt.spawn(async move {
            let (from, to, ms) = (mixer.volume() as f64, if up { vol as f64 } else { 0.0 }, if up { 400 } else { 250 });
            const STEPS: u64 = 25;
            for i in 1..=STEPS {
                tokio::time::sleep(Duration::from_millis(ms / STEPS)).await;
                if fade_gen.load(SeqCst) != id {
                    return;
                }
                // the soft mixer's curve is logarithmic, so a linear ramp here sounds even
                mixer.set_volume((from + (to - from) * i as f64 / STEPS as f64) as u16);
            }
            if !up {
                player.pause();
            }
        });
    }

    /// Cancel any fade and put the mixer back at the user's volume (new track starting).
    pub fn reset_fade(&mut self) {
        self.fade_gen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.mixer.set_volume(self.vol);
        self.want_play = true;
    }

    pub fn seek_to(&mut self, ms: u64) {
        if self.loaded {
            self.player.seek(ms as u32);
            // say so now, not on librespot's Seeked: a client asking right after a seek (the
            // launcher, after ⌘←) would get the old position
            self.pos_at = self.playing.then(Instant::now);
        }
        self.pos_ms = ms;
    }

    pub fn on_media(&mut self, ev: MediaEvent) {
        use MediaEvent as E;
        self.ctl.skipped |= matches!(ev, E::Next | E::Previous);
        match ev {
            E::Toggle => self.toggle(),
            E::Play if !self.want_play => self.toggle(),
            E::Pause if self.want_play => self.toggle(),
            E::Next => self.skip_soon(1),
            E::Previous => self.prev(),
            E::SetPosition(p) => self.seek_to(p.as_millis() as u64),
            _ => {}
        }
    }

    /// Ahead `ms` (back when -).
    pub fn seek_by(&mut self, ms: i64) {
        self.seek_to(self.position().saturating_add_signed(ms));
    }

    pub fn prev(&mut self) {
        if self.position() > 3000 {
            self.seek_to(0);
        } else {
            self.skip_soon(-1);
        }
    }

    /// Manual next/prev: show the new track right away (and silence the old one), but only load
    /// it once skipping settles; see SKIP_SETTLE.
    pub fn skip_soon(&mut self, d: isize) {
        self.player.stop();
        self.load_at = Some(Instant::now() + SKIP_SETTLE);
        self.skip(d);
    }

    /// Queue the whole open list. `start` = chosen track; None = from the top (or a random track if shuffling).
    /// Shuffle the open list and play it: now if it's here, or once it's loaded.
    pub fn shuffle_play_view(&mut self) {
        if !self.shuffle {
            self.toggle_shuffle();
        }
        if self.view.loading || self.view.tracks.is_empty() {
            self.shuffle_on_load = Some(self.view.id);
        } else {
            self.play_from_view(None);
        }
    }

    pub fn play_from_view(&mut self, start: Option<usize>) {
        if self.view.kind != Kind::Tracks || self.view.tracks.is_empty() {
            return;
        }
        if self.view.key == QUEUE_KEY {
            let i = start.unwrap_or(0);
            return match i.checked_sub(self.upnext.len()) {
                None => self.play_queued(i),
                Some(after) => self.play_upcoming(after),
            };
        }
        let q = Queue {
            key: self.view.queue_key(),
            name: self.view.queue_name(),
            tracks: self.view.tracks.clone(),
            order: vec![],
            pos: 0,
            sorted: self.view.order.clone(),
        };
        self.play_queue(q, start);
    }

    /// Make `q` the queue and play it from `start` (None: the top, or a random song if shuffling).
    pub fn play_queue(&mut self, mut q: Queue, start: Option<usize>) {
        if let Some(l) = self.recent_link(&q) {
            self.remember(l);
        }
        q.reorder(self.shuffle, start);
        self.queue = Some(q);
        self.queued = None;
        self.play_fresh();
    }

    /// What playing `q` adds to the recently played: the list it is, or the song when it's one
    /// song. Searches, the queue and the podcast lists aren't kept.
    fn recent_link(&self, q: &Queue) -> Option<Link> {
        let first = q.tracks.first();
        let link = |uri: String, name: String, sub: String, image: Option<String>| Some(Link { uri, name, sub, single: false, image });
        let key = q.key.as_str();
        if key == "liked" {
            return link("liked".into(), "Liked Songs".into(), "Playlist".into(), None);
        }
        let artist = key.strip_prefix("top:").unwrap_or(key);
        if artist.starts_with("spotify:artist:") {
            let name = first.map(|t| t.artists[..t.lead].to_string()).filter(|n| !n.is_empty() && q.tracks.iter().all(|t| t.artists.starts_with(n.as_str())));
            let name = name.unwrap_or_else(|| q.name.split(" · ").next().unwrap_or(&q.name).to_string());
            return link(artist.into(), name, "Artist".into(), read_artist(artist).and_then(|a| a.portrait));
        }
        if key.starts_with("spotify:track:") || key.starts_with("spotify:episode:") {
            return first.map(song_link);
        }
        if key.starts_with("spotify:album:") {
            return link(key.into(), q.name.clone(), format!("Album · {}", first.map_or("", |t| t.artists.as_str())), first.and_then(|t| t.cover.clone()));
        }
        if key.starts_with("spotify:show:") {
            return link(key.into(), q.name.clone(), "Podcast".into(), first.and_then(|t| t.cover.clone()));
        }
        if key.contains(':') {
            return None; // a search, the queue, the podcast lists
        }
        let image = self.playlists.iter().find(|p| p.id.as_deref() == Some(key)).and_then(|p| p.image.clone());
        link(format!("spotify:playlist:{key}"), q.name.clone(), "Playlist".into(), image)
    }

    /// Put `l` first among the recently played (once), and keep the list on disk.
    /// Lists and songs are kept apart (up to RECENT_LISTS and RECENT_SONGS), so a run of songs
    /// doesn't push out the lists you started.
    pub fn remember(&mut self, l: Link) {
        self.recent.retain(|r| r.uri != l.uri);
        self.recent.insert(0, l);
        let mut lists = 0;
        let mut songs = 0;
        self.recent.retain(|r| {
            let n = if is_song(&r.uri) { &mut songs } else { &mut lists };
            *n += 1;
            *n <= if is_song(&r.uri) { RECENT_SONGS } else { RECENT_LISTS }
        });
        save_links(&recent_path(), &self.recent);
    }

    /// A song heard for HEARD_MS goes into the recently played. Run from `tick`.
    pub fn note_heard(&mut self) -> bool {
        if self.heard || !self.playing || self.position() < HEARD_MS {
            return false;
        }
        self.heard = true;
        let Some(l) = self.now().map(song_link) else { return false };
        self.remember(l);
        true
    }

    /// Play what's now current, as a new choice: no retry spent on it, no skip still settling.
    pub fn play_fresh(&mut self) {
        self.retried = false;
        self.load_at = None;
        self.play_current();
    }

    /// Play the i-th queued song now (the ones before it stay queued).
    pub fn play_queued(&mut self, i: usize) {
        if i >= self.upnext.len() {
            return;
        }
        self.queued = Some(self.upnext.remove(i));
        self.queue_changed();
        self.play_fresh();
    }

    /// Jump ahead in the list to the song `i` after the one it's on (the queue stays queued).
    pub fn play_upcoming(&mut self, i: usize) {
        let Some(q) = &mut self.queue else { return };
        if q.pos + 1 + i >= q.order.len() {
            return;
        }
        q.pos += 1 + i;
        self.queued = None;
        self.play_fresh();
    }

    /// Add songs to the end of the queue; with nothing playing (or everything played), the first starts.
    pub fn enqueue(&mut self, tracks: impl IntoIterator<Item = Track>) {
        self.enqueue_at(self.upnext.len(), tracks);
    }

    /// The same, in at position `at` (past the end: at the end).
    pub fn enqueue_at(&mut self, at: usize, tracks: impl IntoIterator<Item = Track>) {
        let at = at.min(self.upnext.len());
        self.upnext.splice(at..at, tracks);
        self.queue_changed();
        if self.cur.is_none() || self.finished {
            self.skip(1);
        }
    }

    pub fn dequeue(&mut self, i: usize) -> Option<Track> {
        let t = (i < self.upnext.len()).then(|| self.upnext.remove(i));
        self.queue_changed();
        t
    }

    /// What's coming: the queue, then the rest of the list.
    fn coming_up(&self) -> Vec<Track> {
        self.upnext.iter().chain(self.queue.iter().flat_map(Queue::upcoming)).cloned().collect()
    }

    /// `Q`: what's coming, as a list (enter plays a song from it, x removes a queued one).
    pub fn open_queue(&mut self) {
        let back = self.view.key != QUEUE_KEY;
        self.show_ready(QUEUE_KEY.into(), "Up next".into(), Arc::new(self.coming_up()), back);
    }

    /// Keep up next views (open or in the history) showing what's coming as it changes.
    pub fn queue_changed(&mut self) {
        if self.view.key != QUEUE_KEY && !self.history.iter().chain(&self.ahead).any(|(v, _)| v.key == QUEUE_KEY) {
            return;
        }
        let tracks = Arc::new(self.coming_up());
        for v in std::iter::once(&mut self.view).chain(self.history.iter_mut().chain(self.ahead.iter_mut()).map(|(v, _)| v)) {
            if v.key == QUEUE_KEY {
                v.tracks = tracks.clone();
            }
        }
        if self.view.key == QUEUE_KEY {
            self.refilter();
            let n = self.shown().len();
            self.tr_state.select((n > 0).then(|| self.tr_state.selected().unwrap_or(0).min(n - 1)));
        }
    }

    /// `e`: queue the highlighted song.
    pub fn enqueue_selected(&mut self) {
        if self.focus != Focus::Tracks || self.view.kind != Kind::Tracks || self.view.key == QUEUE_KEY {
            return;
        }
        let Some(t) = self.selected_track().and_then(|i| self.view.tracks.get(i)).cloned() else { return };
        self.flash(format!("queued {}", t.name));
        self.enqueue([t]);
    }

    /// `x` in the queue: drop the highlighted song.
    pub fn dequeue_selected(&mut self) {
        if self.view.key != QUEUE_KEY {
            self.flash("x removes songs from the queue (Q)");
            return;
        }
        let note = match self.selected_track().and_then(|i| self.dequeue(i)) {
            Some(t) => format!("removed {}", t.name),
            None => "only queued songs can be removed".into(),
        };
        self.flash(note);
    }

    /// What plays after the current song, if that's known (a shuffled wrap picks a new order later).
    pub fn up_next(&self) -> Option<&Track> {
        if self.repeat == Repeat::One {
            return self.now();
        }
        if let Some(t) = self.upnext.first() {
            return Some(t);
        }
        let q = self.queue.as_ref()?;
        let p = match self.repeat {
            _ if q.pos + 1 < q.order.len() => q.pos + 1,
            Repeat::All if !self.shuffle => 0,
            _ => return None,
        };
        q.tracks.get(q.order[p])
    }

    /// Open the list that's playing, on the playing song.
    pub fn show_queue(&mut self) {
        let Some(q) = &self.queue else { return };
        let (idx, key, name, tracks) = (q.idx(), q.key.clone(), q.name.clone(), q.tracks.clone());
        let album = key.starts_with("spotify:album:");
        self.show_ready(key, name, tracks, true);
        // an album opened from the player says whose it is, as one opened any other way does
        if album {
            self.view.by = By::album(read_album_by(&self.view.key)).or_else(|| self.view.tracks.first().map(By::lead));
            self.view.about = read_about(&self.view.key);
        }
        self.tr_state = Sel::with(self.row_of(idx).unwrap_or(0));
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = self.repeat.next();
    }

    pub fn skip(&mut self, d: isize) {
        if d > 0 && !self.upnext.is_empty() {
            self.queued = Some(self.upnext.remove(0));
            self.queue_changed();
            self.retried = false;
            return self.play_current();
        }
        // off a queued song: on to the list's next, or back to where the list is (prev)
        let d = if self.queued.take().is_some() && d < 0 { 0 } else { d };
        let Some(q) = &mut self.queue else {
            self.cur = None;
            return self.stop_finished();
        };
        let len = q.order.len() as isize;
        let mut n = q.pos as isize + d;
        if self.repeat == Repeat::All && (n < 0 || n >= len) {
            if n >= len && self.shuffle {
                // fresh order each pass; don't open the new pass with the song that just ended
                let last = q.idx();
                q.reorder(true, None);
                if q.order.len() > 1 && q.order[0] == last {
                    q.order.swap(0, 1);
                }
                n = 0;
            } else {
                n = n.rem_euclid(len);
            }
        }
        if n < 0 || n >= len {
            return self.stop_finished();
        }
        q.pos = n as usize;
        self.retried = false;
        self.play_current();
    }

    /// Nothing left to play.
    fn stop_finished(&mut self) {
        self.player.stop();
        self.playing = false;
        self.want_play = false;
        self.finished = true;
    }

    pub fn position(&self) -> u64 {
        self.pos_ms + self.pos_at.map_or(0, |t| t.elapsed().as_millis() as u64)
    }

    /// A brief popup note.
    pub fn flash(&mut self, note: impl Into<String>) {
        self.note = Some((note.into(), Instant::now() + Duration::from_millis(1500)));
    }

    /// Volume in percent.
    pub fn vol_pct(&self) -> u32 {
        (self.vol as u32 * 100 + 32767) / 65535
    }

    pub fn set_volume(&mut self, delta: i32) {
        self.vol = (self.vol as i32 + delta).clamp(0, 65535) as u16;
        self.vol_until = Some(Instant::now() + Duration::from_millis(1500));
        if self.want_play {
            self.mixer.set_volume(self.vol);
        }
        self.cache.save_volume(self.vol);
    }

    pub fn on_player(&mut self, ev: PlayerEvent) {
        let sync = matches!(
            ev,
            PlayerEvent::Playing { .. } | PlayerEvent::Paused { .. } | PlayerEvent::Stopped { .. } | PlayerEvent::Seeked { .. }
        );
        self.on_player_inner(ev);
        if sync {
            self.sync_media();
        }
    }

    pub fn on_player_inner(&mut self, ev: PlayerEvent) {
        let is_cur = |id: &SpotifyUri, cur: &Option<SpotifyUri>| cur.as_ref() == Some(id);
        log::debug!("player event: {ev:?}");
        match ev {
            PlayerEvent::Playing { track_id, position_ms, .. } if is_cur(&track_id, &self.cur) => {
                self.playing = true;
                self.fails = 0;
                self.pos_ms = position_ms as u64;
                self.pos_at = Some(Instant::now());
                if self.status.starts_with("can't play") {
                    self.status.clear();
                }
                if let Some(l) = &mut self.listen {
                    l.since.get_or_insert_with(Instant::now);
                    if !l.announced {
                        l.announced = true;
                        l.started = now_secs();
                        if let Some(fm) = &self.lastfm {
                            fm.now_playing(&self.rt, &l.track);
                        }
                    }
                }
                trim_heap();
            }
            PlayerEvent::Paused { track_id, position_ms, .. } if is_cur(&track_id, &self.cur) => {
                self.playing = false;
                self.pos_ms = position_ms as u64;
                self.pos_at = None;
                self.pause_listen();
                self.save_progress();
            }
            PlayerEvent::Seeked { track_id, position_ms, .. }
            | PlayerEvent::PositionCorrection { track_id, position_ms, .. }
                if is_cur(&track_id, &self.cur) =>
            {
                self.pos_ms = position_ms as u64;
                self.pos_at = self.playing.then(Instant::now);
            }
            PlayerEvent::TimeToPreloadNextTrack { track_id, .. } if is_cur(&track_id, &self.cur) => {
                if let Some(t) = self.up_next()
                    && let Ok(u) = SpotifyUri::from_uri(&t.uri)
                {
                    self.player.preload(u);
                }
            }
            PlayerEvent::EndOfTrack { track_id, .. } if is_cur(&track_id, &self.cur) => {
                self.save_progress();
                if self.repeat == Repeat::One {
                    self.play_current();
                } else {
                    self.skip(1);
                }
            }
            PlayerEvent::Unavailable { track_id, .. } if is_cur(&track_id, &self.cur) => {
                self.pause_listen();
                // librespot logs the actual reason (key / file / CDN error) just before this
                let track = self.now();
                let session = match self.session() {
                    None => "not connected",
                    Some(s) if s.is_invalid() => "invalid",
                    Some(_) => "ok",
                };
                log::warn!(
                    "playback failed: {} ({} - {}), session {session}, retried {}, fails {}",
                    track_id.to_uri().unwrap_or_default(),
                    track.map_or("?", |t| &t.name),
                    track.map_or("?", |t| &t.artists),
                    self.retried,
                    self.fails,
                );
                // dead session after sleep/network change shows up as unavailable: retry once
                if !self.retried && self.session().is_some_and(|s| s.is_invalid()) {
                    log::info!("session is dead, reconnecting and retrying");
                    self.retried = true;
                    self.play_current();
                } else {
                    self.fails += 1;
                    if self.fails > self.queue.as_ref().map_or(0, |q| q.order.len()) {
                        log::warn!("every track in the queue failed, stopping");
                        self.status = "nothing in this list is playable".into();
                        self.player.stop();
                        self.want_play = false;
                    } else {
                        log::info!("skipping to the next track");
                        self.status = "can't play that track, skipping".into();
                        self.skip(1);
                    }
                }
            }
            PlayerEvent::Stopped { .. } => {
                self.playing = false;
                self.pos_at = None;
                self.pause_listen();
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn episode_progress_skips_the_start_and_finishes_in_the_last_minute() {
        const HOUR: u64 = 3_600_000;
        assert_eq!(progress_after(4_999, HOUR), None);
        assert_eq!(progress_after(5_000, HOUR), Some(5_000));
        assert_eq!(progress_after(HOUR - 60_001, HOUR), Some(HOUR - 60_001));
        assert_eq!(progress_after(HOUR - 60_000, HOUR), Some(HEARD));
        assert_eq!(progress_after(10_000, 30_000), Some(HEARD)); // shorter than the last minute
    }

    #[test]
    fn a_shows_episodes_never_shuffle() {
        let tracks = Arc::new(parse_tracks(5));
        let mut q = Queue { key: "spotify:show:x".into(), name: String::new(), tracks: tracks.clone(), order: vec![], pos: 0, sorted: vec![] };
        q.reorder(true, Some(2));
        assert_eq!((q.order.as_slice(), q.pos), (&[0, 1, 2, 3, 4][..], 2));
    }

    fn parse_tracks(n: usize) -> Vec<Track> {
        (0..n).filter_map(|i| parse_episode(&serde_json::json!({ "uri": format!("spotify:episode:{i}") }), Some(("show", 0)))).collect()
    }
}

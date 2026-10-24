// Playing: the list and the queue, skipping, pause / resume fades, seeking, volume, and the
// player's events.

use crate::*;

impl App {
    pub fn toggle_shuffle(&mut self) {
        self.shuffle = !self.shuffle;
        if let Some(q) = &mut self.queue {
            let cur = q.idx();
            q.reorder(self.shuffle, Some(cur));
        }
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
        if self.lastfm.is_some() {
            self.listen = Some(Listen { track: t.clone(), started: 0, listened: Duration::ZERO, since: None, announced: false });
        }
        if let Some(m) = &mut self.media {
            let _ = m.set_metadata(MediaMetadata {
                title: Some(&t.name),
                artist: Some(&t.artists),
                album: Some(&t.album),
                cover_url: t.cover.as_deref(),
                duration: Some(Duration::from_millis(t.ms)),
            });
        }
        self.cur = Some(uri);
        self.loaded = false;
        self.pos_ms = pos_ms;
        self.pos_at = None;
        self.plays += 1;
        self.finished = false;
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
        self.set_cur(&t, 0);
        self.load_cur();
    }

    /// Put last session's queue back, paused on the song that was playing.
    pub fn restore(&mut self, last: Last) {
        self.shuffle = last.shuffle;
        self.repeat = last.repeat;
        let t = last.queue.tracks[last.queue.idx()].clone();
        self.queue = Some(last.queue);
        self.set_cur(&t, last.pos_ms);
        self.sync_media();
    }

    pub fn save_last(&self) {
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
            push_tracks(&mut s, &self.queued.iter().chain(&self.upnext).cloned().collect::<Vec<_>>());
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
        let progress = Some(MediaPosition(Duration::from_millis(self.position())));
        let state = match (&self.cur, self.playing) {
            (None, _) => MediaPlayback::Stopped,
            (_, true) => MediaPlayback::Playing { progress },
            (_, false) => MediaPlayback::Paused { progress },
        };
        if let Some(m) = &mut self.media {
            let _ = m.set_playback(state);
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
        } else {
            self.pos_ms = ms;
        }
    }

    pub fn on_media(&mut self, ev: MediaControlEvent) {
        use MediaControlEvent as E;
        let step = Duration::from_secs(10);
        match ev {
            E::Toggle => self.toggle(),
            E::Play if !self.want_play => self.toggle(),
            E::Pause if self.want_play => self.toggle(),
            E::Next => self.skip_soon(1),
            E::Previous => self.prev(),
            E::Seek(d) => self.seek_by(d, step),
            E::SeekBy(d, by) => self.seek_by(d, by),
            E::SetPosition(MediaPosition(p)) => self.seek_to(p.as_millis() as u64),
            _ => {}
        }
    }

    pub fn seek_by(&mut self, d: SeekDirection, by: Duration) {
        let by = by.as_millis() as u64;
        let pos = self.position();
        self.seek_to(match d {
            SeekDirection::Forward => pos + by,
            SeekDirection::Backward => pos.saturating_sub(by),
        });
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
    pub fn play_from_view(&mut self, start: Option<usize>) {
        if self.view.kind != Kind::Tracks || self.view.tracks.is_empty() {
            return;
        }
        if self.view.key == QUEUE_KEY {
            return self.play_queued(start.unwrap_or(0));
        }
        let q = Queue {
            key: self.view.key.clone(),
            name: self.view.title.clone(),
            tracks: self.view.tracks.clone(),
            order: vec![],
            pos: 0,
        };
        self.play_queue(q, start);
    }

    /// Make `q` the queue and play it from `start` (None: the top, or a random song if shuffling).
    pub fn play_queue(&mut self, mut q: Queue, start: Option<usize>) {
        q.reorder(self.shuffle, start);
        self.queue = Some(q);
        self.queued = None;
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
        self.retried = false;
        self.load_at = None;
        self.play_current();
    }

    /// Add songs to the end of the queue; with nothing playing (or everything played), the first starts.
    pub fn enqueue(&mut self, tracks: impl IntoIterator<Item = Track>) {
        self.upnext.extend(tracks);
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

    /// `Q`: the queue as a list (enter plays a song from it, x removes one).
    pub fn open_queue(&mut self) {
        let mut v = self.new_view(QUEUE_KEY.into(), "Queue".into(), Kind::Tracks);
        v.tracks = Arc::new(self.upnext.clone());
        v.loading = false;
        let back = self.view.key != QUEUE_KEY;
        self.show(v, back);
        self.focus = Focus::Tracks;
    }

    /// Keep queue views (open or in the history) showing the queue as it changes.
    pub fn queue_changed(&mut self) {
        let tracks = Arc::new(self.upnext.clone());
        for v in std::iter::once(&mut self.view).chain(self.history.iter_mut().map(|(v, _)| v)) {
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
        self.note = Some((format!("queued {}", t.name), Instant::now() + Duration::from_millis(1500)));
        self.enqueue([t]);
    }

    /// `x` in the queue: drop the highlighted song.
    pub fn dequeue_selected(&mut self) {
        if self.view.key != QUEUE_KEY {
            self.note = Some(("x removes songs from the queue (Q)".into(), Instant::now() + Duration::from_millis(1500)));
            return;
        }
        if let Some(t) = self.selected_track().and_then(|i| self.dequeue(i)) {
            self.note = Some((format!("removed {}", t.name), Instant::now() + Duration::from_millis(1500)));
        }
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
        let mut v = self.new_view(key, name, Kind::Tracks);
        v.tracks = tracks;
        v.loading = false;
        self.show(v, true);
        self.tr_state = TableState::default().with_selected(idx);
        self.focus = Focus::Tracks;
    }

    pub fn cycle_repeat(&mut self) {
        self.repeat = match self.repeat {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
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
            self.player.stop();
            self.playing = false;
            self.want_play = false;
            self.finished = true;
            self.cur = None;
            return;
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
            self.player.stop();
            self.playing = false;
            self.want_play = false;
            self.finished = true;
            return;
        }
        q.pos = n as usize;
        self.retried = false;
        self.play_current();
    }

    pub fn position(&self) -> u64 {
        self.pos_ms + self.pos_at.map_or(0, |t| t.elapsed().as_millis() as u64)
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
                        l.started = unix_now();
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

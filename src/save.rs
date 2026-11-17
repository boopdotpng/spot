// Saving songs to Liked Songs and to their playlists: `L` likes or unlikes a song, `t` opens the
// picker (Liked Songs and every playlist they can add to, each marked if the song's in it), and
// `spot add` / `spot remove` do the same from the command line. Spotify can't be asked whether a
// playlist has a song, so its list of songs is fetched (one lookup, uris only) before adding, and
// when the picker opens; Liked Songs it can be asked about. What Spotify took is written into the
// cached lists too, so they show it before they're next fetched.

use crate::*;

/// Where songs go: Liked Songs (no id) or one of their playlists.
#[derive(Clone)]
pub struct Dest {
    pub id: Option<String>,
    pub name: String,
}

impl Dest {
    pub fn of(p: &Playlist) -> Dest {
        Dest { id: p.id.clone(), name: p.name.clone() }
    }
    /// The view key (and cache name) of its list.
    pub fn key(&self) -> String {
        self.id.clone().unwrap_or_else(|| "liked".into())
    }
}

/// A save, done: each place and how it went, and those it didn't need doing in.
pub struct Saved {
    pub tracks: Vec<Track>,
    pub on: bool, // added (else removed)
    pub done: Vec<(Dest, Result<(), String>)>,
    pub already: Vec<Dest>,
    pub reply: Option<ctl::Reply>, // a `spot add` waiting on it; else the TUI is told
}

/// The `t` picker: one song, and a row for Liked Songs and each playlist it can go in.
pub struct Picker {
    pub track: Track,
    pub rows: Vec<PickRow>,
    pub filter: String,
    pub sel: usize, // index into shown()
}

pub struct PickRow {
    pub dest: Dest,
    pub has: bool,    // the song's in it (as the cache has it; Liked Songs as Spotify says)
    pub marked: bool, // to change: added if it isn't in it, taken out if it is
}

impl Picker {
    /// The rows the filter leaves, as indices into rows.
    pub fn shown(&self) -> Vec<usize> {
        let f = self.filter.to_lowercase();
        (0..self.rows.len()).filter(|&i| self.rows[i].dest.name.to_lowercase().contains(&f)).collect()
    }
}

/// Whether `d` has the song, as Spotify says.
pub async fn has_song(api: &Api, sess: Sess, d: &Dest, uri: &str) -> Result<bool, String> {
    match &d.id {
        Some(id) => Ok(playlist_uris(sess, id).await?.contains(uri)),
        None => Ok(liked_contains(api, &[uri.to_string()]).await?.first().copied().unwrap_or(false)),
    }
}

/// "Liked Songs and bops!", "a, b and c".
pub fn and_list(names: &[&str]) -> String {
    match names {
        [] => String::new(),
        [one] => one.to_string(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}

impl App {
    /// The song `L` and `t` act on: the one under the cursor in a list of songs, else the one playing.
    pub fn song_to_save(&self) -> Option<Track> {
        let listed = self.focus == Focus::Tracks && self.view.kind == Kind::Tracks;
        let at = listed.then(|| self.selected_track().and_then(|i| self.view.tracks.get(i))).flatten();
        at.or(self.now()).cloned()
    }

    /// The playlists songs can be added to: Liked Songs, then theirs, in sidebar order.
    pub fn dests(&self) -> Vec<Dest> {
        self.playlists.iter().filter(|p| p.mine).map(Dest::of).collect()
    }

    /// Whether the cached copy of `key`'s list has the song.
    fn cached_has(&self, key: &str, uri: &str) -> bool {
        self.cached_list(key).iter().any(|t| t.uri == uri)
    }

    /// `L`: like the song, or unlike it if it's liked (asking Spotify which, as the cached Liked
    /// Songs can be behind a like from the phone).
    pub fn toggle_like(&mut self) {
        let Some(t) = self.song_to_save().filter(|t| !t.is_episode()) else {
            return self.flash("no song to like");
        };
        let (api, tx) = (self.api.clone(), self.tx.clone());
        self.rt.spawn(async move {
            let on = match liked_contains(&api, std::slice::from_ref(&t.uri)).await {
                Ok(has) => !has.first().copied().unwrap_or(false),
                Err(e) => return drop(tx.send(Msg::Error(format!("like: {e}")))),
            };
            let r = set_liked(&api, std::slice::from_ref(&t.uri), on).await;
            let done = vec![(Dest::of(&liked()), r)];
            let _ = tx.send(Msg::Saved(Saved { tracks: vec![t], on, done, already: vec![], reply: None }));
        });
    }

    /// `t`: the picker for the song, its rows marked from the cache, then Liked Songs checked.
    pub fn open_picker(&mut self) {
        let Some(track) = self.song_to_save() else {
            return self.flash("no song to add");
        };
        let rows: Vec<PickRow> = self
            .dests()
            .into_iter()
            // episodes can go in playlists, not Liked Songs
            .filter(|d| d.id.is_some() || !track.is_episode())
            .map(|dest| PickRow { has: self.cached_has(&dest.key(), &track.uri), dest, marked: false })
            .collect();
        // the marks are the cache's until Spotify says, a row at a time
        for d in rows.iter().map(|r| r.dest.clone()) {
            let (api, sess, tx, uri) = (self.api.clone(), self.sess.subscribe(), self.tx.clone(), track.uri.clone());
            self.rt.spawn(async move {
                if let Ok(has) = has_song(&api, sess, &d, &uri).await {
                    let _ = tx.send(Msg::Has { key: d.key(), uri, has });
                }
            });
        }
        self.picker = Some(Picker { track, rows, filter: String::new(), sel: 0 });
    }

    /// Spotify says whether a list has a song: the picker shows it, and Liked Songs' cache
    /// follows (a playlist's is put right when it's next opened).
    pub fn on_has(&mut self, key: &str, uri: &str, has: bool) {
        let Some(p) = self.picker.as_mut().filter(|p| p.track.uri == uri) else { return };
        for r in p.rows.iter_mut().filter(|r| r.dest.key() == key) {
            r.has = has;
        }
        let t = p.track.clone();
        if key == "liked" && has != self.cached_has(key, uri) {
            self.cache_saved(key, &[t], has);
        }
    }

    /// A key while the picker's open: typing filters it, tab marks a row, enter does the marked
    /// rows (or the one under the cursor), esc closes it.
    pub fn on_picker_key(&mut self, Press { key, ctrl }: Press) {
        let Some(p) = self.picker.as_mut() else { return };
        let n = p.shown().len();
        match key {
            Key::Esc => self.picker = None,
            Key::Char('c') if ctrl => self.picker = None,
            Key::Down | Key::Char('n') if key == Key::Down || ctrl => p.sel = (p.sel + 1).min(n.saturating_sub(1)),
            Key::Up | Key::Char('p') if key == Key::Up || ctrl => p.sel = p.sel.saturating_sub(1),
            Key::Tab | Key::BackTab => {
                if let Some(&i) = p.shown().get(p.sel) {
                    p.rows[i].marked ^= true;
                    if key == Key::Tab {
                        p.sel = (p.sel + 1).min(n.saturating_sub(1));
                    }
                }
            }
            Key::Backspace => {
                p.filter.pop();
                p.sel = 0;
            }
            Key::Char(c) if !ctrl => {
                p.filter.push(c);
                p.sel = 0;
            }
            Key::Enter => {
                let mut rows: Vec<&PickRow> = p.rows.iter().filter(|r| r.marked).collect();
                if rows.is_empty() {
                    rows.extend(p.shown().get(p.sel).map(|&i| &p.rows[i]));
                }
                let add: Vec<Dest> = rows.iter().filter(|r| !r.has).map(|r| r.dest.clone()).collect();
                let remove: Vec<Dest> = rows.iter().filter(|r| r.has).map(|r| r.dest.clone()).collect();
                let track = p.track.clone();
                self.picker = None;
                if !add.is_empty() {
                    self.save(vec![track.clone()], add, true, None);
                }
                if !remove.is_empty() {
                    self.save(vec![track], remove, false, None);
                }
            }
            _ => {}
        }
    }

    /// Put `tracks` in (or take them out of) each of `dests`, in the background. Each list is
    /// asked first, so a playlist isn't given a song it has (Liked Songs can't have one twice
    /// anyway, but it's said that it was there).
    pub fn save(&mut self, tracks: Vec<Track>, dests: Vec<Dest>, on: bool, reply: Option<ctl::Reply>) {
        let todo: Vec<(Dest, Vec<String>, HashSet<String>)> = dests
            .into_iter()
            .map(|d| {
                let mut seen = HashSet::new();
                // episodes can't be liked
                let uris = tracks.iter().filter(|t| (d.id.is_some() || !t.is_episode()) && seen.insert(&t.uri)).map(|t| t.uri.clone()).collect();
                // what the cache has, in case Spotify can't say
                let cached = self.cached_list(&d.key()).iter().map(|t| t.uri.clone()).collect();
                (d, uris, cached)
            })
            .collect();
        let (api, sess, tx) = (self.api.clone(), self.sess.subscribe(), self.tx.clone());
        self.rt.spawn(async move {
            let (mut done, mut already) = (vec![], vec![]);
            for (d, mut uris, cached) in todo {
                let have: HashSet<String> = match &d.id {
                    Some(id) => playlist_uris(sess.clone(), id).await.unwrap_or_else(|e| {
                        log::warn!("{}: {e}; going by the cache", d.name);
                        cached
                    }),
                    None => match liked_contains(&api, &uris).await {
                        Ok(has) => uris.iter().zip(has).filter(|(_, h)| *h).map(|(u, _)| u.clone()).collect(),
                        Err(_) => cached,
                    },
                };
                uris.retain(|u| have.contains(u) != on);
                if uris.is_empty() {
                    already.push(d);
                    continue;
                }
                let r = match &d.id {
                    None => set_liked(&api, &uris, on).await,
                    Some(id) => set_in_playlist(&api, id, &uris, on).await,
                };
                done.push((d, r));
            }
            let _ = tx.send(Msg::Saved(Saved { tracks, on, done, already, reply }));
        });
    }

    /// A save's done: the cached lists (and the open one) follow, and it's said where it went.
    pub fn on_saved(&mut self, s: Saved) {
        let Saved { tracks, on, done, already, reply } = s;
        for (d, r) in &done {
            match r {
                Ok(()) => self.cache_saved(&d.key(), &tracks, on),
                Err(e) => log::error!("{} {}: {e}", if on { "adding to" } else { "removing from" }, d.name),
            }
        }
        let what = match &tracks[..] {
            [t] => format!("{} — {}", t.name, t.artists),
            ts => format!("{} songs", ts.len()),
        };
        let names = |ok: bool| done.iter().filter(|(_, r)| r.is_ok() == ok).map(|(d, _)| d.name.as_str()).collect::<Vec<_>>();
        let mut lines = vec![];
        let ok = names(true);
        if !ok.is_empty() {
            let verb = match (on, &ok[..]) {
                (true, ["Liked Songs"]) => "liked".to_string(),
                (false, ["Liked Songs"]) => "unliked".to_string(),
                (true, _) => format!("added to {}:", and_list(&ok)),
                (false, _) => format!("removed from {}:", and_list(&ok)),
            };
            lines.push(format!("{verb} {what}"));
        }
        if !already.is_empty() {
            let names: Vec<&str> = already.iter().map(|d| d.name.as_str()).collect();
            lines.push(if on { format!("was in {} already", and_list(&names)) } else { format!("wasn't in {}", and_list(&names)) });
        }
        let failed: Vec<String> = done.iter().filter_map(|(d, r)| r.as_ref().err().map(|e| format!("{}: {e}", d.name))).collect();
        lines.extend(failed.iter().cloned());
        match reply {
            Some(reply) => drop(reply.send((i32::from(!failed.is_empty()), lines.join("\n")))),
            None if !failed.is_empty() => self.status = lines.join(" · "),
            None => self.flash(lines.join(" · ")),
        }
    }

    /// Write a save into the cached copy of `key`'s list (new likes on top, playlist songs at the
    /// end, as Spotify has them), and into the open view if it's that list.
    fn cache_saved(&mut self, key: &str, tracks: &[Track], on: bool) {
        let mut list = (*self.cached_list(key)).clone();
        if on {
            let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs());
            let new: Vec<Track> = tracks
                .iter()
                .filter(|t| !list.iter().any(|h| h.uri == t.uri) && (key != "liked" || !t.is_episode()))
                .map(|t| Track { added: now, ..t.clone() })
                .collect();
            if key == "liked" {
                list.splice(0..0, new);
            } else {
                list.extend(new);
            }
        } else {
            list.retain(|t| !tracks.iter().any(|g| g.uri == t.uri));
        }
        save_tracks(&tracks_path(&list_key(key)), &list);
        if key == "liked" {
            self.liked = Arc::new(list.iter().map(|t| t.uri.clone()).collect());
        }
        if self.view.key == key {
            let _ = self.tx.send(Msg::Replace { view: self.view.id, tracks: Arc::new(list) });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_names() {
        assert_eq!(and_list(&["Liked Songs"]), "Liked Songs");
        assert_eq!(and_list(&["Liked Songs", "bops!"]), "Liked Songs and bops!");
        assert_eq!(and_list(&["a", "b", "c"]), "a, b and c");
    }
}

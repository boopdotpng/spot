// What the ⌥Space launcher (mac/Launcher.swift) asks the player, over the same socket as the
// command line: `spot launcher status | lists | search <words> | songs <uri> | song <uri> | play <uri> | queue <uri>`. Every answer is
// tab-separated rows, for a program rather than a person, so it isn't in `spot help`.
//
// A result row: uri, name, subtitle, cover url, ms (0 for what isn't a song), explicit (1 or
// empty), section, inside an album the song's track number, and an album's year.

use crate::{ctl::{Reply, Then}, *};

type Fail = (i32, String);

/// Popular songs atop an artist's page in the launcher, before their discography.
const ARTIST_TOP: usize = 3;

/// Fields joined by tabs, with any tabs or newlines inside them made spaces.
fn tsv_row(fields: &[&str]) -> String {
    fields.iter().map(|f| f.replace(['\t', '\n'], " ")).collect::<Vec<_>>().join("\t")
}

/// An image near 300 px from a Web API item's `images`.
fn cover(v: &Value) -> String {
    let images = v["images"].as_array().map_or(&[][..], Vec::as_slice);
    let near = images.iter().min_by_key(|i| i["width"].as_u64().unwrap_or(0).abs_diff(300));
    near.and_then(|i| i["url"].as_str()).unwrap_or("").into()
}

fn link_row(l: &Link, section: &str) -> String {
    tsv_row(&[&l.uri, &l.name, &l.sub, l.image.as_deref().unwrap_or(""), "0", "", section])
}

/// One of an artist's releases, its year (which leads its subtitle) a field of its own: the
/// launcher shows it on the right, where a song's length goes. "2004 album · 11 songs" is
/// "Album · 11 songs".
fn release_row(l: &Link, section: &str) -> String {
    let (year, rest) = match l.sub.split_once(' ') {
        Some((y, rest)) if y.len() == 4 && y.bytes().all(|b| b.is_ascii_digit()) => (y, rest),
        _ => ("", l.sub.as_str()),
    };
    let mut c = rest.chars();
    let sub: String = c.next().map(|f| f.to_uppercase().chain(c).collect()).unwrap_or_default();
    tsv_row(&[&l.uri, &l.name, &sub, l.image.as_deref().unwrap_or(""), "0", "", section, "", year])
}

impl App {
    pub fn launcher_cmd(&mut self, args: Vec<String>, reply: &Reply) -> Result<Option<String>, Fail> {
        let usage = || (2, "launcher: status, lists, search <words>, songs <uri>, song <uri>, play <uri> or queue <uri>".to_string());
        let (sub, rest) = args.split_first().ok_or_else(usage)?;
        match sub.as_str() {
            // playing (1 or empty), name, artists, uri, cover url, position ms, length ms,
            // shuffle (1 or empty), repeat (off, all, one); nothing playing is no line
            "status" => {
                let on = |b: bool| if b { "1" } else { "" };
                let repeat = match self.repeat {
                    Repeat::Off => "off",
                    Repeat::All => "all",
                    Repeat::One => "one",
                };
                let row = |t: &Track| {
                    let pos = self.position().min(t.ms).to_string();
                    tsv_row(&[on(self.want_play), &t.name, &t.artists, &t.uri, t.cover.as_deref().unwrap_or(""), &pos, &t.ms.to_string(), on(self.shuffle), repeat])
                };
                Ok(Some(self.now().map(row).unwrap_or_default()))
            }
            // the lists you started lately, the songs you heard lately (HEARD_MS or more), then all
            // of your playlists (the launcher matches them as you type)
            "lists" => {
                let lists = self.recent.iter().filter(|l| !is_song(&l.uri)).map(|l| link_row(l, "Recently played"));
                // the section says they're songs: just whose
                let songs = self.recent.iter().filter(|l| is_song(&l.uri)).map(|l| {
                    let sub = l.sub.split_once(" · ").map_or(l.sub.as_str(), |(_, by)| by);
                    link_row(&Link { sub: sub.into(), ..l.clone() }, "Recent songs")
                });
                let recent = lists.chain(songs);
                let mine = self.playlists.iter().map(|p| {
                    let l = Link { uri: p.uri(), name: p.name.clone(), sub: "Playlist".into(), single: false, image: p.image.clone() };
                    link_row(&l, "Your playlists")
                });
                Ok(Some(recent.chain(mine).collect::<Vec<_>>().join("\n")))
            }
            "search" => {
                if rest.is_empty() {
                    return Err(usage());
                }
                let (api, q, reply) = (self.api.clone(), rest.join(" "), reply.clone());
                self.rt.spawn(async move {
                    let _ = reply.send(match search_all(&api, &q, 5).await {
                        Err(e) => (1, e),
                        Ok(v) => (0, mixed_rows(&q, &v).join("\n")),
                    });
                });
                Ok(None)
            }
            // ⏎ on a result: a playlist (or Liked Songs) plays shuffled, anything else as `play` would
            "play" => {
                let [uri] = &rest[..] else { return Err(usage()) };
                let shuffle = uri.starts_with("spotify:playlist:") || uri == "liked";
                self.load(vec![uri.clone()], Then::Play { from: None, shuffle }, reply.clone());
                Ok(None)
            }
            // ⌘⏎: after what's queued
            "queue" => {
                if rest.is_empty() {
                    return Err(usage());
                }
                let mut uris: Vec<String> = self.upnext.iter().map(|t| t.uri.clone()).collect();
                uris.extend(rest.iter().cloned());
                self.queue_cmd(uris, reply).map(|r| r.map(|_| String::new()))
            }
            "songs" => {
                let Some(uri) = rest.first().cloned() else { return Err(usage()) };
                self.songs(uri, reply);
                Ok(None)
            }
            "song" => {
                let Some(uri) = rest.first().cloned() else { return Err(usage()) };
                self.song_page(uri, reply);
                Ok(None)
            }
            _ => Err(usage()),
        }
    }

    /// → on a song in the launcher: its album, then each of its artists, to go to either.
    /// Portraits of artists not seen before take one more lookup before answering; their pages
    /// are fetched after, so → on one opens at once.
    fn song_page(&mut self, uri: String, reply: &Reply) {
        let (api, sess, reply) = (self.api.clone(), self.sess.subscribe(), reply.clone());
        self.rt.spawn(async move {
            let t = match resolve(api, sess.clone(), &uri, None).await {
                Ok((_, mut t)) if !t.is_empty() => t.remove(0),
                Ok(_) => return drop(reply.send((1, "song not found".into()))),
                Err(e) => return drop(reply.send((1, e))),
            };
            let artists: Vec<(String, String, Option<String>)> = t
                .credits()
                .into_iter()
                .filter_map(|(name, id)| {
                    let uri = id_uri("artist", id)?;
                    let portrait = read_artist(&uri).and_then(|i| i.portrait);
                    Some((uri, name.to_string(), portrait))
                })
                .collect();
            let unseen: Vec<String> = artists.iter().filter(|a| a.2.is_none()).map(|a| a.0.clone()).collect();
            let mut found = HashMap::new();
            if !unseen.is_empty()
                && let Ok(session) = ready(sess.clone()).await
                && let Ok(meta) = batch_meta::<PbArtist>(&session, &unseen, ExtensionKind::ARTIST_V4).await
            {
                for (uri, a) in meta {
                    if let Some(p) = image_url(&a.portrait_group.image).or_else(|| image_url(&a.portrait)) {
                        found.insert(uri, p);
                    }
                }
            }
            let mut rows = vec![];
            if let Some(album) = id_uri("album", t.album_id) {
                rows.push(tsv_row(&[&album, &t.album, "Album", t.cover.as_deref().unwrap_or(""), "0", "", "Album"]));
            }
            for (uri, name, portrait) in &artists {
                let portrait = portrait.as_deref().or(found.get(uri).map(String::as_str)).unwrap_or("");
                rows.push(tsv_row(&[uri, name, "Artist", portrait, "0", "", "Artists"]));
            }
            let _ = reply.send((0, rows.join("\n")));
            for artist in unseen {
                let (nobody, _) = mpsc::channel();
                artist_overview(sess.clone(), nobody, 0, artist, None).await;
            }
        });
    }

    /// An album or playlist's songs, for → in the launcher; an artist's page: their popular
    /// songs, then their albums and singles (each opens with → again).
    fn songs(&mut self, uri: String, reply: &Reply) {
        let (api, sess, reply) = (self.api.clone(), self.sess.subscribe(), reply.clone());
        self.rt.spawn(async move {
            let song = |t: &Track, sub: &str, section: &str, n: &str| {
                tsv_row(&[&t.uri, &t.name, sub, t.cover.as_deref().unwrap_or(""), &t.ms.to_string(), if t.explicit { "1" } else { "" }, section, n])
            };
            if uri.starts_with("spotify:artist:") {
                let mut info = read_artist(&uri);
                if info.is_none() {
                    let (nobody, _) = mpsc::channel();
                    artist_overview(sess, nobody, 0, uri.clone(), None).await;
                    info = read_artist(&uri);
                }
                let _ = reply.send(match info {
                    None => (1, "couldn't get their page".into()),
                    Some(i) => {
                        let top = i.top.iter().take(ARTIST_TOP).map(|t| song(t, &t.album, "Popular", ""));
                        let albums = i.releases.iter().filter(|l| !l.single).map(|l| release_row(l, "Albums"));
                        let singles = i.releases.iter().filter(|l| l.single).map(|l| release_row(l, "Singles"));
                        (0, top.chain(albums).chain(singles).collect::<Vec<_>>().join("\n"))
                    }
                });
                return;
            }
            let album = uri.starts_with("spotify:album:");
            let _ = reply.send(match resolve(api, sess, &uri, None).await {
                Err(e) => (1, e),
                Ok((_, t)) => {
                    let row = |t: &Track| song(t, &t.artists, "", &if album && t.number > 0 { t.number.to_string() } else { String::new() });
                    (0, t.iter().map(row).collect::<Vec<_>>().join("\n"))
                }
            });
        });
    }
}

/// Of the first artist, song and album (in that order of preference on a tie), the one the
/// words name best; the song when none of them matches.
fn top_pick(q: &str, names: [Option<&str>; 3]) -> Option<usize> {
    let best = names.iter().enumerate().filter_map(|(i, n)| Some((i, match_score(q, (*n)?)))).fold(None, |b: Option<(usize, u8)>, (i, sc)| match b {
        Some((_, bs)) if bs >= sc => b,
        _ => Some((i, sc)),
    });
    match best {
        Some((i, sc)) if sc > 0 => Some(i),
        _ => names[1].is_some().then_some(1).or(names.iter().position(Option::is_some)),
    }
}

/// One search's results as the launcher lists them: the top result, three songs, an album and
/// an artist. A song or album on several releases (a deluxe edition, a single and its album) is
/// kept once, and the top result isn't repeated. Artists far less known than the best of them
/// are dropped: searching an album's name finds accounts named after it.
fn mixed_rows(q: &str, v: &Value) -> Vec<String> {
    fn items<'a>(v: &'a Value, k: &str) -> Vec<&'a Value> {
        v[k]["items"].as_array().map(|a| a.iter().collect()).unwrap_or_default()
    }
    let (tracks, albums, mut artists) = (items(v, "tracks"), items(v, "albums"), items(v, "artists"));
    let fame = |i: &Value| i["popularity"].as_u64();
    if let Some(best) = artists.iter().filter_map(|i| fame(i)).max() {
        artists.retain(|i| fame(i).is_none_or(|p| p * 4 >= best));
    }
    let same = |i: &Value| {
        let by: Vec<String> = i["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).map(str::to_lowercase).collect()).unwrap_or_default();
        format!("{}\t{}", i["name"].as_str().unwrap_or("").trim().to_lowercase(), by.join(", "))
    };
    let mut seen = HashSet::new();
    let mut fresh = |kind: &str, i: &Value| seen.insert(format!("{kind}\t{}", same(i)));
    let name = |l: &[&Value]| l.first().and_then(|i| i["name"].as_str()).map(String::from);
    let (an, tn, aln) = (name(&artists), name(&tracks), name(&albums));
    let top = match top_pick(q, [an.as_deref(), tn.as_deref(), aln.as_deref()]) {
        Some(0) => Some(("artist", artists[0])),
        Some(1) => Some(("track", tracks[0])),
        Some(2) => Some(("album", albums[0])),
        _ => None,
    };
    if let Some((kind, i)) = top {
        fresh(kind, i);
    }
    let tracks: Vec<&Value> = tracks.into_iter().filter(|i| fresh("track", i)).collect();
    let albums: Vec<&Value> = albums.into_iter().filter(|i| fresh("album", i)).collect();
    let artists: Vec<&Value> = artists.into_iter().filter(|i| fresh("artist", i)).collect();

    let track_row = |i: &Value, section: &str, kind: bool| {
        let t = parse_track(i)?;
        let sub = if kind { format!("Song · {}", t.artists) } else { format!("{} · {}", t.artists, t.album) };
        Some(tsv_row(&[&t.uri, &t.name, &sub, &cover(&i["album"]), &t.ms.to_string(), if t.explicit { "1" } else { "" }, section]))
    };
    // its artists, its year apart (on the right, as on an artist's page)
    let album_row = |i: &Value, section: &str, kind: bool| {
        let l = parse_link("album", i)?;
        let by = i["artists"].as_array().map(|a| a.iter().filter_map(|x| x["name"].as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default();
        let year = i["release_date"].as_str().and_then(|d| d.get(..4)).unwrap_or("");
        let what = if i["album_type"] == "single" { "Single" } else if i["album_type"] == "compilation" { "Compilation" } else { "Album" };
        let sub = if kind { format!("{what} · {by}") } else { by };
        Some(tsv_row(&[&l.uri, &l.name, &sub, &cover(i), "0", "", section, "", year]))
    };
    let artist_row = |i: &Value, section: &str| {
        let l = parse_link("artist", i)?;
        Some(tsv_row(&[&l.uri, &l.name, "Artist", &cover(i), "0", "", section]))
    };
    let mut rows = vec![];
    rows.extend(top.and_then(|(kind, i)| match kind {
        "artist" => artist_row(i, "Top result"),
        "track" => track_row(i, "Top result", true),
        _ => album_row(i, "Top result", true),
    }));
    rows.extend(tracks.iter().take(3).filter_map(|i| track_row(i, "Songs", false)));
    rows.extend(albums.iter().take(1).filter_map(|i| album_row(i, "Albums", false)));
    rows.extend(artists.iter().take(1).filter_map(|i| artist_row(i, "Artists")));
    rows
}

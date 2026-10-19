# spot

Tiny Spotify TUI: playlists, Liked Songs, search, albums and artists, with audio streamed by
[librespot](https://github.com/librespot-org/librespot), so no Spotify app is needed.
Needs Spotify Premium. macOS and Linux.

## Install

Prebuilt binaries for Apple Silicon macOS and x86_64 Linux:

    curl -fsSL https://raw.githubusercontent.com/boopdotpng/spot/master/install.sh | sh

It installs to `~/.local/bin/spot` (set `SPOT_INSTALL_DIR` to change). Run it again to update.

On anything else (Intel Macs, ARM Linux), or to build from source, use cargo (needs a Rust toolchain, https://rustup.rs):

    cargo install --locked --git https://github.com/boopdotpng/spot

Use `--locked`: vergen is pinned to 9.0.x in Cargo.lock, and librespot-core 0.8 fails to build
with vergen 9.1.

### Linux dependencies

ALSA, OpenSSL and D-Bus (audio, HTTPS, and MPRIS media keys). The prebuilt binary needs the
runtime libraries (usually already installed); building from source needs the headers:

    # Debian / Ubuntu
    sudo apt install build-essential pkg-config libasound2-dev libssl-dev libdbus-1-dev
    # Fedora
    sudo dnf install gcc pkg-config alsa-lib-devel openssl-devel dbus-devel
    # Arch
    sudo pacman -S base-devel pkgconf alsa-lib openssl dbus

## First run

spot needs a Spotify app of your own (free, takes a minute). On first start it asks for one:

1. Open https://developer.spotify.com/dashboard and create an app.
2. Add the redirect URI `http://127.0.0.1:19872/login`.
3. Tick **Web API** and save.
4. Paste the app's Client ID when spot asks for it.

It's saved to `~/.config/spot/client_id` (delete it to set up again, or set `SPOT_CLIENT_ID`
to override). Using a different redirect URI? Set `SPOT_REDIRECT` to match.
Then log in through the browser; there's one more one-time approval for the audio session.

## Last.fm (optional)

Skip this section if you don't use Last.fm; scrobbling stays off unless it's set up.
spot can scrobble what you play to Last.fm:

1. Create an API account at https://www.last.fm/api/account/create with callback URL
   `http://127.0.0.1:19872/lastfm` (same host and port as `SPOT_REDIRECT`, if you changed it).
2. Put the API key and shared secret in `~/.config/spot/lastfm`, one per line (or set
   `LASTFM_API_KEY` and `LASTFM_SECRET`).
3. Start spot and approve it in the browser (once; the session is kept in `~/.cache/spot/lastfm_session`).
   Press `enter` to skip the approval and start without scrobbling.

To turn scrobbling off without deleting the keys, set `SPOT_LASTFM=off`.

Last.fm's rules apply: tracks over 30s scrobble once you've played half of them or 4 minutes,
whichever comes first. Only time actually playing counts, so seeking ahead doesn't. The song
shows as "now playing" on your profile while it plays. Scrobbles that can't be sent (offline)
wait in `~/.cache/spot/scrobbles` and go out later.

## Keys

| key | action |
|---|---|
| `/` | search (`tab` in the prompt cycles songs / albums / artists) |
| `enter` | play song (queues the rest of the list) · open album / artist |
| `ctrl+t` | next theme (desert, lagoon, moss, iris, blossom, mint, sorbet); remembered, light/dark still follows the terminal |
| `ctrl+f` | filter the open list; `enter` plays the match but keeps the whole list as the queue, `esc` closes |
| `space` | pause / resume (with a short volume fade); right after launch, resumes what was playing at quit |
| `s` | shuffle on/off |
| `r` | repeat: off → repeat list → repeat one song |
| `P` | play the whole open list (from a random song if shuffle is on) |
| `n` / `p` | next / previous (previous restarts the song if >3s in) |
| `,` / `.` | seek −10s / +10s |
| `+` / `-` | volume |
| `o` | jump to the list that's playing |
| `a` / `A` | go to the album / artist of the highlighted song (or the playing one, from the playlist pane) |
| `v` | on an artist page: all songs ↔ albums & singles (remembered) |
| `f` | on an artist's releases: all → albums (incl. EPs) → singles (remembered) |
| `esc`, `backspace` | back to the previous list |
| `tab`, `h`/`l`, `←`/`→` | switch pane |
| `j`/`k`, `↑`/`↓`, `g`/`G`, `ctrl+d`/`ctrl+u` | move |
| `?` | key help (any key closes it) |
| `q` | quit |

Media keys and the macOS Now Playing widget work too.

## Notes

- **Shuffle** is a uniform Fisher–Yates shuffle of the whole list (OS-seeded ChaCha RNG):
  every order is equally likely, no artist/popularity weighting. The playing song stays first.
  With repeat on, each pass through the list gets a fresh shuffle.
- **Artists** open as every song (`v` switches to their albums & singles, to pick one) and play their full discography (albums + singles/EPs, newest first, duplicate
  single/album versions removed). Shuffle + `P` = shuffle the whole discography.
- **Quality**: 320 kbps Ogg Vorbis, the highest librespot can stream. Spotify's lossless (FLAC)
  isn't exposed to third-party/Connect players, so librespot can't select it.
- Audio: librespot (no Spotify app needed).
- Playlist list, Liked Songs, search: Web API through your own Spotify app (see First run).
- Playlist / album / artist contents: Spotify's internal metadata endpoints (dev-mode Web API
  apps can't read playlists you don't own).
- Login state lives in `~/.cache/spot` (delete it to log out).
- **Logs**: a detailed log (spot + librespot at debug, including why a track failed to play) is
  always written to `~/.cache/spot/log`; the previous run's is kept as `log.1`.
- **Startup**: playlists, Liked Songs and playlist contents are cached in `~/.cache/spot`, so
  they show instantly and refresh in the background (usually one request to confirm Liked Songs
  is current). The audio session connects in the background too.
- **Albums & artists** you've opened before (track lists and releases) also show instantly from
  the cache and refresh in the background.
- **Audio cache**: played songs are kept in `~/.cache/spot/audio` (as streamed, encrypted), so
  replaying them starts instantly without the network. Capped at 1 GB, least recently played
  dropped first; set `SPOT_AUDIO_CACHE_GB` to change the cap (`0` turns it off).
- **`spot clean`** clears the caches (songs, playlist / album / artist lists) and prints how much
  it freed. Login, settings, the resume point and unsent scrobbles are kept.
- **Resume**: quitting saves the queue, shuffle/repeat and position; next launch shows that song
  paused and preloads it once connected, so `space` starts it right away.
- **Theme**: picks a light or dark palette from the terminal's background color at startup
  (OSC 11; Ghostty, iTerm2, kitty, WezTerm, …) and follows live changes where the terminal
  reports them (Ghostty). Force one with `SPOT_THEME=light` / `dark`.
  `ctrl+t` cycles color themes, each with a light and dark variant.
  The status line sits on a faint tint of the terminal's own background (needs truecolor).
- **Layout**: playlists on the left, the open list (numbered rows) on the right, the player under
  a rule across the bottom, and a vim-style status line last: it holds the search / find prompts,
  notices, key hints for the focused pane, the cursor position and the volume. `?` swaps it for
  a key cheatsheet.

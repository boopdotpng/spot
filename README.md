# spot

Tiny Spotify TUI. Needs Premium. Run `spot`.

## First run

spot needs a Spotify app of your own (free, takes a minute). On first start it asks for one:

1. Open https://developer.spotify.com/dashboard and create an app.
2. Add the redirect URI `http://127.0.0.1:19872/login`.
3. Tick **Web API** and save.
4. Paste the app's Client ID when spot asks for it.

It's saved to `~/.config/spot/client_id` (delete it to set up again, or set `SPOT_CLIENT_ID`
to override). Using a different redirect URI? Set `SPOT_REDIRECT` to match.
Then log in through the browser; there's one more one-time approval for the audio session.

## Keys

| key | action |
|---|---|
| `/` | search (`tab` in the prompt cycles songs / albums / artists) |
| `enter` | play song (queues the rest of the list) · open album / artist |
| `ctrl+t` | next theme (desert, lagoon, moss, iris, blossom, mint, sorbet); remembered, light/dark still follows the terminal |
| `ctrl+f` | filter the open list; `enter` plays the match but keeps the whole list as the queue, `esc` closes |
| `space` | pause / resume (with a short volume fade) |
| `s` | shuffle on/off |
| `r` | repeat: off → repeat list → repeat one song |
| `P` | play the whole open list (from a random song if shuffle is on) |
| `n` / `p` | next / previous (previous restarts the song if >3s in) |
| `,` / `.` | seek −10s / +10s |
| `+` / `-` | volume |
| `o` | jump to the list that's playing |
| `tab`, `h`/`l`, `←`/`→` | switch pane |
| `j`/`k`, `↑`/`↓`, `g`/`G`, `ctrl+d`/`ctrl+u` | move |
| `q` | quit |

Media keys and the macOS Now Playing widget work too.

## Notes

- **Shuffle** is a uniform Fisher–Yates shuffle of the whole list (OS-seeded ChaCha RNG):
  every order is equally likely, no artist/popularity weighting. The playing song stays first.
  With repeat on, each pass through the list gets a fresh shuffle.
- **Artists** play their full discography (albums + singles/EPs, newest first, duplicate
  single/album versions removed). Shuffle + `P` = shuffle the whole discography.
- **Quality**: 320 kbps Ogg Vorbis, the highest librespot can stream. Spotify's lossless (FLAC)
  isn't exposed to third-party/Connect players, so librespot can't select it.
- Audio: librespot (no Spotify app needed).
- Playlist list, Liked Songs, search: Web API through your own Spotify app (see First run).
- Playlist / album / artist contents: Spotify's internal metadata endpoints (dev-mode Web API
  apps can't read playlists you don't own).
- Login state lives in `~/.cache/spot` (delete it to log out).
- **Theme**: picks a light or dark palette from the terminal's background color at startup
  (OSC 11; Ghostty, iTerm2, kitty, WezTerm, …) and follows live changes where the terminal
  reports them (Ghostty). Force one with `SPOT_THEME=light` / `dark`.
  `ctrl+t` cycles color themes, each with a light and dark variant.

## Build

    cargo build --release
    rm -f ~/.cargo/bin/spot && cp target/release/spot ~/.cargo/bin/

`rm` first: overwriting a binary in place makes macOS kill it (stale code signature).
vergen is pinned to 9.0.x in Cargo.lock; librespot-core 0.8 fails to build with vergen 9.1.

### Linux

Needs a Rust toolchain (https://rustup.rs) plus ALSA, OpenSSL and D-Bus headers
(audio, HTTPS, and MPRIS media keys):

    # Debian / Ubuntu
    sudo apt install build-essential pkg-config libasound2-dev libssl-dev libdbus-1-dev
    # Fedora
    sudo dnf install gcc pkg-config alsa-lib-devel openssl-devel dbus-devel
    # Arch
    sudo pacman -S base-devel pkgconf alsa-lib openssl dbus

    cargo build --release --locked
    cp target/release/spot ~/.local/bin/

Use `--locked` so the vergen pin above is respected.

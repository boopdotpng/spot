# spot, for coding agents

spot is a small Spotify TUI for Apple Silicon Macs, with a command line (`spot help`). Audio comes
from librespot, so no Spotify app is needed, but Spotify Premium is. The player runs in the
background; the TUI (ratatui) is a disposable client that attaches to it over a Unix socket. Only
macOS is tested. The platform-specific code is Now Playing and the media keys in
`src/nowplaying.rs` (attached on the main thread, whose run loop `src/main.rs` runs).

## Building it for someone

1. Install Rust with https://rustup.rs (stable; the crate is edition 2024) and the Xcode
   command line tools (`xcode-select --install`). Full Xcode isn't needed.
2. `git clone https://github.com/boopdotpng/spot && cd spot && ./install.sh`. This makes a
   release build and copies it to `~/.local/bin/spot`, and builds the ⌥Space launcher into
   `~/Applications/Spot Launcher.app` (it starts at login). The first build takes a few minutes;
   later ones are incremental.
3. Make sure `~/.local/bin` is on `PATH`.
4. First run: `spot` in a terminal asks for the Client ID of a Spotify app of the user's own. They
   create it at https://developer.spotify.com/dashboard with the redirect URI
   `http://127.0.0.1:19872/login` and **Web API** ticked. Then it logs in through the browser. The
   user has to do this part; see "First run" in the README.

To update, `git pull` and run `./install.sh` again. It restarts a running player on the new build
(still playing); an open TUI needs `spot` again.

## Working on it

- While working, build and test with debug builds only: `cargo build`, `cargo test`, and run
  `target/debug/spot`. An incremental debug build takes about a second.
- Run `./install.sh` only once, at the end, when the change is finished and the user wants it
  installed. Don't run it, or `cargo build --release`, to check each step.
- Never build `--profile dist` locally. It's the fully optimized build releases ship, and a
  single rebuild takes several minutes.
- Don't hide cargo's output with `grep`. Pipe it through `tail` instead, so a "Blocking waiting
  for file lock" message still shows.
- Only one player runs at a time: it holds a lock in `~/.cache/spot`. `target/debug/spot` attaches
  to whatever player is running, even the installed one; `spot quit` first to test your build's
  player.
- To check the TUI without a terminal of your own, run it in tmux
  (`tmux new-session -d -x 140 -y 40 target/debug/spot`), drive it with `tmux send-keys` and read
  it with `tmux capture-pane -p`. Inside tmux the cover is drawn in half blocks.
- The log is at `~/.cache/spot/log` (the previous run's at `log.1`).

## Dependencies

- **librespot-core** is vendored in `vendor/librespot-core` with a fix for Spotify refusing audio
  keys. `vendor/README.md` explains it.
- Builds use `--locked`.
- Logging is a small `log::Log` in `src/main.rs` (levels in `LOG_LEVELS`), not env_logger.

## Where things are

Each file starts with a comment saying what it holds.

- `src/main.rs`: startup, the main loop, and the command line's entry point.
- `src/app.rs`, `src/model.rs`: the player's state and the messages its main loop runs on.
- `src/views.rs`, `src/artist.rs`, `src/keys.rs`: what the right pane shows, artist pages, and keys.
- `src/ui.rs`, `src/theme.rs`, `src/art.rs`: drawing the TUI, its colors, and the cover.
- `src/terminal.rs`: attaching a terminal to the background player, and the client side of it.
- `src/playback.rs`: the list, the queue, skipping, seeking, volume.
- `src/nowplaying.rs`: macOS Now Playing and the media keys, straight to MediaPlayer on objc2.
- `src/lists.rs`, `src/metadata.rs`, `src/webapi.rs`: fetching from Spotify (its internal metadata
  API for list contents, the Web API for the library and search).
- `src/cache.rs`: everything on disk under `~/.cache/spot`.
- `src/ctl.rs`, `src/cli.rs`: the command line (`spot play`, `spot queue`, …) over a Unix socket.
- `src/settings.rs`: `~/.config/spot/config.toml`, documented in `docs/config.md`.
- `src/launch.rs`: what the ⌥Space launcher asks the player (`spot launcher …`, tab-separated).
- `mac/Launcher.swift`: the ⌥Space launcher, plain AppKit on Liquid Glass. Keep it redrawing
  rarely: redrawing its card twice a second held ~100 MB of window server buffers.
- `install.sh`: builds and installs spot and the launcher.
- `docs/skills/spot/SKILL.md`: the Claude Code skill for driving spot from the command line.

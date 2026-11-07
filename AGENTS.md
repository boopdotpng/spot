# spot, for coding agents

spot is a small Spotify player for Apple Silicon Macs (macOS 14+): a native window drawn with
Rune, a `⌥Space` search panel, and a command line (`spot help`). Audio comes from librespot, so
no Spotify app is needed, but Spotify Premium is. Only macOS is supported. Porting to another
platform is up to you: the AppKit code is in `src/gui/menus.rs` and `src/gui/launcher.rs`.

## Building it for someone

1. Install Rust with https://rustup.rs (stable; the crate is edition 2024) and the Xcode
   command line tools (`xcode-select --install`). Full Xcode isn't needed.
2. `git clone https://github.com/boopdotpng/spot && cd spot && macos/bundle.sh`. This makes a
   release build, installs `~/Applications/Spot.app`, links `~/.local/bin/spot` to it, and adds a
   login item that starts spot without its window (`--no-login` leaves that out). The first
   build takes a few minutes; later ones are incremental.
3. Make sure `~/.local/bin` is on `PATH`.
4. First run: `spot` asks for the Client ID of a Spotify app of the user's own. They create it at
   https://developer.spotify.com/dashboard with the redirect URI `http://127.0.0.1:19872/login`
   and **Web API** ticked. Then it logs in through the browser. The user has to do this part;
   see "First run" in the README.

To update, `git pull` and run `macos/bundle.sh` again, then `spot quit` and `spot`.

## Working on it

- While working, build and test with debug builds only: `cargo build`, `cargo test`, and run
  `target/debug/spot`. An incremental debug build takes about a second.
- Run `macos/bundle.sh` only once, at the end, when the change is finished and the user wants it
  installed. Don't run it, or `cargo build --release`, to check each step.
- Never build `--profile dist` locally. It's the fully optimized build releases ship, and a
  single rebuild takes several minutes.
- Don't hide cargo's output with `grep`. Pipe it through `tail` instead, so a "Blocking waiting
  for file lock" message still shows.
- Only one spot runs at a time: it holds a lock in `~/.cache/spot`. While the installed Spot.app
  is running, `target/debug/spot` won't start a second player; run `spot quit` first.
- `SPOT_RUNE` passes Rune's own flags for rendering without a window (`--snap out.png`,
  `--tree`, `--script`), for checking the UI. The headless clock doesn't wait for the network, so
  only cached lists show.
- The log is at `~/.cache/spot/log` (the previous run's at `log.1`).

## Dependencies

- **Rune** is pre-release and comes from spot's fork: the `spot` branch of
  https://github.com/boopdotpng/rune (upstream's dev branch plus spot's fixes), pinned to a rev in
  `Cargo.toml`. To change Rune, commit to that branch and bump the rev. To try a change first,
  point a `[patch."https://github.com/boopdotpng/rune"]` at a local checkout in an untracked
  `.cargo/config.toml`, and don't commit the `Cargo.lock` it produces.
- **librespot-core** is vendored in `vendor/librespot-core` with a fix for Spotify refusing audio
  keys. `vendor/README.md` explains it.
- Builds use `--locked`. vergen is pinned to 9.0.x in `Cargo.lock`, and librespot-core 0.8 fails
  to build with vergen 9.1.

## Where things are

Each file starts with a comment saying what it holds.

- `src/main.rs`: startup, and the command line's entry point.
- `src/app.rs`, `src/model.rs`: the player's state and the messages its main loop runs on.
- `src/views.rs`, `src/artist.rs`, `src/keys.rs`: what the window shows, artist pages, and keys.
- `src/playback.rs`: the list, the queue, skipping, seeking, volume.
- `src/lists.rs`, `src/metadata.rs`, `src/webapi.rs`: fetching from Spotify (its internal metadata
  API for list contents, the Web API for the library and search).
- `src/cache.rs`: everything on disk under `~/.cache/spot`.
- `src/ctl.rs`, `src/cli.rs`: the command line (`spot play`, `spot queue`, …) over a Unix socket.
- `src/settings.rs`: `~/.config/spot/config.toml`, documented in `docs/config.md`.
- `src/gui/`: the window (`view.rs`, `rows.rs`, `artist.rs`, `player.rs`, `prefs.rs`), the
  `⌥Space` panel (`panel.rs`, `launcher.rs`) and the menu bar (`menus.rs`).
- `macos/bundle.sh`: builds and installs Spot.app; `macos/icon/`: the icon (`icon.svg` is the
  source of `spot.icns`).
- `docs/skills/spot/SKILL.md`: the Claude Code skill for driving spot from the command line.

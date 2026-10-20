---
name: spot
description: Play and control Spotify music through the `spot` CLI. Use when the user asks to play, queue, skip, pause or search for music, change volume, see what's playing, or have you DJ.
allowed-tools: Bash Monitor
---

# spot

Full reference: `spot help`.

- `spot status`: if it exits 3, nothing is running, so `spot start` (headless, returns once ready). Never start a second player, and don't `spot quit` unless asked. "not set up yet" means the user has to run `spot` once in a terminal to log in.
- `play` doesn't search. `spot search <words>` (`--albums`, `--artists`, `--playlists`, `-n 20`, field filters like `artist:x year:1997`), then `spot play N` for result N. Prefer the original over covers and live versions.
- A song plays now and the queue continues after it. An album, artist or playlist replaces the queue. `spot play liked` plays Liked Songs; `spot play <playlist name>` plays one of theirs.
- `spot queue add N…` plays those results next. Controls: `pause`, `play`, `next`, `prev`, `seek +10`, `vol 40`, `shuffle on`, `repeat all`, `queue`.
- DJ: Monitor `spot watch --before 30`. On each `ending` line (it shows what's next), `spot queue add` something else if it should change.

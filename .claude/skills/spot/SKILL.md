---
name: spot
description: Play and control Spotify music through the `spot` CLI. Use when the user asks to play, queue, skip, pause or search for music, change volume, see what's playing, or have you DJ.
allowed-tools: Bash Monitor
---

# spot

Full reference: `spot help`.

- `spot status`: if it exits 3, nothing is running, so `spot start` (headless, returns once ready). Never start a second player, and don't `spot quit` unless asked. "not set up yet" means the user has to run `spot` once in a terminal to log in.
- `spot play <words>` plays one of their playlists by name, else Spotify's top song for the words; `spot play album|artist|song|playlist <words>` picks the kind. The first output line says what it searched (`the top song for '…':`), so check the result is what they meant. If several of their playlists match, it lists them numbered: `spot play N`.
- For anything the top hit might get wrong (a specific version, a cover vs the original), `spot search <words>` (`--albums`, `--artists`, `--playlists`, `-n 20`, field filters like `artist:x year:1997`), then `spot play N` for result N. Prefer the original over covers and live versions.
- A song plays now, then the queue and the list carry on. An album, artist or playlist replaces the list (the queue stays). `spot play liked` plays Liked Songs.
- `spot queue N…` (or `queue add`) appends results to the queue (`spot queue <words>` works like play), which plays before the list carries on. `spot queue` shows it; `spot queue rm N` removes by queue position. Controls: `pause`, `play`, `next`, `prev`, `seek +10`, `vol 40`, `shuffle on`, `repeat all`, `queue`.
- DJ: Monitor `spot watch --before 30`. On each `ending` line (it shows what's next), `spot queue add` the next pick if the queue is empty or `next` isn't right.

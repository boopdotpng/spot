---
name: spot
description: Play and control Spotify music through the `spot` CLI. Use when the user asks to play, queue, skip, pause or search for music, change volume, see what's playing, or have you DJ.
allowed-tools: Bash Monitor
---

# spot

Full reference: `spot help`. Everything is named by uri; `status`, `search` and `library` print them.

- `spot status` first. Exit code 3 means it isn't running: `spot start` (headless, returns once ready; it comes back paused where it was, so `spot set paused=no` if they want music). Never start a second player, and don't `spot quit` unless asked. "not set up yet" means the user has to run `spot` once in a terminal to log in.
- Finding music: `spot search <words>` (songs; `--type album|artist|playlist|show|episode`, `-n 20`, filters like `artist:x year:1997`), or `spot library` for their Liked Songs, playlists and podcasts. Prefer the original over covers and live versions.
- `spot play <uri>`: a song plays now, then the queue and the list carry on; an album, artist, playlist or show replaces the list (`--from <song uri>` starts it there).
- `spot queue <uri>...` sets the whole queue in order, so include what's already queued that should stay (from `status`). It plays before the list carries on. `spot queue clear` empties it.
- `spot set paused=yes|no volume=40|+10 shuffle=on repeat=off|all|one seek=1:30|+10`, `spot next`, `spot prev`. Commands that change something print the status after; check it.
- DJ: Monitor `spot watch`. On each `ending` line (30s before a song ends, with what's next), set the queue if it's empty or the next song isn't right. A `skip` line means the user skipped or seeked with a media key, e.g. passing on a song.

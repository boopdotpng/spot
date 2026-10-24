# vendor

`librespot-core/` is librespot-core 0.8.0 from crates.io, patched in through `[patch.crates-io]` in `Cargo.toml`. Every change is marked `spot patch`.

Spotify rate-limits audio key requests per account: a burst of about 8, then roughly one every 30 seconds. When the limit is hit it answers `error audio key 0 2`. Stock librespot asks once, then decodes the still-encrypted file, which fails, so spot skipped to the next track. Each skip used up more requests, which turned one refusal into a run of "can't play that track". The player also asks a second time for every failed load.

- `src/audio_key.rs`: retries `0 2` refusals and timeouts for up to 40 seconds, with backoff capped at 2 seconds. Refused requests don't seem to count against the limit. Other codes (`0 1`) fail at once. After giving up on a file, it fails at once for 10 seconds, so the player's second request doesn't wait another 40.
- `src/cache.rs`: keys are cached in `~/.cache/spot/keys/` (a key never changes), so replays don't use the limit at all.

Drop the vendored copy when upstream handles this (librespot PR #1763 is the closest attempt).

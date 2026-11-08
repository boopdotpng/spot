# Config

spot reads `~/.config/spot/config.toml` (`$XDG_CONFIG_HOME/spot/config.toml` if that's set). The
file is optional, and so is every key in it. The Settings page (`⌘,`) changes every key here,
writing just that line and keeping the rest of the file.

The first launch writes it with every setting commented out; uncomment one to change it. Leaving
them commented means you pick up new defaults.

spot reads it when the player starts, so run `spot quit` and start it again after editing it by
hand. From Settings, the page offers a restart. A mistake in the file shows as a message in the window and in the log, and that setting keeps
its default.

## Settings

| Key | Default | Values | What it does |
| --- | --- | --- | --- |
| `client_id` | none | your Spotify app's client id, in quotes | Saved here by the first-run setup. `SPOT_CLIENT_ID` overrides it. |
| `quality` | `320` | `96`, `160`, `320` | Audio quality in kbps. 320 is the most librespot can stream. |
| `audio_cache_gb` | `1` | `0` or above; decimals work | How much played music to keep on disk, least recently played dropped first. `0` turns the cache off. `SPOT_AUDIO_CACHE_GB` overrides it. |
| `normalize` | `true` | `true`, `false` | Play every song at about the same loudness, using Spotify's loudness data. |
| `hotkey` | `"option+space"` | modifiers (`ctrl`, `option`, `cmd`, `shift`) and a key (a letter, digit, `space`, `return`, `tab`, `f1`-`f12`), joined with `+`; f-keys need no modifier | What opens the `⌥Space` search, from any app. Takes effect when spot restarts. |
| `appearance` | `"system"` | `"system"`, `"light"`, `"dark"` | Light or dark colors. `"system"` follows macOS, and changes with it. Takes effect at once from Settings. |

```toml
client_id = "0123456789abcdef0123456789abcdef"
quality = 320
audio_cache_gb = 1
normalize = true
hotkey = "option+space"
appearance = "system"
```

`client_id` goes above any section, like every key in this table. Installs from before
config.toml keep theirs in `~/.config/spot/client_id`, which still works.

## Colors

spot has no color themes (`appearance` above picks only light or dark): the window is near-monochrome, and its one accent comes from the playing
song's cover. `[themes…]` sections from older versions are ignored.

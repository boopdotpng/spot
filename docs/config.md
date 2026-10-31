# Config

spot reads `~/.config/spot/config.toml` (`$XDG_CONFIG_HOME/spot/config.toml` if that's set). The
file is optional, and so is every key in it.

The first launch writes it with every setting commented out; uncomment one to change it. Leaving
them commented means you pick up new defaults.

spot reads it when the player starts, so run `spot quit` and start it again after a change. A
mistake in the file shows on the status line and in the log, and that setting keeps its default.

## Settings

| Key | Default | Values | What it does |
| --- | --- | --- | --- |
| `client_id` | none | your Spotify app's client id, in quotes | Saved here by the first-run setup. `SPOT_CLIENT_ID` overrides it. |
| `quality` | `320` | `96`, `160`, `320` | Audio quality in kbps. 320 is the most librespot can stream. |
| `audio_cache_gb` | `1` | `0` or above; decimals work | How much played music to keep on disk, least recently played dropped first. `0` turns the cache off. `SPOT_AUDIO_CACHE_GB` overrides it. |
| `popup` | `true` | `true`, `false` | macOS launcher: show what's playing when a media key skips or seeks. |
| `popup_seconds` | `3` | above `0`; decimals work | How long the popup stays up. |

```toml
client_id = "0123456789abcdef0123456789abcdef"
quality = 320
audio_cache_gb = 1
popup = true
popup_seconds = 3
```

`client_id` goes above any `[themes…]` section, like every key in this table. Installs from before
config.toml keep theirs in `~/.config/spot/client_id`, which still works.

## Themes

Themes color the TUI. The macOS launcher doesn't use them: it's Liquid Glass with your system
accent color.

The built-in themes are `desert`, `lagoon`, `moss`, `iris`, `blossom`, `mint` and `sorbet`. A
`[themes.<name>.dark]` or `[themes.<name>.light]` section changes one of those, or adds a new one
to the end of the `ctrl+t` cycle. Light or dark follows your terminal.

- Changing a built-in: name only the colors to change.
- A new theme: unnamed colors come from `desert`. With no `.light` section, its dark colors are
  used for both.

Colors are written `"#rrggbb"`.

| Color | Used for |
| --- | --- |
| `accent` | focus, what's playing |
| `accent2` | the context's name |
| `deep` | unfocused selection, progress track |
| `hl` | focused selection background |
| `text` | text |
| `sub` | secondary text |
| `dim` | borders, durations |
| `gold` | shuffle, repeat, prompt labels |
| `pink` | Liked Songs |
| `err` | errors |

```toml
# a brighter accent for moss
[themes.moss.dark]
accent = "#88cc55"

# a new theme
[themes.midnight.dark]
accent = "#7aa2f7"
text = "#c0caf5"
sub = "#7982a9"
```

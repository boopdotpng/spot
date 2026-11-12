# Config

spot reads `~/.config/spot/config.toml` (`$XDG_CONFIG_HOME/spot/config.toml` if that's set). The
file is optional, and so is every key in it.

The first launch writes it with every setting commented out; uncomment one to change it. Leaving
them commented means you pick up new defaults.

spot reads it when the player starts, so run `spot quit` and start it again after editing it. A
mistake in the file shows as a message on the TUI's status line and in the log, and that setting
keeps its default.

## Settings

| Key | Default | Values | What it does |
| --- | --- | --- | --- |
| `client_id` | none | your Spotify app's client id, in quotes | Saved here by the first-run setup. `SPOT_CLIENT_ID` overrides it. |
| `quality` | `320` | `96`, `160`, `320` | Audio quality in kbps. 320 is the most librespot can stream. |
| `audio_cache_gb` | `1` | `0` or above; decimals work | How much played music to keep on disk, least recently played dropped first. `0` turns the cache off. `SPOT_AUDIO_CACHE_GB` overrides it. |
| `normalize` | `true` | `true`, `false` | Play every song at about the same loudness, using Spotify's loudness data. |

```toml
client_id = "0123456789abcdef0123456789abcdef"
quality = 320
audio_cache_gb = 1
normalize = true
```

`client_id` goes above any section, like every key in this table. Installs from before
config.toml keep theirs in `~/.config/spot/client_id`, which still works.

## Themes

Themes color the TUI; `ctrl+t` cycles them, and the one picked is remembered.

The built-in themes are `desert`, `lagoon`, `moss`, `iris`, `blossom`, `mint` and `sorbet`. A
`[themes.<name>.dark]` or `[themes.<name>.light]` section changes one of those, or adds a new one
to the end of the `ctrl+t` cycle. Light or dark follows your terminal (`SPOT_THEME=light` or `dark` forces it).

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

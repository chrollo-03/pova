# Keys

pova uses btop's idea of keys: one keystroke per thing, with the important
letters highlighted in the borders. Press `?` inside pova for the full list,
including any keys you've remapped.

| Key | Does | Key | Does |
|---|---|---|---|
| `space` | play / pause | `n` `q` `l` `/` `b` | now playing, queue, lyrics, search, library |
| `←` `→` | seek 5 s | `tab` / `shift+tab` | next / previous tab |
| `H` `L` | seek 30 s | `1` `2` `3` | show / hide the three boxes |
| `<` `>` or `P` `N` | previous / next track | `j` `k` `↑` `↓` `g` `G` | move around lists |
| `+` `-` `m` | volume, mute | `enter` | play or open the selection |
| `S` `R` | shuffle, repeat (off / all / one) | `a` / `A` | add to queue / play next |
| `f` | like or unlike the current song | `r` | start a radio from the selection; on the playing song it keeps playing and only what's next is replaced |
| `F` | dislike the current song and skip it | | |
| `p` | add the selected (or playing) song to one of your playlists, or a new one | `d` in Library | remove the selected song from your open playlist |
| `D` in Library | delete the selected or open playlist (asks yes / no first, starts on no) | | |
| `v` | change visualizer mode | `d`, `J` `K`, `C` | queue: remove, move, clear |
| `t` | theme picker (live preview) | `[` `]` | change search filter / library section |
| `z` | sleep timer (countdown with fade-out, or end of track) | `i` | interlink & signal path (artist, album, links, codec/bitrate) |
| `e` | 10-band graphic equalizer & presets | `B` | set / clear A-B section loop |
| `y` | copy universal interlink (`song.link`) | `(` `)` | playback speed slower / faster |
| `W` | listening stats (Spotify Wrapped Mini) | `O` | save / remove track for offline playback |
| `esc` | back, or the menu | `c` | jump to the playing track |
| `Q` | quit | `F5` | reload the current list |

**In the search box:** type, and suggestions appear underneath. `↑` `↓` pick
one, `→` accepts the dimmed completion, `tab` changes the filter (Songs,
Videos, Albums, Artists, Playlists), `enter` searches. You can also paste any
YouTube Music, YouTube, Spotify, Tidal, or `song.link` URL straight into the
search box and press `enter` to open or play it. `ctrl+w` deletes the word
before the cursor and `ctrl+u` everything before it.

**Interactive synced lyrics:** in the Lyrics tab (`l`), press `↑` / `↓` and
`enter` — or click any synced lyric line — to jump straight to that moment in
the song. Press `c` (or `esc`) to return to live auto-follow.

**The Esc menu.** Like btop, pressing `esc` when there's nothing to go back
from opens a menu with the pova logo drawn in block characters, in the logo's
own violet-to-pink gradient. From it you pick a theme, change the visualizer
mode, switch autoplay, album art, even loudness, notifications, Tidal-style
audio quality tier, Spotify-style crossfade, 10-band equalizer, sleep timer,
playback speed, pitch lock, and queue deduplication. Your choices are saved.

The mouse works too: click tabs, buttons, list rows and the progress bar, and
scroll through lists. Click or scroll the volume bar to set the volume, click
`vol` or the percentage to mute, and click the `v <mode>` badge on the
visualizer to switch modes. In the help, theme and Esc menu popups the mouse
wheel scrolls, `g` / `G` jump to the top / bottom, and in the Esc menu `←` / `→`
change a setting backwards / forwards.

To remap keys, see [configuration.md](configuration.md).

Back to the [README](../README.md).

# Controlling pova from outside

While pova is open in a terminal, you can drive it from anywhere else: a
keyboard shortcut, a script or a status bar. Run `pova` followed by a command:

| Command | Does |
|---|---|
| `pova toggle` | play / pause |
| `pova play`, `pova pause` | resume, pause |
| `pova next`, `pova prev` | next track, previous track (or restart the current one) |
| `pova like` | like or unlike the current song (needs you to be signed in) |
| `pova dislike` | dislike the current song and skip it (needs you to be signed in) |
| `pova volume 40`, `pova volume +5`, `pova volume -5` | set or change the volume |
| `pova seek 90`, `pova seek +10`, `pova seek -10` | jump to a time, or skip in seconds |
| `pova add <song>` | search and add the top result to the end of the queue |
| `pova play-next <song>` | search and play the top result after the current song |
| `pova play-now <song>` | search and play the top result right away |
| `pova open <url>` | open a YouTube Music, YouTube, Spotify, Tidal, or `song.link` URL |
| `pova sleep 30`, `pova sleep track`, `pova sleep +5`, `pova sleep off` | set, adjust, query, or cancel the sleep timer |
| `pova speed 1.25`, `pova speed +`, `pova speed -` | set or step playback speed |
| `pova eq bass`, `pova eq flat`, `pova eq off` | apply an equalizer preset or turn EQ off/on |
| `pova quality max`, `pova quality low` | switch audio quality tier (`max`, `high`, `normal`, `low`) |
| `pova ab-loop` | set point A, set point B, or clear A-B section loop |
| `pova stats` | print your local listening stats summary (plays, hours, streak, top artist/track) |
| `pova offline` | save or remove the playing track in the local offline cache |
| `pova discord on`, `pova discord off`, `pova discord toggle` | control Discord Rich Presence |
| `pova status` | print what's playing (see below) |

The search commands take plain words, no quotes needed:
`pova add seether fine again`.

If pova isn't running, or a command can't be done (the queue is empty, say),
it prints why and exits with status 1, so scripts can tell.

## Keyboard shortcuts

Bind the commands to keys in your desktop or window manager. For example:

**KDE Plasma:** System Settings → Keyboard → Shortcuts → Add New → Command
or Script, then enter `pova toggle` and pick a key.

**Hyprland** (`~/.config/hypr/hyprland.conf`):

```
bind = SUPER, P, exec, pova toggle
bind = SUPER, bracketright, exec, pova next
bind = SUPER, bracketleft, exec, pova prev
```

**sway / i3** (`~/.config/sway/config`):

```
bindsym $mod+p exec pova toggle
bindsym $mod+bracketright exec pova next
```

Media keys and the Plasma media widget already work through
[mpv-mpris](https://github.com/hoyon/mpv-mpris); these commands add search,
the queue and likes on top.

## Status bars

`pova status` prints one line, `{artist} - {title}` by default, and nothing
when no song is loaded. Choose your own format with `-f`:

```sh
pova status -f '{state}: {artist} - {title} ({position}/{duration}) {liked}'
# playing: Seether - Fine Again (1:03/4:03) ♥
```

Placeholders: `{title}` `{artist}` `{album}` `{state}` (playing or paused)
`{position}` `{duration}` `{volume}` `{liked}` (♥ or nothing)
`{sleep}` (countdown or `end of track`) `{quality}` (e.g. `MAX · OPUS 48kHz · 158k`)
`{speed}` (e.g. `1.0x`) `{eq}` (active preset or `off`).

**waybar** (`~/.config/waybar/config`):

```json
"custom/pova": {
    "exec": "pova status 2>/dev/null",
    "interval": 2,
    "on-click": "pova toggle",
    "on-scroll-up": "pova next",
    "on-scroll-down": "pova prev"
}
```

**polybar:**

```ini
[module/pova]
type = custom/script
exec = pova status 2>/dev/null
interval = 2
click-left = pova toggle
```

## How it works

pova listens on a Unix socket at `$XDG_RUNTIME_DIR/pova/control.sock`, which
only your user can reach. If you open a second pova, the first one keeps the
socket. Each command is one line of text, and the answer is one line starting
with `ok` or `error`.

Back to the [README](../README.md).

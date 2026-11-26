# Configuration and themes

`~/.config/pova/config.toml` is created on first run with every option in it:

```toml
theme = "Default"          # any btop theme name: "dracula", "gruvbox_dark", "tokyo-night"...
theme_background = true    # false keeps your terminal's own (maybe transparent) background
rounded_corners = true
volume = 70
autoplay = true            # keep playing similar songs when the queue ends
album_art = true
image_protocol = "auto"    # auto | kitty | sixel | iterm2 | halfblocks
mpv_args = []              # extra flags for mpv, if you want them
resume = true              # start where you left off: same queue, same spot, paused
normalize_loudness = true  # play every song at the same loudness
notifications = false      # desktop popup with the cover art when the song changes
discord_rpc = false        # show "Listening to..." status on Discord (local IPC socket)
discord_app_id = ""        # optional custom Discord application ID
audio_quality = "max"      # max | high | normal | low (data saver)
crossfade_secs = 0         # smooth fade-out / fade-in at track transitions (0..12 seconds)
sleep_fade_secs = 10       # fade-out window (seconds) before a sleep timer pauses playback
playback_speed = 1.0       # playback speed multiplier (0.5..2.0)
pitch_correction = true    # keep original pitch when playback_speed != 1.0

[eq]
enabled = false
preset = "flat"            # flat | bass_boost | vocal | rock | electronic | acoustic | pop | jazz | classical | treble_boost | late_night | custom
gains = [0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0]

[viz]
mode = "bars"              # bars | mirror | mono | wave
source = "mpv"             # "mpv" = only pova's audio; "system" = everything, like plain cava
bar_width = 2
bar_spacing = 1
noise_reduction = 0.77     # cava's smoothing: higher is calmer
framerate = 60
low_cut_off = 50
high_cut_off = 10000
sample_rate = 44100

[keys]                     # remap anything; press ? in pova for action names
next = ["N", "ctrl+right"]
```

pova remembers your last theme, volume, visualizer mode, box layout and menu
settings in `~/.local/state/pova/state.toml`.

**Even loudness and notifications** can also be switched in pova's `esc`
menu. What you pick there wins over `config.toml`.

- `normalize_loudness` runs mpv's loudness filter (EBU R128, aiming at the
  -14 LUFS YouTube itself uses), so a quiet acoustic song and a loud master
  play at about the same volume. If you set your own audio filter with
  `--af=` in `mpv_args`, that replaces pova's.
- `notifications` needs `notify-send` (the `libnotify` package; `libnotify-bin`
  on Debian and Ubuntu). Each new song replaces the previous popup, so they
  don't pile up.

**Themes** are looked up in `~/.config/pova/themes/`, `~/.config/btop/themes/`
and `/usr/share/btop/themes/`. Run `pova --list-themes` to see what you have.

Built in, with nothing to install: btop's Default and the four
[Catppuccin](https://catppuccin.com) flavours, `catppuccin_latte`,
`catppuccin_frappe`, `catppuccin_macchiato` and `catppuccin_mocha` (from
[catppuccin/btop](https://github.com/catppuccin/btop)). A theme file with the
same name in one of the folders above replaces the built-in copy.

<p align="center"><img src="../assets/screenshots/catppuccin-flavours.jpg" alt="pova in Catppuccin Latte, Frappé, Macchiato and Mocha" width="800"></p>
To make your own, copy a btop theme and change it. These are the keys pova
reads:

| btop key | used by pova for |
|---|---|
| `main_bg`, `main_fg`, `title`, `hi_fg`, `selected_bg/fg`, `inactive_fg`, `div_line` | the same things as in btop |
| `cpu_box`, `proc_box`, `mem_box` | the visualizer, player and controls borders |
| `cpu_start`, `cpu_mid`, `cpu_end` | the visualizer's colour gradient |
| `download_start/mid/end` | the progress and volume bars |

pova also understands a few keys btop doesn't have, and ignores the rest:

| pova key | used for | default |
|---|---|---|
| `viz_start/mid/end` | the visualizer gradient, instead of `cpu_*` | `cpu_*` |
| `progress_start/mid/end` | the progress and volume bars, instead of `download_*` | `download_*` |
| `logo_start/mid/end` | the logo and highlight in the Esc menu | pova's violet to pink |
| `logo_text` | the logo's letters | near white |
| `error` | the error box and "Yes, delete" button | red |

btop ignores these keys, so one theme file works in both. Text on the Esc
menu highlight and the delete button turns black or white by itself,
whichever reads better.

The full list of action names for `[keys]` is in [keys.md](keys.md).

Back to the [README](../README.md).

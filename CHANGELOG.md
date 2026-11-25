# Changelog

## 1.3.0 — browser sign-in, Brand Accounts, smoother UI & multi-arch packages

- **One-click browser profile sign-in and `pova login` / `pova logout`.** The
  Library tab now shows the `https://music.youtube.com` link right away along
  with every installed browser profile by name (including multiple Brave,
  Chrome, Chromium, Edge, Vivaldi, and Firefox profiles) instead of manual
  DevTools instructions. Picking a profile signs in immediately or opens
  `music.youtube.com` in that profile and polls until you finish signing in
  with Google. Also adds custom profile input (`p`), masked manual paste (`c`),
  `Sign in` / `Sign out` in the Esc menu, clickable login status in the player
  border, and `pova login` / `pova logout` CLI commands without `python3`.
- **YouTube Brand Account support & explicit account switcher.** Bootstraps
  YouTube Music's active session (`ytcfg`), signs requests with session-bound
  `SAPISIDHASH` (`1P`/`3P`) plus `X-Goog-AuthUser` / `X-Goog-PageId` /
  `onBehalfOfUser`, shows the active account name in the player border, and
  adds an explicit account switcher (`u` in Library or **Switch account** in
  the Esc menu) saved across restarts.
- **Instant Search tab switching & smoother UI/UX.** Cycling onto the Search
  tab with `Tab` / `Shift+Tab` / `2` no longer traps you in edit mode (press
  `/` or `Enter` to type, `Backspace` to step back out of nested views).
  Coalesces bursts of terminal input with immediate redraws, throttles
  non-lyrics playback redraws, renders Queue and Browse lists windowed with a
  vertical scrollbar and `pos/total` counter, and makes all overlay rows and
  buttons clickable.
- **Multi-arch releases, Arch package, Nix Flake & one-line installer.** Adds
  native Linux `aarch64` (`arm64`) `.deb`, `.rpm`, `.AppImage`, and `.tar.gz`
  packages, a native Arch Linux `.pkg.tar.zst` package, a multi-arch signed
  `apt` repository on GitHub Pages, `flake.nix`, `scripts/install.sh`, and a
  unified `SHA256SUMS` manifest.

## 1.2.2 — Catppuccin themes

- **Catppuccin, built in.** Latte, Frappé, Macchiato and Mocha are in the
  theme picker (`t`) with nothing to install, from catppuccin/btop.
- **Themes can now colour every part of pova**, including the Esc menu logo
  and highlight and the error box, with the new `logo_*` and `error` keys.
  Text on coloured highlights picks black or white for contrast, so light
  themes stay readable.

## 1.2.1 — apt repository, Liked songs fix

- **apt repository** for Debian, Ubuntu, Linux Mint and Pop!_OS: add it once
  and pova updates with `sudo apt upgrade`. It serves the same signed `.deb`
  as the release page. See the README for the four setup lines.
- **Fixed: Liked songs or a playlist failing with "Couldn't load".** One
  unusual entry (a video whose channel name has no channel link) made the
  whole list fail, and long lists could stop early. pova now reads such pages
  leniently, skipping only entries that can't be played, and retries a page
  that arrives cut off on a slow connection.

## 1.2.0 — even loudness, dislike, notifications

- **Even loudness between songs.** mpv's loudness filter evens out the
  volume from one song to the next. On by default (`normalize_loudness`).
- **Dislike.** `F` (or `pova dislike`) gives the current song a thumbs-down on
  YouTube Music and skips it, so radio and autoplay steer away from it.
- **Song-change notifications.** A desktop popup with the cover art when the
  song changes, replacing the previous one. Off by default (`notifications`);
  needs `notify-send` from libnotify.
- **Up next.** Now Playing shows the song that plays after the current one.
- **Two new switches in the Esc menu**, under Album art: Even loudness and
  Notifications. They change live and are remembered between runs.

## 1.1.2 — smoother first run, offline recovery

More fixes from [chrollo-03](https://github.com/chrollo-03) (#12, #14): clearer
help when something is missing on first launch, recovery after starting
offline, and a safer sign-in cookie.

- **Expanded startup preflight checks.** Detect `rpm-ostree` (atomic Fedora),
  `apt` (with `apt update`), `pacman`, `zypper`, `xbps-install`, `apk`,
  `emerge`, `eopkg`, and `nix`; catch common mistakes (forgot `chmod +x`,
  `~/.local/bin` not on `PATH`, Flatpak-only `mpv`, non-TTY stdin/stdout); and
  show a startup notice if PipeWire visualizer tools are missing.
- **Offline recovery & cookie hot-reload.** Starting pova while offline no
  longer permanently stops YouTube Music requests; pova reconnects on the next
  request once the network is back. Deleting `cookie.txt` signs out live,
  signing out clears stale `♥` likes, and `import-cookie.sh` now writes
  `cookie.txt` atomically with mode 600.
- **Corrupt album art recovery.** Album art is decoded before being cached
  atomically, and corrupt cached files are removed and re-downloaded instead of
  staying broken.
- **Fixes & polish.** Single-pass `pova status` template expansion (prevents
  track titles containing `{...}` from re-expanding) and accurate `{state}`
  while idle; clicking an error box closes it before any overlay underneath;
  removing a duplicate track from a playlist drops only one copy and playlist
  deletion matches with or without the `VL` prefix; `mpv < 0.38` compatible
  load when resuming at 0:00; horizontal scrolling for long search queries; and
  initial selection + paste support in the Add to Playlist overlay.

## 1.1.1 — community fixes

The first release with outside contributions: thanks to
[chrollo-03](https://github.com/chrollo-03) for the fixes and polish below (#1).

- **Mouse & overlay controls.** Click or scroll the volume bar in the Controls
  box to set volume, click the `vol` / `%` label to mute, and click the
  visualizer mode badge to switch modes. Overlays now support mouse scrolling
  and `g`/`G` navigation, and `←` cycles visualizer modes backwards in the Esc
  menu.
- **Search box word delete.** `Ctrl+W` deletes the previous word and `Ctrl+U`
  clears before the cursor.
- **Startup dependency check.** Refuse to start into a broken silent TUI when
  `mpv` or `yt-dlp` is missing, and print the exact install command for the
  detected package manager (`dnf`, `apt`, `pacman`, `zypper`).
- **Fixes.** Keep LRCLIB lyrics matches when a track has no duration (such as
  artist top songs), preserve stereo L/R alignment in PipeWire capture, prevent
  infinite retry loops on unplayable tracks, keep mpv startup errors visible,
  revert theme previews on mouse dismiss, ignore clicks on section headers, and
  fix `pova pause` / `pova play` while idle.

## 1.1.0 — pick up where you left off

- **Resume.** Quit pova and come back to the same queue, paused at the same
  second; press space to carry on. Turn it off with `resume = false`.
- **Playlists.** `p` adds the selected (or playing) song to one of your
  playlists, or to a new private one. `d` in the Library tab takes a song out
  of the playlist you have open, and `D` deletes a playlist after a yes / no
  question (No is the default, so a stray Enter is safe).
- **Control pova from anywhere.** `pova next`, `pova toggle`,
  `pova add <song>`, `pova volume +5`, `pova status -f '{artist} - {title}'`
  and more talk to the running pova, for keyboard shortcuts and status bars.
  See [docs/remote-control.md](docs/remote-control.md).
- **Errors you can read.** Errors now open in their own box with the whole
  message and what to do about it, instead of being cut off in the border.
- **Expired sign-ins are caught.** pova checks the login at startup and shows
  "signed out" (with how to fix it) instead of a false "signed in". After
  `pova-import-cookie`, it signs back in by itself, no restart needed.

## 1.0.1 — packages

- **.deb and .rpm packages.** `sudo dnf install ./pova-*.rpm` or
  `sudo apt install ./pova_*.deb` installs pova with mpv, yt-dlp and the
  PipeWire tools from your distro's repositories, and adds it to the app menu.
- **AppImage** for every release.
- The sign-in helper is installed as the `pova-import-cookie` command.
- Every package is test-installed on clean Fedora and Ubuntu systems before
  it's published.

## 1.0.0 — first release

pova plays YouTube Music in your terminal, looks like btop, and has a real
cava visualizer.

**Playing music**
- Search songs, videos, albums, artists and playlists, with suggestions as you
  type and typo correction ("call me when your sober" still finds it).
- Queue songs (play next or add to the end), reorder and remove them; autoplay
  keeps going with a radio of similar songs.
- Gapless playback, and songs start in about a third of a second because pova
  looks up the next ones ahead of time.
- Time-synced lyrics from LRCLIB, falling back to YouTube Music's lyrics.
- Sign in with one command (`scripts/import-cookie.sh`) for your library,
  liked songs, history and likes; songs you've liked show a ♥ everywhere.

**Looks**
- A Rust port of cava's `cavacore`, listening only to pova's own audio; four
  modes (bars, mirror, mono, wave).
- Real album art in kitty; a halfblock version elsewhere.
- Every btop theme, with a live picker, and a btop-style Esc menu.
- Mouse support.

**Under the hood**
- Clean shutdown: mpv and the audio capture always exit with pova.
- See [docs/how-it-works.md](docs/how-it-works.md) for the architecture and
  known limits.

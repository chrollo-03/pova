<p align="center">
  <img src="assets/logo.png" alt="pova" width="620">
</p>

<p align="center">
  <b>YouTube Music, a cava visualizer, and btop's sense of style, in one terminal window.</b><br>
  Written in Rust. Built for people who would rather <code>alt-tab</code> to a terminal than to a browser tab.
</p>

<p align="center">
  <a href="https://github.com/ratatui/awesome-ratatui#music-and-audio"><img src="https://awesome.re/mentioned-badge.svg" alt="Mentioned in Awesome Ratatui"></a>
</p>

<p align="center">
  <a href="https://github.com/chrollo-03/pova/releases/latest"><img src="https://img.shields.io/github/v/release/chrollo-03/pova" alt="Latest release"></a>
  <a href="https://github.com/chrollo-03/pova/actions/workflows/ci.yml"><img src="https://github.com/chrollo-03/pova/actions/workflows/ci.yml/badge.svg?branch=main" alt="CI"></a>
  <a href="https://github.com/chrollo-03/pova/actions/workflows/release.yml"><img src="https://github.com/chrollo-03/pova/actions/workflows/release.yml/badge.svg" alt="Releases"></a>
  <a href="LICENSE"><img src="https://img.shields.io/github/license/chrollo-03/pova" alt="MIT license"></a>
  <a href="https://ratatui.rs/"><img src="https://ratatui.rs/built-with-ratatui/badge.svg" alt="Built With Ratatui"></a>
</p>

---

## So what is this

You like your music. You like your terminal. You like watching bars jump.
Until now that meant three programs: a browser tab with YouTube Music in it,
`cava` in one pane, and some regret.

**pova** is all of that in one place. It looks like btop, it plays YouTube
Music, and its visualizer is a real port of cava that listens to *pova's own
audio* and nothing else. Your Discord notification sounds are not music, and
pova knows that.

<p align="center">
  <a href="assets/pova-launch.mp4">
    <img src="assets/pova-tour.gif" alt="The 40-second pova tour, hosted by the pova cat: search, queue, lyrics, themes and likes" width="720">
  </a><br>
  <sub><a href="assets/pova-launch.mp4">Watch it with sound (MP4)</a> · music: Sascha Ende, ende.app (CC BY 4.0)</sub>
</p>

<p align="center">
  <img src="assets/screenshot.png" alt="pova playing Ugly (Acoustic) by The Exies: mirror visualizer on the left, album art and the song on the right" width="900">
</p>

## Install

pova runs on Linux with PipeWire. It was built on Fedora KDE with kitty, and
kitty shows real album art (other terminals get a blocky version).

| Method | Best for | Installs mpv, yt-dlp and the PipeWire tools for you |
|---|---|---|
| [One-line installer](#one-line-installer) | Any Linux distro (`x86_64` and `aarch64`): auto-detects and verifies checksums | yes (via dnf/apt/pacman) |
| [apt repository](#debian-and-ubuntu-apt) | Debian, Ubuntu, Linux Mint, Pop!_OS (`amd64` & `arm64`): updates with your system | yes |
| [`.rpm` package](#fedora-rpm) | Fedora and other dnf-based distros (`x86_64` & `aarch64`) | yes |
| [`.deb` package](#debian-and-ubuntu-deb) | apt-based distros (`amd64` & `arm64`), without adding a repository | yes |
| [Arch Linux package](#arch-linux-pkgtarzst--pkgbuild) | Arch, EndeavourOS, CachyOS, Manjaro (`.pkg.tar.zst` or `PKGBUILD`) | yes |
| [AppImage](#appimage) | Any distro (`x86_64` & `aarch64`), nothing to install | no |
| [Prebuilt binary](#prebuilt-binary) | Any distro (`x86_64` & `aarch64`), a single file in your `PATH` | no |
| [Nix Flake](#nix--nixos) | NixOS and Nix users (`x86_64` & `aarch64`) | yes |
| [From source](#from-source) | Rust users and other CPU architectures | no |

Every file is on the [latest release](https://github.com/chrollo-03/pova/releases/latest)
page, with per-file `.sha256` files and a combined `SHA256SUMS` manifest. The
commands below use version 1.3.0.

### One-line installer

Auto-detects your distro and CPU architecture (`x86_64` or `aarch64`), verifies
the SHA-256 checksum, and installs the native `.rpm`, `.deb`, `.pkg.tar.zst`,
or binary package:

```sh
curl -fsSL https://raw.githubusercontent.com/chrollo-03/pova/main/scripts/install.sh | bash
```

Force a specific method with `--method binary`, `--method appimage`, `--method rpm`,
`--method deb`, or `--method pacman`.

### Debian and Ubuntu (apt)

Add the signed apt repository once, and from then on pova updates with
`sudo apt upgrade` like everything else. Supports both `amd64` and `arm64`,
pulls in `mpv`, `yt-dlp`, and `pipewire-bin`, and adds pova to your app menu.

```sh
sudo install -d -m 0755 /etc/apt/keyrings
curl -fsSL https://chrollo-03.github.io/pova/apt/pova.gpg | sudo tee /etc/apt/keyrings/pova.gpg > /dev/null
echo "deb [signed-by=/etc/apt/keyrings/pova.gpg] https://chrollo-03.github.io/pova/apt stable main" | sudo tee /etc/apt/sources.list.d/pova.list
sudo apt update && sudo apt install pova
```

### Fedora (.rpm)

`dnf` installs pova and pulls in `mpv`, `yt-dlp`, and `pipewire-utils` from
Fedora's own repositories, and adds pova to your app menu.

```sh
# x86_64
sudo dnf install https://github.com/chrollo-03/pova/releases/download/v1.3.0/pova-1.3.0-1.x86_64.rpm

# aarch64
sudo dnf install https://github.com/chrollo-03/pova/releases/download/v1.3.0/pova-1.3.0-1.aarch64.rpm
```

### Debian and Ubuntu (.deb)

`apt` installs pova and pulls in `mpv`, `yt-dlp`, and `pipewire-bin`.

```sh
# amd64 (use arm64 on aarch64)
curl -LO https://github.com/chrollo-03/pova/releases/download/v1.3.0/pova_1.3.0-1_amd64.deb
sudo apt install ./pova_1.3.0-1_amd64.deb
```

On older releases such as Ubuntu 22.04, the distro's `yt-dlp` is often too old
for YouTube. If songs won't start, get a current one with `pipx install yt-dlp`.

### Arch Linux (`.pkg.tar.zst` & `PKGBUILD`)

Install the prebuilt pacman package directly (pulls in `mpv`, `yt-dlp`, and
`pipewire-audio`):

```sh
curl -LO https://github.com/chrollo-03/pova/releases/download/v1.3.0/pova-bin-1.3.0-1-x86_64.pkg.tar.zst
sudo pacman -U ./pova-bin-1.3.0-1-x86_64.pkg.tar.zst
```

Or build the package locally from `packaging/aur/pova-bin/PKGBUILD` with `makepkg -si`.

### AppImage

One file that runs on any distro (`x86_64` or `aarch64`). You install pova's
helpers yourself ([see below](#the-helpers-pova-needs)).

```sh
curl -LO https://github.com/chrollo-03/pova/releases/download/v1.3.0/pova-v1.3.0-x86_64.AppImage
chmod +x pova-v1.3.0-x86_64.AppImage
./pova-v1.3.0-x86_64.AppImage
```

### Prebuilt binary

A plain binary plus the sign-in helper (`x86_64` or `aarch64`). You install
pova's helpers yourself ([see below](#the-helpers-pova-needs)).

```sh
curl -LO https://github.com/chrollo-03/pova/releases/download/v1.3.0/pova-v1.3.0-x86_64-linux.tar.gz
tar xzf pova-v1.3.0-x86_64-linux.tar.gz
install -Dm755 pova-v1.3.0-x86_64-linux/pova ~/.local/bin/pova
```

### Nix / NixOS

Includes `mpv`, `yt-dlp`, `pipewire`, and `nodejs` wrapped into `PATH`:

```sh
nix run github:chrollo-03/pova
# or install into your profile:
nix profile install github:chrollo-03/pova
```

### From source

Needs a Rust toolchain (1.90+) from <https://rustup.rs>:

```sh
cargo install --git https://github.com/chrollo-03/pova --tag v1.3.0
```

### The helpers pova needs

The `.rpm` and `.deb` install these for you. For the other methods:

```sh
sudo dnf install mpv yt-dlp pipewire-utils nodejs        # Fedora
sudo apt install mpv yt-dlp pipewire-bin nodejs          # Debian, Ubuntu
sudo pacman -S mpv yt-dlp pipewire-audio nodejs          # Arch
```

| Program | What pova uses it for |
|---|---|
| `mpv` | playing the audio, with no window |
| `yt-dlp` | turning a YouTube Music link into an audio stream |
| `nodejs` or `deno` | lets yt-dlp solve YouTube's JavaScript challenge |
| `pw-record`, `pw-dump` (PipeWire tools) | capturing pova's audio for the visualizer |
| `btop` (optional) | about 40 extra themes |
| `mpv-mpris` (optional) | media keys and your desktop's media widget |

### Play something

```sh
pova                                   # then press / and search
pova --play "seether fine again"       # or straight from the shell
```

## What you can do

### Search from your terminal

Press `/`, start typing, and suggestions drop down under the box. Spelling
is optional: type `call me when your sober` and pova still finds Evanescence,
the same way YouTube Music's own "did you mean" would. `tab` switches between
Songs, Videos, Albums, Artists and Playlists; `enter` plays.

<p align="center"><img src="assets/gifs/search.gif" alt="searching for fine again seether and playing it" width="900"></p>

### Queue things up

`A` plays the selected song next, `a` adds it to the end. In the queue (`q`),
`J` and `K` move songs up and down, `d` removes them. When the queue runs out,
autoplay keeps going with a radio of similar songs, like the website does.

<p align="center"><img src="assets/gifs/queue.gif" alt="adding songs from search to the queue and reordering them" width="900"></p>

### Move between songs

`N` and `P` skip forward and back, `←` `→` seek 5 seconds, `H` `L` seek 30.
pova looks up the next songs while the current one plays, so skipping is
near-instant, and the next track is preloaded so albums play gaplessly.

<p align="center"><img src="assets/gifs/skip.gif" alt="skipping between songs with new album art and the visualizer following along" width="900"></p>

### Live lyrics

Press `l`. Time-synced lyrics from LRCLIB scroll and highlight as the song
plays. If LRCLIB has nothing, pova asks YouTube Music. If *nobody* has
lyrics, you get the cat. (See below. Be nice to the cat.)

<p align="center"><img src="assets/gifs/lyrics.gif" alt="synced lyrics of Fine Again highlighting line by line" width="900"></p>

### Like songs

Press `f` to like or unlike what's playing. Songs you've already liked show a
♥ everywhere: in Now Playing, the queue and search results. `F` dislikes the
song and skips it, which also steers your radio and autoplay away from it.
This needs you to sign in once (next section).

<p align="center"><img src="assets/gifs/like.gif" alt="pressing f to like Fine Again, a heart appears" width="900"></p>

### And also

- **Four visualizer modes** (`v`): stereo bars, mirror, mono and a waveform.
- **Every btop theme** (`t`): pova reads btop's `.theme` files as they are, with a live preview. The four Catppuccin flavours are built in.
- **The Esc menu**: btop-style, with the pova logo drawn in block characters, for themes, the visualizer, autoplay and album art.
- **Your library** (`b`): your playlists, liked songs and history once you're signed in, with Brand Account support and an explicit account switcher (`u`).
- **Your playlists** (`p`): add the song you're on to one of your playlists or a new one, take songs out with `d`, and delete a playlist with `D` (it asks first).
- **Even loudness**: every song plays at about the same volume, so a quiet acoustic track doesn't follow a loud one at half the level. On by default; switch it in the `esc` menu.
- **Song-change notifications**: a desktop popup with the cover art when the song changes. Off by default; turn it on in the `esc` menu.
- **Up next**: Now Playing shows which song comes after this one.
- **Picks up where you left off**: quit and come back to the same queue, paused at the same second.
- **Control it from anywhere**: `pova next`, `pova add <song>`, `pova status` and more, for keyboard shortcuts and status bars. See [docs/remote-control.md](docs/remote-control.md).
- **Mouse support**: click tabs, buttons, list rows, the progress bar and the volume bar, and scroll anywhere.

Press `?` in pova for every key, or read [docs/keys.md](docs/keys.md).

## Signing in

Search, playback, radio and lyrics work without an account. To like songs and
see your library, open the **Library** tab (`b`), make sure you're signed in at
`https://music.youtube.com` (click or press `o` to open it), and pick your
browser profile (including individual Brave, Chrome, and Firefox profiles). If
that profile isn't signed in yet, pova opens `music.youtube.com` in it and
finishes signing in automatically once you log in with Google.

Prefer the command line?

```sh
pova login                             # auto-detects your browser
pova login chrome                      # or firefox, chromium, brave, edge, vivaldi, opera
pova logout                            # sign out
```

pova reads only your `youtube.com` cookies via `yt-dlp`, saves them in
`~/.config/pova/cookie.txt` (mode `600`, readable only by you), and signs in
right away with no restart. Treat `cookie.txt` like a password. See
[docs/signing-in.md](docs/signing-in.md) for browser profiles and manual paste.

## Meet the cat

<p align="center">
  <img src="assets/mascot-sad-cat.png" alt="the pova cat, sad, next to a muted music note" width="420">
</p>

This is pova's mascot. It lives in the Lyrics tab and appears only when a
song has no lyrics anywhere: not on LRCLIB, not on YouTube Music, nowhere. It's
not angry. It's just disappointed. Mostly it shows up for instrumentals,
obscure uploads and "Me at the zoo".

## If you like pova

<p align="center">
  <a href="https://github.com/P0r0ro/pova"><img src="assets/mascot-star.gif" alt="the pova cat hugging a pink heart" width="160"></a>
</p>

<p align="center">
  pova is a one-person project, and a star is how other people find it.<br>
  If it earned a spot in your terminal, starring the repo helps more than you'd think<br>
  and means a lot to the person who built it. It also keeps the cat from being sad.
</p>

<p align="center">
  <a href="https://github.com/P0r0ro/pova"><img src="https://img.shields.io/github/stars/P0r0ro/pova?style=social" alt="Star pova on GitHub"></a>
</p>

## More

- [docs/keys.md](docs/keys.md): every key
- [docs/configuration.md](docs/configuration.md): the config file, themes, remapping keys
- [docs/signing-in.md](docs/signing-in.md): signing in, step by step
- [docs/remote-control.md](docs/remote-control.md): `pova next`, `pova add`, `pova status` for shortcuts and status bars
- [docs/how-it-works.md](docs/how-it-works.md): how pova works inside, and its known limits
- [docs/development.md](docs/development.md): building, tests, CI and releasing
- [CONTRIBUTING.md](CONTRIBUTING.md): how to send a change

## Credits

pova stands on the shoulders of
[mpv](https://mpv.io),
[yt-dlp](https://github.com/yt-dlp/yt-dlp),
[cava](https://github.com/karlstav/cava) (its visualizer is ported from cava's `cavacore`),
[btop](https://github.com/aristocratos/btop) for the look and the themes,
[ytmapi-rs](https://github.com/nick42d/youtui),
[LRCLIB](https://lrclib.net),
[ratatui](https://ratatui.rs) and
[ratatui-image](https://github.com/benjajaja/ratatui-image).

## License

MIT, see [LICENSE](LICENSE). The parts ported from cava and ratatui-image keep
their own MIT notices in [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).

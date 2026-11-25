//! pova — a btop-style YouTube Music player with a built-in cava visualizer.

mod app;
mod art;
mod auth;
mod config;
mod control;
mod innertube;
mod keymap;
mod lenient;
mod lyrics;
mod model;
mod notify;
mod player;
mod resolve;
mod theme;
mod ui;
mod viz;
mod ytm;

use std::io::{self, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::{Parser, Subcommand};
use crossterm::event::{
    DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture, EventStream,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode};
use futures::StreamExt;
use ratatui::Terminal;
use ratatui::backend::CrosstermBackend;
use ratatui_image::picker::Picker;
use ratatui_image::thread::{ResizeRequest, ThreadProtocol};
use tokio::sync::mpsc::unbounded_channel;

use crate::app::{App, Channels};
use crate::config::{Config, Paths, State};
use crate::keymap::Keymap;
use crate::player::mpv::{self, MpvConfig, PlayerCmd};

#[derive(Parser, Debug)]
#[command(version, about = "btop-style YouTube Music player with a cava visualizer")]
struct Cli {
    /// Theme name (btop .theme file stem), overrides config/state.
    #[arg(short, long)]
    theme: Option<String>,
    /// Search for songs and start playing the first result.
    #[arg(short, long, value_name = "QUERY")]
    play: Option<String>,
    /// Disable album art.
    #[arg(long)]
    no_art: bool,
    /// Use an alternative config file.
    #[arg(short, long, value_name = "FILE")]
    config: Option<PathBuf>,
    /// List available themes and exit.
    #[arg(long)]
    list_themes: bool,
    /// Log verbosity written to ~/.cache/pova/pova.log (error, warn, info, debug, trace).
    #[arg(long, default_value = "info")]
    log_level: tracing::Level,
    /// Control a running pova, or sign in / out.
    #[command(subcommand)]
    command: Option<CliCommand>,
}

/// CLI subcommands (`pova login`, `pova logout`, and remote control commands).
#[derive(Subcommand, Debug)]
enum CliCommand {
    /// Sign in to YouTube Music using your browser's cookies (auto-detects browser if omitted).
    Login {
        /// Optional browser name or profile, e.g. `firefox`, `chrome`, or `chrome:Profile 2`.
        browser: Option<String>,
    },
    /// Sign out of YouTube Music (removes the saved cookie file).
    Logout,
    /// Play / pause.
    Toggle,
    /// Resume playback.
    Play,
    /// Pause playback.
    Pause,
    /// Next track.
    Next,
    /// Previous track (or restart the current one).
    Prev,
    /// Like / unlike the current song.
    Like,
    /// Dislike the current song and skip it.
    Dislike,
    /// Set the volume: 40, +5 or -5.
    #[command(allow_hyphen_values = true)]
    Volume { amount: String },
    /// Seek: 90 (seconds in), +10 or -10.
    #[command(allow_hyphen_values = true)]
    Seek { amount: String },
    /// Search and add the first song to the end of the queue.
    Add {
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Search and play the first song after the current one.
    PlayNext {
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Search and play the first song right away.
    PlayNow {
        #[arg(required = true)]
        query: Vec<String>,
    },
    /// Print what's playing, e.g. for a status bar.
    Status {
        /// Placeholders: {title} {artist} {album} {state} {position} {duration} {volume} {liked}
        #[arg(short, long, default_value = control::STATUS_FORMAT)]
        format: String,
    },
}

impl CliCommand {
    fn into_remote(self) -> Option<control::Command> {
        use control::Command as C;
        Some(match self {
            CliCommand::Login { .. } | CliCommand::Logout => return None,
            CliCommand::Toggle => C::Toggle,
            CliCommand::Play => C::Play,
            CliCommand::Pause => C::Pause,
            CliCommand::Next => C::Next,
            CliCommand::Prev => C::Prev,
            CliCommand::Like => C::Like,
            CliCommand::Dislike => C::Dislike,
            CliCommand::Volume { amount } => C::Volume(amount),
            CliCommand::Seek { amount } => C::Seek(amount),
            CliCommand::Add { query } => C::Add(query.join(" ")),
            CliCommand::PlayNext { query } => C::PlayNext(query.join(" ")),
            CliCommand::PlayNow { query } => C::PlayNow(query.join(" ")),
            CliCommand::Status { format } => C::Status(format),
        })
    }
}

fn main() -> anyhow::Result<()> {
    let mut cli = Cli::parse();
    let paths = Paths::new()?;

    if let Some(cmd) = cli.command.take() {
        match cmd {
            CliCommand::Login { browser } => {
                let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
                if let Err(e) = rt.block_on(auth::login_cli(&paths.cookie_file(), browser.as_deref())) {
                    eprintln!("pova: {e}");
                    std::process::exit(1);
                }
                return Ok(());
            }
            CliCommand::Logout => {
                match auth::remove_cookie_file(&paths.cookie_file()) {
                    Ok(true) => println!("Signed out."),
                    Ok(false) => println!("Already signed out."),
                    Err(e) => {
                        eprintln!("pova: {e}");
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }
            other => {
                let remote = other.into_remote().expect("non-auth command");
                let socket = control::socket_path(&paths.runtime_dir);
                return match control::send(&socket, &remote) {
                    Ok(answer) => {
                        if !answer.is_empty() {
                            println!("{answer}");
                        }
                        Ok(())
                    }
                    Err(e) => {
                        eprintln!("pova: {e}");
                        std::process::exit(1);
                    }
                };
            }
        }
    }

    if cli.list_themes {
        for (name, path) in theme::list_themes(&paths.config_dir) {
            match path {
                Some(p) => println!("{name:<28} {}", p.display()),
                None => println!("{name:<28} (built-in)"),
            }
        }
        return Ok(());
    }

    if let Err(msg) = check_runtime_deps() {
        eprint!("{msg}");
        std::process::exit(1);
    }

    use std::io::IsTerminal;
    if !io::stdin().is_terminal() || !io::stdout().is_terminal() {
        eprintln!(
            "pova's interface needs an interactive terminal.\n\
             To control a running pova from a script, use a subcommand like `pova toggle` or `pova status`.\n\
             Run `pova --help` for the full list."
        );
        std::process::exit(1);
    }

    // Logs go to a file only; the TUI owns the terminal.
    let appender = tracing_appender::rolling::never(&paths.cache_dir, "pova.log");
    let (writer, _log_guard) = tracing_appender::non_blocking(appender);
    // Verbose levels apply to pova only; dependencies stay at warn.
    let filter = tracing_subscriber::EnvFilter::new(format!("warn,pova={}", cli.log_level));
    tracing_subscriber::fmt()
        .with_writer(writer)
        .with_ansi(false)
        .with_env_filter(filter)
        .init();
    tracing::info!("pova {} starting", env!("CARGO_PKG_VERSION"));

    let mut cfg = Config::load(cli.config.as_deref().unwrap_or(&paths.config_file()))?;
    if cli.no_art {
        cfg.album_art = false;
    }
    let mut state = State::load(&paths.state_file());
    if let Some(t) = &cli.theme {
        state.theme = Some(t.clone());
    }

    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(run(cli, cfg, paths, state))
}

type Term = Terminal<CrosstermBackend<io::Stdout>>;

fn setup_terminal() -> anyhow::Result<(Term, bool)> {
    enable_raw_mode()?;
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen, EnableMouseCapture, EnableBracketedPaste)?;
    // Kitty keyboard protocol: unambiguous Esc / shift+arrows.
    let enhanced = matches!(crossterm::terminal::supports_keyboard_enhancement(), Ok(true));
    if enhanced {
        execute!(
            out,
            PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
        )?;
    }
    let mut term = Terminal::new(CrosstermBackend::new(out))?;
    term.hide_cursor()?;
    term.clear()?;
    Ok((term, enhanced))
}

fn restore_terminal(enhanced: bool) {
    let mut out = io::stdout();
    if enhanced {
        let _ = execute!(out, PopKeyboardEnhancementFlags);
    }
    let _ = execute!(
        out,
        DisableBracketedPaste,
        DisableMouseCapture,
        LeaveAlternateScreen,
        crossterm::cursor::Show
    );
    let _ = disable_raw_mode();
    let _ = out.flush();
}

/// Whether the terminal reliably answers ratatui-image's capability query.
/// If a terminal never answers, the query thread stays blocked on stdin and
/// swallows the user's next key press, so only query known-good terminals.
fn terminal_answers_queries() -> bool {
    let env = |k: &str| std::env::var(k).unwrap_or_default();
    if !env("TMUX").is_empty() || env("TERM").starts_with("screen") {
        return false;
    }
    let term = env("TERM");
    let program = env("TERM_PROGRAM").to_lowercase();
    !env("KITTY_WINDOW_ID").is_empty()
        || term.contains("kitty")
        || term.contains("ghostty")
        || term.starts_with("foot")
        || !env("WEZTERM_EXECUTABLE").is_empty()
        || !env("KONSOLE_VERSION").is_empty()
        || ["wezterm", "ghostty", "iterm.app"].contains(&program.as_str())
}

/// Pick the image protocol before the event stream starts reading stdin.
fn make_picker(protocol: &str) -> Picker {
    use ratatui_image::picker::ProtocolType;
    let forced = match protocol.to_ascii_lowercase().as_str() {
        "kitty" => Some(ProtocolType::Kitty),
        "sixel" => Some(ProtocolType::Sixel),
        "iterm2" => Some(ProtocolType::Iterm2),
        "halfblocks" => Some(ProtocolType::Halfblocks),
        _ => None,
    };
    let mut picker = if forced != Some(ProtocolType::Halfblocks) && terminal_answers_queries() {
        match Picker::from_query_stdio() {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("graphics query failed ({e}); using halfblocks");
                Picker::halfblocks()
            }
        }
    } else {
        Picker::halfblocks()
    };
    if let Some(p) = forced {
        picker.set_protocol_type(p);
    }
    tracing::info!("image protocol: {:?}", picker.protocol_type());
    picker
}

type Resized = Result<ratatui_image::thread::ResizeResponse, ratatui_image::errors::Errors>;

/// Thread that resizes/encodes images for one `ThreadProtocol`.
fn spawn_resize_worker(
    name: &str,
) -> std::io::Result<(
    tokio::sync::mpsc::UnboundedSender<ResizeRequest>,
    tokio::sync::mpsc::UnboundedReceiver<Resized>,
)> {
    let (req_tx, mut req_rx) = unbounded_channel::<ResizeRequest>();
    let (done_tx, done_rx) = unbounded_channel();
    std::thread::Builder::new().name(name.into()).spawn(move || {
        while let Some(req) = req_rx.blocking_recv() {
            if done_tx.send(req.resize_encode()).is_err() {
                break;
            }
        }
    })?;
    Ok((req_tx, done_rx))
}

/// Discard pending terminal input (late query responses) for up to `quiet`.
fn drain_stdin(quiet: Duration) {
    while let Ok(true) = crossterm::event::poll(quiet) {
        if crossterm::event::read().is_err() {
            break;
        }
    }
}

async fn run(cli: Cli, cfg: Config, paths: Paths, state: State) -> anyhow::Result<()> {
    let (keymap, key_warnings) = Keymap::new(&cfg.keys);

    // ── visualizer threads ──
    let client_name = mpv::audio_client_name();
    let mut viz = viz::spawn(&cfg.viz, client_name.clone());

    // ── actors ──
    let (player_tx, player_cmd_rx) = unbounded_channel();
    let (player_ev_tx, mut player_rx) = unbounded_channel();
    let mpv_cfg = MpvConfig {
        socket: paths.runtime_dir.join(format!("pova-{}.sock", std::process::id())),
        volume: state.volume.unwrap_or(cfg.volume).min(100),
        extra_args: cfg.mpv_args.clone(),
        client_name: client_name.clone(),
        normalize: state.normalize_loudness.unwrap_or(cfg.normalize_loudness),
    };
    let player_task = tokio::spawn(mpv::run(mpv_cfg, player_cmd_rx, player_ev_tx));

    let (ytm_tx, ytm_req_rx) = unbounded_channel();
    let (ytm_ev_tx, mut ytm_rx) = unbounded_channel();
    tokio::spawn(ytm::run(
        paths.cookie_file(),
        state.account.clone(),
        ytm_req_rx,
        ytm_ev_tx,
    ));

    let (lyrics_tx, lyrics_req_rx) = unbounded_channel();
    let (lyrics_ev_tx, mut lyrics_rx) = unbounded_channel();
    tokio::spawn(lyrics::run(lyrics_req_rx, lyrics_ev_tx));

    let (resolve_tx, resolve_req_rx) = unbounded_channel();
    let (resolve_ev_tx, mut resolve_rx) = unbounded_channel();
    tokio::spawn(resolve::run(resolve_req_rx, resolve_ev_tx));

    // Remote control (`pova next` etc.).
    let control_socket = control::socket_path(&paths.runtime_dir);
    let (control_tx, mut control_rx) = unbounded_channel::<control::Request>();
    let control_owned = match control::bind(&control_socket) {
        Some(listener) => {
            tokio::spawn(control::serve(listener, control_tx));
            true
        }
        None => false,
    };

    let (art_tx, art_req_rx) = unbounded_channel();
    let (art_ev_tx, mut art_rx) = unbounded_channel();
    tokio::spawn(art::run(paths.cache_dir.join("art"), art_req_rx, art_ev_tx));

    // ── terminal ──
    let (mut terminal, enhanced) = setup_terminal()?;
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal(enhanced);
        default_hook(info);
    }));

    // Prepared even when art is off, so it can be switched on from the Esc
    // menu (cheap); `--no-art` skips images entirely.
    let picker = if cli.no_art {
        None
    } else {
        Some(make_picker(&cfg.image_protocol))
    };
    // Terminals that answer the graphics query late (e.g. inside tmux) would
    // otherwise have the replies glued to the user's first key presses.
    drain_stdin(Duration::from_millis(150));

    // Image resize/encode runs off the UI thread; one worker per image slot so
    // their responses (ids) can't get mixed up.
    let (resize_tx, mut resized_rx) = spawn_resize_worker("art-resize")?;
    let art_proto = picker.as_ref().map(|_| ThreadProtocol::new(resize_tx, None));
    // The sad-cat mascot shown when a track has no lyrics (embedded asset).
    let (cat_tx, mut cat_resized_rx) = spawn_resize_worker("cat-resize")?;
    let cat_proto = picker.as_ref().and_then(|p| {
        let img = image::load_from_memory(include_bytes!("../assets/mascot-sad-cat-tui.png")).ok()?;
        Some(ThreadProtocol::new(cat_tx, Some(p.new_resize_protocol(img))))
    });

    let (notify_tx, notify_rx) = unbounded_channel();
    tokio::spawn(notify::run(
        paths.cache_dir.join("art"),
        paths.cache_dir.clone(),
        notify_rx,
    ));

    let channels = Channels {
        player: player_tx.clone(),
        ytm: ytm_tx,
        lyrics: lyrics_tx,
        art: art_tx,
        resolve: resolve_tx,
        notify: notify_tx,
    };
    let mut app = App::new(cfg, paths, state, keymap, channels, picker, art_proto);
    app.cat_art = cat_proto;
    for w in key_warnings {
        app.flash(w, true);
    }
    if app.cfg.notifications && mpv::which("notify-send").is_none() {
        app.cfg.notifications = false;
        app.flash(notify::MISSING, true);
    }
    match cli.play {
        Some(q) => app.play_query(q),
        None => {
            app.restore_session();
        }
    }
    // After resume / --play, so their status line doesn't replace this notice.
    if app.show_viz && (mpv::which("pw-record").is_none() || mpv::which("pw-dump").is_none()) {
        tracing::warn!("pw-record or pw-dump not found on PATH; visualizer will stay idle");
        app.flash("Visualizer needs PipeWire tools (pw-record / pw-dump)", false);
    }

    let mut events = EventStream::new();
    let mut tick = tokio::time::interval(Duration::from_secs_f64(1.0 / 60.0));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut last_draw = Instant::now() - Duration::from_secs(1);
    // Closing the terminal window (SIGHUP) or `kill` (SIGTERM) must still
    // tear down mpv and restore the terminal.
    use tokio::signal::unix::{SignalKind, signal};
    let mut sigterm = signal(SignalKind::terminate())?;
    let mut sighup = signal(SignalKind::hangup())?;

    let result: anyhow::Result<()> = async {
        loop {
            tokio::select! {
                ev = events.next() => match ev {
                    Some(Ok(ev)) => app.on_terminal(ev),
                    Some(Err(e)) => return Err(e).context("reading terminal events"),
                    None => break,
                },
                _ = sigterm.recv() => app.quit = true,
                _ = sighup.recv() => app.quit = true,
                Some(ev) = player_rx.recv() => app.on_player(ev),
                Some(ev) = ytm_rx.recv() => app.on_ytm(ev),
                Some(ev) = lyrics_rx.recv() => app.on_lyrics(ev),
                Some(ev) = art_rx.recv() => app.on_art(ev),
                Some(ev) = resolve_rx.recv() => app.on_resolved(ev),
                Some(req) = control_rx.recv() => {
                    let answer = app.on_control(req.command);
                    let _ = req.reply.send(answer);
                }
                Some(done) = resized_rx.recv() => {
                    match done {
                        Ok(resp) => {
                            if let Some(a) = app.art.as_mut() {
                                a.update_resized_protocol(resp);
                            }
                        }
                        Err(e) => tracing::warn!("art encode: {e}"),
                    }
                    app.dirty = true;
                }
                Some(done) = cat_resized_rx.recv() => {
                    if let (Ok(resp), Some(c)) = (done, app.cat_art.as_mut()) {
                        c.update_resized_protocol(resp);
                    }
                    app.dirty = true;
                }
                _ = tick.tick() => {
                    if viz.frames.has_changed().unwrap_or(false) {
                        app.viz_frame = viz.frames.borrow_and_update().clone();
                        if app.show_viz {
                            app.dirty = true;
                        }
                    }
                    // Redraw on change; spinners animate at ~8 fps.
                    let animate = app.animating() && last_draw.elapsed() > Duration::from_millis(120);
                    if app.tick() || animate {
                        let mut want = 0;
                        terminal.draw(|f| want = ui::draw(f, &mut app))?;
                        if want > 0 {
                            viz.set_bars(want);
                        }
                        last_draw = Instant::now();
                    }
                }
            }
            if app.quit {
                break;
            }
        }
        Ok(())
    }
    .await;

    // ── teardown ──
    app.save_state();
    app.save_session();
    let _ = player_tx.send(PlayerCmd::Quit);
    let _ = tokio::time::timeout(Duration::from_secs(2), player_task).await;
    viz.shutdown();
    if control_owned {
        let _ = std::fs::remove_file(&control_socket);
    }
    restore_terminal(enhanced);
    tracing::info!("bye");
    result
}

/// Environment probed during the startup dependency check (abstracted for tests).
struct DepProbe<'a> {
    has: &'a dyn Fn(&str) -> bool,
    non_exec: &'a dyn Fn(&str) -> Option<PathBuf>,
    local_bin: &'a dyn Fn(&str) -> Option<PathBuf>,
    flatpak_mpv: bool,
    ostree: bool,
}

/// Verify playback-critical helpers (`mpv`, `yt-dlp`) exist on PATH before
/// entering the TUI. Returns a message with distro-appropriate install
/// instructions and common mistake hints when something is missing.
fn check_runtime_deps() -> Result<(), String> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let has = |bin: &str| mpv::which(bin).is_some();
    let non_exec = |bin: &str| mpv::find_non_executable(bin);
    let local_bin = |bin: &str| {
        let p = home.as_ref()?.join(".local/bin").join(bin);
        p.is_file().then_some(p)
    };
    let flatpak_mpv = std::path::Path::new("/var/lib/flatpak/app/io.mpv.Mpv").is_dir()
        || home
            .as_ref()
            .is_some_and(|h| h.join(".local/share/flatpak/app/io.mpv.Mpv").is_dir());
    let ostree = std::path::Path::new("/run/ostree-booted").exists();
    missing_deps_message(&DepProbe {
        has: &has,
        non_exec: &non_exec,
        local_bin: &local_bin,
        flatpak_mpv,
        ostree,
    })
}

struct DistroPkg {
    cmd: &'static str,
    mpv: &'static str,
    yt_dlp: &'static str,
    pipewire: &'static str,
    node: &'static str,
    note: Option<&'static str>,
}

fn detect_distro(probe: &DepProbe<'_>) -> Option<DistroPkg> {
    let has = probe.has;
    if (probe.ostree || !has("dnf")) && has("rpm-ostree") {
        Some(DistroPkg {
            cmd: "rpm-ostree install",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire-utils",
            node: "nodejs",
            note: Some("Then reboot (or run pova inside a distrobox/toolbox)."),
        })
    } else if has("dnf") {
        Some(DistroPkg {
            cmd: "sudo dnf install",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire-utils",
            node: "nodejs",
            note: None,
        })
    } else if has("apt-get") || has("apt") {
        Some(DistroPkg {
            cmd: "sudo apt update && sudo apt install",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire-bin",
            node: "nodejs",
            note: None,
        })
    } else if has("pacman") {
        Some(DistroPkg {
            cmd: "sudo pacman -S",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire-audio",
            node: "nodejs",
            note: None,
        })
    } else if has("zypper") {
        Some(DistroPkg {
            cmd: "sudo zypper install",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire-tools",
            node: "nodejs",
            note: None,
        })
    } else if has("xbps-install") {
        Some(DistroPkg {
            cmd: "sudo xbps-install -S",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire",
            node: "nodejs",
            note: None,
        })
    } else if has("apk") {
        Some(DistroPkg {
            cmd: "sudo apk add",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire-tools",
            node: "nodejs",
            note: None,
        })
    } else if has("emerge") {
        Some(DistroPkg {
            cmd: "sudo emerge",
            mpv: "media-video/mpv",
            yt_dlp: "net-misc/yt-dlp",
            pipewire: "media-video/pipewire",
            node: "net-libs/nodejs",
            note: None,
        })
    } else if has("eopkg") {
        Some(DistroPkg {
            cmd: "sudo eopkg install",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire",
            node: "nodejs",
            note: None,
        })
    } else if has("nixos-rebuild") || has("nix") {
        Some(DistroPkg {
            cmd: "nix-shell -p",
            mpv: "mpv",
            yt_dlp: "yt-dlp",
            pipewire: "pipewire",
            node: "nodejs",
            note: Some("Or add those packages to environment.systemPackages in configuration.nix."),
        })
    } else {
        None
    }
}

fn missing_deps_message(probe: &DepProbe<'_>) -> Result<(), String> {
    let has = probe.has;
    let missing_mpv = !has("mpv");
    let missing_ytdlp = !has("yt-dlp");
    if !missing_mpv && !missing_ytdlp {
        return Ok(());
    }

    let mut missing = Vec::new();
    if missing_mpv {
        missing.push("mpv");
    }
    if missing_ytdlp {
        missing.push("yt-dlp");
    }
    let missing_pw = !has("pw-record") || !has("pw-dump");
    let missing_js = !has("node") && !has("deno");
    let what = missing.join(" and ");
    let verb = if missing.len() == 1 { "it isn't" } else { "they aren't" };
    let mut msg = format!("pova needs {what} to play music, but {verb} on your PATH.\n");

    // Catch common mistakes before telling the user to reinstall packages.
    for &bin in &missing {
        if let Some(p) = (probe.non_exec)(bin) {
            msg.push_str(&format!(
                "\nFound {bin} at {}, but it isn't executable. Fix it with:\n  chmod +x {}\n",
                p.display(),
                p.display()
            ));
        } else if let Some(p) = (probe.local_bin)(bin) {
            msg.push_str(&format!(
                "\nFound {bin} at {}, but ~/.local/bin isn't in your PATH.\n\
                 Add this to your shell rc file (~/.bashrc or ~/.zshrc) and restart your terminal:\n  \
                 export PATH=\"$HOME/.local/bin:$PATH\"\n",
                p.display()
            ));
        }
    }
    if missing_mpv && probe.flatpak_mpv {
        msg.push_str(
            "\nNote: mpv looks installed as a Flatpak, which pova can't control over IPC.\n\
             Install your distro's native mpv package instead.\n",
        );
    }

    if let Some(d) = detect_distro(probe) {
        let mut pkgs = Vec::new();
        if missing_mpv {
            pkgs.push(d.mpv);
        }
        if missing_ytdlp {
            pkgs.push(d.yt_dlp);
        }
        if missing_pw {
            pkgs.push(d.pipewire);
        }
        if missing_js {
            pkgs.push(d.node);
        }
        msg.push_str(&format!("\nInstall with:\n  {} {}\n", d.cmd, pkgs.join(" ")));
        if let Some(note) = d.note {
            msg.push_str(&format!("{note}\n"));
        }
    } else {
        msg.push_str(
            "\nInstall the helpers for your distro:\n  \
             sudo dnf install mpv yt-dlp pipewire-utils nodejs      # Fedora / RHEL\n  \
             sudo apt update && sudo apt install mpv yt-dlp pipewire-bin nodejs  # Debian / Ubuntu\n  \
             sudo pacman -S mpv yt-dlp pipewire-audio nodejs        # Arch\n  \
             sudo zypper install mpv yt-dlp pipewire-tools nodejs   # openSUSE\n  \
             sudo xbps-install -S mpv yt-dlp pipewire nodejs        # Void\n  \
             sudo apk add mpv yt-dlp pipewire-tools nodejs          # Alpine\n",
        );
    }
    Err(msg)
}

#[cfg(test)]
mod dep_tests {
    use super::*;

    fn probe_with<'a>(
        has: &'a dyn Fn(&str) -> bool,
        non_exec: &'a dyn Fn(&str) -> Option<PathBuf>,
        local_bin: &'a dyn Fn(&str) -> Option<PathBuf>,
    ) -> DepProbe<'a> {
        DepProbe {
            has,
            non_exec,
            local_bin,
            flatpak_mpv: false,
            ostree: false,
        }
    }

    #[test]
    fn ok_when_mpv_and_ytdlp_present() {
        let none = |_: &str| None;
        let has = |b: &str| matches!(b, "mpv" | "yt-dlp");
        assert_eq!(missing_deps_message(&probe_with(&has, &none, &none)), Ok(()));
    }

    #[test]
    fn suggests_exact_command_per_distro() {
        let none = |_: &str| None;
        // Fedora with only mpv missing
        let fedora = |b: &str| matches!(b, "yt-dlp" | "pw-record" | "pw-dump" | "node" | "dnf");
        let err = missing_deps_message(&probe_with(&fedora, &none, &none)).unwrap_err();
        assert!(err.contains("sudo dnf install mpv\n"), "{err}");

        // Atomic Fedora (Silverblue / Bazzite)
        let ostree_has = |b: &str| matches!(b, "dnf" | "rpm-ostree");
        let mut p = probe_with(&ostree_has, &none, &none);
        p.ostree = true;
        let err = missing_deps_message(&p).unwrap_err();
        assert!(
            err.contains("rpm-ostree install mpv yt-dlp pipewire-utils nodejs"),
            "{err}"
        );

        // Debian / Ubuntu, Arch, openSUSE, Void, Alpine, Gentoo, Solus, NixOS
        for (mgr, expected) in [
            (
                "apt",
                "sudo apt update && sudo apt install mpv yt-dlp pipewire-bin nodejs",
            ),
            ("pacman", "sudo pacman -S mpv yt-dlp pipewire-audio nodejs"),
            ("zypper", "sudo zypper install mpv yt-dlp pipewire-tools nodejs"),
            ("xbps-install", "sudo xbps-install -S mpv yt-dlp pipewire nodejs"),
            ("apk", "sudo apk add mpv yt-dlp pipewire-tools nodejs"),
            (
                "emerge",
                "sudo emerge media-video/mpv net-misc/yt-dlp media-video/pipewire net-libs/nodejs",
            ),
            ("eopkg", "sudo eopkg install mpv yt-dlp pipewire nodejs"),
            ("nix", "nix-shell -p mpv yt-dlp pipewire nodejs"),
        ] {
            let has = |b: &str| b == mgr;
            let err = missing_deps_message(&probe_with(&has, &none, &none)).unwrap_err();
            assert!(err.contains(expected), "{mgr}: {err}");
        }

        // Unknown distro fallback
        let empty = |_: &str| false;
        let fallback = missing_deps_message(&probe_with(&empty, &none, &none)).unwrap_err();
        assert!(fallback.contains("sudo pacman -S") && fallback.contains("sudo apk add"));
    }

    #[test]
    fn catches_chmod_missing_path_and_flatpak_mpv() {
        let has = |b: &str| b == "dnf";
        let non_exec = |b: &str| (b == "yt-dlp").then(|| PathBuf::from("/home/u/.local/bin/yt-dlp"));
        let local_bin = |b: &str| (b == "mpv").then(|| PathBuf::from("/home/u/.local/bin/mpv"));
        let mut p = probe_with(&has, &non_exec, &local_bin);
        p.flatpak_mpv = true;
        let err = missing_deps_message(&p).unwrap_err();
        assert!(err.contains("chmod +x /home/u/.local/bin/yt-dlp"), "{err}");
        assert!(err.contains("~/.local/bin isn't in your PATH"), "{err}");
        assert!(err.contains("Flatpak"), "{err}");
    }
}

//! Headless mpv controlled over its JSON IPC socket.
//!
//! The actor owns the mpv child process. It accepts [`PlayerCmd`]s and emits
//! [`PlayerEvent`]s. mpv's internal playlist only ever holds the current track
//! plus the preloaded next one (for gapless playback); the real queue lives in
//! the app.

use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;

use anyhow::Context;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::process::{Child, Command};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

#[derive(Debug, Clone)]
pub enum PlayerCmd {
    /// Replace playback with `url`, preloading `next` after it.
    Load {
        url: String,
        next: Option<String>,
        /// Start this many seconds in (resume).
        start: Option<f64>,
        /// Load without starting playback (resume).
        paused: bool,
    },
    /// Replace whatever is queued after the current file.
    SetNext(Option<String>),
    TogglePause,
    /// Explicitly pause or unpause without toggling.
    SetPause(bool),
    SeekRelative(f64),
    SeekAbsolute(f64),
    SetVolume(f64),
    SetMute(bool),
    /// Loop the current file forever (repeat-one).
    SetLoopFile(bool),
    /// Even out loudness between songs (adds / removes [`NORMALIZE_FILTER`]).
    SetNormalize(bool),
    /// Apply or clear pova's labeled 10-band EQ filter (`@povaeq:...`).
    SetEq(Option<String>),
    /// Playback speed multiplier (0.25..=3.0).
    SetSpeed(f64),
    /// Keep original musical pitch when speed != 1.0.
    SetPitchCorrection(bool),
    /// Update `ytdl-format` for future tracks loaded via mpv's yt-dlp hook.
    SetYtdlFormat(String),
    Stop,
    Quit,
}

#[derive(Debug, Clone)]
pub enum PlayerEvent {
    Ready,
    TimePos(f64),
    Duration(f64),
    Paused(bool),
    Volume(f64),
    Muted(bool),
    Speed(f64),
    AudioCodec(Option<String>),
    SampleRate(Option<u32>),
    Channels(Option<u8>),
    AudioBitrate(Option<u64>),
    /// The file mpv is currently playing changed.
    Path(Option<String>),
    /// mpv has nothing to play.
    Idle(bool),
    Buffering(bool),
    /// A file finished; `reason` is mpv's end-file reason (eof, error, ...).
    EndFile {
        reason: String,
        error: Option<String>,
    },
    Error(String),
    Exited,
}

pub struct MpvConfig {
    pub socket: PathBuf,
    pub volume: u8,
    pub extra_args: Vec<String>,
    /// `--audio-client-name`, unique per pova instance (see [`audio_client_name`]).
    pub client_name: String,
    /// Start with [`NORMALIZE_FILTER`] on.
    pub normalize: bool,
    /// Initial `ytdl-format` selector (from [`crate::audio::AudioQuality`]).
    pub ytdl_format: String,
    /// Initial labeled EQ filter (`@povaeq:...`), if enabled.
    pub eq_filter: Option<String>,
    /// Initial playback speed (default 1.0).
    pub speed: f64,
    /// Keep original pitch when speed != 1.0.
    pub pitch_correction: bool,
}

/// Label of pova's loudness filter in mpv's `af` chain, so it can be removed
/// again without touching filters from the user's `mpv_args`.
const NORMALIZE_LABEL: &str = "@povanorm";

/// EBU R128 loudness normalization to -14 LUFS (YouTube's own reference
/// level), with a true-peak ceiling so loud masters don't clip. loudnorm
/// outputs 192 kHz in this mode, so resample back to a normal rate.
pub const NORMALIZE_FILTER: &str = "lavfi=[loudnorm=I=-14:TP=-1.5:LRA=11,aresample=48000]";

fn normalize_filter() -> String {
    format!("{NORMALIZE_LABEL}:{NORMALIZE_FILTER}")
}

const OBSERVED: &[&str] = &[
    "time-pos",
    "duration",
    "pause",
    "volume",
    "mute",
    "path",
    "idle-active",
    "paused-for-cache",
    "speed",
    "audio-codec-name",
    "audio-params/samplerate",
    "audio-params/channel-count",
    "audio-bitrate",
];

fn spawn_mpv(cfg: &MpvConfig) -> anyhow::Result<Child> {
    let _ = std::fs::remove_file(&cfg.socket);
    let mut cmd = Command::new("mpv");
    cmd.arg("--idle=yes")
        .arg("--no-video")
        .arg("--no-terminal")
        .arg("--force-window=no")
        .arg("--audio-display=no")
        .arg(format!("--input-ipc-server={}", cfg.socket.display()))
        .arg(format!("--audio-client-name={}", cfg.client_name))
        .arg(format!("--volume={}", cfg.volume))
        .arg("--volume-max=100")
        .arg("--prefetch-playlist=yes")
        .arg("--gapless-audio=weak")
        .arg("--cache=yes")
        .arg(format!("--ytdl-format={}", cfg.ytdl_format))
        .arg(format!("--speed={:.2}", cfg.speed.clamp(0.25, 3.0)))
        .arg(format!(
            "--audio-pitch-correction={}",
            if cfg.pitch_correction { "yes" } else { "no" }
        ))
        .arg("--keep-open=no");
    if cfg.normalize {
        cmd.arg(format!("--af-append={}", normalize_filter()));
    }
    if let Some(eq) = &cfg.eq_filter {
        cmd.arg(format!("--af-append={eq}"));
    }
    // yt-dlp needs a JS runtime for YouTube's signature challenges; it only
    // enables deno by default, so point it at node when that is all we have.
    if which("deno").is_none() && which("node").is_some() {
        cmd.arg("--ytdl-raw-options-append=js-runtimes=node");
    }
    cmd.args(&cfg.extra_args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    // SAFETY: prctl is async-signal-safe; ensures mpv dies with us even on SIGKILL.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    cmd.spawn().context("failed to start mpv (is it installed?)")
}

/// PipeWire client name of this instance's mpv: `pova-<pid>`. Lets the
/// visualizer find *our* stream even when several pova sessions run.
pub fn audio_client_name() -> String {
    format!("pova-{}", std::process::id())
}

fn is_executable(p: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    p.is_file() && p.metadata().is_ok_and(|m| m.permissions().mode() & 0o111 != 0)
}

pub fn which(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| is_executable(p))
}

/// File named `bin` on PATH that exists but lacks execute permission (`chmod +x`).
pub fn find_non_executable(bin: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|d| d.join(bin))
        .find(|p| p.is_file() && !is_executable(p))
}

/// Run the player actor until `Quit` or mpv exits.
pub async fn run(cfg: MpvConfig, mut cmds: UnboundedReceiver<PlayerCmd>, events: UnboundedSender<PlayerEvent>) {
    if let Err(e) = run_inner(&cfg, &mut cmds, &events).await {
        tracing::error!("player: {e:#}");
        let _ = events.send(PlayerEvent::Error(format!("{e:#}")));
    }
    let _ = std::fs::remove_file(&cfg.socket);
    let _ = events.send(PlayerEvent::Exited);
}

async fn run_inner(
    cfg: &MpvConfig,
    cmds: &mut UnboundedReceiver<PlayerCmd>,
    events: &UnboundedSender<PlayerEvent>,
) -> anyhow::Result<()> {
    let mut child = spawn_mpv(cfg)?;

    // Wait for the IPC socket to appear.
    let mut stream = None;
    for _ in 0..100 {
        if let Ok(s) = UnixStream::connect(&cfg.socket).await {
            stream = Some(s);
            break;
        }
        if let Some(status) = child.try_wait()? {
            anyhow::bail!("mpv exited early ({status})");
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let stream = stream.context("timed out connecting to mpv IPC socket")?;
    let (read, mut write) = stream.into_split();

    let mut request_id: u64 = 0;
    let mut send = async |write: &mut tokio::net::unix::OwnedWriteHalf, cmd: Value| -> anyhow::Result<()> {
        request_id += 1;
        let mut line = serde_json::to_vec(&json!({ "command": cmd, "request_id": request_id }))?;
        line.push(b'\n');
        write.write_all(&line).await?;
        Ok(())
    };

    for (i, name) in OBSERVED.iter().enumerate() {
        send(&mut write, json!(["observe_property", i + 1, name])).await?;
    }
    let _ = events.send(PlayerEvent::Ready);

    let mut lines = BufReader::new(read).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                match line? {
                    Some(line) => handle_line(&line, events),
                    None => break, // socket closed: mpv quit
                }
            }
            cmd = cmds.recv() => {
                let Some(cmd) = cmd else { break };
                tracing::debug!("mpv <- {cmd:?}");
                match cmd {
                    PlayerCmd::Load { url, next, start, paused } => {
                        if let Some(s) = start {
                            // Per-file options (mpv >= 0.38: index, then options).
                            send(&mut write, json!(["set_property", "pause", paused])).await?;
                            send(&mut write, json!(["loadfile", url, "replace", -1, format!("start={s:.1}")])).await?;
                        } else if paused {
                            // Plain 3-arg loadfile works on older mpv (< 0.38) too.
                            send(&mut write, json!(["set_property", "pause", true])).await?;
                            send(&mut write, json!(["loadfile", url, "replace"])).await?;
                        } else {
                            send(&mut write, json!(["loadfile", url, "replace"])).await?;
                            send(&mut write, json!(["set_property", "pause", false])).await?;
                        }
                        if let Some(next) = next {
                            send(&mut write, json!(["loadfile", next, "append"])).await?;
                        }
                    }
                    PlayerCmd::SetNext(next) => {
                        // playlist-clear keeps the currently playing entry.
                        send(&mut write, json!(["playlist-clear"])).await?;
                        if let Some(next) = next {
                            send(&mut write, json!(["loadfile", next, "append"])).await?;
                        }
                    }
                    PlayerCmd::TogglePause => send(&mut write, json!(["cycle", "pause"])).await?,
                    PlayerCmd::SetPause(p) => send(&mut write, json!(["set_property", "pause", p])).await?,
                    PlayerCmd::SeekRelative(s) => send(&mut write, json!(["seek", s, "relative"])).await?,
                    PlayerCmd::SeekAbsolute(s) => send(&mut write, json!(["seek", s, "absolute"])).await?,
                    PlayerCmd::SetVolume(v) => send(&mut write, json!(["set_property", "volume", v])).await?,
                    PlayerCmd::SetMute(m) => send(&mut write, json!(["set_property", "mute", m])).await?,
                    PlayerCmd::SetLoopFile(on) => {
                        send(&mut write, json!(["set_property", "loop-file", if on { "inf" } else { "no" }])).await?
                    }
                    PlayerCmd::SetNormalize(on) => {
                        if on {
                            send(&mut write, json!(["af", "add", normalize_filter()])).await?
                        } else {
                            send(&mut write, json!(["af", "remove", NORMALIZE_LABEL])).await?
                        }
                    }
                    PlayerCmd::SetEq(filter) => {
                        let _ = send(&mut write, json!(["af", "remove", crate::audio::EQ_FILTER_LABEL])).await;
                        if let Some(f) = filter {
                            send(&mut write, json!(["af", "add", f])).await?;
                        }
                    }
                    PlayerCmd::SetSpeed(s) => {
                        send(&mut write, json!(["set_property", "speed", s.clamp(0.25, 3.0)])).await?
                    }
                    PlayerCmd::SetPitchCorrection(on) => {
                        send(
                            &mut write,
                            json!(["set_property", "audio-pitch-correction", if on { "yes" } else { "no" }]),
                        )
                        .await?
                    }
                    PlayerCmd::SetYtdlFormat(fmt) => {
                        send(&mut write, json!(["set_property", "ytdl-format", fmt])).await?
                    }
                    PlayerCmd::Stop => send(&mut write, json!(["stop"])).await?,
                    PlayerCmd::Quit => {
                        let _ = send(&mut write, json!(["quit"])).await;
                        break;
                    }
                }
            }
            status = child.wait() => {
                anyhow::bail!("mpv exited unexpectedly ({})", status?);
            }
        }
    }

    // Graceful shutdown, then make sure.
    if tokio::time::timeout(Duration::from_millis(800), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
    }
    Ok(())
}

fn handle_line(line: &str, events: &UnboundedSender<PlayerEvent>) {
    let Ok(v) = serde_json::from_str::<Value>(line) else {
        tracing::warn!("mpv: unparsable line {line}");
        return;
    };
    if let Some(err) = v.get("error").and_then(Value::as_str)
        && err != "success"
    {
        tracing::warn!("mpv command failed: {line}");
    }
    let Some(event) = v.get("event").and_then(Value::as_str) else {
        return;
    };
    let ev = match event {
        "property-change" => {
            let name = v.get("name").and_then(Value::as_str).unwrap_or_default();
            let data = v.get("data");
            let num = || data.and_then(Value::as_f64);
            let flag = || data.and_then(Value::as_bool);
            match name {
                "time-pos" => num().map(PlayerEvent::TimePos),
                "duration" => num().map(PlayerEvent::Duration),
                "pause" => flag().map(PlayerEvent::Paused),
                "volume" => num().map(PlayerEvent::Volume),
                "mute" => flag().map(PlayerEvent::Muted),
                "path" => Some(PlayerEvent::Path(data.and_then(Value::as_str).map(str::to_string))),
                "idle-active" => flag().map(PlayerEvent::Idle),
                "paused-for-cache" => flag().map(PlayerEvent::Buffering),
                "speed" => num().map(PlayerEvent::Speed),
                "audio-codec-name" => Some(PlayerEvent::AudioCodec(
                    data.and_then(Value::as_str).map(str::to_string),
                )),
                "audio-params/samplerate" => Some(PlayerEvent::SampleRate(
                    num().filter(|&v| v > 0.0).map(|v| v.round() as u32),
                )),
                "audio-params/channel-count" => Some(PlayerEvent::Channels(
                    num().filter(|&v| v > 0.0).map(|v| v.round() as u8),
                )),
                "audio-bitrate" => Some(PlayerEvent::AudioBitrate(
                    num().filter(|&v| v > 0.0).map(|v| v.round() as u64),
                )),
                _ => None,
            }
        }
        "end-file" => Some(PlayerEvent::EndFile {
            reason: v.get("reason").and_then(Value::as_str).unwrap_or("unknown").to_string(),
            error: v.get("file_error").and_then(Value::as_str).map(str::to_string),
        }),
        _ => None,
    };
    if let Some(ev) = ev {
        let _ = events.send(ev);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_property_events() {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        handle_line(
            r#"{"event":"property-change","id":1,"name":"time-pos","data":12.5}"#,
            &tx,
        );
        handle_line(r#"{"event":"property-change","id":5,"name":"path","data":null}"#, &tx);
        handle_line(
            r#"{"event":"end-file","reason":"error","file_error":"loading failed"}"#,
            &tx,
        );
        handle_line(r#"{"request_id":1,"error":"success"}"#, &tx);
        assert!(matches!(rx.try_recv(), Ok(PlayerEvent::TimePos(t)) if t == 12.5));
        assert!(matches!(rx.try_recv(), Ok(PlayerEvent::Path(None))));
        assert!(matches!(rx.try_recv(), Ok(PlayerEvent::EndFile { reason, .. }) if reason == "error"));
        assert!(rx.try_recv().is_err());
    }
}

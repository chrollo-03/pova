//! Native Discord Rich Presence over local Unix IPC sockets (`discord-ipc-0..9`).
//!
//! Zero extra Cargo dependencies: implements Discord's binary frame protocol
//! (`opcode: u32 LE`, `len: u32 LE`, JSON payload) directly over `tokio::net::UnixStream`.
//! Displays a Spotify-style "Listening to ..." activity with live album art,
//! progress bar timestamps, and a clickable `song.link` universal interlink button.

use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::UnixStream;
use tokio::sync::mpsc::UnboundedReceiver;

/// Default Discord Application ID for pova (can be overridden via `discord_app_id` in config.toml).
pub const DEFAULT_APP_ID: &str = "1310265874295001128";

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;

#[derive(Clone, Debug, PartialEq)]
pub enum PresenceCmd {
    SetEnabled(bool),
    Update(Option<PresenceTrack>),
    Clear,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PresenceTrack {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    pub album: Option<String>,
    pub art_url: String,
    pub quality_badge: String,
    pub position: f64,
    pub duration: f64,
    pub paused: bool,
}

/// Candidate Unix socket paths used by native, Flatpak, and Snap Discord installs on Linux.
pub fn socket_candidates() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for var in ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"] {
        if let Ok(v) = std::env::var(var)
            && !v.is_empty()
        {
            let p = PathBuf::from(&v);
            dirs.push(p.clone());
            dirs.push(p.join("app/com.discordapp.Discord"));
            dirs.push(p.join("app/dev.vencord.Vesktop"));
            dirs.push(p.join("snap.discord"));
        }
    }
    dirs.push(PathBuf::from("/tmp"));

    let mut out = Vec::new();
    for dir in dirs {
        for i in 0..10 {
            out.push(dir.join(format!("discord-ipc-{i}")));
        }
    }
    out
}

/// Encode a Discord IPC frame: `[opcode: u32 LE][len: u32 LE][utf8 JSON]`.
pub fn encode_frame(opcode: u32, payload: &Value) -> Vec<u8> {
    let body = payload.to_string();
    let bytes = body.as_bytes();
    let mut buf = Vec::with_capacity(8 + bytes.len());
    buf.extend_from_slice(&opcode.to_le_bytes());
    buf.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
    buf.extend_from_slice(bytes);
    buf
}

/// Build the `SET_ACTIVITY` JSON payload for `track` (or `None` to clear).
pub fn build_activity_payload(track: Option<&PresenceTrack>, pid: u32, now_secs: u64) -> Value {
    let activity = match track {
        Some(t) => {
            let details = truncate_discord(&t.title, 128);
            let state = if t.paused {
                truncate_discord(&format!("Paused · {}", t.artists), 128)
            } else if t.artists.trim().is_empty() {
                "YouTube Music".into()
            } else {
                truncate_discord(&t.artists, 128)
            };
            let large_text = match t.album.as_deref().filter(|a| !a.is_empty()) {
                Some(alb) => truncate_discord(&format!("{alb} · {}", t.quality_badge), 128),
                None => truncate_discord(&format!("pova · {}", t.quality_badge), 128),
            };
            let mut act = json!({
                "type": 2, // Listening
                "details": details,
                "state": state,
                "assets": {
                    "large_image": t.art_url,
                    "large_text": large_text,
                },
                "buttons": [
                    {
                        "label": "Open on song.link",
                        "url": crate::interlink::universal_url(&t.video_id),
                    },
                    {
                        "label": "YouTube Music",
                        "url": crate::interlink::ytm_url(&t.video_id, None),
                    }
                ]
            });
            if !t.paused && t.duration > 1.0 && t.position.is_finite() {
                let start = now_secs.saturating_sub(t.position.max(0.0).floor() as u64);
                let end = start.saturating_add(t.duration.ceil() as u64);
                act["timestamps"] = json!({ "start": start, "end": end });
            }
            act
        }
        None => Value::Null,
    };
    json!({
        "cmd": "SET_ACTIVITY",
        "args": {
            "pid": pid,
            "activity": activity,
        },
        "nonce": format!("pova-{pid}-{now_secs}"),
    })
}

fn truncate_discord(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() < 2 {
        return format!("{s}  ");
    }
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

async fn connect(app_id: &str) -> Option<UnixStream> {
    for path in socket_candidates() {
        let Ok(mut stream) = UnixStream::connect(&path).await else {
            continue;
        };
        let hello = encode_frame(OP_HANDSHAKE, &json!({ "v": 1, "client_id": app_id }));
        if stream.write_all(&hello).await.is_err() {
            continue;
        }
        // Read handshake response frame (header + payload) with a short timeout.
        let mut hdr = [0u8; 8];
        let ok = tokio::time::timeout(Duration::from_secs(2), async {
            stream.read_exact(&mut hdr).await?;
            let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
            if len > 65_536 {
                return Err(std::io::Error::other("oversized discord handshake"));
            }
            let mut body = vec![0u8; len];
            stream.read_exact(&mut body).await?;
            Ok(())
        })
        .await;
        if matches!(ok, Ok(Ok(()))) {
            tracing::info!("discord rich presence connected ({})", path.display());
            return Some(stream);
        }
    }
    None
}

async fn send_activity(stream: &mut UnixStream, track: Option<&PresenceTrack>, pid: u32) -> bool {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let frame = encode_frame(OP_FRAME, &build_activity_payload(track, pid, now));
    if stream.write_all(&frame).await.is_err() {
        return false;
    }
    // Drain reply non-blockingly / with short timeout so Discord's socket buffer never fills.
    let mut hdr = [0u8; 8];
    if let Ok(Ok(_)) = tokio::time::timeout(Duration::from_millis(250), stream.read_exact(&mut hdr)).await {
        let len = u32::from_le_bytes([hdr[4], hdr[5], hdr[6], hdr[7]]) as usize;
        if len <= 65_536 {
            let mut body = vec![0u8; len];
            let _ = tokio::time::timeout(Duration::from_millis(250), stream.read_exact(&mut body)).await;
        }
    }
    true
}

/// Run the Discord Rich Presence background actor.
pub async fn run(mut enabled: bool, app_id: String, mut rx: UnboundedReceiver<PresenceCmd>) {
    let pid = std::process::id();
    let app_id = if app_id.trim().is_empty() {
        DEFAULT_APP_ID.to_string()
    } else {
        app_id
    };
    let mut stream: Option<UnixStream> = None;
    let mut latest: Option<PresenceTrack> = None;
    let mut retry = tokio::time::interval(Duration::from_secs(15));
    retry.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            cmd = rx.recv() => {
                let Some(cmd) = cmd else { break };
                match cmd {
                    PresenceCmd::SetEnabled(on) => {
                        enabled = on;
                        if !enabled {
                            if let Some(mut s) = stream.take() {
                                let _ = send_activity(&mut s, None, pid).await;
                                let _ = s.write_all(&encode_frame(OP_CLOSE, &json!({}))).await;
                            }
                        } else {
                            if stream.is_none() {
                                stream = connect(&app_id).await;
                            }
                            if let Some(s) = stream.as_mut()
                                && !send_activity(s, latest.as_ref(), pid).await
                            {
                                stream = None;
                            }
                        }
                    }
                    PresenceCmd::Update(track) => {
                        latest = track;
                        if !enabled {
                            continue;
                        }
                        if stream.is_none() {
                            stream = connect(&app_id).await;
                        }
                        if let Some(s) = stream.as_mut()
                            && !send_activity(s, latest.as_ref(), pid).await
                        {
                            stream = None;
                        }
                    }
                    PresenceCmd::Clear => {
                        latest = None;
                        if let Some(s) = stream.as_mut()
                            && !send_activity(s, None, pid).await
                        {
                            stream = None;
                        }
                    }
                }
            }
            _ = retry.tick() => {
                if enabled && stream.is_none() && latest.is_some() {
                    stream = connect(&app_id).await;
                    if let Some(s) = stream.as_mut()
                        && !send_activity(s, latest.as_ref(), pid).await
                    {
                        stream = None;
                    }
                }
            }
        }
    }
    if let Some(mut s) = stream {
        let _ = send_activity(&mut s, None, pid).await;
        let _ = s.write_all(&encode_frame(OP_CLOSE, &json!({}))).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_little_endian_ipc_frame() {
        let frame = encode_frame(OP_HANDSHAKE, &json!({"v": 1}));
        assert_eq!(&frame[0..4], &0u32.to_le_bytes());
        let len = u32::from_le_bytes([frame[4], frame[5], frame[6], frame[7]]) as usize;
        assert_eq!(len, frame.len() - 8);
        assert_eq!(&frame[8..], br#"{"v":1}"#);
    }

    #[test]
    fn builds_listening_activity_with_timestamps_and_songlink_button() {
        let t = PresenceTrack {
            video_id: "abc123".into(),
            title: "Fine Again".into(),
            artists: "Seether".into(),
            album: Some("Disclaimer".into()),
            art_url: "https://example.com/art.jpg".into(),
            quality_badge: "MAX · OPUS 48kHz".into(),
            position: 40.0,
            duration: 240.0,
            paused: false,
        };
        let v = build_activity_payload(Some(&t), 42, 1000);
        assert_eq!(v["args"]["activity"]["type"], 2);
        assert_eq!(v["args"]["activity"]["details"], "Fine Again");
        assert_eq!(v["args"]["activity"]["timestamps"]["start"], 960);
        assert_eq!(v["args"]["activity"]["timestamps"]["end"], 1200);
        assert_eq!(
            v["args"]["activity"]["buttons"][0]["url"],
            "https://song.link/y/abc123"
        );

        let paused = PresenceTrack { paused: true, ..t };
        let vp = build_activity_payload(Some(&paused), 42, 1000);
        assert!(vp["args"]["activity"].get("timestamps").is_none());
        assert_eq!(vp["args"]["activity"]["state"], "Paused · Seether");
    }
}

//! Stream-URL resolver actor.
//!
//! Turning a YouTube Music link into a playable audio URL takes yt-dlp about
//! 3 s (it has to solve YouTube's JS challenge). This actor runs that work
//! ahead of time for tracks the user is likely to play next, so mpv can start
//! them from a direct URL in well under a second. Resolution is deliberately
//! conservative (at most two concurrent yt-dlp runs, de-duplicated) to stay
//! far from YouTube's rate limits.

use std::collections::HashSet;
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::process::Command;
use tokio::sync::Semaphore;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::player::mpv::which;

const MAX_CONCURRENT: usize = 2;
const TIMEOUT: Duration = Duration::from_secs(30);
/// Treat URLs as stale this long before YouTube's `expire=` deadline.
const EXPIRY_MARGIN: Duration = Duration::from_secs(10 * 60);
/// Used when the URL carries no `expire=` parameter.
const DEFAULT_LIFETIME: Duration = Duration::from_secs(4 * 60 * 60);

#[derive(Debug)]
pub struct ResolveReq {
    pub video_id: String,
    pub format: String,
}

#[derive(Debug)]
pub struct Resolved {
    pub video_id: String,
    /// Direct audio URL and the time it should no longer be used.
    pub stream: Option<(String, SystemTime)>,
}

pub async fn run(mut reqs: UnboundedReceiver<ResolveReq>, events: UnboundedSender<Resolved>) {
    let slots = Arc::new(Semaphore::new(MAX_CONCURRENT));
    let in_flight = Arc::new(std::sync::Mutex::new(HashSet::<String>::new()));
    let node_runtime = which("deno").is_none() && which("node").is_some();
    while let Some(req) = reqs.recv().await {
        if !in_flight.lock().unwrap().insert(req.video_id.clone()) {
            continue;
        }
        let (slots, in_flight, events) = (slots.clone(), in_flight.clone(), events.clone());
        tokio::spawn(async move {
            let _permit = slots.acquire().await;
            let started = std::time::Instant::now();
            let stream = match resolve(&req.video_id, &req.format, node_runtime).await {
                Ok(url) => {
                    tracing::debug!("resolved {} in {:.1}s", req.video_id, started.elapsed().as_secs_f32());
                    let deadline = expiry(&url, SystemTime::now());
                    Some((url, deadline))
                }
                Err(e) => {
                    tracing::info!("resolve {} failed: {e:#}", req.video_id);
                    None
                }
            };
            in_flight.lock().unwrap().remove(&req.video_id);
            let _ = events.send(Resolved {
                video_id: req.video_id,
                stream,
            });
        });
    }
}

async fn resolve(video_id: &str, format: &str, node_runtime: bool) -> anyhow::Result<String> {
    let mut cmd = Command::new("yt-dlp");
    cmd.args(["-q", "--no-warnings", "--no-playlist", "-f", format, "-g"]);
    if node_runtime {
        cmd.args(["--js-runtimes", "node"]);
    }
    cmd.arg(format!("https://music.youtube.com/watch?v={video_id}"))
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    // SAFETY: prctl is async-signal-safe; yt-dlp dies with pova.
    unsafe {
        cmd.pre_exec(|| {
            libc::prctl(libc::PR_SET_PDEATHSIG, libc::SIGTERM);
            Ok(())
        });
    }
    let out = tokio::time::timeout(TIMEOUT, cmd.output()).await??;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("yt-dlp: {}", err.lines().last().unwrap_or("failed"));
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .find(|l| l.starts_with("http"))
        .map(str::to_string)
        .ok_or_else(|| anyhow::anyhow!("yt-dlp printed no URL"))
}

/// When a googlevideo URL should stop being used (`expire=<unix>` minus a margin).
pub fn expiry(url: &str, now: SystemTime) -> SystemTime {
    let expire = url
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("expire="))
        .and_then(|v| v.parse::<u64>().ok())
        .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
    match expire {
        Some(t) => t.checked_sub(EXPIRY_MARGIN).unwrap_or(now),
        None => now + DEFAULT_LIFETIME,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expiry_from_url() {
        let now = UNIX_EPOCH + Duration::from_secs(1_000_000);
        let url = "https://rr1---sn.googlevideo.com/videoplayback?expire=1021600&ei=x&id=y";
        assert_eq!(expiry(url, now), UNIX_EPOCH + Duration::from_secs(1_021_600 - 600));
        assert_eq!(expiry("https://x/y?id=1", now), now + DEFAULT_LIFETIME);
    }
}

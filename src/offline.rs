//! Offline audio cache: save any track locally via `yt-dlp` for instant 0 ms
//! playback even with no internet connection.
//!
//! Files live in `~/.cache/pova/offline/` with a JSON index (`index.json`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};

use crate::model::Track;
use crate::player::mpv::which;

const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(180);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct OfflineEntry {
    pub track: Track,
    /// Filename relative to the offline cache directory.
    pub file: String,
    pub bytes: u64,
    pub saved_at: u64,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct OfflineIndex {
    pub entries: HashMap<String, OfflineEntry>,
}

impl OfflineIndex {
    pub fn load(dir: &Path) -> OfflineIndex {
        let index_path = dir.join("index.json");
        let Ok(text) = std::fs::read_to_string(&index_path) else {
            return OfflineIndex::default();
        };
        let mut idx: OfflineIndex = serde_json::from_str(&text).unwrap_or_default();
        // Drop stale entries whose audio file was deleted externally.
        idx.entries.retain(|_, e| dir.join(&e.file).is_file());
        idx
    }

    pub fn save(&self, dir: &Path) {
        let _ = std::fs::create_dir_all(dir);
        let path = dir.join("index.json");
        let tmp = dir.join("index.json.tmp");
        let res = serde_json::to_vec_pretty(self)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, &path));
        if let Err(e) = res {
            tracing::warn!("saving offline index: {e}");
        }
    }

    /// Absolute local path for `video_id` if cached and present on disk.
    pub fn local_path(&self, dir: &Path, video_id: &str) -> Option<PathBuf> {
        let entry = self.entries.get(video_id)?;
        let p = dir.join(&entry.file);
        p.is_file().then_some(p)
    }

    pub fn contains(&self, video_id: &str) -> bool {
        self.entries.contains_key(video_id)
    }

    pub fn remove(&mut self, dir: &Path, video_id: &str) -> Option<OfflineEntry> {
        let entry = self.entries.remove(video_id)?;
        let _ = std::fs::remove_file(dir.join(&entry.file));
        self.save(dir);
        Some(entry)
    }

    pub fn tracks_newest_first(&self) -> Vec<Track> {
        let mut list: Vec<&OfflineEntry> = self.entries.values().collect();
        list.sort_by(|a, b| b.saved_at.cmp(&a.saved_at));
        list.into_iter().map(|e| e.track.clone()).collect()
    }

    pub fn total_bytes(&self) -> u64 {
        self.entries.values().map(|e| e.bytes).sum()
    }
}

#[derive(Debug)]
pub struct OfflineReq {
    pub track: Track,
    pub format: String,
}

#[derive(Debug)]
pub enum OfflineEvent {
    Saved(OfflineEntry),
    Failed { video_id: String, title: String, error: String },
}

pub async fn run(dir: PathBuf, mut reqs: UnboundedReceiver<OfflineReq>, events: UnboundedSender<OfflineEvent>) {
    let _ = tokio::fs::create_dir_all(&dir).await;
    let node_runtime = which("deno").is_none() && which("node").is_some();
    while let Some(req) = reqs.recv().await {
        let dir = dir.clone();
        let events = events.clone();
        tokio::spawn(async move {
            let vid = req.track.video_id.clone();
            let title = req.track.title.clone();
            match download_track(&dir, &req.track, &req.format, node_runtime).await {
                Ok(entry) => {
                    let _ = events.send(OfflineEvent::Saved(entry));
                }
                Err(e) => {
                    let _ = events.send(OfflineEvent::Failed {
                        video_id: vid,
                        title,
                        error: format!("{e:#}"),
                    });
                }
            }
        });
    }
}

fn sanitize_id(id: &str) -> String {
    id.chars()
        .filter(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        .collect()
}

async fn download_track(dir: &Path, track: &Track, format: &str, node_runtime: bool) -> anyhow::Result<OfflineEntry> {
    tokio::fs::create_dir_all(dir).await?;
    let safe_id = sanitize_id(&track.video_id);
    anyhow::ensure!(!safe_id.is_empty(), "invalid video id");
    let template = dir.join(format!("{safe_id}.%(ext)s"));

    let mut cmd = Command::new("yt-dlp");
    cmd.args([
        "-q",
        "--no-warnings",
        "--no-playlist",
        "-f",
        format,
        "-o",
        &template.to_string_lossy(),
        "--print",
        "after_move:filepath",
    ]);
    if node_runtime {
        cmd.args(["--js-runtimes", "node"]);
    }
    cmd.arg(track.url())
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

    let out = tokio::time::timeout(DOWNLOAD_TIMEOUT, cmd.output()).await??;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        anyhow::bail!("{}", err.lines().last().unwrap_or("yt-dlp download failed"));
    }
    let path_str = String::from_utf8_lossy(&out.stdout)
        .lines()
        .rev()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .ok_or_else(|| anyhow::anyhow!("yt-dlp reported no output file"))?
        .to_string();
    let final_path = PathBuf::from(&path_str);
    let meta = tokio::fs::metadata(&final_path).await?;
    let file = final_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| anyhow::anyhow!("unexpected file name"))?
        .to_string();
    let saved_at = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    Ok(OfflineEntry {
        track: track.clone(),
        file,
        bytes: meta.len(),
        saved_at,
    })
}

pub fn format_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.1} GB", bytes as f64 / (1024.0 * 1024.0 * 1024.0))
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{bytes} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: format!("Song {id}"),
            artists: "Band".into(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: Some(180.0),
            thumbnail: None,
            liked: None,
        }
    }

    #[test]
    fn offline_index_persists_resolves_and_removes() {
        let dir = crate::app::test_paths().cache_dir.join("offline");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.opus"), b"fake audio bytes").unwrap();

        let mut idx = OfflineIndex::default();
        idx.entries.insert(
            "a".into(),
            OfflineEntry {
                track: sample("a"),
                file: "a.opus".into(),
                bytes: 16,
                saved_at: 100,
            },
        );
        idx.save(&dir);

        let loaded = OfflineIndex::load(&dir);
        assert!(loaded.contains("a"));
        assert_eq!(loaded.local_path(&dir, "a"), Some(dir.join("a.opus")));
        assert_eq!(loaded.total_bytes(), 16);

        let mut loaded = loaded;
        loaded.remove(&dir, "a");
        assert!(!loaded.contains("a"));
        assert!(!dir.join("a.opus").exists());
    }

    #[test]
    fn formats_byte_sizes() {
        assert_eq!(format_bytes(500), "500 B");
        assert_eq!(format_bytes(2048), "2 KB");
        assert_eq!(format_bytes(5 * 1024 * 1024), "5.0 MB");
    }
}

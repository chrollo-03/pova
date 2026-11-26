//! Spotify Wrapped-style local listening statistics (`~/.local/state/pova/stats.json`).
//!
//! Tracks plays, listening duration, daily streaks, top tracks, top artists, and
//! recent listens — 100% local and private.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::model::Track;

const MAX_RECENT: usize = 50;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackStat {
    pub track: TrackStub,
    pub plays: u64,
    pub seconds: f64,
    pub last_played: u64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TrackStub {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
}

impl From<&Track> for TrackStub {
    fn from(t: &Track) -> Self {
        Self {
            video_id: t.video_id.clone(),
            title: t.title.clone(),
            artists: t.artists.clone(),
            artist_id: t.artist_id.clone(),
            album: t.album.clone(),
            album_id: t.album_id.clone(),
            duration: t.duration,
            thumbnail: t.thumbnail.clone(),
        }
    }
}

impl TrackStub {
    pub fn to_track(&self) -> Track {
        Track {
            video_id: self.video_id.clone(),
            title: self.title.clone(),
            artists: self.artists.clone(),
            artist_id: self.artist_id.clone(),
            album: self.album.clone(),
            album_id: self.album_id.clone(),
            duration: self.duration,
            thumbnail: self.thumbnail.clone(),
            liked: None,
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ArtistStat {
    pub name: String,
    pub artist_id: Option<String>,
    pub plays: u64,
    pub seconds: f64,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Stats {
    pub total_plays: u64,
    pub total_seconds: f64,
    /// UTC day numbers (`unix_secs / 86400`) on which at least one track was played.
    pub active_days: BTreeSet<u64>,
    pub tracks: HashMap<String, TrackStat>,
    pub artists: HashMap<String, ArtistStat>,
    /// Most recent played `video_id`s (newest first).
    pub recent: Vec<String>,
}

impl Stats {
    pub fn load(path: &Path) -> Stats {
        let Ok(text) = std::fs::read_to_string(path) else {
            return Stats::default();
        };
        serde_json::from_str(&text).unwrap_or_else(|e| {
            tracing::warn!("ignoring unreadable {}: {e}", path.display());
            Stats::default()
        })
    }

    pub fn save(&self, path: &Path) {
        let tmp = path.with_extension("json.tmp");
        let res = serde_json::to_vec(self)
            .map_err(std::io::Error::other)
            .and_then(|bytes| std::fs::write(&tmp, bytes))
            .and_then(|()| std::fs::rename(&tmp, path));
        if let Err(e) = res {
            tracing::warn!("saving stats: {e}");
        }
    }

    /// Record a completed/counted play of `track` and its listened duration.
    pub fn record_play(&mut self, track: &Track, listened_secs: f64, now_unix: u64) {
        let secs = listened_secs.max(0.0);
        self.total_plays += 1;
        self.total_seconds += secs;
        self.active_days.insert(now_unix / 86_400);

        let entry = self
            .tracks
            .entry(track.video_id.clone())
            .or_insert_with(|| TrackStat {
                track: TrackStub::from(track),
                plays: 0,
                seconds: 0.0,
                last_played: now_unix,
            });
        entry.track = TrackStub::from(track);
        entry.plays += 1;
        entry.seconds += secs;
        entry.last_played = now_unix;

        let primary = crate::lyrics::primary_artist(&track.artists);
        if !primary.is_empty() {
            let key = primary.to_ascii_lowercase();
            let a = self.artists.entry(key).or_insert_with(|| ArtistStat {
                name: primary.clone(),
                artist_id: track.artist_id.clone(),
                plays: 0,
                seconds: 0.0,
            });
            if a.artist_id.is_none() {
                a.artist_id = track.artist_id.clone();
            }
            a.plays += 1;
            a.seconds += secs;
        }

        self.recent.retain(|id| id != &track.video_id);
        self.recent.insert(0, track.video_id.clone());
        self.recent.truncate(MAX_RECENT);
    }

    /// Add incremental listening seconds without incrementing play count.
    pub fn add_listening_time(&mut self, track: &Track, delta_secs: f64) {
        if !delta_secs.is_finite() || delta_secs <= 0.0 || delta_secs > 15.0 {
            return;
        }
        self.total_seconds += delta_secs;
        if let Some(t) = self.tracks.get_mut(&track.video_id) {
            t.seconds += delta_secs;
        }
        let primary = crate::lyrics::primary_artist(&track.artists);
        if !primary.is_empty()
            && let Some(a) = self.artists.get_mut(&primary.to_ascii_lowercase())
        {
            a.seconds += delta_secs;
        }
    }

    /// Consecutive days with listening activity ending today or yesterday.
    pub fn streak_days(&self, now_unix: u64) -> u64 {
        let today = now_unix / 86_400;
        let mut cursor = if self.active_days.contains(&today) {
            today
        } else if today > 0 && self.active_days.contains(&(today - 1)) {
            today - 1
        } else {
            return 0;
        };
        let mut streak = 0u64;
        while self.active_days.contains(&cursor) {
            streak += 1;
            if cursor == 0 {
                break;
            }
            cursor -= 1;
        }
        streak
    }

    pub fn top_tracks(&self, limit: usize) -> Vec<&TrackStat> {
        let mut list: Vec<&TrackStat> = self.tracks.values().collect();
        list.sort_by(|a, b| {
            b.plays
                .cmp(&a.plays)
                .then_with(|| b.seconds.total_cmp(&a.seconds))
                .then_with(|| b.last_played.cmp(&a.last_played))
        });
        list.truncate(limit);
        list
    }

    pub fn top_artists(&self, limit: usize) -> Vec<&ArtistStat> {
        let mut list: Vec<&ArtistStat> = self.artists.values().collect();
        list.sort_by(|a, b| {
            b.plays
                .cmp(&a.plays)
                .then_with(|| b.seconds.total_cmp(&a.seconds))
        });
        list.truncate(limit);
        list
    }

    pub fn recent_tracks(&self, limit: usize) -> Vec<&TrackStat> {
        self.recent
            .iter()
            .filter_map(|id| self.tracks.get(id))
            .take(limit)
            .collect()
    }

    /// Compact one-line summary for `pova stats`.
    pub fn cli_summary(&self, now_unix: u64) -> String {
        if self.total_plays == 0 {
            return "No listening stats recorded yet — play a song for 25+ seconds!".into();
        }
        let streak = self.streak_days(now_unix);
        let top_artist = self
            .top_artists(1)
            .first()
            .map(|a| format!("{} ({} plays)", a.name, a.plays))
            .unwrap_or_else(|| "—".into());
        let top_track = self
            .top_tracks(1)
            .first()
            .map(|t| format!("{} ({} plays)", t.track.title, t.plays))
            .unwrap_or_else(|| "—".into());
        format!(
            "{} plays · {} · {}d streak · top artist: {} · top track: {}",
            self.total_plays,
            format_hours(self.total_seconds),
            streak,
            top_artist,
            top_track
        )
    }
}

pub fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn format_hours(secs: f64) -> String {
    let total_mins = (secs.max(0.0) / 60.0).round() as u64;
    if total_mins >= 60 {
        format!("{}h {:02}m", total_mins / 60, total_mins % 60)
    } else if total_mins > 0 {
        format!("{total_mins}m")
    } else {
        format!("{}s", secs.max(0.0).round() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_track(id: &str, title: &str, artist: &str) -> Track {
        Track {
            video_id: id.into(),
            title: title.into(),
            artists: artist.into(),
            artist_id: Some("UC1".into()),
            album: Some("Album".into()),
            album_id: None,
            duration: Some(200.0),
            thumbnail: None,
            liked: None,
        }
    }

    #[test]
    fn records_plays_ranks_top_and_computes_streak() {
        let mut s = Stats::default();
        let day = 20_000 * 86_400;
        let t1 = sample_track("a", "Fine Again", "Seether");
        let t2 = sample_track("b", "Broken", "Seether & Amy Lee");
        let t3 = sample_track("c", "One More Time", "Daft Punk");

        s.record_play(&t1, 180.0, day - 2 * 86_400);
        s.record_play(&t1, 200.0, day - 86_400);
        s.record_play(&t2, 240.0, day);
        s.record_play(&t3, 300.0, day);

        assert_eq!(s.total_plays, 4);
        assert_eq!(s.streak_days(day), 3);
        assert_eq!(s.top_tracks(2)[0].track.video_id, "a");
        assert_eq!(s.top_artists(2)[0].name, "Seether");
        assert_eq!(s.top_artists(2)[0].plays, 3);
        assert_eq!(s.recent_tracks(2)[0].track.video_id, "c");
        assert!(s.cli_summary(day).contains("3d streak"));
    }

    #[test]
    fn round_trips_to_disk() {
        let dir = crate::app::test_paths().state_dir;
        let path = dir.join("stats.json");
        let mut s = Stats::default();
        s.record_play(&sample_track("x", "Song", "Band"), 95.0, 1_700_000_000);
        s.save(&path);
        assert_eq!(Stats::load(&path), s);
    }
}

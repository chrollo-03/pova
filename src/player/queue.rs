//! The play queue. pova owns it; mpv only ever holds "current + next".

use rand::seq::SliceRandom;

use crate::model::Track;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Repeat {
    #[default]
    Off,
    All,
    One,
}

impl Repeat {
    pub fn next(self) -> Repeat {
        match self {
            Repeat::Off => Repeat::All,
            Repeat::All => Repeat::One,
            Repeat::One => Repeat::Off,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            Repeat::Off => "off",
            Repeat::All => "all",
            Repeat::One => "one",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Entry {
    /// Monotonic insertion number; sorting by it undoes a shuffle.
    pub uid: u64,
    pub track: Track,
}

#[derive(Debug, Default)]
pub struct Queue {
    pub entries: Vec<Entry>,
    pub current: Option<usize>,
    pub shuffle: bool,
    pub repeat: Repeat,
    next_uid: u64,
}

impl Queue {
    pub fn len(&self) -> usize {
        self.entries.len()
    }
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    pub fn current_track(&self) -> Option<&Track> {
        self.current.and_then(|i| self.entries.get(i)).map(|e| &e.track)
    }
    pub fn get(&self, i: usize) -> Option<&Track> {
        self.entries.get(i).map(|e| &e.track)
    }

    fn make(&mut self, tracks: Vec<Track>) -> Vec<Entry> {
        tracks
            .into_iter()
            .map(|track| {
                self.next_uid += 1;
                Entry {
                    uid: self.next_uid,
                    track,
                }
            })
            .collect()
    }

    /// Index the player should move to automatically after the current track.
    pub fn next_index(&self) -> Option<usize> {
        let cur = self.current?;
        match self.repeat {
            Repeat::One => Some(cur),
            _ if cur + 1 < self.len() => Some(cur + 1),
            Repeat::All if !self.is_empty() => Some(0),
            _ => None,
        }
    }

    /// Index for an explicit "next" key press (ignores repeat-one).
    pub fn skip_index(&self) -> Option<usize> {
        let cur = self.current?;
        if cur + 1 < self.len() {
            Some(cur + 1)
        } else if self.repeat == Repeat::All && !self.is_empty() {
            Some(0)
        } else {
            None
        }
    }

    pub fn prev_index(&self) -> Option<usize> {
        let cur = self.current?;
        if cur > 0 {
            Some(cur - 1)
        } else if self.repeat == Repeat::All && !self.is_empty() {
            Some(self.len() - 1)
        } else {
            None
        }
    }

    /// Insert at the end (or at random upcoming positions while shuffled).
    pub fn append(&mut self, tracks: Vec<Track>) {
        let entries = self.make(tracks);
        if self.shuffle && self.current.is_some() {
            let mut rng = rand::rng();
            for e in entries {
                let lo = self.current.map_or(0, |c| c + 1);
                let pos = rand::RngExt::random_range(&mut rng, lo..=self.len());
                self.entries.insert(pos, e);
            }
        } else {
            self.entries.extend(entries);
        }
    }

    /// Insert right after the current track.
    pub fn insert_next(&mut self, tracks: Vec<Track>) {
        let at = self.current.map_or(self.len(), |c| c + 1);
        let entries = self.make(tracks);
        self.entries.splice(at..at, entries);
    }

    /// Replace the whole queue, starting at `start`.
    pub fn replace(&mut self, tracks: Vec<Track>, start: usize) {
        self.entries = self.make(tracks);
        self.current = if self.entries.is_empty() {
            None
        } else {
            Some(start.min(self.len() - 1))
        };
        if self.shuffle {
            self.shuffle_upcoming();
        }
    }

    pub fn remove(&mut self, i: usize) -> Option<Track> {
        if i >= self.len() {
            return None;
        }
        let e = self.entries.remove(i);
        if let Some(c) = self.current {
            if i < c {
                self.current = Some(c - 1);
            } else if i == c {
                // The playing track was removed: nothing is "current" in the
                // queue any more; the caller decides whether to skip.
                self.current = if self.is_empty() {
                    None
                } else {
                    Some(c.min(self.len() - 1))
                };
            }
        }
        Some(e.track)
    }

    /// Swap item `i` with `i-1` (`up`) or `i+1`. Returns the new index.
    pub fn move_item(&mut self, i: usize, up: bool) -> Option<usize> {
        let j = if up { i.checked_sub(1)? } else { i + 1 };
        if i >= self.len() || j >= self.len() {
            return None;
        }
        self.entries.swap(i, j);
        self.current = self.current.map(|c| {
            if c == i {
                j
            } else if c == j {
                i
            } else {
                c
            }
        });
        Some(j)
    }

    /// Remove everything except the current track.
    pub fn clear_upcoming(&mut self) {
        match self.current {
            Some(c) => {
                let cur = self.entries.remove(c);
                self.entries = vec![cur];
                self.current = Some(0);
            }
            None => self.entries.clear(),
        }
    }

    /// Drop the songs after the current one, keeping what already played.
    pub fn truncate_after_current(&mut self) {
        match self.current {
            Some(c) => self.entries.truncate(c + 1),
            None => self.entries.clear(),
        }
    }

    fn shuffle_upcoming(&mut self) {
        let start = self.current.map_or(0, |c| c + 1);
        if start < self.len() {
            self.entries[start..].shuffle(&mut rand::rng());
        }
    }

    pub fn set_shuffle(&mut self, on: bool) {
        self.shuffle = on;
        if on {
            self.shuffle_upcoming();
        } else {
            // Restore insertion order of the upcoming tracks.
            let start = self.current.map_or(0, |c| c + 1);
            if start < self.len() {
                self.entries[start..].sort_by_key(|e| e.uid);
            }
        }
    }

    pub fn position_of(&self, video_id: &str) -> Option<usize> {
        // Prefer the occurrence closest after the current one.
        let cur = self.current.unwrap_or(0);
        let ids = |i: &usize| self.entries[*i].track.video_id == video_id;
        (cur..self.len()).find(ids).or_else(|| (0..cur).find(ids))
    }

    pub fn contains(&self, video_id: &str) -> bool {
        self.entries.iter().any(|e| e.track.video_id == video_id)
    }

    /// Total remaining seconds in the queue (including the rest of the current track).
    pub fn remaining_duration(&self, current_pos: f64) -> f64 {
        let Some(cur) = self.current else {
            return self.entries.iter().filter_map(|e| e.track.duration).sum();
        };
        let cur_rem = self
            .entries
            .get(cur)
            .and_then(|e| e.track.duration)
            .map(|d| (d - current_pos).max(0.0))
            .unwrap_or(0.0);
        let upcoming: f64 = self
            .entries
            .iter()
            .skip(cur + 1)
            .filter_map(|e| e.track.duration)
            .sum();
        cur_rem + upcoming
    }

    /// Remove duplicate upcoming tracks (keeping the current track and first upcoming copy).
    pub fn deduplicate_upcoming(&mut self) -> usize {
        let start = self.current.map_or(0, |c| c + 1);
        if start >= self.entries.len() {
            return 0;
        }
        let mut seen = std::collections::HashSet::new();
        if let Some(cur) = self.current_track() {
            seen.insert(cur.video_id.clone());
        }
        let before = self.entries.len();
        let mut idx = 0;
        self.entries.retain(|e| {
            let keep = if idx < start {
                true
            } else {
                seen.insert(e.track.video_id.clone())
            };
            idx += 1;
            keep
        });
        before - self.entries.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(id: &str) -> Track {
        Track {
            video_id: id.into(),
            title: id.into(),
            artists: String::new(),
            artist_id: None,
            album: None,
            album_id: None,
            duration: None,
            thumbnail: None,
            liked: None,
        }
    }
    fn ids(q: &Queue) -> String {
        q.entries
            .iter()
            .map(|e| e.track.video_id.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    #[test]
    fn next_and_repeat() {
        let mut q = Queue::default();
        q.replace(vec![t("a"), t("b"), t("c")], 0);
        assert_eq!(q.next_index(), Some(1));
        q.current = Some(2);
        assert_eq!(q.next_index(), None);
        q.repeat = Repeat::All;
        assert_eq!(q.next_index(), Some(0));
        q.repeat = Repeat::One;
        assert_eq!(q.next_index(), Some(2));
        assert_eq!(q.skip_index(), None);
        assert_eq!(q.prev_index(), Some(1));
    }

    #[test]
    fn insert_remove_move() {
        let mut q = Queue::default();
        q.replace(vec![t("a"), t("b"), t("c")], 1);
        q.insert_next(vec![t("x")]);
        assert_eq!(ids(&q), "abxc");
        q.remove(0);
        assert_eq!((ids(&q).as_str(), q.current), ("bxc", Some(0)));
        assert_eq!(q.move_item(0, false), Some(1));
        assert_eq!((ids(&q).as_str(), q.current), ("xbc", Some(1)));
        assert_eq!(q.move_item(0, true), None);
        q.clear_upcoming();
        assert_eq!((ids(&q).as_str(), q.current), ("b", Some(0)));
    }

    #[test]
    fn shuffle_keeps_played_and_unshuffle_restores() {
        let mut q = Queue::default();
        let tracks: Vec<Track> = "abcdefghijklmnop".chars().map(|c| t(&c.to_string())).collect();
        q.replace(tracks, 3);
        q.set_shuffle(true);
        assert_eq!(&ids(&q)[..4], "abcd");
        assert_eq!(q.current_track().unwrap().video_id, "d");
        let mut sorted: Vec<char> = ids(&q).chars().collect();
        sorted.sort();
        assert_eq!(sorted.into_iter().collect::<String>(), "abcdefghijklmnop");
        q.set_shuffle(false);
        assert_eq!(ids(&q), "abcdefghijklmnop");
    }

    #[test]
    fn position_prefers_after_current() {
        let mut q = Queue::default();
        q.replace(vec![t("a"), t("b"), t("a")], 1);
        assert_eq!(q.position_of("a"), Some(2));
        assert_eq!(q.position_of("z"), None);
    }
}

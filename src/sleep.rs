//! Spotify-style sleep timer with smooth audiophile fade-out and "end of track" mode.

use std::time::{Duration, Instant};

/// Default fade-out window before the countdown pauses playback.
pub const DEFAULT_FADE_SECS: u64 = 10;

/// Quick-pick presets shown in the sleep timer overlay.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepPreset {
    EndOfTrack,
    Minutes(u64),
    Custom,
    Off,
}

impl SleepPreset {
    pub const MENU_ITEMS: &'static [SleepPreset] = &[
        SleepPreset::EndOfTrack,
        SleepPreset::Minutes(5),
        SleepPreset::Minutes(10),
        SleepPreset::Minutes(15),
        SleepPreset::Minutes(30),
        SleepPreset::Minutes(45),
        SleepPreset::Minutes(60),
        SleepPreset::Custom,
        SleepPreset::Off,
    ];

    pub fn label(self) -> String {
        match self {
            SleepPreset::EndOfTrack => "End of current track".into(),
            SleepPreset::Minutes(60) => "1 hour".into(),
            SleepPreset::Minutes(m) => format!("{m} minutes"),
            SleepPreset::Custom => "Custom minutes…".into(),
            SleepPreset::Off => "Turn off timer".into(),
        }
    }
}

/// Quick cycle steps for Esc menu ←/→ navigation.
const QUICK_CYCLE: &[SleepPreset] = &[
    SleepPreset::Off,
    SleepPreset::EndOfTrack,
    SleepPreset::Minutes(15),
    SleepPreset::Minutes(30),
    SleepPreset::Minutes(45),
    SleepPreset::Minutes(60),
];

#[derive(Clone, Debug, PartialEq)]
pub enum SleepMode {
    Off,
    EndOfTrack,
    Countdown {
        ends_at: Instant,
        total: Duration,
        /// Volume captured when fade-out starts, restored right after pausing.
        base_volume: Option<f64>,
        /// Last volume level pushed to mpv during fade-out (deduplicates IPC writes).
        last_sent_volume: Option<f64>,
    },
}

/// Result of ticking the sleep timer each frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SleepAction {
    None,
    /// Smoothly attenuate mpv volume during the final fade-out window.
    SetFadeVolume(f64),
    /// Timer expired: pause playback and restore `restore_volume` if we faded.
    Trigger { restore_volume: Option<f64> },
}

/// Parsed remote CLI argument for `pova sleep <arg>`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SleepCommand {
    Status,
    Off,
    EndOfTrack,
    SetMinutes(u64),
    AdjustMinutes(i64),
}

#[derive(Clone, Debug)]
pub struct SleepTimer {
    pub mode: SleepMode,
    pub fade_secs: u64,
}

impl Default for SleepTimer {
    fn default() -> Self {
        Self {
            mode: SleepMode::Off,
            fade_secs: DEFAULT_FADE_SECS,
        }
    }
}

impl SleepTimer {
    pub fn is_active(&self) -> bool {
        !matches!(self.mode, SleepMode::Off)
    }

    pub fn is_end_of_track(&self) -> bool {
        matches!(self.mode, SleepMode::EndOfTrack)
    }

    /// Whether the timer is currently ramping the volume down.
    pub fn is_fading(&self) -> bool {
        matches!(
            self.mode,
            SleepMode::Countdown {
                base_volume: Some(_),
                ..
            }
        )
    }

    /// Remaining duration for countdown mode.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match self.mode {
            SleepMode::Countdown { ends_at, .. } => Some(ends_at.saturating_duration_since(now)),
            _ => None,
        }
    }

    /// Set a countdown for `minutes` (clamped to 1..=720). Returns any saved volume
    /// that needs to be restored if we cancelled an in-progress fade.
    pub fn set_minutes(&mut self, minutes: u64, now: Instant) -> Option<f64> {
        let restore = self.cancel();
        let mins = minutes.clamp(1, 720);
        let total = Duration::from_secs(mins * 60);
        self.mode = SleepMode::Countdown {
            ends_at: now + total,
            total,
            base_volume: None,
            last_sent_volume: None,
        };
        restore
    }

    /// Stop automatically once the current track ends.
    pub fn set_end_of_track(&mut self) -> Option<f64> {
        let restore = self.cancel();
        self.mode = SleepMode::EndOfTrack;
        restore
    }

    /// Extend or shorten an active countdown (or start one if positive).
    /// Returns `(new_status_message, optional_volume_to_restore)`.
    pub fn adjust_minutes(&mut self, delta: i64, now: Instant) -> (String, Option<f64>) {
        let restore = self.take_fade_volume();
        match &mut self.mode {
            SleepMode::Countdown {
                ends_at,
                total,
                base_volume,
                last_sent_volume,
            } => {
                let rem_secs = ends_at.saturating_duration_since(now).as_secs() as i64;
                let next_secs = rem_secs + delta * 60;
                if next_secs <= 0 {
                    self.mode = SleepMode::Off;
                    ("Sleep timer turned off".into(), restore)
                } else {
                    let dur = Duration::from_secs((next_secs as u64).min(720 * 60));
                    *ends_at = now + dur;
                    *total = (*total).max(dur);
                    *base_volume = None;
                    *last_sent_volume = None;
                    (format!("Sleep timer: {}", format_countdown(dur)), restore)
                }
            }
            _ if delta > 0 => {
                let mins = (delta as u64).clamp(1, 720);
                self.set_minutes(mins, now);
                (format!("Sleep timer: {mins}m"), restore)
            }
            _ => {
                self.mode = SleepMode::Off;
                ("Sleep timer is off".into(), restore)
            }
        }
    }

    /// Cancel the timer. If a fade-out was in progress, returns the user's
    /// pre-fade volume so the caller can restore it.
    pub fn cancel(&mut self) -> Option<f64> {
        let restore = self.take_fade_volume();
        self.mode = SleepMode::Off;
        restore
    }

    fn take_fade_volume(&mut self) -> Option<f64> {
        if let SleepMode::Countdown { base_volume, .. } = &mut self.mode {
            base_volume.take()
        } else {
            None
        }
    }

    /// Cycle quick presets for Esc menu ←/→. Returns `(flash_msg, optional_restore_vol)`.
    pub fn cycle_quick(&mut self, forward: bool, now: Instant) -> (String, Option<f64>) {
        let cur_idx = match self.mode {
            SleepMode::Off => 0,
            SleepMode::EndOfTrack => 1,
            SleepMode::Countdown { ends_at, .. } => {
                let mins = ends_at.saturating_duration_since(now).as_secs().div_ceil(60);
                QUICK_CYCLE
                    .iter()
                    .position(|p| matches!(p, SleepPreset::Minutes(m) if *m >= mins))
                    .unwrap_or(QUICK_CYCLE.len() - 1)
            }
        };
        let n = QUICK_CYCLE.len();
        let next = QUICK_CYCLE[if forward { (cur_idx + 1) % n } else { (cur_idx + n - 1) % n }];
        match next {
            SleepPreset::Off => ("Sleep timer off".into(), self.cancel()),
            SleepPreset::EndOfTrack => ("Sleep timer: end of track".into(), self.set_end_of_track()),
            SleepPreset::Minutes(m) => (format!("Sleep timer: {m} min"), self.set_minutes(m, now)),
            SleepPreset::Custom => ("Sleep timer off".into(), self.cancel()),
        }
    }

    /// Update fade-out volume or fire when the countdown finishes.
    pub fn tick(&mut self, now: Instant, current_volume: f64, is_playing: bool) -> SleepAction {
        let SleepMode::Countdown {
            ends_at,
            total,
            base_volume,
            last_sent_volume,
        } = &mut self.mode
        else {
            return SleepAction::None;
        };
        if now >= *ends_at {
            let restore_volume = base_volume.take();
            self.mode = SleepMode::Off;
            return SleepAction::Trigger { restore_volume };
        }
        if self.fade_secs == 0 || !is_playing {
            return SleepAction::None;
        }
        let fade_window = (self.fade_secs as f64).min(total.as_secs_f64() * 0.5).max(1.0);
        let rem = ends_at.saturating_duration_since(now).as_secs_f64();
        if rem <= fade_window {
            let base = *base_volume.get_or_insert(current_volume);
            let progress = (1.0 - rem / fade_window).clamp(0.0, 1.0);
            // Smooth equal-power cosine fade-out: 1.0 -> 0.0
            let factor = 0.5 * (1.0 + (std::f64::consts::PI * progress).cos());
            let target = (base * factor).clamp(0.0, 100.0);
            let should_send = last_sent_volume.is_none_or(|prev| (prev - target).abs() >= 0.5);
            if should_send {
                *last_sent_volume = Some(target);
                return SleepAction::SetFadeVolume(target);
            }
        }
        SleepAction::None
    }

    /// Short badge text for Now Playing / player border / `{sleep}` status placeholder.
    pub fn badge(&self, now: Instant) -> Option<String> {
        match self.mode {
            SleepMode::Off => None,
            SleepMode::EndOfTrack => Some("end of track".into()),
            SleepMode::Countdown { ends_at, .. } => Some(format_countdown(ends_at.saturating_duration_since(now))),
        }
    }

    /// Menu value shown in the Esc menu (`off`, `end of track`, `14:32`).
    pub fn menu_value(&self, now: Instant) -> String {
        self.badge(now).unwrap_or_else(|| "off".into())
    }
}

pub fn format_countdown(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3600 {
        format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60)
    } else {
        format!("{}:{:02}", secs / 60, secs % 60)
    }
}

/// Parse `pova sleep <arg>`: `off`, `track`, `end`, `status`, `+5`, `-5`, `30`, `30m`, `1h`, `1h30m`.
pub fn parse_cli(arg: &str) -> Result<SleepCommand, String> {
    let s = arg.trim().to_ascii_lowercase();
    if s.is_empty() || s == "status" {
        return Ok(SleepCommand::Status);
    }
    if matches!(s.as_str(), "off" | "cancel" | "stop" | "clear" | "0") {
        return Ok(SleepCommand::Off);
    }
    if matches!(s.as_str(), "track" | "end" | "song" | "eot" | "end-of-track") {
        return Ok(SleepCommand::EndOfTrack);
    }
    if let Some(rest) = s.strip_prefix('+') {
        let mins = parse_duration_minutes(rest)?;
        return Ok(SleepCommand::AdjustMinutes(mins as i64));
    }
    if let Some(rest) = s.strip_prefix('-') {
        let mins = parse_duration_minutes(rest)?;
        return Ok(SleepCommand::AdjustMinutes(-(mins as i64)));
    }
    let mins = parse_duration_minutes(&s)?;
    if mins == 0 {
        Ok(SleepCommand::Off)
    } else {
        Ok(SleepCommand::SetMinutes(mins))
    }
}

fn parse_duration_minutes(s: &str) -> Result<u64, String> {
    let s = s.trim();
    if let Ok(n) = s.parse::<u64>() {
        return Ok(n);
    }
    if let Some(m) = s.strip_suffix('m').and_then(|v| v.parse::<u64>().ok()) {
        return Ok(m);
    }
    if let Some(h) = s.strip_suffix('h').and_then(|v| v.parse::<u64>().ok()) {
        return Ok(h.saturating_mul(60));
    }
    if let Some((h_part, m_part)) = s.split_once('h') {
        let m_part = m_part.strip_suffix('m').unwrap_or(m_part);
        if let (Ok(h), Ok(m)) = (h_part.parse::<u64>(), m_part.parse::<u64>()) {
            return Ok(h.saturating_mul(60).saturating_add(m));
        }
    }
    Err(format!(
        "`{s}` isn't a sleep duration; use minutes (15, 30m, 1h), +5, -5, track, or off"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn countdown_fades_smoothly_then_triggers_and_restores_volume() {
        let start = Instant::now();
        let mut timer = SleepTimer::default();
        timer.set_minutes(1, start);
        assert!(timer.is_active());
        assert_eq!(timer.badge(start).as_deref(), Some("1:00"));

        // Well before fade window: no volume change.
        assert_eq!(
            timer.tick(start + Duration::from_secs(30), 80.0, true),
            SleepAction::None
        );
        assert!(!timer.is_fading());

        // Midpoint of 10s fade window (5s left): ~50% of base volume.
        match timer.tick(start + Duration::from_secs(55), 80.0, true) {
            SleepAction::SetFadeVolume(v) => assert!((v - 40.0).abs() < 1.0, "got {v}"),
            other => panic!("expected fade volume, got {other:?}"),
        }
        assert!(timer.is_fading());

        // Expired: triggers pause and returns original volume 80.0.
        assert_eq!(
            timer.tick(start + Duration::from_secs(60), 0.0, true),
            SleepAction::Trigger {
                restore_volume: Some(80.0)
            }
        );
        assert!(!timer.is_active());
    }

    #[test]
    fn cancelling_during_fade_returns_base_volume() {
        let start = Instant::now();
        let mut timer = SleepTimer::default();
        timer.set_minutes(1, start);
        let _ = timer.tick(start + Duration::from_secs(56), 70.0, true);
        assert_eq!(timer.cancel(), Some(70.0));
        assert!(!timer.is_active());
    }

    #[test]
    fn parses_cli_sleep_commands() {
        assert_eq!(parse_cli("off"), Ok(SleepCommand::Off));
        assert_eq!(parse_cli("track"), Ok(SleepCommand::EndOfTrack));
        assert_eq!(parse_cli("end"), Ok(SleepCommand::EndOfTrack));
        assert_eq!(parse_cli("30"), Ok(SleepCommand::SetMinutes(30)));
        assert_eq!(parse_cli("45m"), Ok(SleepCommand::SetMinutes(45)));
        assert_eq!(parse_cli("1h"), Ok(SleepCommand::SetMinutes(60)));
        assert_eq!(parse_cli("1h30m"), Ok(SleepCommand::SetMinutes(90)));
        assert_eq!(parse_cli("+5"), Ok(SleepCommand::AdjustMinutes(5)));
        assert_eq!(parse_cli("-10"), Ok(SleepCommand::AdjustMinutes(-10)));
        assert_eq!(parse_cli("status"), Ok(SleepCommand::Status));
        assert!(parse_cli("banana").is_err());
    }
}

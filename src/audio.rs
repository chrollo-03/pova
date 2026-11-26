//! Tidal & Spotify-inspired audio engine controls:
//! - Tidal-style audio quality selector & live signal path telemetry (codec, sample rate, bitrate)
//! - Spotify-style 10-band graphic equalizer with curated presets (live mpv lavfi filter)
//! - Smooth track crossfade envelope (0..12 s)
//! - Playback speed & pitch-correction controls
//! - A-B section loop for practice or replaying a favorite bridge/solo

use serde::{Deserialize, Serialize};

/// Label of pova's equalizer filter in mpv's `af` chain so it can be replaced
/// or removed without touching user filters or `@povanorm`.
pub const EQ_FILTER_LABEL: &str = "@povaeq";

/// Center frequencies (Hz) for the 10-band ISO graphic equalizer.
pub const EQ_FREQS: [u32; 10] = [32, 64, 125, 250, 500, 1000, 2000, 4000, 8000, 16000];
pub const EQ_LABELS: [&str; 10] = ["32", "64", "125", "250", "500", "1k", "2k", "4k", "8k", "16k"];
pub const EQ_MIN_DB: f32 = -12.0;
pub const EQ_MAX_DB: f32 = 12.0;

/// Tidal-style audio quality tier controlling `yt-dlp` format selection.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AudioQuality {
    /// Highest quality available (prefers high-bitrate Opus, then bestaudio).
    #[default]
    Max,
    /// High quality (up to 256 kbps).
    High,
    /// Normal quality (up to 128 kbps).
    Normal,
    /// Data saver (up to 64 kbps).
    Low,
}

impl AudioQuality {
    pub fn next(self) -> AudioQuality {
        match self {
            AudioQuality::Max => AudioQuality::High,
            AudioQuality::High => AudioQuality::Normal,
            AudioQuality::Normal => AudioQuality::Low,
            AudioQuality::Low => AudioQuality::Max,
        }
    }

    pub fn prev(self) -> AudioQuality {
        match self {
            AudioQuality::Max => AudioQuality::Low,
            AudioQuality::High => AudioQuality::Max,
            AudioQuality::Normal => AudioQuality::High,
            AudioQuality::Low => AudioQuality::Normal,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AudioQuality::Max => "max",
            AudioQuality::High => "high",
            AudioQuality::Normal => "normal",
            AudioQuality::Low => "low (saver)",
        }
    }

    pub fn badge(self) -> &'static str {
        match self {
            AudioQuality::Max => "MAX",
            AudioQuality::High => "HIGH",
            AudioQuality::Normal => "NORM",
            AudioQuality::Low => "SAVER",
        }
    }

    /// `yt-dlp` / `mpv --ytdl-format` format selector for this tier.
    pub fn ytdl_format(self) -> &'static str {
        match self {
            AudioQuality::Max => "bestaudio[acodec^=opus]/bestaudio/best",
            AudioQuality::High => "bestaudio[abr<=256]/bestaudio/best",
            AudioQuality::Normal => "bestaudio[abr<=128]/bestaudio/best",
            AudioQuality::Low => "bestaudio[abr<=64]/worstaudio/bestaudio",
        }
    }
}

/// Live audio stream telemetry reported by mpv (Tidal-style Signal Path).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct AudioStreamInfo {
    pub codec: Option<String>,
    /// Sample rate in Hz (e.g. 48000, 44100).
    pub sample_rate: Option<u32>,
    /// Channel count (e.g. 2 for stereo).
    pub channels: Option<u8>,
    /// Audio bitrate in bits/sec.
    pub bitrate: Option<u64>,
}

impl AudioStreamInfo {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    /// Compact Tidal-style quality pill for Now Playing, e.g. `MAX · OPUS 48kHz · 158k`.
    pub fn badge(&self, quality: AudioQuality) -> String {
        let mut parts = vec![quality.badge().to_string()];
        if let Some(codec) = self.codec.as_deref().filter(|s| !s.is_empty()) {
            let mut c = codec.to_ascii_uppercase();
            if let Some(sr) = self.sample_rate {
                let khz = sr as f64 / 1000.0;
                if (khz - khz.round()).abs() < 0.05 {
                    c.push_str(&format!(" {:.0}kHz", khz));
                } else {
                    c.push_str(&format!(" {:.1}kHz", khz));
                }
            }
            parts.push(c);
        }
        if let Some(bps) = self.bitrate.filter(|&b| b > 0) {
            parts.push(format!("{}k", bps / 1000));
        }
        parts.join(" · ")
    }

    /// Detailed rows for the Tidal-style Signal Path card inside the Interlink/Info overlay.
    pub fn signal_path_rows(
        &self,
        quality: AudioQuality,
        normalize: bool,
        eq: &EqSettings,
        speed: f64,
        pitch_correction: bool,
    ) -> Vec<(String, String)> {
        let codec = self
            .codec
            .as_deref()
            .map(str::to_ascii_uppercase)
            .unwrap_or_else(|| "—".into());
        let rate = self
            .sample_rate
            .map(|r| {
                let khz = r as f64 / 1000.0;
                if (khz - khz.round()).abs() < 0.05 {
                    format!("{:.0} kHz", khz)
                } else {
                    format!("{:.1} kHz", khz)
                }
            })
            .unwrap_or_else(|| "—".into());
        let ch = match self.channels {
            Some(1) => "Mono".into(),
            Some(2) => "Stereo (2.0)".into(),
            Some(n) => format!("{n}ch"),
            None => "—".into(),
        };
        let br = self
            .bitrate
            .filter(|&b| b > 0)
            .map(|b| format!("{} kbps", b / 1000))
            .unwrap_or_else(|| "—".into());
        let eq_val = if eq.is_active() {
            format!("{} (active)", eq.preset.label())
        } else {
            "bypassed (bit-transparent)".into()
        };
        let norm_val = if normalize {
            "EBU R128 (-14 LUFS)"
        } else {
            "off (original dynamics)"
        };
        let speed_val = if (speed - 1.0).abs() < 0.005 {
            "1.0x (original)".into()
        } else if pitch_correction {
            format!("{:.2}x (pitch locked)", speed)
        } else {
            format!("{:.2}x (vinyl pitch)", speed)
        };
        vec![
            ("Quality tier".into(), quality.badge().into()),
            ("Stream".into(), format!("{codec} · {rate} · {ch} · {br}")),
            ("Loudness".into(), norm_val.into()),
            ("Equalizer".into(), eq_val),
            ("Playback".into(), speed_val),
        ]
    }
}

/// Curated Spotify-style equalizer presets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EqPreset {
    #[default]
    Flat,
    BassBoost,
    Vocal,
    Rock,
    Electronic,
    Acoustic,
    Pop,
    Jazz,
    Classical,
    TrebleBoost,
    LateNight,
    Custom,
}

impl EqPreset {
    pub const CYCLABLE: &'static [EqPreset] = &[
        EqPreset::Flat,
        EqPreset::BassBoost,
        EqPreset::Vocal,
        EqPreset::Rock,
        EqPreset::Electronic,
        EqPreset::Acoustic,
        EqPreset::Pop,
        EqPreset::Jazz,
        EqPreset::Classical,
        EqPreset::TrebleBoost,
        EqPreset::LateNight,
    ];

    pub fn label(self) -> &'static str {
        match self {
            EqPreset::Flat => "Flat",
            EqPreset::BassBoost => "Bass Boost",
            EqPreset::Vocal => "Vocal",
            EqPreset::Rock => "Rock",
            EqPreset::Electronic => "Electronic",
            EqPreset::Acoustic => "Acoustic",
            EqPreset::Pop => "Pop",
            EqPreset::Jazz => "Jazz",
            EqPreset::Classical => "Classical",
            EqPreset::TrebleBoost => "Treble Boost",
            EqPreset::LateNight => "Late Night",
            EqPreset::Custom => "Custom",
        }
    }

    pub fn gains(self) -> [f32; 10] {
        match self {
            EqPreset::Flat | EqPreset::Custom => [0.0; 10],
            EqPreset::BassBoost => [6.0, 5.0, 4.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 1.0],
            EqPreset::Vocal => [-2.0, -2.0, -1.0, 1.0, 3.0, 4.0, 4.0, 2.0, 0.0, -1.0],
            EqPreset::Rock => [4.0, 3.0, 1.0, -1.0, -1.0, 1.0, 3.0, 4.0, 4.0, 3.0],
            EqPreset::Electronic => [5.0, 4.0, 2.0, 0.0, -2.0, 0.0, 2.0, 3.0, 4.0, 5.0],
            EqPreset::Acoustic => [2.0, 2.0, 1.0, 1.0, 2.0, 2.0, 3.0, 2.0, 2.0, 1.0],
            EqPreset::Pop => [-1.0, 1.0, 3.0, 4.0, 2.0, 0.0, -1.0, 1.0, 2.0, 2.0],
            EqPreset::Jazz => [2.0, 1.0, 0.0, 2.0, -1.0, -1.0, 0.0, 1.0, 2.0, 3.0],
            EqPreset::Classical => [3.0, 2.0, 1.0, 0.0, 0.0, 0.0, -1.0, 1.0, 2.0, 3.0],
            EqPreset::TrebleBoost => [0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 3.0, 5.0, 6.0, 6.0],
            EqPreset::LateNight => [3.0, 2.0, 1.0, 0.0, 1.0, 1.0, 0.0, -2.0, -3.0, -4.0],
        }
    }

    pub fn from_name(s: &str) -> Option<EqPreset> {
        let key = s.trim().to_ascii_lowercase().replace(['-', ' '], "_");
        match key.as_str() {
            "flat" => Some(EqPreset::Flat),
            "bass" | "bass_boost" | "bassboost" => Some(EqPreset::BassBoost),
            "vocal" | "vocals" => Some(EqPreset::Vocal),
            "rock" => Some(EqPreset::Rock),
            "electronic" | "edm" => Some(EqPreset::Electronic),
            "acoustic" => Some(EqPreset::Acoustic),
            "pop" => Some(EqPreset::Pop),
            "jazz" => Some(EqPreset::Jazz),
            "classical" => Some(EqPreset::Classical),
            "treble" | "treble_boost" | "trebleboost" => Some(EqPreset::TrebleBoost),
            "late_night" | "latenight" | "night" => Some(EqPreset::LateNight),
            "custom" => Some(EqPreset::Custom),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct EqSettings {
    pub enabled: bool,
    pub preset: EqPreset,
    pub gains: [f32; 10],
}

impl Default for EqSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            preset: EqPreset::Flat,
            gains: [0.0; 10],
        }
    }
}

impl EqSettings {
    pub fn apply_preset(&mut self, preset: EqPreset) {
        self.preset = preset;
        if preset != EqPreset::Custom {
            self.gains = preset.gains();
        }
        self.enabled = preset != EqPreset::Flat;
    }

    pub fn cycle_preset(&mut self, forward: bool) {
        let list = EqPreset::CYCLABLE;
        let idx = list.iter().position(|&p| p == self.preset).unwrap_or(0);
        let n = list.len();
        let next = list[if forward { (idx + 1) % n } else { (idx + n - 1) % n }];
        self.apply_preset(next);
    }

    pub fn adjust_band(&mut self, band: usize, delta_db: f32) {
        if let Some(g) = self.gains.get_mut(band) {
            *g = (*g + delta_db).clamp(EQ_MIN_DB, EQ_MAX_DB).round();
            self.preset = EqPreset::Custom;
            self.enabled = true;
        }
    }

    pub fn reset_flat(&mut self) {
        self.preset = EqPreset::Flat;
        self.gains = [0.0; 10];
    }

    pub fn is_active(&self) -> bool {
        self.enabled && self.gains.iter().any(|&g| g.abs() >= 0.1)
    }

    /// Build the labeled mpv audio filter chain, or `None` when bypassed/flat.
    pub fn mpv_filter(&self) -> Option<String> {
        if !self.is_active() {
            return None;
        }
        let filters: Vec<String> = EQ_FREQS
            .iter()
            .zip(self.gains.iter())
            .filter(|(_, g)| g.abs() >= 0.1)
            .map(|(f, g)| format!("equalizer=f={f}:t=q:w=1.4:g={g:.1}"))
            .collect();
        if filters.is_empty() {
            None
        } else {
            Some(format!("{EQ_FILTER_LABEL}:lavfi=[{}]", filters.join(",")))
        }
    }

    pub fn summary(&self) -> String {
        if !self.enabled {
            "off".into()
        } else if !self.is_active() {
            "flat".into()
        } else {
            self.preset.label().to_ascii_lowercase()
        }
    }
}

/// Curated playback speed steps.
pub const SPEED_STEPS: &[f64] = &[0.5, 0.75, 0.85, 1.0, 1.15, 1.25, 1.5, 1.75, 2.0];

pub fn step_speed(current: f64, faster: bool) -> f64 {
    if faster {
        SPEED_STEPS
            .iter()
            .copied()
            .find(|&s| s > current + 0.01)
            .unwrap_or(*SPEED_STEPS.last().unwrap())
    } else {
        SPEED_STEPS
            .iter()
            .rev()
            .copied()
            .find(|&s| s < current - 0.01)
            .unwrap_or(SPEED_STEPS[0])
    }
}

pub fn format_speed(speed: f64) -> String {
    let s = speed.clamp(0.25, 3.0);
    if ((s * 100.0).round() as i64) % 10 == 0 {
        format!("{s:.1}x")
    } else {
        format!("{s:.2}x")
    }
}

/// Spotify-style smooth track fade presets (seconds).
pub const CROSSFADE_STEPS: &[u8] = &[0, 2, 4, 6, 8, 10, 12];

pub fn cycle_crossfade(current: u8, forward: bool) -> u8 {
    let idx = CROSSFADE_STEPS
        .iter()
        .position(|&s| s >= current)
        .unwrap_or(0);
    let n = CROSSFADE_STEPS.len();
    CROSSFADE_STEPS[if forward { (idx + 1) % n } else { (idx + n - 1) % n }]
}

/// Compute smooth cosine fade-in / fade-out volume envelope for track transitions.
/// Returns `None` when outside the fade windows (full `base_volume`).
pub fn crossfade_volume(base_volume: f64, time: f64, duration: f64, fade_secs: u8) -> Option<f64> {
    if fade_secs == 0 || !duration.is_finite() || duration <= 4.0 || !time.is_finite() || time < 0.0 {
        return None;
    }
    let fade_out = (fade_secs as f64).min(duration * 0.35);
    let fade_in = (fade_out * 0.5).max(0.5);
    let rem = duration - time;
    if rem <= fade_out && rem >= 0.0 {
        let progress = (1.0 - rem / fade_out).clamp(0.0, 1.0);
        let factor = 0.5 * (1.0 + (std::f64::consts::PI * progress).cos());
        return Some((base_volume * factor).clamp(0.0, 100.0));
    }
    if time < fade_in {
        let progress = (time / fade_in).clamp(0.0, 1.0);
        let factor = 0.5 * (1.0 - (std::f64::consts::PI * progress).cos());
        return Some((base_volume * factor).clamp(0.0, 100.0));
    }
    None
}

/// A-B loop state for repeating a section of the current song.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum AbLoop {
    #[default]
    Off,
    PointA(f64),
    Active { a: f64, b: f64 },
}

impl AbLoop {
    /// Advance Off -> PointA -> Active -> Off.
    pub fn toggle(&mut self, pos: f64) -> String {
        let pos = pos.max(0.0);
        match *self {
            AbLoop::Off => {
                *self = AbLoop::PointA(pos);
                format!(
                    "Loop point A set at {} — press B again for point B",
                    crate::model::format_time(pos)
                )
            }
            AbLoop::PointA(a) => {
                if (pos - a).abs() < 0.5 {
                    *self = AbLoop::Off;
                    "A-B loop cleared (points too close)".into()
                } else {
                    let (lo, hi) = if pos >= a { (a, pos) } else { (pos, a) };
                    *self = AbLoop::Active { a: lo, b: hi };
                    format!(
                        "A-B loop: {} → {}",
                        crate::model::format_time(lo),
                        crate::model::format_time(hi)
                    )
                }
            }
            AbLoop::Active { .. } => {
                *self = AbLoop::Off;
                "A-B loop cleared".into()
            }
        }
    }

    pub fn clear(&mut self) {
        *self = AbLoop::Off;
    }

    /// If playback reached/passed point B, returns point A to seek back to.
    pub fn wrap_target(&self, pos: f64) -> Option<f64> {
        match *self {
            AbLoop::Active { a, b } if pos >= b => Some(a),
            _ => None,
        }
    }

    pub fn badge(&self) -> Option<String> {
        match *self {
            AbLoop::Off => None,
            AbLoop::PointA(a) => Some(format!("A {}→…", crate::model::format_time(a))),
            AbLoop::Active { a, b } => Some(format!(
                "A-B {}→{}",
                crate::model::format_time(a),
                crate::model::format_time(b)
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn eq_filter_only_emits_nonzero_bands_when_enabled() {
        let mut eq = EqSettings::default();
        assert_eq!(eq.mpv_filter(), None);
        eq.apply_preset(EqPreset::BassBoost);
        let f = eq.mpv_filter().expect("active filter");
        assert!(f.starts_with("@povaeq:lavfi=[equalizer=f=32:"));
        assert!(f.contains("f=64:"));
        assert!(!f.contains("f=500:"), "zero-gain bands skipped");
        eq.enabled = false;
        assert_eq!(eq.mpv_filter(), None);
    }

    #[test]
    fn stream_info_formats_tidal_style_badge() {
        let info = AudioStreamInfo {
            codec: Some("opus".into()),
            sample_rate: Some(48000),
            channels: Some(2),
            bitrate: Some(158_400),
        };
        assert_eq!(info.badge(AudioQuality::Max), "MAX · OPUS 48kHz · 158k");
        let empty = AudioStreamInfo::default();
        assert_eq!(empty.badge(AudioQuality::Low), "SAVER");
    }

    #[test]
    fn crossfade_envelope_ramps_smoothly() {
        assert_eq!(crossfade_volume(80.0, 50.0, 200.0, 6), None);
        // Near start (t=0): ~0
        assert!(crossfade_volume(80.0, 0.0, 200.0, 6).unwrap() < 1.0);
        // Halfway through 6s fade-out (t=197): ~40
        let mid = crossfade_volume(80.0, 197.0, 200.0, 6).unwrap();
        assert!((mid - 40.0).abs() < 1.0, "got {mid}");
        assert_eq!(crossfade_volume(80.0, 197.0, 200.0, 0), None);
    }

    #[test]
    fn ab_loop_cycles_and_wraps() {
        let mut ab = AbLoop::Off;
        ab.toggle(30.0);
        assert_eq!(ab, AbLoop::PointA(30.0));
        ab.toggle(60.0);
        assert_eq!(ab, AbLoop::Active { a: 30.0, b: 60.0 });
        assert_eq!(ab.wrap_target(59.5), None);
        assert_eq!(ab.wrap_target(60.1), Some(30.0));
        ab.toggle(45.0);
        assert_eq!(ab, AbLoop::Off);
    }

    #[test]
    fn speed_stepping() {
        assert_eq!(step_speed(1.0, true), 1.15);
        assert_eq!(step_speed(1.0, false), 0.85);
        assert_eq!(format_speed(1.0), "1.0x");
        assert_eq!(format_speed(1.25), "1.25x");
    }
}

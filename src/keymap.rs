//! Key bindings: named actions, defaults, and `[keys]` overrides from config.

use std::collections::HashMap;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

macro_rules! actions {
    ($($variant:ident => $name:literal, $desc:literal;)*) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Action { $($variant),* }
        impl Action {
            pub const ALL: &'static [Action] = &[$(Action::$variant),*];
            pub fn name(self) -> &'static str { match self { $(Action::$variant => $name),* } }
            pub fn description(self) -> &'static str { match self { $(Action::$variant => $desc),* } }
            pub fn from_name(s: &str) -> Option<Action> {
                match s { $($name => Some(Action::$variant),)* _ => None }
            }
        }
    };
}

actions! {
    Quit => "quit", "Quit pova";
    PlayPause => "play_pause", "Play / pause";
    SeekForward => "seek_forward", "Seek forward 5s";
    SeekBackward => "seek_backward", "Seek backward 5s";
    SeekForwardBig => "seek_forward_big", "Seek forward 30s";
    SeekBackwardBig => "seek_backward_big", "Seek backward 30s";
    Next => "next", "Next track";
    Prev => "prev", "Previous track / restart";
    VolumeUp => "volume_up", "Volume +5";
    VolumeDown => "volume_down", "Volume -5";
    Mute => "mute", "Mute / unmute";
    Shuffle => "shuffle", "Toggle shuffle";
    Repeat => "repeat", "Cycle repeat (off / all / one)";
    Like => "like", "Like / unlike current track";
    Dislike => "dislike", "Dislike current track and skip it";
    AddToPlaylist => "add_to_playlist", "Add selection (or current track) to a playlist";
    DeletePlaylist => "delete_playlist", "Delete the selected / open playlist (asks first)";
    VizMode => "viz_mode", "Cycle visualizer mode";
    Themes => "themes", "Theme picker";
    Help => "help", "Show this help";
    Menu => "menu", "Back / close / main menu";
    TabNext => "tab_next", "Next tab";
    TabPrev => "tab_prev", "Previous tab";
    TabNowPlaying => "tab_now_playing", "Now Playing tab";
    TabQueue => "tab_queue", "Queue tab";
    TabLyrics => "tab_lyrics", "Lyrics tab";
    TabSearch => "tab_search", "Search tab (start typing)";
    TabLibrary => "tab_library", "Library tab";
    ToggleViz => "toggle_viz", "Show / hide visualizer box";
    TogglePlayer => "toggle_player", "Show / hide player box";
    ToggleControls => "toggle_controls", "Show / hide controls box";
    Up => "up", "Move up";
    Down => "down", "Move down";
    Top => "top", "Jump to top";
    Bottom => "bottom", "Jump to bottom";
    PageUp => "page_up", "Page up";
    PageDown => "page_down", "Page down";
    Select => "select", "Play / open selection";
    Append => "append", "Add selection to end of queue";
    PlayNext => "play_next", "Play selection next";
    Remove => "remove", "Remove from queue / from your playlist";
    MoveUp => "move_up", "Move queue item up";
    MoveDown => "move_down", "Move queue item down";
    Radio => "radio", "Start radio from selection";
    SubPrev => "sub_prev", "Previous filter / section";
    SubNext => "sub_next", "Next filter / section";
    ClearQueue => "clear_queue", "Clear queue (keeps current)";
    Refresh => "refresh", "Reload current view";
    JumpToCurrent => "jump_current", "Jump to playing track";
    SleepTimer => "sleep_timer", "Sleep timer (countdown / end of track)";
    Interlink => "interlink", "Interlink: artist, album, links & signal path";
    Equalizer => "equalizer", "10-band equalizer & audio presets";
    AbLoop => "ab_loop", "Set / clear A-B section loop";
    CopyLink => "copy_link", "Copy universal interlink (song.link)";
    SpeedUp => "speed_up", "Playback speed faster";
    SpeedDown => "speed_down", "Playback speed slower";
    Stats => "stats", "Listening stats (Wrapped Mini)";
    SaveOffline => "save_offline", "Save / remove track for offline playback";
}

/// A normalised key: for printable characters SHIFT is folded into the char.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Key {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Key {
    pub fn from_event(ev: &KeyEvent) -> Key {
        let mut mods = ev.modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let code = match ev.code {
            KeyCode::Char(c) => {
                mods.remove(KeyModifiers::SHIFT);
                // ctrl+letter arrives lowercase on most terminals
                KeyCode::Char(if mods.contains(KeyModifiers::CONTROL) {
                    c.to_ascii_lowercase()
                } else {
                    c
                })
            }
            KeyCode::BackTab => {
                mods.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            other => other,
        };
        Key { code, mods }
    }

    /// Parse "space", "ctrl+c", "shift+left", "F1", "N", "<", ...
    pub fn parse(spec: &str) -> Option<Key> {
        let spec = spec.trim();
        if spec.is_empty() {
            return None;
        }
        let mut mods = KeyModifiers::NONE;
        let mut rest = spec;
        // A bare "+" or "-" is a key, not a separator.
        while rest.len() > 1 {
            let lower = rest.to_ascii_lowercase();
            let (m, len) = if lower.starts_with("ctrl+") {
                (KeyModifiers::CONTROL, 5)
            } else if lower.starts_with("alt+") {
                (KeyModifiers::ALT, 4)
            } else if lower.starts_with("shift+") {
                (KeyModifiers::SHIFT, 6)
            } else {
                break;
            };
            mods |= m;
            rest = &rest[len..];
        }
        let code = match rest.to_ascii_lowercase().as_str() {
            "space" => KeyCode::Char(' '),
            "enter" | "return" => KeyCode::Enter,
            "esc" | "escape" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            "backtab" => KeyCode::BackTab,
            "backspace" => KeyCode::Backspace,
            "delete" | "del" => KeyCode::Delete,
            "insert" => KeyCode::Insert,
            "left" => KeyCode::Left,
            "right" => KeyCode::Right,
            "up" => KeyCode::Up,
            "down" => KeyCode::Down,
            "home" => KeyCode::Home,
            "end" => KeyCode::End,
            "pageup" | "pgup" => KeyCode::PageUp,
            "pagedown" | "pgdn" => KeyCode::PageDown,
            s if s.len() > 1 && s.starts_with('f') && s[1..].parse::<u8>().is_ok() => KeyCode::F(s[1..].parse().ok()?),
            _ => {
                let mut chars = rest.chars();
                let c = chars.next()?;
                if chars.next().is_some() {
                    return None;
                }
                let c = if mods.contains(KeyModifiers::SHIFT) {
                    c.to_ascii_uppercase()
                } else {
                    c
                };
                KeyCode::Char(c)
            }
        };
        let ev = KeyEvent::new(code, mods);
        Some(Key::from_event(&ev))
    }

    pub fn display(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("ctrl+");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("alt+");
        }
        if self.mods.contains(KeyModifiers::SHIFT) {
            s.push_str("shift+");
        }
        match self.code {
            KeyCode::Char(' ') => s.push_str("space"),
            KeyCode::Char(c) => s.push(c),
            KeyCode::Enter => s.push_str("enter"),
            KeyCode::Esc => s.push_str("esc"),
            KeyCode::Tab => s.push_str("tab"),
            KeyCode::BackTab => s.push_str("shift+tab"),
            KeyCode::Left => s.push('←'),
            KeyCode::Right => s.push('→'),
            KeyCode::Up => s.push('↑'),
            KeyCode::Down => s.push('↓'),
            KeyCode::PageUp => s.push_str("pgup"),
            KeyCode::PageDown => s.push_str("pgdn"),
            KeyCode::Home => s.push_str("home"),
            KeyCode::End => s.push_str("end"),
            KeyCode::Delete => s.push_str("del"),
            KeyCode::F(n) => s.push_str(&format!("F{n}")),
            other => s.push_str(&format!("{other:?}")),
        }
        s
    }
}

pub struct Keymap {
    map: HashMap<Key, Action>,
}

fn defaults() -> Vec<(Action, &'static [&'static str])> {
    use Action::*;
    vec![
        (Quit, &["Q", "ctrl+c"]),
        (PlayPause, &["space"]),
        (SeekForward, &["right", "."]),
        (SeekBackward, &["left", ","]),
        (SeekForwardBig, &["shift+right", "L"]),
        (SeekBackwardBig, &["shift+left", "H"]),
        (Next, &[">", "N"]),
        (Prev, &["<", "P"]),
        (VolumeUp, &["+", "="]),
        (VolumeDown, &["-", "_"]),
        (Mute, &["m"]),
        (Shuffle, &["S"]),
        (Repeat, &["R"]),
        (Like, &["f"]),
        (Dislike, &["F"]),
        (AddToPlaylist, &["p"]),
        (DeletePlaylist, &["D"]),
        (VizMode, &["v"]),
        (Themes, &["t"]),
        (Help, &["?", "F1"]),
        (Menu, &["esc"]),
        (TabNext, &["tab"]),
        (TabPrev, &["backtab"]),
        (TabNowPlaying, &["n"]),
        (TabQueue, &["q"]),
        (TabLyrics, &["l"]),
        (TabSearch, &["/", "s"]),
        (TabLibrary, &["b"]),
        (ToggleViz, &["1"]),
        (TogglePlayer, &["2"]),
        (ToggleControls, &["3"]),
        (Up, &["up", "k"]),
        (Down, &["down", "j"]),
        (Top, &["g", "home"]),
        (Bottom, &["G", "end"]),
        (PageUp, &["pageup", "ctrl+u"]),
        (PageDown, &["pagedown", "ctrl+d"]),
        (Select, &["enter"]),
        (Append, &["a"]),
        (PlayNext, &["A"]),
        (Remove, &["d", "delete"]),
        (MoveUp, &["K", "shift+up"]),
        (MoveDown, &["J", "shift+down"]),
        (Radio, &["r"]),
        (SubPrev, &["["]),
        (SubNext, &["]"]),
        (ClearQueue, &["C"]),
        (Refresh, &["F5", "ctrl+r"]),
        (JumpToCurrent, &["c"]),
        (SleepTimer, &["z"]),
        (Interlink, &["i"]),
        (Equalizer, &["e"]),
        (AbLoop, &["B"]),
        (CopyLink, &["y"]),
        (SpeedUp, &[")"]),
        (SpeedDown, &["("]),
        (Stats, &["W"]),
        (SaveOffline, &["O"]),
    ]
}

impl Keymap {
    /// Build the keymap. Overrides replace *all* default keys of that action.
    /// Returns warnings for unknown action names or unparsable keys.
    pub fn new(overrides: &HashMap<String, Vec<String>>) -> (Keymap, Vec<String>) {
        let mut warnings = Vec::new();
        let mut bindings: HashMap<Action, Vec<Key>> = HashMap::new();
        for (action, keys) in defaults() {
            bindings.insert(action, keys.iter().filter_map(|k| Key::parse(k)).collect());
        }
        for (name, keys) in overrides {
            let Some(action) = Action::from_name(name) else {
                warnings.push(format!("unknown action in [keys]: {name}"));
                continue;
            };
            let mut parsed = Vec::new();
            for k in keys {
                match Key::parse(k) {
                    Some(key) => parsed.push(key),
                    None => warnings.push(format!("bad key {k:?} for {name}")),
                }
            }
            // A key bound by an override is removed from any other action.
            for other in bindings.values_mut() {
                other.retain(|k| !parsed.contains(k));
            }
            bindings.insert(action, parsed);
        }
        let mut map = HashMap::new();
        for (action, keys) in bindings {
            for key in keys {
                map.insert(key, action);
            }
        }
        (Keymap { map }, warnings)
    }

    pub fn action(&self, ev: &KeyEvent) -> Option<Action> {
        self.map.get(&Key::from_event(ev)).copied()
    }

    /// Keys bound to an action, for help/hints (sorted, stable).
    pub fn keys_for(&self, action: Action) -> Vec<String> {
        let mut keys: Vec<String> = self
            .map
            .iter()
            .filter(|(_, a)| **a == action)
            .map(|(k, _)| k.display())
            .collect();
        keys.sort_by_key(|k| (k.chars().count(), k.clone()));
        keys
    }

    pub fn hint(&self, action: Action) -> String {
        self.keys_for(action).into_iter().next().unwrap_or_else(|| "-".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_keys() {
        assert_eq!(Key::parse("space").unwrap().code, KeyCode::Char(' '));
        let k = Key::parse("ctrl+c").unwrap();
        assert_eq!((k.code, k.mods), (KeyCode::Char('c'), KeyModifiers::CONTROL));
        let k = Key::parse("shift+left").unwrap();
        assert_eq!((k.code, k.mods), (KeyCode::Left, KeyModifiers::SHIFT));
        assert_eq!(Key::parse("+").unwrap().code, KeyCode::Char('+'));
        assert_eq!(Key::parse("F5").unwrap().code, KeyCode::F(5));
        assert!(Key::parse("bogus").is_none());
    }

    #[test]
    fn shifted_chars_match() {
        let (km, w) = Keymap::new(&HashMap::new());
        assert!(w.is_empty());
        let ev = KeyEvent::new(KeyCode::Char('N'), KeyModifiers::SHIFT);
        assert_eq!(km.action(&ev), Some(Action::Next));
        let ev = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(km.action(&ev), Some(Action::TabNowPlaying));
        let ev = KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(km.action(&ev), Some(Action::TabPrev));
    }

    #[test]
    fn overrides_replace_and_steal() {
        let mut o = HashMap::new();
        o.insert("next".to_string(), vec!["n".to_string()]);
        o.insert("nonsense".to_string(), vec!["x".to_string()]);
        let (km, w) = Keymap::new(&o);
        assert_eq!(w.len(), 1);
        let ev = KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE);
        assert_eq!(km.action(&ev), Some(Action::Next));
        let ev = KeyEvent::new(KeyCode::Char('>'), KeyModifiers::NONE);
        assert_eq!(km.action(&ev), None);
    }
}

//! Interlink: seamless navigation across artists/albums/radio, universal
//! cross-platform sharing (`song.link` / YouTube Music / Spotify / Tidal),
//! and opening external YouTube / Spotify / Tidal / song.link URLs right in pova.

use std::io::Write;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::ytm::SearchFilter;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkKind {
    Track,
    Album,
    Playlist,
    Artist,
}

/// An external music URL recognized by pova (pasted into Search or passed via `pova open`).
#[derive(Clone, Debug, PartialEq)]
pub enum ExternalLink {
    YouTubeTrack {
        video_id: String,
        start_secs: Option<f64>,
    },
    YouTubePlaylist {
        playlist_id: String,
    },
    YouTubeAlbum {
        browse_id: String,
    },
    YouTubeArtist {
        browse_id: String,
    },
    CrossPlatform {
        service: &'static str,
        kind: LinkKind,
        url: String,
    },
}

/// Outcome of resolving a cross-platform (Spotify / Tidal / song.link) URL.
#[derive(Clone, Debug, PartialEq)]
pub enum ResolvedLink {
    /// Matched directly to a YouTube Music video id.
    YouTubeVideo {
        video_id: String,
        title: Option<String>,
        artist: Option<String>,
    },
    /// Fallback search query on YouTube Music (e.g. playlists, albums, artists, or oEmbed match).
    Search { query: String, filter: SearchFilter },
}

/// Universal cross-platform interlink (opens Spotify, Tidal, Apple Music, YouTube Music).
pub fn universal_url(video_id: &str) -> String {
    format!("https://song.link/y/{video_id}")
}

/// Direct YouTube Music watch URL, optionally at a specific timestamp.
pub fn ytm_url(video_id: &str, start_secs: Option<f64>) -> String {
    match start_secs.filter(|s| s.is_finite() && *s >= 1.0) {
        Some(s) => format!("https://music.youtube.com/watch?v={video_id}&t={}s", s.floor() as u64),
        None => format!("https://music.youtube.com/watch?v={video_id}"),
    }
}

pub fn spotify_search_url(title: &str, artist: &str) -> String {
    let q = percent_encode(&format!("{title} {artist}").trim().to_string());
    format!("https://open.spotify.com/search/{q}")
}

pub fn tidal_search_url(title: &str, artist: &str) -> String {
    let q = percent_encode(&format!("{title} {artist}").trim().to_string());
    format!("https://listen.tidal.com/search?q={q}")
}

/// Percent-encode a string for URL path/query segments.
pub fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            b' ' => out.push_str("%20"),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Parse a YouTube, YouTube Music, Spotify, Tidal, or song.link URL / URI.
pub fn parse_external_link(raw: &str) -> Option<ExternalLink> {
    let s = raw.trim().trim_matches(|c| matches!(c, '<' | '>' | '"' | '\''));
    if s.is_empty() || s.contains(char::is_whitespace) {
        return None;
    }

    // Spotify URI form: `spotify:track:<id>`
    if let Some(rest) = s.strip_prefix("spotify:") {
        let mut parts = rest.split(':');
        let kind_str = parts.next()?;
        let id = parts.next().filter(|id| !id.is_empty())?;
        let (kind, seg) = match kind_str {
            "track" => (LinkKind::Track, "track"),
            "album" => (LinkKind::Album, "album"),
            "playlist" => (LinkKind::Playlist, "playlist"),
            "artist" => (LinkKind::Artist, "artist"),
            _ => return None,
        };
        return Some(ExternalLink::CrossPlatform {
            service: "Spotify",
            kind,
            url: format!("https://open.spotify.com/{seg}/{id}"),
        });
    }

    let without_scheme = s
        .strip_prefix("https://")
        .or_else(|| s.strip_prefix("http://"))?;
    let (host_port, path_and_query) = match without_scheme.split_once('/') {
        Some((h, rest)) => (h, rest),
        None => return None,
    };
    let host = host_port
        .split(':')
        .next()
        .unwrap_or(host_port)
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").or_else(|| host.strip_prefix("m.")).unwrap_or(&host);

    let (path, query) = match path_and_query.split_once('?') {
        Some((p, q)) => (p, q.split('#').next().unwrap_or(q)),
        None => (path_and_query.split('#').next().unwrap_or(path_and_query), ""),
    };

    // YouTube / YouTube Music
    if matches!(host, "music.youtube.com" | "youtube.com") {
        if path == "watch" || path.starts_with("watch/") {
            if let Some(vid) = query_param(query, "v").filter(|v| is_valid_id(v)) {
                let start = query_param(query, "t").and_then(parse_yt_timestamp);
                return Some(ExternalLink::YouTubeTrack {
                    video_id: vid.to_string(),
                    start_secs: start,
                });
            }
            if let Some(list) = query_param(query, "list").filter(|v| is_valid_id(v)) {
                return Some(ExternalLink::YouTubePlaylist {
                    playlist_id: list.to_string(),
                });
            }
        }
        if path == "playlist"
            && let Some(list) = query_param(query, "list").filter(|v| is_valid_id(v))
        {
            return Some(ExternalLink::YouTubePlaylist {
                playlist_id: list.to_string(),
            });
        }
        if let Some(id) = path.strip_prefix("shorts/").map(|p| p.split('/').next().unwrap_or(p))
            && is_valid_id(id)
        {
            return Some(ExternalLink::YouTubeTrack {
                video_id: id.to_string(),
                start_secs: query_param(query, "t").and_then(parse_yt_timestamp),
            });
        }
        if let Some(id) = path.strip_prefix("browse/").map(|p| p.split('/').next().unwrap_or(p))
            && is_valid_id(id)
        {
            if id.starts_with("MPREb_") || id.starts_with("OLAK") {
                return Some(ExternalLink::YouTubeAlbum {
                    browse_id: id.to_string(),
                });
            }
            if id.starts_with("VL") {
                return Some(ExternalLink::YouTubePlaylist {
                    playlist_id: id.to_string(),
                });
            }
            if id.starts_with("UC") {
                return Some(ExternalLink::YouTubeArtist {
                    browse_id: id.to_string(),
                });
            }
        }
        if let Some(id) = path.strip_prefix("channel/").map(|p| p.split('/').next().unwrap_or(p))
            && is_valid_id(id)
        {
            return Some(ExternalLink::YouTubeArtist {
                browse_id: id.to_string(),
            });
        }
        return None;
    }

    if host == "youtu.be" {
        let id = path.split('/').next().unwrap_or("");
        if is_valid_id(id) {
            return Some(ExternalLink::YouTubeTrack {
                video_id: id.to_string(),
                start_secs: query_param(query, "t").and_then(parse_yt_timestamp),
            });
        }
        return None;
    }

    // Spotify web URLs (including intl-xx/track/... paths)
    if host == "open.spotify.com" || host == "play.spotify.com" {
        let segs: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let kind = segs.iter().find_map(|&seg| match seg {
            "track" => Some(LinkKind::Track),
            "album" => Some(LinkKind::Album),
            "playlist" => Some(LinkKind::Playlist),
            "artist" => Some(LinkKind::Artist),
            _ => None,
        })?;
        return Some(ExternalLink::CrossPlatform {
            service: "Spotify",
            kind,
            url: s.to_string(),
        });
    }

    // Tidal URLs
    if matches!(host, "tidal.com" | "listen.tidal.com") {
        let segs: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
        let kind = segs.iter().find_map(|&seg| match seg {
            "track" => Some(LinkKind::Track),
            "album" => Some(LinkKind::Album),
            "playlist" => Some(LinkKind::Playlist),
            "artist" => Some(LinkKind::Artist),
            _ => None,
        })?;
        return Some(ExternalLink::CrossPlatform {
            service: "Tidal",
            kind,
            url: s.to_string(),
        });
    }

    // Universal song.link / album.link / odesli.co / Apple Music
    if matches!(host, "song.link" | "album.link" | "odesli.co" | "music.apple.com") {
        let service = if host == "music.apple.com" {
            "Apple Music"
        } else {
            "song.link"
        };
        let kind = if host == "album.link" || path.contains("/album/") {
            LinkKind::Album
        } else {
            LinkKind::Track
        };
        return Some(ExternalLink::CrossPlatform {
            service,
            kind,
            url: s.to_string(),
        });
    }

    None
}

fn query_param<'a>(query: &'a str, key: &str) -> Option<&'a str> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key && !v.is_empty()).then_some(v)
    })
}

fn is_valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
}

/// Parse YouTube `t=` values: `90`, `90s`, `1m30s`, `1h2m3s`.
pub fn parse_yt_timestamp(raw: &str) -> Option<f64> {
    let s = raw.trim().to_ascii_lowercase();
    if let Ok(secs) = s.parse::<f64>()
        && secs.is_finite()
        && secs >= 0.0
    {
        return Some(secs);
    }
    let mut total = 0u64;
    let mut num = 0u64;
    let mut any = false;
    for ch in s.chars() {
        if let Some(d) = ch.to_digit(10) {
            num = num.saturating_mul(10).saturating_add(d as u64);
            any = true;
        } else {
            match ch {
                'h' => total = total.saturating_add(num.saturating_mul(3600)),
                'm' => total = total.saturating_add(num.saturating_mul(60)),
                's' => total = total.saturating_add(num),
                _ => return None,
            }
            num = 0;
        }
    }
    if any {
        Some((total + num) as f64)
    } else {
        None
    }
}

/// Resolve a Spotify / Tidal / Apple Music / song.link URL to a YouTube Music
/// video ID (via Odesli/song.link API) or a fallback search query (via oEmbed).
pub async fn resolve_cross_platform(
    http: &reqwest::Client,
    url: &str,
    service: &str,
    kind: LinkKind,
) -> anyhow::Result<ResolvedLink> {
    // 1. Try Odesli / song.link universal interlink resolver (no auth needed).
    let api = format!("https://api.song.link/v1-alpha.1/links?url={}", percent_encode(url));
    if let Ok(resp) = http.get(&api).send().await
        && resp.status().is_success()
        && let Ok(json) = resp.json::<Value>().await
        && let Some(resolved) = parse_odesli_response(&json, kind)
    {
        return Ok(resolved);
    }

    // 2. Fallback: Spotify oEmbed gives track/album/playlist title cleanly.
    if service == "Spotify" {
        let oembed = format!("https://open.spotify.com/oembed?url={}", percent_encode(url));
        if let Ok(resp) = http.get(&oembed).send().await
            && resp.status().is_success()
            && let Ok(json) = resp.json::<Value>().await
            && let Some(title) = json.get("title").and_then(Value::as_str)
        {
            let filter = match kind {
                LinkKind::Album => SearchFilter::Albums,
                LinkKind::Playlist => SearchFilter::Playlists,
                LinkKind::Artist => SearchFilter::Artists,
                _ => SearchFilter::Songs,
            };
            return Ok(ResolvedLink::Search {
                query: title.to_string(),
                filter,
            });
        }
    }

    anyhow::bail!("Couldn't resolve {service} link. Try searching for the song name directly.")
}

/// Extract a direct YouTube Music video ID (or title+artist search) from an Odesli JSON payload.
pub fn parse_odesli_response(json: &Value, kind: LinkKind) -> Option<ResolvedLink> {
    let entity_meta = json.get("entitiesByUniqueId").and_then(Value::as_object).and_then(|map| {
        let uid = json.get("entityUniqueId").and_then(Value::as_str);
        uid.and_then(|id| map.get(id)).or_else(|| map.values().next())
    });
    let title = entity_meta
        .and_then(|e| e.get("title"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let artist = entity_meta
        .and_then(|e| e.get("artistName"))
        .and_then(Value::as_str)
        .map(str::to_string);

    if kind == LinkKind::Track {
        let ytm_url = json
            .pointer("/linksByPlatform/youtubeMusic/url")
            .or_else(|| json.pointer("/linksByPlatform/youtube/url"))
            .and_then(Value::as_str);
        if let Some(u) = ytm_url
            && let Some(vid) = crate::model::video_id_from_url(u)
        {
            return Some(ResolvedLink::YouTubeVideo {
                video_id: vid,
                title,
                artist,
            });
        }
    }

    let query = match (title, artist) {
        (Some(t), Some(a)) if !t.is_empty() && !a.is_empty() => format!("{t} {a}"),
        (Some(t), _) if !t.is_empty() => t,
        _ => return None,
    };
    let filter = match kind {
        LinkKind::Album => SearchFilter::Albums,
        LinkKind::Playlist => SearchFilter::Playlists,
        LinkKind::Artist => SearchFilter::Artists,
        _ => SearchFilter::Songs,
    };
    Some(ResolvedLink::Search { query, filter })
}

/// Copy `text` to the system clipboard. Tries Wayland (`wl-copy`), X11 (`xclip` / `xsel`),
/// and falls back to OSC 52 so copying works over SSH and in modern terminals too.
pub fn copy_to_clipboard(text: &str) -> Result<&'static str, String> {
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|v| !v.is_empty());
    let mut candidates: Vec<(&str, &[&str])> = Vec::new();
    if wayland {
        candidates.push(("wl-copy", &[]));
    }
    candidates.push(("xclip", &["-selection", "clipboard"]));
    candidates.push(("xsel", &["--clipboard", "--input"]));
    if !wayland {
        candidates.push(("wl-copy", &[]));
    }

    for (bin, args) in candidates {
        if crate::player::mpv::which(bin).is_none() {
            continue;
        }
        if let Ok(mut child) = Command::new(bin)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            let wrote = child
                .stdin
                .take()
                .is_some_and(|mut s| s.write_all(text.as_bytes()).is_ok());
            if wrote && child.wait().is_ok_and(|st| st.success()) {
                return Ok("clipboard");
            }
        }
    }

    // Fallback: OSC 52 terminal clipboard escape sequence.
    let encoded = base64_encode(text.as_bytes());
    let mut out = std::io::stdout();
    write!(out, "\x1b]52;c;{encoded}\x07")
        .and_then(|()| out.flush())
        .map_err(|e| format!("couldn't copy to clipboard: {e}"))?;
    Ok("terminal clipboard")
}

/// Open an `https://` URL in the user's desktop browser.
pub fn open_in_browser(url: &str) -> Result<(), String> {
    if !url.starts_with("https://") {
        return Err("refused to open non-https URL".into());
    }
    Command::new("xdg-open")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| format!("couldn't open browser via xdg-open: {e}"))
}

/// Dependency-free standard base64 encoder (used for OSC 52 clipboard fallback).
pub fn base64_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let n = (b0 << 16) | (b1 << 8) | b2;
        out.push(TABLE[((n >> 18) & 63) as usize] as char);
        out.push(TABLE[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(TABLE[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(TABLE[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_youtube_spotify_tidal_and_songlink_urls() {
        assert_eq!(
            parse_external_link("https://music.youtube.com/watch?v=dQw4w9WgXcQ&t=1m30s"),
            Some(ExternalLink::YouTubeTrack {
                video_id: "dQw4w9WgXcQ".into(),
                start_secs: Some(90.0),
            })
        );
        assert_eq!(
            parse_external_link("https://youtu.be/dQw4w9WgXcQ?t=45"),
            Some(ExternalLink::YouTubeTrack {
                video_id: "dQw4w9WgXcQ".into(),
                start_secs: Some(45.0),
            })
        );
        assert_eq!(
            parse_external_link("https://music.youtube.com/playlist?list=PL12345"),
            Some(ExternalLink::YouTubePlaylist {
                playlist_id: "PL12345".into(),
            })
        );
        assert_eq!(
            parse_external_link("https://music.youtube.com/browse/MPREb_abc123"),
            Some(ExternalLink::YouTubeAlbum {
                browse_id: "MPREb_abc123".into(),
            })
        );
        assert_eq!(
            parse_external_link("https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT?si=xyz"),
            Some(ExternalLink::CrossPlatform {
                service: "Spotify",
                kind: LinkKind::Track,
                url: "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT?si=xyz".into(),
            })
        );
        assert_eq!(
            parse_external_link("spotify:track:4cOdK2wGLETKBW3PvgPWqT"),
            Some(ExternalLink::CrossPlatform {
                service: "Spotify",
                kind: LinkKind::Track,
                url: "https://open.spotify.com/track/4cOdK2wGLETKBW3PvgPWqT".into(),
            })
        );
        assert_eq!(
            parse_external_link("https://listen.tidal.com/track/12345678"),
            Some(ExternalLink::CrossPlatform {
                service: "Tidal",
                kind: LinkKind::Track,
                url: "https://listen.tidal.com/track/12345678".into(),
            })
        );
        assert_eq!(
            parse_external_link("https://song.link/s/4cOdK2wGLETKBW3PvgPWqT"),
            Some(ExternalLink::CrossPlatform {
                service: "song.link",
                kind: LinkKind::Track,
                url: "https://song.link/s/4cOdK2wGLETKBW3PvgPWqT".into(),
            })
        );
        assert_eq!(parse_external_link("seether fine again"), None);
    }

    #[test]
    fn parses_odesli_response_to_youtube_video() {
        let payload = json!({
            "entityUniqueId": "SPOTIFY_SONG::123",
            "entitiesByUniqueId": {
                "SPOTIFY_SONG::123": {
                    "title": "Fine Again",
                    "artistName": "Seether"
                }
            },
            "linksByPlatform": {
                "youtubeMusic": {
                    "url": "https://music.youtube.com/watch?v=abc123XYZ"
                }
            }
        });
        assert_eq!(
            parse_odesli_response(&payload, LinkKind::Track),
            Some(ResolvedLink::YouTubeVideo {
                video_id: "abc123XYZ".into(),
                title: Some("Fine Again".into()),
                artist: Some("Seether".into()),
            })
        );
    }

    #[test]
    fn base64_encodes_rfc4648_vectors() {
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"f"), "Zg==");
        assert_eq!(base64_encode(b"fo"), "Zm8=");
        assert_eq!(base64_encode(b"foo"), "Zm9v");
        assert_eq!(base64_encode(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn outbound_interlinks_format_cleanly() {
        assert_eq!(universal_url("abc123"), "https://song.link/y/abc123");
        assert_eq!(
            ytm_url("abc123", Some(92.7)),
            "https://music.youtube.com/watch?v=abc123&t=92s"
        );
        assert_eq!(
            spotify_search_url("Fine Again", "Seether"),
            "https://open.spotify.com/search/Fine%20Again%20Seether"
        );
    }
}

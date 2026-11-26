//! Internal data model, independent of ytmapi-rs types.

#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Track {
    pub video_id: String,
    pub title: String,
    pub artists: String,
    #[serde(default)]
    pub artist_id: Option<String>,
    pub album: Option<String>,
    pub album_id: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    pub thumbnail: Option<String>,
    pub liked: Option<bool>,
}

impl Track {
    pub fn url(&self) -> String {
        format!("https://music.youtube.com/watch?v={}", self.video_id)
    }

    /// High-resolution square artwork URL.
    pub fn art_url(&self) -> String {
        match &self.thumbnail {
            Some(url) if url.contains("googleusercontent.com") || url.contains("ggpht.com") => {
                upscale_google_thumb(url, 800)
            }
            _ => format!("https://i.ytimg.com/vi/{}/hqdefault.jpg", self.video_id),
        }
    }
}

/// `...=w60-h60-l90-rj` -> `...=w{size}-h{size}-l90-rj`
pub fn upscale_google_thumb(url: &str, size: u32) -> String {
    match url.rfind('=') {
        Some(i) => {
            let params = &url[i + 1..];
            let rest: Vec<&str> = params
                .split('-')
                .filter(|p| {
                    let is_size = p.len() >= 2
                        && matches!(p.as_bytes()[0], b'w' | b'h' | b's')
                        && p[1..].chars().all(|c| c.is_ascii_digit());
                    !is_size && !p.is_empty()
                })
                .collect();
            let mut out = format!("{}=w{size}-h{size}", &url[..i]);
            for r in rest {
                out.push('-');
                out.push_str(r);
            }
            out
        }
        None => format!("{url}=w{size}-h{size}"),
    }
}

#[derive(Clone, Debug)]
pub struct AlbumRef {
    pub id: String,
    pub title: String,
    pub subtitle: String,
}

#[derive(Clone, Debug)]
pub struct ArtistRef {
    pub id: String,
    pub name: String,
    pub subtitle: String,
}

#[derive(Clone, Debug)]
pub struct PlaylistRef {
    pub id: String,
    pub title: String,
    pub subtitle: String,
}

/// Anything that can appear in a browsable list.
#[derive(Clone, Debug)]
pub enum Item {
    Track(Track),
    Album(AlbumRef),
    Artist(ArtistRef),
    Playlist(PlaylistRef),
    /// Non-selectable section heading.
    Header(String),
}

impl Item {
    pub fn as_track(&self) -> Option<&Track> {
        match self {
            Item::Track(t) => Some(t),
            _ => None,
        }
    }
}

/// "3:45" / "1:02:03" -> seconds.
pub fn parse_duration(s: &str) -> Option<f64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let mut total = 0.0;
    for part in s.split(':') {
        total = total * 60.0 + part.trim().parse::<f64>().ok()?;
    }
    Some(total)
}

pub fn format_time(secs: f64) -> String {
    if !secs.is_finite() || secs < 0.0 {
        return "--:--".into();
    }
    let s = secs as u64;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

/// Extract `v=` from a YouTube watch URL.
pub fn video_id_from_url(url: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    query
        .split('&')
        .find_map(|kv| kv.strip_prefix("v="))
        .map(|v| v.split('#').next().unwrap_or(v).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!(parse_duration("3:45"), Some(225.0));
        assert_eq!(parse_duration("1:02:03"), Some(3723.0));
        assert_eq!(parse_duration(""), None);
        assert_eq!(format_time(225.4), "3:45");
        assert_eq!(format_time(3723.0), "1:02:03");
    }

    #[test]
    fn video_ids() {
        assert_eq!(
            video_id_from_url("https://music.youtube.com/watch?v=abc123&list=x").as_deref(),
            Some("abc123")
        );
        assert_eq!(video_id_from_url("https://x/y"), None);
    }

    #[test]
    fn thumb_upscale() {
        assert_eq!(
            upscale_google_thumb("https://lh3.googleusercontent.com/abc=w60-h60-l90-rj", 800),
            "https://lh3.googleusercontent.com/abc=w800-h800-l90-rj"
        );
        assert_eq!(
            upscale_google_thumb("https://yt3.ggpht.com/abc=s120", 800),
            "https://yt3.ggpht.com/abc=w800-h800"
        );
    }
}

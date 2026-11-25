//! Lenient reading of playlist pages (Liked songs and any playlist).
//!
//! ytmapi-rs parses a whole page or nothing: one odd entry, such as a video
//! whose channel name has no channel link, fails the page and every song on
//! it. When that happens pova fetches the same pages itself, through
//! ytmapi-rs's own request plumbing and sign-in, and reads each entry
//! leniently, skipping only entries that can't be played at all.

use std::borrow::Cow;

use serde_json::Value;
use ytmapi_rs::YtMusic;
use ytmapi_rs::auth::AuthToken;
use ytmapi_rs::parse::{ParseFrom, ProcessedResult};
use ytmapi_rs::query::{PostMethod, PostQuery, Query};

use crate::model::{Track, parse_duration};

/// One page of a playlist (`browse`), the first or a continuation.
pub struct BrowsePage {
    browse_id: String,
    continuation: Option<String>,
}

/// Placeholder output: these queries are only sent with `raw_json_query`.
#[derive(Debug)]
pub struct Unparsed;

impl ParseFrom<BrowsePage> for Unparsed {
    fn parse_from(_: ProcessedResult<BrowsePage>) -> ytmapi_rs::Result<Self> {
        Ok(Unparsed)
    }
}

impl<A: AuthToken> Query<A> for BrowsePage {
    type Output = Unparsed;
    type Method = PostMethod;
}

impl PostQuery for BrowsePage {
    fn header(&self) -> serde_json::Map<String, Value> {
        serde_json::Map::from_iter([("browseId".to_string(), Value::String(self.browse_id.clone()))])
    }
    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        match &self.continuation {
            // Same parameters ytmapi-rs sends for its own continuations.
            Some(c) => vec![("ctoken", c.as_str().into()), ("continuation", c.as_str().into())],
            None => vec![],
        }
    }
    fn path(&self) -> &str {
        "browse"
    }
}

/// Every track of playlist `browse_id` (the "VL…" form), up to `max_pages`
/// pages. Fails only if the first page can't be fetched at all.
pub async fn playlist_tracks<A: AuthToken>(
    yt: &YtMusic<A>,
    browse_id: &str,
    max_pages: usize,
) -> anyhow::Result<Vec<Track>> {
    let mut tracks = Vec::new();
    let mut continuation = None;
    for page in 0..max_pages {
        let query = BrowsePage {
            browse_id: browse_id.to_string(),
            continuation: continuation.take(),
        };
        let raw = match fetch(yt, &query).await {
            Ok(raw) => raw,
            Err(e) if page == 0 => return Err(e.into()),
            Err(e) => {
                tracing::warn!("playlist {browse_id}: stopping after {page} pages: {e}");
                break;
            }
        };
        let json: Value = serde_json::from_str(&raw)?;
        let (items, next) = page_items(&json);
        tracks.extend(items);
        match next {
            Some(token) => continuation = Some(token),
            None => break,
        }
    }
    Ok(tracks)
}

/// Every track of playlist `browse_id` (the "VL…" form) fetched through
/// [`crate::innertube::Authed`] so Brand Account delegation (`onBehalfOfUser`)
/// and `X-Goog-AuthUser` / `X-Goog-PageId` headers apply to every page.
pub async fn playlist_tracks_authed(
    yt: &crate::innertube::Authed,
    browse_id: &str,
    max_pages: usize,
) -> anyhow::Result<Vec<Track>> {
    let mut tracks = Vec::new();
    let mut continuation: Option<String> = None;
    for page in 0..max_pages {
        let (body, params) = match continuation.take() {
            Some(c) => (
                serde_json::Map::from_iter([("continuation".to_string(), Value::String(c.clone()))]),
                vec![
                    ("ctoken".to_string(), c.clone()),
                    ("continuation".to_string(), c),
                    ("type".to_string(), "next".to_string()),
                ],
            ),
            None => (
                serde_json::Map::from_iter([("browseId".to_string(), Value::String(browse_id.to_string()))]),
                Vec::new(),
            ),
        };
        let raw = match fetch_authed(yt, body, params).await {
            Ok(raw) => raw,
            Err(e) if page == 0 => return Err(e.into()),
            Err(e) => {
                tracing::warn!("playlist {browse_id}: stopping after {page} pages: {e}");
                break;
            }
        };
        let json: Value = serde_json::from_str(&raw)?;
        let (items, next) = page_items(&json);
        tracks.extend(items);
        match next {
            Some(token) => continuation = Some(token),
            None => break,
        }
    }
    Ok(tracks)
}

async fn fetch_authed(
    yt: &crate::innertube::Authed,
    body: serde_json::Map<String, Value>,
    params: Vec<(String, String)>,
) -> ytmapi_rs::Result<String> {
    let mut attempt = 1;
    loop {
        match yt.post_raw("browse", body.clone(), params.clone()).await {
            Err(e) if attempt < 3 => {
                tracing::debug!("playlist page: attempt {attempt} failed, retrying: {e}");
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)).await;
            }
            res => return res,
        }
    }
}

/// One page, tried up to 3 times: on a slow connection a long page's
/// response sometimes arrives cut off, which a second try fixes.
async fn fetch<A: AuthToken>(yt: &YtMusic<A>, query: &BrowsePage) -> ytmapi_rs::Result<String> {
    let mut attempt = 1;
    loop {
        match yt.raw_json_query::<BrowsePage>(query).await {
            Err(e) if attempt < 3 => {
                tracing::debug!("playlist page: attempt {attempt} failed, retrying: {e}");
                attempt += 1;
                tokio::time::sleep(std::time::Duration::from_millis(500 * attempt)).await;
            }
            res => return res,
        }
    }
}

/// The tracks on one page and the token for the next page, if any.
pub(crate) fn page_items(json: &Value) -> (Vec<Track>, Option<String>) {
    let list = json
        .pointer("/onResponseReceivedActions/0/appendContinuationItemsAction/continuationItems")
        .or_else(|| find_key(json, "musicPlaylistShelfRenderer").and_then(|s| s.get("contents")))
        .and_then(Value::as_array);
    let Some(list) = list else {
        return (Vec::new(), None);
    };
    let mut tracks = Vec::new();
    let mut next = None;
    for entry in list {
        if let Some(item) = entry.get("musicResponsiveListItemRenderer") {
            tracks.extend(track(item));
        } else if let Some(token) = entry
            .pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
            .and_then(Value::as_str)
        {
            next = Some(token.to_string());
        }
    }
    (tracks, next)
}

/// First object stored under `key` anywhere in `v` (depth-first).
fn find_key<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(map) => map.get(key).or_else(|| map.values().find_map(|c| find_key(c, key))),
        Value::Array(items) => items.iter().find_map(|c| find_key(c, key)),
        _ => None,
    }
}

fn str_at<'a>(v: &'a Value, ptr: &str) -> Option<&'a str> {
    v.pointer(ptr).and_then(Value::as_str)
}

/// The text runs of flex column `n` (title, artists / channel, album).
fn column(item: &Value, n: usize) -> Option<&Vec<Value>> {
    item.pointer(&format!(
        "/flexColumns/{n}/musicResponsiveListItemFlexColumnRenderer/text/runs"
    ))
    .and_then(Value::as_array)
}

/// One playlist entry, or None if it can't be played (deleted, greyed out,
/// no video id). Everything else is optional.
fn track(item: &Value) -> Option<Track> {
    if str_at(item, "/musicItemRendererDisplayPolicy") == Some("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT") {
        return None;
    }
    let video_id = str_at(
        item,
        "/overlay/musicItemThumbnailOverlayRenderer/content/musicPlayButtonRenderer/playNavigationEndpoint/watchEndpoint/videoId",
    )
    .or_else(|| str_at(item, "/playlistItemData/videoId"))?
    .to_string();
    let title = column(item, 0)?.first()?.get("text")?.as_str()?.to_string();
    if title == "Song deleted" {
        return None;
    }
    // Artists ("A, B & C") or the channel name; the separators are runs too.
    let artists = column(item, 1)
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default();
    let album_run = column(item, 2).and_then(|runs| runs.first());
    let album = album_run
        .and_then(|r| r.get("text").and_then(Value::as_str))
        .filter(|s| !s.is_empty())
        .map(str::to_string);
    let album_id = album_run
        .and_then(|r| str_at(r, "/navigationEndpoint/browseEndpoint/browseId"))
        .map(str::to_string);
    let duration = item
        .pointer("/fixedColumns/0/musicResponsiveListItemFixedColumnRenderer/text")
        .and_then(|t| {
            t.get("simpleText")
                .and_then(Value::as_str)
                .or_else(|| str_at(t, "/runs/0/text"))
        })
        .and_then(parse_duration);
    let thumbnail = item
        .pointer("/thumbnail/musicThumbnailRenderer/thumbnail/thumbnails")
        .and_then(Value::as_array)
        .and_then(|t| {
            t.iter()
                .max_by_key(|t| t.get("width").and_then(Value::as_u64).unwrap_or(0))
        })
        .and_then(|t| t.get("url").and_then(Value::as_str))
        .map(str::to_string);
    let liked = item
        .pointer("/menu/menuRenderer/topLevelButtons")
        .and_then(Value::as_array)
        .and_then(|b| b.iter().find_map(|b| str_at(b, "/likeButtonRenderer/likeStatus")))
        .map(|s| s == "LIKE");
    Some(Track {
        video_id,
        title,
        artists,
        album,
        album_id,
        duration,
        thumbnail,
        liked,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn entry(video_id: &str, title: &str, col1: Value, col2: Value, like: &str) -> Value {
        json!({ "musicResponsiveListItemRenderer": {
            "overlay": { "musicItemThumbnailOverlayRenderer": { "content": { "musicPlayButtonRenderer": {
                "playNavigationEndpoint": { "watchEndpoint": { "videoId": video_id } } } } } },
            "flexColumns": [
                { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [ { "text": title } ] } } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": col1 } },
                { "musicResponsiveListItemFlexColumnRenderer": { "text": col2 } }
            ],
            "fixedColumns": [ { "musicResponsiveListItemFixedColumnRenderer": {
                "text": { "runs": [ { "text": "3:35" } ] } } } ],
            "thumbnail": { "musicThumbnailRenderer": { "thumbnail": { "thumbnails": [
                { "url": "https://i.ytimg.com/small.jpg", "width": 60 },
                { "url": "https://i.ytimg.com/big.jpg", "width": 544 } ] } } },
            "menu": { "menuRenderer": { "topLevelButtons": [ { "likeButtonRenderer": { "likeStatus": like } } ] } }
        } })
    }

    /// The shape that broke Liked songs: a video whose channel name has no
    /// channel link (no navigationEndpoint), between two normal songs.
    fn first_page() -> Value {
        let song = entry(
            "s1",
            "Call Me When You're Sober",
            json!({ "runs": [ { "text": "Evanescence",
                "navigationEndpoint": { "browseEndpoint": { "browseId": "UCx" } } } ] }),
            json!({ "runs": [ { "text": "The Open Door",
                "navigationEndpoint": { "browseEndpoint": { "browseId": "MPREb_1" } } } ] }),
            "LIKE",
        );
        let unlinked_video = entry(
            "v2",
            "Live at the Pub",
            json!({ "runs": [ { "text": "Some Uploader" } ] }),
            json!({}),
            "LIKE",
        );
        let two_artists = entry(
            "s3",
            "Fine Again",
            json!({ "runs": [ { "text": "Seether" }, { "text": " & " }, { "text": "Amy Lee" } ] }),
            json!({ "runs": [ { "text": "Disclaimer" } ] }),
            "INDIFFERENT",
        );
        let deleted = entry("s4", "Song deleted", json!({}), json!({}), "INDIFFERENT");
        json!({ "contents": { "twoColumnBrowseResultsRenderer": { "secondaryContents": {
            "sectionListRenderer": { "contents": [ { "musicPlaylistShelfRenderer": { "contents": [
                song, unlinked_video, two_artists, deleted,
                { "continuationItemRenderer": { "continuationEndpoint": {
                    "continuationCommand": { "token": "NEXT1" } } } }
            ] } } ] } } } } })
    }

    #[test]
    fn reads_the_page_that_broke_liked_songs() {
        let (tracks, next) = page_items(&first_page());
        assert_eq!(next.as_deref(), Some("NEXT1"));
        let ids: Vec<&str> = tracks.iter().map(|t| t.video_id.as_str()).collect();
        assert_eq!(ids, ["s1", "v2", "s3"], "deleted entry skipped, the rest kept");
        let s1 = &tracks[0];
        assert_eq!(s1.artists, "Evanescence");
        assert_eq!(s1.album.as_deref(), Some("The Open Door"));
        assert_eq!(s1.album_id.as_deref(), Some("MPREb_1"));
        assert_eq!(s1.duration, Some(215.0));
        assert_eq!(s1.thumbnail.as_deref(), Some("https://i.ytimg.com/big.jpg"));
        assert_eq!(s1.liked, Some(true));
        let v2 = &tracks[1];
        assert_eq!(
            (v2.title.as_str(), v2.artists.as_str()),
            ("Live at the Pub", "Some Uploader")
        );
        assert_eq!(v2.album, None);
        assert_eq!(tracks[2].artists, "Seether & Amy Lee");
        assert_eq!(tracks[2].liked, Some(false));
    }

    #[test]
    fn reads_continuation_pages_and_stops_at_the_end() {
        let page = json!({ "onResponseReceivedActions": [ { "appendContinuationItemsAction": {
            "continuationItems": [ entry("s9", "Always", json!({ "runs": [ { "text": "Saliva" } ] }),
                json!({}), "INDIFFERENT") ] } } ] });
        let (tracks, next) = page_items(&page);
        assert_eq!(tracks.len(), 1);
        assert_eq!(tracks[0].artists, "Saliva");
        assert_eq!(next, None, "no token: last page");
    }

    #[test]
    fn greyed_out_and_id_less_entries_are_skipped() {
        let mut grey = entry("g1", "Region locked", json!({}), json!({}), "INDIFFERENT");
        grey["musicResponsiveListItemRenderer"]["musicItemRendererDisplayPolicy"] =
            json!("MUSIC_ITEM_RENDERER_DISPLAY_POLICY_GREY_OUT");
        let no_id = json!({ "musicResponsiveListItemRenderer": { "flexColumns": [
            { "musicResponsiveListItemFlexColumnRenderer": { "text": { "runs": [ { "text": "x" } ] } } } ] } });
        assert!(track(&grey["musicResponsiveListItemRenderer"]).is_none());
        assert!(track(&no_id["musicResponsiveListItemRenderer"]).is_none());
    }

    #[test]
    fn continuation_query_sends_the_token_like_ytmapi_rs() {
        let q = BrowsePage {
            browse_id: "VLLM".into(),
            continuation: Some("T".into()),
        };
        assert_eq!(q.path(), "browse");
        assert_eq!(q.header().get("browseId"), Some(&json!("VLLM")));
        let params: Vec<(&str, String)> = q.params().into_iter().map(|(k, v)| (k, v.into_owned())).collect();
        assert_eq!(params, [("ctoken", "T".to_string()), ("continuation", "T".to_string())]);
    }
}

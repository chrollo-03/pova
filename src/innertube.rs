//! Signed-in YouTube Music (InnerTube) requests, built the way the web client
//! and yt-dlp send them:
//!
//! - `Authorization: SAPISIDHASH … SAPISID1PHASH … SAPISID3PHASH …`, bound to
//!   the browser's user session when YouTube tells us one.
//! - `X-Goog-AuthUser` for the active Google account slot in the browser.
//! - `X-Goog-PageId` plus `context.user.onBehalfOfUser` for YouTube Brand
//!   Accounts (channels). Without these YouTube answers as the plain Google
//!   account, whose library is often empty even though the browser shows
//!   playlists.
//!
//! The account details come from the `ytcfg` block that `music.youtube.com`
//! returns for the user's own cookies. Nothing here logs cookie values.

use std::borrow::Cow;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};
use ytmapi_rs::YtMusic;
use ytmapi_rs::auth::{AuthToken, LoggedIn, RawResult};
use ytmapi_rs::parse::{ParseFrom, ProcessedResult};
use ytmapi_rs::query::{PostMethod, PostQuery, Query};

const ORIGIN: &str = "https://music.youtube.com";
const USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/131.0.0.0 Safari/537.36";

/// Account details read from `music.youtube.com`'s `ytcfg`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Identity {
    pub client_version: Option<String>,
    pub logged_in: Option<bool>,
    /// Google account slot (`authuser`) the browser has active.
    pub session_index: Option<String>,
    /// Brand Account / channel page id, when the browser has one selected.
    pub delegated_session_id: Option<String>,
    pub user_session_id: Option<String>,
}

pub fn parse_ytcfg(html: &str) -> Identity {
    let datasync = ytcfg_value(html, "DATASYNC_ID");
    // "DELEGATED||USER" for a Brand Account, "USER||" for the main account.
    let (ds_delegated, ds_user) = match datasync.as_deref().and_then(|d| d.split_once("||")) {
        Some((a, b)) if !b.is_empty() => (non_empty(a), non_empty(b)),
        Some((a, _)) => (None, non_empty(a)),
        None => (None, None),
    };
    Identity {
        client_version: ytcfg_value(html, "INNERTUBE_CLIENT_VERSION"),
        logged_in: ytcfg_value(html, "LOGGED_IN").map(|v| v == "true"),
        session_index: ytcfg_value(html, "SESSION_INDEX").filter(|v| v.chars().all(|c| c.is_ascii_digit())),
        delegated_session_id: ytcfg_value(html, "DELEGATED_SESSION_ID")
            .filter(|v| v.chars().all(|c| c.is_ascii_digit()))
            .or(ds_delegated),
        user_session_id: ytcfg_value(html, "USER_SESSION_ID").or(ds_user),
    }
}

fn non_empty(s: &str) -> Option<String> {
    let s = s.trim();
    (!s.is_empty()).then(|| s.to_string())
}

/// First non-empty value of `"KEY": "value"` / `"KEY": 123` / `"KEY": true` in `html`.
fn ytcfg_value(html: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\":");
    let mut rest = html;
    while let Some(pos) = rest.find(&needle) {
        rest = &rest[pos + needle.len()..];
        let after = rest.trim_start();
        let value = match after.strip_prefix('"') {
            Some(s) => s.split('"').next().unwrap_or("").to_string(),
            None => after
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric() || *c == '.')
                .collect(),
        };
        if !value.is_empty() && value != "null" {
            return Some(value);
        }
    }
    None
}

fn cookie_value(cookies: &str, name: &str) -> Option<String> {
    cookies
        .split(';')
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| k.trim() == name)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// Browser cookie auth for one YouTube identity (Google account slot plus
/// optional Brand Account).
#[derive(Clone)]
pub struct AccountToken {
    cookies: String,
    sapisid: Option<String>,
    sapisid_1p: Option<String>,
    sapisid_3p: Option<String>,
    client_version: String,
    auth_user: String,
    page_id: Option<String>,
    user_session_id: Option<String>,
}

impl std::fmt::Debug for AccountToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountToken")
            .field("auth_user", &self.auth_user)
            .field("brand_account", &self.page_id.is_some())
            .finish_non_exhaustive()
    }
}

impl AccountToken {
    fn new(cookies: &str, id: &Identity, client_version: String) -> Option<Self> {
        let sapisid_3p = cookie_value(cookies, "__Secure-3PAPISID");
        let sapisid = cookie_value(cookies, "SAPISID").or_else(|| sapisid_3p.clone())?;
        Some(Self {
            cookies: cookies.to_string(),
            sapisid: Some(sapisid),
            sapisid_1p: cookie_value(cookies, "__Secure-1PAPISID"),
            sapisid_3p,
            client_version,
            auth_user: id.session_index.clone().unwrap_or_else(|| "0".into()),
            page_id: id.delegated_session_id.clone(),
            user_session_id: id.user_session_id.clone(),
        })
    }

    pub fn auth_user(&self) -> &str {
        &self.auth_user
    }

    pub fn page_id(&self) -> Option<&str> {
        self.page_id.as_deref()
    }

    pub fn has_user_session(&self) -> bool {
        self.user_session_id.is_some()
    }

    /// Same cookies, signed with the classic (session-less) SAPISIDHASH.
    pub fn without_user_session(&self) -> Self {
        Self {
            user_session_id: None,
            ..self.clone()
        }
    }

    /// Same cookies, targeting another account slot and/or Brand Account.
    pub fn for_account(&self, auth_user: &str, page_id: Option<&str>) -> Self {
        Self {
            // The session id belongs to the current Google account only.
            user_session_id: if auth_user == self.auth_user {
                self.user_session_id.clone()
            } else {
                None
            },
            auth_user: auth_user.to_string(),
            page_id: page_id.map(str::to_string),
            ..self.clone()
        }
    }

    fn authorization(&self) -> String {
        let ts = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
        [
            ("SAPISIDHASH", &self.sapisid),
            ("SAPISID1PHASH", &self.sapisid_1p),
            ("SAPISID3PHASH", &self.sapisid_3p),
        ]
        .into_iter()
        .filter_map(|(scheme, sid)| {
            sid.as_deref()
                .map(|sid| sid_authorization(scheme, sid, ts, self.user_session_id.as_deref()))
        })
        .collect::<Vec<_>>()
        .join(" ")
    }
}

/// One `SAPISID*HASH` entry, as the YouTube web client builds it.
fn sid_authorization(scheme: &str, sid: &str, ts: u64, user_session: Option<&str>) -> String {
    match user_session {
        Some(u) => format!("{scheme} {ts}_{}_u", sha1_hex(format!("{u} {ts} {sid} {ORIGIN}").as_bytes())),
        None => format!("{scheme} {ts}_{}", sha1_hex(format!("{ts} {sid} {ORIGIN}").as_bytes())),
    }
}

impl AuthToken for AccountToken {
    fn headers(&self) -> ytmapi_rs::Result<impl IntoIterator<Item = (&str, Cow<'_, str>)>> {
        let mut headers: Vec<(&str, Cow<'_, str>)> = vec![
            ("User-Agent", USER_AGENT.into()),
            ("X-Origin", ORIGIN.into()),
            ("Origin", ORIGIN.into()),
            ("Referer", "https://music.youtube.com/".into()),
            ("Content-Type", "application/json".into()),
            ("Accept", "*/*".into()),
            ("Accept-Encoding", "gzip, deflate".into()),
            ("Authorization", self.authorization().into()),
            ("Cookie", self.cookies.as_str().into()),
            ("X-Goog-AuthUser", self.auth_user.as_str().into()),
            ("X-Youtube-Client-Name", "67".into()),
            ("X-Youtube-Client-Version", self.client_version.as_str().into()),
            ("X-Youtube-Bootstrap-Logged-In", "true".into()),
        ];
        if let Some(p) = &self.page_id {
            headers.push(("X-Goog-PageId", p.as_str().into()));
        }
        Ok(headers)
    }

    fn client_version(&self) -> Cow<'_, str> {
        self.client_version.as_str().into()
    }

    fn deserialize_response<Q>(raw: RawResult<Q, Self>) -> ytmapi_rs::Result<ProcessedResult<Q>> {
        ProcessedResult::try_from(raw)
    }
}

impl LoggedIn for AccountToken {}

/// A POST request whose JSON body (including `context`) we build ourselves.
/// ytmapi-rs merges `header()` over its default body, so our `context` wins.
pub struct Post {
    path: String,
    body: Map<String, Value>,
    params: Vec<(String, String)>,
}

impl Query<AccountToken> for Post {
    type Output = ();
    type Method = PostMethod;
}

impl ParseFrom<Post> for () {
    fn parse_from(_: ProcessedResult<Post>) -> ytmapi_rs::Result<Self> {
        Ok(())
    }
}

impl PostQuery for Post {
    fn header(&self) -> Map<String, Value> {
        self.body.clone()
    }
    fn params(&self) -> Vec<(&str, Cow<'_, str>)> {
        self.params
            .iter()
            .map(|(k, v)| (k.as_str(), Cow::Borrowed(v.as_str())))
            .collect()
    }
    fn path(&self) -> &str {
        &self.path
    }
}

/// Signed-in client for one identity.
#[derive(Clone)]
pub struct Authed {
    pub yt: YtMusic<AccountToken>,
    token: AccountToken,
}

impl Authed {
    pub fn new(token: AccountToken) -> Self {
        Self {
            yt: YtMusic::from_auth_token(token.clone()),
            token,
        }
    }

    pub fn token(&self) -> &AccountToken {
        &self.token
    }

    /// The `context` object the web client sends, delegating to the Brand
    /// Account when one is selected.
    pub fn context(&self) -> Value {
        let mut user = Map::new();
        if let Some(p) = &self.token.page_id {
            user.insert("onBehalfOfUser".into(), json!(p));
        }
        json!({
            "client": {
                "clientName": "WEB_REMIX",
                "clientVersion": self.token.client_version,
                "hl": "en",
            },
            "user": user,
        })
    }

    /// POST `body` to `path` as this identity and return the raw JSON.
    pub async fn post_raw(
        &self,
        path: &str,
        mut body: Map<String, Value>,
        params: Vec<(String, String)>,
    ) -> ytmapi_rs::Result<String> {
        body.insert("context".into(), self.context());
        let post = Post {
            path: path.to_string(),
            body,
            params,
        };
        let raw = self.yt.raw_json_query::<Post>(post).await?;
        check_api_error(&raw)?;
        Ok(raw)
    }

    /// Run a ytmapi-rs query as this identity and parse it with ytmapi-rs.
    pub async fn query<Q>(&self, q: &Q) -> ytmapi_rs::Result<Q::Output>
    where
        Q: PostQuery + Query<AccountToken>,
    {
        let params = q
            .params()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.into_owned()))
            .collect();
        let raw = self.post_raw(q.path(), q.header(), params).await?;
        ytmapi_rs::process_json::<Q, AccountToken>(raw, q)
    }
}

/// Turn `{"error": {...}}` bodies into errors `ytm::explain` understands.
fn check_api_error(raw: &str) -> ytmapi_rs::Result<()> {
    if !raw.trim_start().starts_with("{\"error\"") {
        return Ok(());
    }
    let Ok(v) = serde_json::from_str::<Value>(raw) else {
        return Ok(());
    };
    let Some(err) = v.get("error") else {
        return Ok(());
    };
    let code = err.get("code").and_then(Value::as_u64).unwrap_or(0);
    let message = err.get("message").and_then(Value::as_str).unwrap_or("");
    Err(std::io::Error::other(format!("Http error code {code} recieved in response. Message: <{message}>.")).into())
}

/// Read the account details for `cookies` from `music.youtube.com` and build
/// a token for the identity the browser currently has active.
pub async fn bootstrap(cookies: &str) -> ytmapi_rs::Result<(AccountToken, Identity)> {
    let cookies = cookies.trim();
    let http = ytmapi_rs::Client::new()?;
    let page = http
        .get_query(
            ORIGIN,
            [
                ("User-Agent", Cow::Borrowed(USER_AGENT)),
                ("Cookie", Cow::Borrowed(cookies)),
                ("Accept-Language", Cow::Borrowed("en-US,en;q=0.9")),
            ],
            &(),
        )
        .await?
        .text;
    let id = parse_ytcfg(&page);
    let Some(client_version) = id.client_version.clone() else {
        return Err(std::io::Error::other("YouTube Music didn't send its page configuration; try again").into());
    };
    let Some(token) = AccountToken::new(cookies, &id, client_version) else {
        return Err(std::io::Error::other("the saved cookie has no SAPISID; sign in again").into());
    };
    Ok((token, id))
}

/// Minimal SHA-1 (only used for the SAPISIDHASH request signature).
fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w: Vec<u32> = chunk
            .chunks_exact(4)
            .map(|b| u32::from_be_bytes([b[0], b[1], b[2], b[3]]))
            .collect();
        for i in 16..80 {
            let v = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
            w.push(v);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(*wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        for (x, y) in h.iter_mut().zip([a, b, c, d, e]) {
            *x = x.wrapping_add(y);
        }
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha1_matches_known_vectors() {
        assert_eq!(sha1_hex(b""), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(sha1_hex(b"abc"), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(
            sha1_hex(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq"),
            "84983e441c3bd26ebaae4aa1f95129e5e54670f1"
        );
    }

    #[test]
    fn sid_authorization_formats_like_the_web_client() {
        let plain = sid_authorization("SAPISIDHASH", "abc", 1700000000, None);
        let expected = sha1_hex(b"1700000000 abc https://music.youtube.com");
        assert_eq!(plain, format!("SAPISIDHASH 1700000000_{expected}"));
        let bound = sid_authorization("SAPISID3PHASH", "abc", 1700000000, Some("42"));
        let expected = sha1_hex(b"42 1700000000 abc https://music.youtube.com");
        assert_eq!(bound, format!("SAPISID3PHASH 1700000000_{expected}_u"));
    }

    #[test]
    fn parses_brand_account_from_ytcfg() {
        let html = r#"<script>ytcfg.set({"INNERTUBE_CLIENT_VERSION":"1.20261118.01.00","LOGGED_IN":true,
            "SESSION_INDEX":"1","DATASYNC_ID":"111222333444555666777||999888777666","DELEGATED_SESSION_ID":null});</script>"#;
        let id = parse_ytcfg(html);
        assert_eq!(id.client_version.as_deref(), Some("1.20261118.01.00"));
        assert_eq!(id.logged_in, Some(true));
        assert_eq!(id.session_index.as_deref(), Some("1"));
        assert_eq!(id.delegated_session_id.as_deref(), Some("111222333444555666777"));
        assert_eq!(id.user_session_id.as_deref(), Some("999888777666"));
    }

    #[test]
    fn parses_main_account_from_ytcfg() {
        let html = r#"ytcfg.set({"SESSION_INDEX":0,"DATASYNC_ID":"999888777666||","INNERTUBE_CLIENT_VERSION":"1.2"})"#;
        let id = parse_ytcfg(html);
        assert_eq!(id.session_index.as_deref(), Some("0"));
        assert_eq!(id.delegated_session_id, None);
        assert_eq!(id.user_session_id.as_deref(), Some("999888777666"));
        assert_eq!(id.logged_in, None);
    }

    #[test]
    fn brand_account_context_and_headers() {
        let id = Identity {
            session_index: Some("1".into()),
            delegated_session_id: Some("111222333444555666777".into()),
            user_session_id: Some("42".into()),
            ..Identity::default()
        };
        let tok = AccountToken::new("__Secure-3PAPISID=x; __Secure-1PAPISID=y; SID=z", &id, "1.2".into()).unwrap();
        let authed = Authed::new(tok.clone());
        let ctx = authed.context();
        assert_eq!(ctx["user"]["onBehalfOfUser"], "111222333444555666777");
        assert_eq!(ctx["client"]["clientName"], "WEB_REMIX");
        let headers: Vec<(String, String)> = tok
            .headers()
            .unwrap()
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.into_owned()))
            .collect();
        let get = |name: &str| headers.iter().find(|(k, _)| k == name).map(|(_, v)| v.clone());
        assert_eq!(get("X-Goog-AuthUser").as_deref(), Some("1"));
        assert_eq!(get("X-Goog-PageId").as_deref(), Some("111222333444555666777"));
        let auth = get("Authorization").unwrap();
        assert!(auth.starts_with("SAPISIDHASH "));
        assert!(auth.contains(" SAPISID1PHASH ") && auth.contains(" SAPISID3PHASH "));
        assert!(auth.ends_with("_u"));

        let other = tok.for_account("2", None);
        assert_eq!(other.auth_user(), "2");
        assert_eq!(other.page_id(), None);
        assert!(!other.has_user_session());
        assert!(Authed::new(other).context()["user"].get("onBehalfOfUser").is_none());
    }

    #[test]
    fn api_error_bodies_become_errors() {
        assert!(check_api_error(r#"{"contents":{}}"#).is_ok());
        let err = check_api_error(r#"{"error":{"code":401,"message":"Login required"}}"#).unwrap_err();
        assert!(err.to_string().contains("401"));
    }
}

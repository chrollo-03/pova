//! YouTube Music actor (ytmapi-rs). Every request runs as its own task and
//! reports back through [`YtmEvent`]s.

use std::path::PathBuf;

use futures::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use ytmapi_rs::YtMusic;
use ytmapi_rs::auth::noauth::NoAuthToken;
use ytmapi_rs::common::{AlbumID, ArtistChannelID, LikeStatus, PlaylistID, Thumbnail, VideoID, YoutubeID};
use ytmapi_rs::parse::{HistoryItem, PlaylistItem, SearchResultPlaylist, SearchResultVideo};
use ytmapi_rs::query::GetPlaylistTracksQuery;

use crate::config::AccountChoice;
use crate::innertube::Authed;
use crate::model::{AlbumRef, ArtistRef, Item, PlaylistRef, Track, parse_duration};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum SearchFilter {
    #[default]
    Songs,
    Videos,
    Albums,
    Artists,
    Playlists,
}

impl SearchFilter {
    pub const ALL: [SearchFilter; 5] = [
        SearchFilter::Songs,
        SearchFilter::Videos,
        SearchFilter::Albums,
        SearchFilter::Artists,
        SearchFilter::Playlists,
    ];
    pub fn label(self) -> &'static str {
        match self {
            SearchFilter::Songs => "Songs",
            SearchFilter::Videos => "Videos",
            SearchFilter::Albums => "Albums",
            SearchFilter::Artists => "Artists",
            SearchFilter::Playlists => "Playlists",
        }
    }
}

/// Something that can be loaded into a list view.
#[derive(Clone, Debug, PartialEq)]
pub enum View {
    Search { filter: SearchFilter, query: String },
    Album(String),
    Artist(String),
    Playlist(String),
    LibraryPlaylists,
    LikedSongs,
    History,
}

#[derive(Debug)]
pub enum YtmReq {
    Load {
        token: u64,
        view: View,
    },
    Radio {
        seed: String,
    },
    Rate {
        video_id: String,
        liked: bool,
    },
    /// Thumbs-down: YouTube plays it less and steers radio away from it.
    Dislike {
        video_id: String,
    },
    Lyrics {
        video_id: String,
    },
    /// Autocomplete suggestions for a partially typed query.
    Suggest {
        query: String,
    },
    AddToPlaylist {
        playlist_id: String,
        playlist_title: String,
        video_id: String,
        title: String,
    },
    /// New private playlist containing one song.
    CreatePlaylist {
        name: String,
        video_id: String,
        title: String,
    },
    RemoveFromPlaylist {
        playlist_id: String,
        video_id: String,
        title: String,
    },
    DeletePlaylist {
        playlist_id: String,
        title: String,
    },
    /// Import YouTube Music cookies from a specific browser/profile spec.
    ImportBrowser {
        spec: String,
        label: String,
    },
    /// Quietly check whether the user has finished signing in for `spec`.
    PollSignIn {
        spec: String,
        label: String,
    },
    /// Open `https://music.youtube.com` in the user's browser.
    OpenSignInLink {
        spec: Option<String>,
    },
    /// Save a manually pasted Cookie header and sign in.
    SaveCookie {
        raw: String,
    },
    /// Remove the saved cookie and drop back to signed-out mode.
    SignOut,
    /// Explicitly fetch the user's YouTube accounts / Brand channels for the account picker.
    ListAccounts,
    /// Switch the signed-in client to an explicitly chosen Google account slot / Brand Account.
    SwitchAccount {
        auth_user: String,
        page_id: Option<String>,
        name: String,
    },
}

/// One YouTube account or Brand channel shown in the account switcher.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AccountInfo {
    pub name: String,
    pub subtitle: String,
    pub auth_user: String,
    pub page_id: Option<String>,
    pub active: bool,
}

/// One autocomplete entry.
#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    pub text: String,
    /// From the signed-in user's own search history.
    pub history: bool,
}

#[derive(Debug)]
pub enum YtmEvent {
    Suggestions {
        query: String,
        items: Vec<Suggestion>,
    },
    Auth {
        logged_in: bool,
        message: Option<String>,
    },
    /// The chosen browser profile isn't signed in to YouTube Music yet; we opened
    /// `https://music.youtube.com` and will watch `spec` while the user signs in.
    AuthNeedsBrowser {
        spec: String,
        label: String,
        browser_opened: bool,
    },
    /// A background poll while waiting for browser sign-in didn't find a login yet.
    AuthPollPending,
    Loaded {
        token: u64,
        title: String,
        items: Vec<Item>,
    },
    Radio {
        seed: String,
        tracks: Vec<Track>,
    },
    Rated {
        video_id: String,
        liked: bool,
    },
    Disliked {
        video_id: String,
    },
    Lyrics {
        video_id: String,
        text: Option<String>,
    },
    Failed {
        token: Option<u64>,
        message: String,
    },
    /// A playlist edit worked. `removed` = (playlist id, video id).
    PlaylistEdited {
        message: String,
        removed: Option<(String, String)>,
    },
    PlaylistDeleted {
        playlist_id: String,
        title: String,
    },
    /// Result of an explicit `ListAccounts` request for the account switcher overlay.
    Accounts {
        items: Vec<AccountInfo>,
    },
    /// Active YouTube account / Brand Account changed.
    AccountSwitched {
        auth_user: String,
        page_id: Option<String>,
        name: String,
    },
}

#[derive(Clone)]
enum Client {
    Anon(YtMusic<NoAuthToken>),
    Auth(Box<Authed>),
}

/// Run `$body` with `$yt` bound to whichever client variant we have.
macro_rules! with_yt {
    ($client:expr, $yt:ident => $body:expr) => {
        match $client {
            Client::Anon($yt) => $body,
            Client::Auth(authed) => {
                let $yt = &authed.yt;
                $body
            }
        }
    };
}

/// Max continuation pages fetched for long playlists (~100 tracks each).
const MAX_PAGES: usize = 20;

/// Shown when YouTube Music rejects the saved cookie.
pub const SIGNED_OUT: &str = "YouTube Music signed you out: the saved login cookie has expired. \
Open the Library tab (b) to sign in again, or run `pova login`.";

enum AuthOutcome {
    SignedIn {
        client: Authed,
    },
    NeedsBrowser {
        spec: String,
        label: String,
        browser_opened: bool,
    },
    PollPending,
    Error {
        message: String,
    },
}

pub(crate) enum CookieCheckError {
    NotSignedIn,
    Other(String),
}

fn browse_body(browse_id: &str) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([("browseId".to_string(), Value::from(browse_id))])
}

/// Build an authenticated client in memory from `cookie` (applying `saved` if
/// the user explicitly picked an account/Brand channel earlier, else using the
/// browser's active identity from `music.youtube.com`'s `ytcfg`) and verify
/// that YouTube Music treats it as signed in (`logged_in != "0"`).
async fn build_authed(cookie: &str, saved: Option<&AccountChoice>) -> Result<Authed, CookieCheckError> {
    let (token, id) = crate::innertube::bootstrap(cookie)
        .await
        .map_err(|e| CookieCheckError::Other(explain(&e.to_string()).0))?;
    let token = match saved {
        Some(choice) => token.for_account(&choice.auth_user, choice.page_id.as_deref()),
        None => token,
    };
    tracing::debug!(
        "YouTube session: page_logged_in={:?}, auth_user={}, brand_account={}, user_session={}",
        id.logged_in,
        token.auth_user(),
        token.page_id().is_some(),
        token.has_user_session()
    );
    let mut authed = Authed::new(token.clone());
    let mut state = login_state(&authed).await;
    if state != Some(true) && token.has_user_session() {
        // Fall back to classic session-less SAPISIDHASH if YouTube rejected the
        // session-bound hash for this identity.
        let plain = Authed::new(token.without_user_session());
        let plain_state = login_state(&plain).await;
        if plain_state == Some(true) {
            authed = plain;
            state = plain_state;
        }
    }
    if state == Some(false) {
        return Err(CookieCheckError::NotSignedIn);
    }
    Ok(authed)
}

/// Build an authenticated client in memory from `cookie` and verify that
/// YouTube Music accepts it as signed in (`logged_in != "0"`).
pub(crate) async fn verify_cookie(cookie: &str) -> Result<Authed, CookieCheckError> {
    build_authed(cookie, None).await
}

/// Extract cookies from a specific browser/profile, verify with YouTube Music
/// in memory, and only write `cookie_file` once YouTube confirms sign-in.
async fn verify_browser_login(spec: String, label: String, cookie_file: PathBuf, is_poll: bool) -> AuthOutcome {
    use crate::auth::{ExtractError, extract_from_browser, open_in_browser, write_cookie_file};
    let cookie = match extract_from_browser(&spec).await {
        Ok(c) => c,
        Err(ExtractError::NotSignedIn { .. }) => {
            return if is_poll {
                AuthOutcome::PollPending
            } else {
                let browser_opened = open_in_browser(&spec);
                AuthOutcome::NeedsBrowser {
                    spec,
                    label,
                    browser_opened,
                }
            };
        }
        Err(ExtractError::Other(message)) => return AuthOutcome::Error { message },
    };

    match verify_cookie(&cookie).await {
        Ok(yt) => match write_cookie_file(&cookie_file, &cookie) {
            Ok(()) => AuthOutcome::SignedIn { client: yt },
            Err(e) => AuthOutcome::Error {
                message: format!("Signed in, but couldn't save the cookie file: {e}"),
            },
        },
        Err(CookieCheckError::NotSignedIn) => {
            if is_poll {
                AuthOutcome::PollPending
            } else {
                let browser_opened = open_in_browser(&spec);
                AuthOutcome::NeedsBrowser {
                    spec,
                    label,
                    browser_opened,
                }
            }
        }
        Err(CookieCheckError::Other(msg)) => AuthOutcome::Error {
            message: format!("Couldn't sign in with {label} ({msg}). Check your internet connection and try again."),
        },
    }
}

pub async fn run(
    cookie_file: PathBuf,
    mut saved_account: Option<AccountChoice>,
    mut reqs: UnboundedReceiver<YtmReq>,
    events: UnboundedSender<YtmEvent>,
) {
    let mut cookie_stamp = file_stamp(&cookie_file);
    let mut client = connect(&cookie_file, saved_account.as_ref(), &events).await;
    // Requests run as separate tasks; one that finds the login rejected
    // reports it here so the client can drop back to signed-out.
    let (expired_tx, mut expired_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    // Interactive sign-in tasks (browser cookie extraction / paste) report here
    // so yt-dlp never blocks search or radio requests.
    let (auth_done_tx, mut auth_done_rx) = tokio::sync::mpsc::unbounded_channel::<AuthOutcome>();
    let mut watch = tokio::time::interval(std::time::Duration::from_secs(2));
    watch.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            req = reqs.recv() => {
                let Some(req) = req else { break };
                match req {
                    YtmReq::ImportBrowser { spec, label } => {
                        let cookie_file = cookie_file.clone();
                        let auth_done_tx = auth_done_tx.clone();
                        tokio::spawn(async move {
                            let outcome = verify_browser_login(spec, label, cookie_file, false).await;
                            let _ = auth_done_tx.send(outcome);
                        });
                        continue;
                    }
                    YtmReq::PollSignIn { spec, label } => {
                        let cookie_file = cookie_file.clone();
                        let auth_done_tx = auth_done_tx.clone();
                        tokio::spawn(async move {
                            let outcome = verify_browser_login(spec, label, cookie_file, true).await;
                            let _ = auth_done_tx.send(outcome);
                        });
                        continue;
                    }
                    YtmReq::OpenSignInLink { spec } => {
                        crate::auth::open_in_browser(spec.as_deref().unwrap_or(""));
                        continue;
                    }
                    YtmReq::SaveCookie { raw } => {
                        let cookie_file = cookie_file.clone();
                        let auth_done_tx = auth_done_tx.clone();
                        tokio::spawn(async move {
                            let outcome = match crate::auth::normalize_pasted_cookie(&raw) {
                                Err(e) => AuthOutcome::Error { message: e.to_string() },
                                Ok(cookie) => match verify_cookie(&cookie).await {
                                    Ok(yt) => match crate::auth::write_cookie_file(&cookie_file, &cookie) {
                                        Ok(()) => AuthOutcome::SignedIn { client: yt },
                                        Err(e) => AuthOutcome::Error {
                                            message: format!("Couldn't save cookie file: {e}"),
                                        },
                                    },
                                    Err(CookieCheckError::NotSignedIn) => AuthOutcome::Error {
                                        message: "That cookie isn't signed in to YouTube Music (or has expired). \
                                                  Sign in at music.youtube.com and copy a fresh Cookie header."
                                            .into(),
                                    },
                                    Err(CookieCheckError::Other(msg)) => AuthOutcome::Error {
                                        message: format!("Couldn't sign in with the pasted cookie ({msg})."),
                                    },
                                },
                            };
                            let _ = auth_done_tx.send(outcome);
                        });
                        continue;
                    }
                    YtmReq::SignOut => {
                        match crate::auth::remove_cookie_file(&cookie_file) {
                            Ok(_) => {
                                cookie_stamp = file_stamp(&cookie_file);
                                saved_account = None;
                                tracing::info!("signed out by user");
                                let _ = events.send(YtmEvent::Auth {
                                    logged_in: false,
                                    message: None,
                                });
                                if matches!(client, Some(Client::Auth(_))) {
                                    client = YtMusic::new_unauthenticated().await.ok().map(Client::Anon);
                                }
                            }
                            Err(e) => {
                                let _ = events.send(YtmEvent::Failed {
                                    token: None,
                                    message: format!("Couldn't sign out: {e}"),
                                });
                            }
                        }
                        continue;
                    }
                    YtmReq::ListAccounts => {
                        let Some(Client::Auth(authed)) = client.clone() else {
                            let _ = events.send(YtmEvent::Accounts { items: Vec::new() });
                            continue;
                        };
                        let events = events.clone();
                        tokio::spawn(async move {
                            let items = list_accounts(&authed).await;
                            let _ = events.send(YtmEvent::Accounts { items });
                        });
                        continue;
                    }
                    YtmReq::SwitchAccount {
                        auth_user,
                        page_id,
                        name,
                    } => {
                        let Some(Client::Auth(authed)) = client.as_ref() else {
                            let _ = events.send(YtmEvent::Failed {
                                token: None,
                                message: "Sign in first before switching accounts.".into(),
                            });
                            continue;
                        };
                        let switched = Authed::new(authed.token().for_account(&auth_user, page_id.as_deref()));
                        tracing::info!(
                            "switched YouTube account: auth_user={auth_user}, brand_account={}",
                            page_id.is_some()
                        );
                        saved_account = Some(AccountChoice {
                            auth_user: auth_user.clone(),
                            page_id: page_id.clone(),
                            name: Some(name.clone()),
                        });
                        client = Some(Client::Auth(Box::new(switched)));
                        let _ = events.send(YtmEvent::AccountSwitched {
                            auth_user,
                            page_id,
                            name,
                        });
                        continue;
                    }
                    _ => {}
                }
                // If startup happened offline, reconnect lazily once the network is back.
                if client.is_none() {
                    client = connect(&cookie_file, saved_account.as_ref(), &events).await;
                }
                let Some(c) = client.clone() else {
                    let _ = events.send(offline_event(req));
                    continue;
                };
                let events = events.clone();
                let expired_tx = expired_tx.clone();
                tokio::spawn(async move {
                    let (ev, auth_rejected) = handle(&c, req).await;
                    let _ = events.send(ev);
                    if auth_rejected && matches!(c, Client::Auth(_)) {
                        let _ = expired_tx.send(());
                    }
                });
            }
            Some(outcome) = auth_done_rx.recv() => {
                match outcome {
                    AuthOutcome::SignedIn { client: yt } => {
                        cookie_stamp = file_stamp(&cookie_file);
                        saved_account = None;
                        client = Some(Client::Auth(Box::new(yt)));
                        let _ = events.send(YtmEvent::Auth {
                            logged_in: true,
                            message: None,
                        });
                    }
                    AuthOutcome::NeedsBrowser {
                        spec,
                        label,
                        browser_opened,
                    } => {
                        let _ = events.send(YtmEvent::AuthNeedsBrowser {
                            spec,
                            label,
                            browser_opened,
                        });
                    }
                    AuthOutcome::PollPending => {
                        let _ = events.send(YtmEvent::AuthPollPending);
                    }
                    AuthOutcome::Error { message } => {
                        tracing::warn!("sign-in failed: {message}");
                        let _ = events.send(YtmEvent::Auth {
                            logged_in: matches!(client, Some(Client::Auth(_))),
                            message: Some(message),
                        });
                    }
                }
            }
            Some(()) = expired_rx.recv() => {
                if matches!(client, Some(Client::Auth(_))) {
                    tracing::warn!("login cookie rejected; continuing signed out");
                    let _ = events.send(YtmEvent::Auth {
                        logged_in: false,
                        message: Some(SIGNED_OUT.into()),
                    });
                    if let Ok(yt) = YtMusic::new_unauthenticated().await {
                        client = Some(Client::Anon(yt));
                    }
                }
            }
            _ = watch.tick() => {
                // A freshly imported cookie signs pova back in without a restart;
                // removing the cookie file drops back to signed-out.
                let stamp = file_stamp(&cookie_file);
                if stamp != cookie_stamp {
                    cookie_stamp = stamp;
                    if stamp.is_some() {
                        tracing::info!("cookie file changed; reconnecting");
                        if let Some(c) = connect(&cookie_file, saved_account.as_ref(), &events).await {
                            client = Some(c);
                        }
                    } else if matches!(client, Some(Client::Auth(_))) {
                        saved_account = None;
                        tracing::info!("cookie file removed; continuing signed out");
                        let _ = events.send(YtmEvent::Auth {
                            logged_in: false,
                            message: None,
                        });
                        client = YtMusic::new_unauthenticated().await.ok().map(Client::Anon);
                    }
                }
            }
        }
    }
}

fn offline_event(req: YtmReq) -> YtmEvent {
    let message = "couldn't reach YouTube Music. Check your internet connection and try again.".to_string();
    match req {
        YtmReq::Load { token, .. } => YtmEvent::Failed {
            token: Some(token),
            message,
        },
        YtmReq::Suggest { query } => YtmEvent::Suggestions {
            query,
            items: Vec::new(),
        },
        YtmReq::Lyrics { video_id } => YtmEvent::Lyrics { video_id, text: None },
        _ => YtmEvent::Failed { token: None, message },
    }
}

/// Whether YouTube treats this client as signed in, from the `logged_in`
/// flag it puts in every response. None if the check itself failed.
async fn login_state(yt: &Authed) -> Option<bool> {
    let raw = yt
        .post_raw("browse", browse_body("FEmusic_liked_playlists"), Vec::new())
        .await;
    match raw {
        Ok(raw) => {
            let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
            let state = find_logged_in(&json);
            tracing::debug!("login check: logged_in = {state:?}");
            state
        }
        Err(e) => {
            tracing::debug!("login check failed: {e}");
            let (_, rejected) = explain(&e.to_string());
            if rejected { Some(false) } else { None }
        }
    }
}

/// Finds `{"key": "logged_in", "value": "0" | "1"}` anywhere in a response.
fn find_logged_in(v: &serde_json::Value) -> Option<bool> {
    match v {
        serde_json::Value::Object(map) => {
            if map.get("key").and_then(serde_json::Value::as_str) == Some("logged_in") {
                return match map.get("value").and_then(serde_json::Value::as_str) {
                    Some("1") => Some(true),
                    Some("0") => Some(false),
                    _ => None,
                };
            }
            map.values().find_map(find_logged_in)
        }
        serde_json::Value::Array(items) => items.iter().find_map(find_logged_in),
        _ => None,
    }
}

fn file_stamp(path: &std::path::Path) -> Option<std::time::SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

async fn connect(
    cookie_file: &PathBuf,
    saved_account: Option<&AccountChoice>,
    events: &UnboundedSender<YtmEvent>,
) -> Option<Client> {
    if cookie_file.exists() {
        let auth_res = match tokio::fs::read_to_string(cookie_file).await {
            Ok(cookie) => build_authed(cookie.trim(), saved_account).await,
            Err(e) => Err(CookieCheckError::Other(e.to_string())),
        };
        match auth_res {
            Err(CookieCheckError::NotSignedIn) => {
                tracing::warn!("cookie accepted but YouTube reports logged_in=0");
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: Some(SIGNED_OUT.into()),
                });
            }
            Ok(yt) => {
                let _ = events.send(YtmEvent::Auth {
                    logged_in: true,
                    message: None,
                });
                return Some(Client::Auth(Box::new(yt)));
            }
            Err(CookieCheckError::Other(msg)) => {
                tracing::warn!("cookie auth failed: {msg}");
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: Some(format!(
                        "Couldn't sign in with the saved cookie ({msg}). Continuing signed out; \
                         open the Library tab (b) to sign in again, or run `pova login`."
                    )),
                });
            }
        }
    }
    match YtMusic::new_unauthenticated().await {
        Ok(yt) => {
            if !cookie_file.exists() {
                let _ = events.send(YtmEvent::Auth {
                    logged_in: false,
                    message: None,
                });
            }
            Some(Client::Anon(yt))
        }
        Err(e) => {
            tracing::error!("ytm connect failed: {e}");
            let _ = events.send(YtmEvent::Failed {
                token: None,
                message: format!("Couldn't reach YouTube Music: {}", explain(&e.to_string()).0),
            });
            None
        }
    }
}

/// Explicitly list the Google accounts and YouTube Brand channels available in
/// the current browser cookie session. Never called unless the user opens the
/// account switcher.
async fn list_accounts(yt: &Authed) -> Vec<AccountInfo> {
    let cur_user = yt.token().auth_user().to_string();
    let cur_page = yt.token().page_id().map(str::to_string);

    let mut found = query_accounts_list(yt, &cur_user).await;
    // If currently on a Brand Account or secondary auth_user and the list came
    // back empty, query slot 0 without page_id to see every account/channel.
    if found.is_empty() && (cur_user != "0" || cur_page.is_some()) {
        let root = Authed::new(yt.token().for_account("0", None));
        found = query_accounts_list(&root, "0").await;
    }

    for acct in &mut found {
        acct.active = acct.auth_user == cur_user && acct.page_id == cur_page;
    }

    // Ensure the currently active identity is always listed even if YouTube
    // returned an empty switcher response.
    if !found.iter().any(|a| a.active) {
        let label = match &cur_page {
            Some(_) => format!("Active Brand Account (slot {cur_user})"),
            None => format!("Google account #{cur_user}"),
        };
        found.insert(
            0,
            AccountInfo {
                name: label,
                subtitle: if cur_page.is_some() {
                    "Brand Account".into()
                } else {
                    format!("authuser={cur_user}")
                },
                auth_user: cur_user.clone(),
                page_id: cur_page.clone(),
                active: true,
            },
        );
    }

    // If YouTube only reported a single account, offer manual Google account
    // slots (0, 1, 2) as fallbacks for multi-login browser cookies.
    if found.len() <= 1 {
        for slot in ["0", "1", "2"] {
            if !found.iter().any(|a| a.auth_user == slot && a.page_id.is_none()) {
                found.push(AccountInfo {
                    name: format!("Google account #{slot}"),
                    subtitle: format!("authuser={slot}"),
                    auth_user: slot.into(),
                    page_id: None,
                    active: cur_user == slot && cur_page.is_none(),
                });
            }
        }
    }
    found
}

async fn query_accounts_list(yt: &Authed, default_auth_user: &str) -> Vec<AccountInfo> {
    let full = serde_json::Map::from_iter([
        (
            "requestType".to_string(),
            Value::from("ACCOUNTS_LIST_REQUEST_TYPE_CHANNEL_SWITCHER"),
        ),
        ("callCircumstance".to_string(), Value::from("SWITCHING_USERS_FULL")),
    ]);
    let plain = serde_json::Map::from_iter([("callCircumstance".to_string(), Value::from("SWITCHING_USERS_FULL"))]);
    for body in [full, plain] {
        match yt.post_raw("account/accounts_list", body, Vec::new()).await {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(json) => {
                    let parsed = parse_accounts_list(&json, default_auth_user);
                    if !parsed.is_empty() {
                        tracing::debug!("accounts_list: found {} account(s)", parsed.len());
                        return parsed;
                    }
                }
                Err(e) => tracing::debug!("accounts_list JSON parse failed: {e}"),
            },
            Err(e) => tracing::debug!("accounts_list query failed: {}", explain(&e.to_string()).0),
        }
    }
    Vec::new()
}

fn json_text(v: &Value) -> Option<String> {
    if let Some(s) = v.get("simpleText").and_then(Value::as_str) {
        let s = s.trim();
        if !s.is_empty() {
            return Some(s.to_string());
        }
    }
    if let Some(runs) = v.get("runs").and_then(Value::as_array) {
        let joined: String = runs
            .iter()
            .filter_map(|r| r.get("text").and_then(Value::as_str))
            .collect();
        let trimmed = joined.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    None
}

fn parse_accounts_list(v: &Value, default_auth_user: &str) -> Vec<AccountInfo> {
    let mut out = Vec::new();
    collect_accounts(v, default_auth_user, &mut out);
    out
}

fn collect_accounts(v: &Value, default_auth_user: &str, out: &mut Vec<AccountInfo>) {
    match v {
        Value::Object(map) => {
            if let Some(item) = map.get("accountItemRenderer") {
                if let Some(acct) = parse_account_item(item, default_auth_user)
                    && !out
                        .iter()
                        .any(|e| e.auth_user == acct.auth_user && e.page_id == acct.page_id)
                {
                    out.push(acct);
                }
                return;
            }
            for child in map.values() {
                collect_accounts(child, default_auth_user, out);
            }
        }
        Value::Array(items) => {
            for child in items {
                collect_accounts(child, default_auth_user, out);
            }
        }
        _ => {}
    }
}

fn parse_account_item(item: &Value, default_auth_user: &str) -> Option<AccountInfo> {
    let tokens = item
        .pointer("/serviceEndpoint/selectActiveIdentityEndpoint/supportedTokens")
        .or_else(|| item.pointer("/tokens"))
        .and_then(Value::as_array)?;
    let mut page_id: Option<String> = None;
    let mut auth_user: Option<String> = None;
    for tok in tokens {
        if let Some(pid) = tok
            .pointer("/pageIdToken/pageId")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
        {
            page_id = Some(pid.to_string());
        }
        if page_id.is_none()
            && let Some(ds) = tok.pointer("/datasyncIdToken/datasyncIdToken").and_then(Value::as_str)
            && let Some((head, tail)) = ds.split_once("||")
            && !head.is_empty()
            && !tail.is_empty()
            && head.chars().all(|c| c.is_ascii_digit())
        {
            page_id = Some(head.to_string());
        }
        if let Some(url) = tok.pointer("/accountSigninToken/signinUrl").and_then(Value::as_str)
            && let Some(u) = extract_authuser_param(url)
        {
            auth_user = Some(u);
        }
    }
    if page_id.is_none() && auth_user.is_none() {
        return None;
    }
    let auth_user = auth_user.unwrap_or_else(|| default_auth_user.to_string());
    let name = item.get("accountName").and_then(json_text).unwrap_or_else(|| {
        if page_id.is_some() {
            "Brand Account".into()
        } else {
            format!("Google account #{auth_user}")
        }
    });
    let handle = item
        .get("channelHandle")
        .and_then(json_text)
        .or_else(|| item.get("accountByline").and_then(json_text));
    let subtitle = match (handle, page_id.is_some()) {
        (Some(h), true) => format!("{h} · Brand Account"),
        (Some(h), false) => h,
        (None, true) => "Brand Account".into(),
        (None, false) => format!("authuser={auth_user}"),
    };
    let active = item.get("isSelected").and_then(Value::as_bool).unwrap_or(false);
    Some(AccountInfo {
        name,
        subtitle,
        auth_user,
        page_id,
        active,
    })
}

fn extract_authuser_param(url: &str) -> Option<String> {
    let decoded = url
        .replace("%3D", "=")
        .replace("%3d", "=")
        .replace("%26", "&")
        .replace("%3F", "?");
    decoded
        .split(['?', '&'])
        .find_map(|kv| kv.strip_prefix("authuser="))
        .map(|v| v.chars().take_while(char::is_ascii_digit).collect::<String>())
        .filter(|v| !v.is_empty())
}

/// Run one request. The flag is true when YouTube Music rejected the login.
async fn handle(client: &Client, req: YtmReq) -> (YtmEvent, bool) {
    let mut rejected = false;
    let mut fail = |context: &str, e: &dyn std::fmt::Display| {
        let (text, auth) = explain(&format!("{e:#}"));
        rejected |= auth;
        if context.is_empty() {
            text
        } else {
            format!("{context}: {text}")
        }
    };
    let ev = match req {
        YtmReq::Load { token, view } => match load(client, &view).await {
            Ok((title, items)) => YtmEvent::Loaded { token, title, items },
            Err(e) => {
                tracing::warn!("load {view:?}: {e:#}");
                YtmEvent::Failed {
                    token: Some(token),
                    message: fail("", &e),
                }
            }
        },
        YtmReq::Radio { seed } => {
            let res =
                with_yt!(client, yt => yt.get_watch_playlist_from_video_id(VideoID::from_raw(seed.clone())).await);
            match res {
                Ok(tracks) => YtmEvent::Radio {
                    tracks: tracks
                        .into_iter()
                        .filter(|t| t.video_id.get_raw() != seed)
                        .map(|t| Track {
                            video_id: t.video_id.get_raw().to_string(),
                            title: t.title,
                            artists: t.author,
                            album: None,
                            album_id: None,
                            duration: parse_duration(&t.duration),
                            thumbnail: best_thumb(&t.thumbnails),
                            liked: None,
                        })
                        .collect(),
                    seed,
                },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail("Couldn't start radio", &e),
                },
            }
        }
        YtmReq::Rate { video_id, liked } => {
            let Client::Auth(yt) = client else {
                let ev = YtmEvent::Failed {
                    token: None,
                    message: "Liking songs needs you to be signed in. Open the Library tab (b) to sign in, \
                              or run `pova login`."
                        .into(),
                };
                return (ev, false);
            };
            let status = if liked {
                LikeStatus::Liked
            } else {
                LikeStatus::Indifferent
            };
            let query = ytmapi_rs::query::RateSongQuery::new(VideoID::from_raw(video_id.clone()), status);
            match yt.query(&query).await {
                Ok(()) => YtmEvent::Rated { video_id, liked },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail(
                        if liked {
                            "Couldn't like the song"
                        } else {
                            "Couldn't remove the like"
                        },
                        &e,
                    ),
                },
            }
        }
        YtmReq::Dislike { video_id } => {
            let Client::Auth(yt) = client else {
                let ev = YtmEvent::Failed {
                    token: None,
                    message: "Disliking songs needs you to be signed in. Open the Library tab (b) to sign in, \
                              or run `pova login`."
                        .into(),
                };
                return (ev, false);
            };
            let query = ytmapi_rs::query::RateSongQuery::new(VideoID::from_raw(video_id.clone()), LikeStatus::Disliked);
            match yt.query(&query).await {
                Ok(()) => YtmEvent::Disliked { video_id },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail("Couldn't dislike the song", &e),
                },
            }
        }
        YtmReq::Lyrics { video_id } => {
            let text = with_yt!(client, yt => async {
                let id = yt.get_lyrics_id(VideoID::from_raw(video_id.clone())).await.ok()?;
                yt.get_lyrics(id).await.ok()
            }.await)
            .map(|l| l.lyrics)
            .filter(|l| !l.trim().is_empty());
            YtmEvent::Lyrics { video_id, text }
        }
        YtmReq::Suggest { query } => {
            let items = match with_yt!(client, yt => yt.get_search_suggestions(query.as_str()).await) {
                Ok(list) => list
                    .into_iter()
                    .map(|s| Suggestion {
                        text: s.get_text(),
                        history: s.suggestion_type == ytmapi_rs::common::SuggestionType::History,
                    })
                    .collect(),
                Err(e) => {
                    tracing::debug!("suggestions for {query:?}: {e}");
                    Vec::new()
                }
            };
            YtmEvent::Suggestions { query, items }
        }
        YtmReq::AddToPlaylist {
            playlist_id,
            playlist_title,
            video_id,
            title,
        } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => {
                use ytmapi_rs::query::AddPlaylistItemsQuery;
                use ytmapi_rs::query::playlist::DuplicateHandlingMode;
                let query = AddPlaylistItemsQuery::new_from_videos(
                    PlaylistID::from_raw(bare_playlist_id(&playlist_id)),
                    [VideoID::from_raw(video_id.clone())],
                    DuplicateHandlingMode::default(),
                );
                match yt.query(&query).await {
                    Ok(_) => YtmEvent::PlaylistEdited {
                        message: format!("Added “{title}” to {playlist_title}"),
                        removed: None,
                    },
                    Err(e) => YtmEvent::Failed {
                        token: None,
                        message: fail(&format!("Couldn't add “{title}” to {playlist_title}"), &e),
                    },
                }
            }
        },
        YtmReq::CreatePlaylist { name, video_id, title } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => {
                use ytmapi_rs::query::playlist::{CreatePlaylistQuery, PrivacyStatus};
                let query = CreatePlaylistQuery::new(&name, None, PrivacyStatus::Private)
                    .with_video_ids([VideoID::from_raw(video_id.clone())]);
                match yt.query(&query).await {
                    Ok(_) => YtmEvent::PlaylistEdited {
                        message: format!("Created the private playlist {name} with “{title}”"),
                        removed: None,
                    },
                    Err(e) => YtmEvent::Failed {
                        token: None,
                        message: fail(&format!("Couldn't create the playlist {name}"), &e),
                    },
                }
            }
        },
        YtmReq::DeletePlaylist { playlist_id, title } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => {
                use ytmapi_rs::query::DeletePlaylistQuery;
                let query = DeletePlaylistQuery::new(PlaylistID::from_raw(bare_playlist_id(&playlist_id)));
                match yt.query(&query).await {
                    Ok(()) => YtmEvent::PlaylistDeleted { playlist_id, title },
                    Err(e) => YtmEvent::Failed {
                        token: None,
                        message: fail(
                            &format!("Couldn't delete {title} (you can only delete playlists you made)"),
                            &e,
                        ),
                    },
                }
            }
        },
        YtmReq::RemoveFromPlaylist {
            playlist_id,
            video_id,
            title,
        } => match need_auth(client) {
            Err(e) => YtmEvent::Failed {
                token: None,
                message: e.to_string(),
            },
            Ok(yt) => match remove_from_playlist(yt, &playlist_id, &video_id).await {
                Ok(true) => YtmEvent::PlaylistEdited {
                    message: format!("Removed “{title}” from the playlist (p adds it back)"),
                    removed: Some((playlist_id, video_id)),
                },
                Ok(false) => YtmEvent::Failed {
                    token: None,
                    message: format!(
                        "Couldn't find “{title}” in the first page of this playlist (pova only \
                         looks at the first ~100 songs for now). Remove it on music.youtube.com instead."
                    ),
                },
                Err(e) => YtmEvent::Failed {
                    token: None,
                    message: fail(&format!("Couldn't remove “{title}”"), &e),
                },
            },
        },
        YtmReq::ImportBrowser { .. }
        | YtmReq::PollSignIn { .. }
        | YtmReq::OpenSignInLink { .. }
        | YtmReq::SaveCookie { .. }
        | YtmReq::SignOut
        | YtmReq::ListAccounts
        | YtmReq::SwitchAccount { .. } => {
            unreachable!("handled directly in ytm::run")
        }
    };
    (ev, rejected)
}

/// Turn a ytmapi-rs or network error into a sentence a person can act on.
/// The flag is true when YouTube Music rejected the login (expired cookie).
pub fn explain(raw: &str) -> (String, bool) {
    let lower = raw.to_lowercase();
    if lower.contains("must be signed in")
        || lower.contains("error code 401")
        || lower.contains("not logged in")
        || lower.contains("login required")
    {
        return (SIGNED_OUT.to_string(), true);
    }
    if lower.contains("needs you to be signed in") {
        return (raw.to_string(), false);
    }
    if lower.contains("error sending request")
        || lower.contains("dns error")
        || lower.contains("failed to lookup")
        || lower.contains("connection refused")
        || lower.contains("connection reset")
        || lower.contains("timed out")
        || lower.contains("network is unreachable")
    {
        return (
            "couldn't reach YouTube Music. Check your internet connection and try again.".into(),
            false,
        );
    }
    // "Http error code 429 recieved in response. Message: <...>."
    if let Some(rest) = raw.split("Http error code ").nth(1) {
        let code: String = rest.chars().take_while(char::is_ascii_digit).collect();
        let message = rest
            .split_once('<')
            .and_then(|(_, m)| m.rsplit_once('>'))
            .map(|(m, _)| m.trim().trim_end_matches('.'))
            .filter(|m| !m.is_empty());
        let hint = match code.as_str() {
            "429" => " YouTube is rate-limiting requests; wait a minute and try again.",
            "403" => {
                " YouTube refused the request; signing in again from the Library tab (b) or `pova login` may help."
            }
            c if c.starts_with('5') => " YouTube Music is having trouble; try again in a moment.",
            _ => "",
        };
        return match message {
            Some(m) => (format!("YouTube Music answered with error {code}: {m}.{hint}"), false),
            None => (format!("YouTube Music answered with error {code}.{hint}"), false),
        };
    }
    (raw.to_string(), false)
}

/// Playlist ids without the "VL" browse prefix (what the edit endpoints want).
fn bare_playlist_id(id: &str) -> String {
    id.strip_prefix("VL").unwrap_or(id).to_string()
}

/// Removing needs the song's per-playlist "setVideoId", which ytmapi-rs
/// doesn't parse, so read it from the raw playlist page. Ok(false) when the
/// song isn't on the first page.
async fn remove_from_playlist(yt: &Authed, playlist_id: &str, video_id: &str) -> anyhow::Result<bool> {
    use ytmapi_rs::common::SetVideoID;
    use ytmapi_rs::query::RemovePlaylistItemsQuery;
    let raw = yt
        .post_raw(
            "browse",
            browse_body(&format!("VL{}", bare_playlist_id(playlist_id))),
            Vec::new(),
        )
        .await?;
    let json: serde_json::Value = serde_json::from_str(&raw)?;
    let Some(set_id) = find_set_video_id(&json, video_id) else {
        return Ok(false);
    };
    let query = RemovePlaylistItemsQuery::new(
        PlaylistID::from_raw(bare_playlist_id(playlist_id)),
        [SetVideoID::from_raw(set_id)],
    );
    yt.query(&query).await?;
    Ok(true)
}

/// `playlistItemData: {videoId, playlistSetVideoId}` for `video_id`.
fn find_set_video_id(v: &serde_json::Value, video_id: &str) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => {
            if let Some(data) = map.get("playlistItemData")
                && data.get("videoId").and_then(serde_json::Value::as_str) == Some(video_id)
                && let Some(set) = data.get("playlistSetVideoId").and_then(serde_json::Value::as_str)
            {
                return Some(set.to_string());
            }
            map.values().find_map(|x| find_set_video_id(x, video_id))
        }
        serde_json::Value::Array(items) => items.iter().find_map(|x| find_set_video_id(x, video_id)),
        _ => None,
    }
}

fn need_auth(client: &Client) -> anyhow::Result<&Authed> {
    match client {
        Client::Auth(yt) => Ok(yt),
        Client::Anon(_) => anyhow::bail!(
            "Your library needs you to be signed in. Open the Library tab (b) to sign in, or run `pova login`."
        ),
    }
}

async fn load(client: &Client, view: &View) -> anyhow::Result<(String, Vec<Item>)> {
    Ok(match view {
        View::Search { filter, query } => {
            let (items, corrected) = search_corrected(client, *filter, query).await?;
            match corrected {
                Some(c) => (format!("{} · showing results for “{c}”", filter.label()), items),
                None => (format!("{} · {}", filter.label(), query), items),
            }
        }
        View::Album(id) => {
            let album = with_yt!(client, yt => yt.get_album(AlbumID::from_raw(id.clone())).await)?;
            let artists = album
                .artists
                .iter()
                .map(|a| a.name.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            let thumb = best_thumb(&album.thumbnails);
            let items = album
                .tracks
                .into_iter()
                .map(|s| {
                    Item::Track(Track {
                        video_id: s.video_id.get_raw().to_string(),
                        title: s.title,
                        artists: artists.clone(),
                        album: Some(album.title.clone()),
                        album_id: Some(id.clone()),
                        duration: parse_duration(&s.duration),
                        thumbnail: thumb.clone(),
                        liked: like_flag(&s.like_status),
                    })
                })
                .collect();
            (format!("{} — {} ({})", album.title, artists, album.year), items)
        }
        View::Artist(id) => {
            let artist = with_yt!(client, yt => yt.get_artist(ArtistChannelID::from_raw(id.clone())).await)?;
            let mut items = Vec::new();
            let top = artist.top_releases;
            if let Some(songs) = top.songs {
                items.push(Item::Header("Top songs".into()));
                for s in songs.results {
                    items.push(Item::Track(Track {
                        video_id: s.video_id.get_raw().to_string(),
                        title: s.title,
                        artists: join_artists(&s.artists),
                        album: Some(s.album.name.clone()),
                        album_id: Some(s.album.id.get_raw().to_string()),
                        duration: None,
                        thumbnail: None,
                        liked: like_flag(&s.like_status),
                    }));
                }
            }
            for (label, albums) in [("Albums", top.albums), ("Singles & EPs", top.singles)] {
                let Some(albums) = albums else { continue };
                items.push(Item::Header(label.into()));
                for a in albums.results {
                    items.push(Item::Album(AlbumRef {
                        id: a.album_id.get_raw().to_string(),
                        title: a.title,
                        subtitle: a.year,
                    }));
                }
            }
            (artist.name, items)
        }
        View::Playlist(id) => {
            let tracks = playlist_tracks(client, id).await?;
            let title = if id.trim_start_matches("VL") == "LM" {
                "Liked songs"
            } else {
                "Playlist"
            }
            .to_string();
            (title, tracks)
        }
        View::LikedSongs => {
            need_auth(client)?;
            let tracks = playlist_tracks(client, "LM").await?;
            tracing::info!("loaded {} liked songs", tracks.len());
            ("Liked songs".into(), tracks)
        }
        View::LibraryPlaylists => {
            let yt = need_auth(client)?;
            let items = load_library_playlists(yt).await?;
            tracing::info!("loaded {} library playlists", items.len());
            ("Your playlists".into(), items)
        }
        View::History => {
            let yt = need_auth(client)?;
            let periods = yt.query(&ytmapi_rs::query::GetHistoryQuery).await?;
            let mut items = Vec::new();
            for period in periods {
                items.push(Item::Header(period.period_name));
                for it in period.items {
                    match it {
                        HistoryItem::Song(s) => items.push(Item::Track(Track {
                            video_id: s.video_id.get_raw().to_string(),
                            title: s.title,
                            artists: join_artists(&s.artists),
                            album: Some(s.album.name.clone()),
                            album_id: Some(s.album.id.get_raw().to_string()),
                            duration: parse_duration(&s.duration),
                            thumbnail: best_thumb(&s.thumbnails),
                            liked: like_flag(&s.like_status),
                        })),
                        HistoryItem::Video(v) => items.push(Item::Track(Track {
                            video_id: v.video_id.get_raw().to_string(),
                            title: v.title,
                            artists: v.channel_name,
                            album: None,
                            album_id: None,
                            duration: parse_duration(&v.duration),
                            thumbnail: best_thumb(&v.thumbnails),
                            liked: like_flag(&v.like_status),
                        })),
                        _ => {}
                    }
                }
            }
            tracing::info!("loaded {} history items", items.len());
            ("History".into(), items)
        }
    })
}

/// Fetch a browse page and up to `max_pages - 1` continuation pages of the
/// list it contains.
async fn browse_pages(yt: &Authed, browse_id: &str, max_pages: usize) -> anyhow::Result<Vec<Value>> {
    let raw = yt.post_raw("browse", browse_body(browse_id), Vec::new()).await?;
    let first: Value = serde_json::from_str(&raw)?;
    let mut token = find_items_continuation(&first);
    let mut pages = vec![first];
    while let Some(t) = token.take() {
        if pages.len() >= max_pages {
            break;
        }
        let body = serde_json::Map::from_iter([("continuation".to_string(), Value::from(t.as_str()))]);
        let params = vec![
            ("ctoken".to_string(), t.clone()),
            ("continuation".to_string(), t),
            ("type".to_string(), "next".to_string()),
        ];
        match yt.post_raw("browse", body, params).await {
            Ok(raw) => match serde_json::from_str::<Value>(&raw) {
                Ok(page) => {
                    token = find_items_continuation(&page);
                    pages.push(page);
                }
                Err(e) => {
                    tracing::warn!("browse {browse_id}: stopping after {} pages: {e}", pages.len());
                    break;
                }
            },
            Err(e) => {
                tracing::warn!("browse {browse_id}: stopping after {} pages: {e}", pages.len());
                break;
            }
        }
    }
    Ok(pages)
}

fn is_list_item(v: &Value) -> bool {
    v.get("musicResponsiveListItemRenderer").is_some() || v.get("musicTwoRowItemRenderer").is_some()
}

fn find_items_continuation(v: &Value) -> Option<String> {
    match v {
        Value::Array(items) => {
            if items.iter().any(is_list_item)
                && let Some(token) = items.iter().find_map(|i| {
                    i.pointer("/continuationItemRenderer/continuationEndpoint/continuationCommand/token")
                        .and_then(Value::as_str)
                })
            {
                return Some(token.to_string());
            }
            items.iter().find_map(find_items_continuation)
        }
        Value::Object(map) => {
            let has_items = ["contents", "items"]
                .iter()
                .filter_map(|k| map.get(*k).and_then(Value::as_array))
                .any(|a| a.iter().any(is_list_item));
            if has_items
                && let Some(token) = map
                    .get("continuations")
                    .and_then(|c| c.pointer("/0/nextContinuationData/continuation"))
                    .and_then(Value::as_str)
            {
                return Some(token.to_string());
            }
            map.values().find_map(find_items_continuation)
        }
        _ => None,
    }
}

/// Load the user's library playlists.
///
/// Parses `FEmusic_liked_playlists` directly through [`Authed`] so Brand
/// Accounts (`onBehalfOfUser` + `X-Goog-PageId`) work, pagination is followed,
/// and `ytmapi-rs` never drops a user's first playlist.
async fn load_library_playlists(yt: &Authed) -> anyhow::Result<Vec<Item>> {
    let pages = browse_pages(yt, "FEmusic_liked_playlists", 10).await?;
    let mut out: Vec<PlaylistRef> = Vec::new();
    for page in &pages {
        for p in parse_library_playlists_json(page) {
            if !out.iter().any(|e| e.id == p.id) {
                out.push(p);
            }
        }
    }
    if out.is_empty() {
        tracing::info!(
            "library playlists: 0 parsed from {} page(s); logged_in={:?}, renderers={}",
            pages.len(),
            pages.first().and_then(find_logged_in),
            describe_renderers(&pages)
        );
    }
    Ok(out.into_iter().map(Item::Playlist).collect())
}

/// Non-identifying summary of which list renderers a response contained
/// (for the log when something parses as empty).
fn describe_renderers(pages: &[Value]) -> String {
    fn count(v: &Value, counts: &mut std::collections::BTreeMap<&'static str, usize>) {
        const NAMES: [&str; 5] = [
            "musicTwoRowItemRenderer",
            "musicResponsiveListItemRenderer",
            "gridRenderer",
            "musicPlaylistShelfRenderer",
            "musicShelfRenderer",
        ];
        match v {
            Value::Object(map) => {
                for (k, child) in map {
                    if let Some(name) = NAMES.iter().find(|n| **n == k.as_str()) {
                        *counts.entry(name).or_default() += 1;
                    }
                    count(child, counts);
                }
            }
            Value::Array(items) => items.iter().for_each(|c| count(c, counts)),
            _ => {}
        }
    }
    let mut counts = std::collections::BTreeMap::new();
    pages.iter().for_each(|p| count(p, &mut counts));
    if counts.is_empty() {
        return "none".into();
    }
    counts
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn parse_library_playlists_json(v: &serde_json::Value) -> Vec<PlaylistRef> {
    let mut out = Vec::new();
    collect_library_playlists(v, &mut out);
    out
}

fn collect_library_playlists(v: &serde_json::Value, out: &mut Vec<PlaylistRef>) {
    match v {
        serde_json::Value::Object(map) => {
            let parsed = map
                .get("musicTwoRowItemRenderer")
                .map(parse_mtrir_playlist)
                .or_else(|| map.get("musicResponsiveListItemRenderer").map(parse_mrlir_playlist));
            if let Some(parsed) = parsed {
                if let Some(p) = parsed
                    && !out.iter().any(|existing| existing.id == p.id)
                {
                    out.push(p);
                }
                return;
            }
            for child in map.values() {
                collect_library_playlists(child, out);
            }
        }
        serde_json::Value::Array(items) => {
            for child in items {
                collect_library_playlists(child, out);
            }
        }
        _ => {}
    }
}

fn parse_mrlir_playlist(mrlir: &Value) -> Option<PlaylistRef> {
    let browse_id = mrlir
        .pointer("/navigationEndpoint/browseEndpoint/browseId")
        .or_else(|| {
            mrlir.pointer(
                "/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/navigationEndpoint/browseEndpoint/browseId",
            )
        })
        .and_then(Value::as_str)?;
    let bare = browse_id.strip_prefix("VL")?;
    if matches!(bare, "LM" | "SE" | "SS" | "RDPN") {
        return None;
    }
    let title = mrlir
        .pointer("/flexColumns/0/musicResponsiveListItemFlexColumnRenderer/text/runs/0/text")
        .and_then(Value::as_str)?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let subtitle = mrlir
        .pointer("/flexColumns/1/musicResponsiveListItemFlexColumnRenderer/text/runs")
        .and_then(Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default();
    Some(PlaylistRef {
        id: browse_id.to_string(),
        title,
        subtitle,
    })
}

fn parse_mtrir_playlist(mtrir: &serde_json::Value) -> Option<PlaylistRef> {
    let browse_id = mtrir
        .pointer("/navigationEndpoint/browseEndpoint/browseId")
        .or_else(|| mtrir.pointer("/title/runs/0/navigationEndpoint/browseEndpoint/browseId"))
        .and_then(serde_json::Value::as_str)?;
    // Real playlists use VL-prefixed browse ids; skip special system lists.
    let bare = browse_id.strip_prefix("VL")?;
    if matches!(bare, "LM" | "SE" | "SS" | "RDPN") {
        return None;
    }
    let title = mtrir
        .pointer("/title/runs/0/text")
        .and_then(serde_json::Value::as_str)?
        .trim()
        .to_string();
    if title.is_empty() {
        return None;
    }
    let subtitle = mtrir
        .pointer("/subtitle/runs")
        .and_then(serde_json::Value::as_array)
        .map(|runs| {
            runs.iter()
                .filter_map(|r| r.get("text").and_then(serde_json::Value::as_str))
                .collect::<String>()
        })
        .unwrap_or_default();
    Some(PlaylistRef {
        id: browse_id.to_string(),
        title,
        subtitle,
    })
}

async fn playlist_tracks(client: &Client, id: &str) -> anyhow::Result<Vec<Item>> {
    // The browse endpoint wants the "VL"-prefixed form of the playlist id.
    let browse_id = if id.starts_with("VL") {
        id.to_string()
    } else {
        format!("VL{id}")
    };
    if let Client::Auth(yt) = client {
        let tracks = crate::lenient::playlist_tracks_authed(yt, &browse_id, MAX_PAGES).await?;
        return Ok(tracks.into_iter().map(Item::Track).collect());
    }
    let query = GetPlaylistTracksQuery::new(PlaylistID::from_raw(browse_id.clone()));
    let results: Vec<ytmapi_rs::Result<Vec<PlaylistItem>>> =
        with_yt!(client, yt => yt.stream(&query).take(MAX_PAGES).collect().await);
    let mut pages: Vec<Vec<PlaylistItem>> = Vec::new();
    let mut failed = None;
    for r in results {
        match r {
            Ok(page) => pages.push(page),
            Err(e) => {
                failed = Some(e);
                break;
            }
        }
    }
    if let Some(e) = failed {
        // ytmapi-rs drops a whole page over one odd entry (and can't go on to
        // the next page): read the list again leniently instead.
        tracing::info!(
            "playlist {id}: ytmapi-rs couldn't read page {}, reading it leniently: {e}",
            pages.len() + 1
        );
        match with_yt!(client, yt => crate::lenient::playlist_tracks(yt, &browse_id, MAX_PAGES).await) {
            Ok(tracks) => return Ok(tracks.into_iter().map(Item::Track).collect()),
            Err(le) if pages.is_empty() => return Err(le),
            Err(le) => tracing::warn!(
                "playlist {id}: lenient read failed too, keeping {} pages: {le}",
                pages.len()
            ),
        }
    }
    Ok(pages
        .into_iter()
        .flatten()
        .filter_map(playlist_item)
        .map(Item::Track)
        .collect())
}

fn playlist_item(item: PlaylistItem) -> Option<Track> {
    match item {
        PlaylistItem::Song(s) if s.is_available => Some(Track {
            video_id: s.video_id.get_raw().to_string(),
            title: s.title,
            artists: join_artists(&s.artists),
            album: Some(s.album.name.clone()),
            album_id: Some(s.album.id.get_raw().to_string()),
            duration: parse_duration(&s.duration),
            thumbnail: best_thumb(&s.thumbnails),
            liked: like_flag(&s.like_status),
        }),
        PlaylistItem::Video(v) if v.is_available => Some(Track {
            video_id: v.video_id.get_raw().to_string(),
            title: v.title,
            artists: v.channel_name,
            album: None,
            album_id: None,
            duration: parse_duration(&v.duration),
            thumbnail: best_thumb(&v.thumbnails),
            liked: like_flag(&v.like_status),
        }),
        _ => None,
    }
}

/// Search like the YouTube Music search bar: if the query is misspelled, YTM
/// answers with a "Did you mean / Showing results for" block that ytmapi-rs
/// parses as zero results. In that case take YTM's corrected query from the
/// raw response and search again with it.
async fn search_corrected(
    client: &Client,
    filter: SearchFilter,
    query: &str,
) -> anyhow::Result<(Vec<Item>, Option<String>)> {
    let items = search(client, filter, query).await?;
    if !items.is_empty() {
        return Ok((items, None));
    }
    let Some(corrected) = corrected_query(client, query).await else {
        return Ok((items, None));
    };
    if corrected.eq_ignore_ascii_case(query.trim()) {
        return Ok((items, None));
    }
    tracing::info!("search {query:?}: using YTM correction {corrected:?}");
    let items = search(client, filter, &corrected).await?;
    Ok((items, Some(corrected)))
}

/// YTM's spelling correction for `query`, if it offers one.
async fn corrected_query(client: &Client, query: &str) -> Option<String> {
    use ytmapi_rs::query::SearchQuery;
    use ytmapi_rs::query::search::BasicSearch;
    let q: SearchQuery<BasicSearch> = query.to_string().into();
    let raw = with_yt!(client, yt => yt.raw_json_query(q).await).ok()?;
    let json: serde_json::Value = serde_json::from_str(&raw).ok()?;
    find_correction(&json)
}

fn find_correction(v: &serde_json::Value) -> Option<String> {
    match v {
        serde_json::Value::Object(map) => map
            .get("correctedQueryEndpoint")
            .and_then(|e| e.pointer("/searchEndpoint/query"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string)
            .or_else(|| map.values().find_map(find_correction)),
        serde_json::Value::Array(items) => items.iter().find_map(find_correction),
        _ => None,
    }
}

async fn search(client: &Client, filter: SearchFilter, query: &str) -> anyhow::Result<Vec<Item>> {
    let q = query.to_string();
    Ok(match filter {
        SearchFilter::Songs => with_yt!(client, yt => yt.search_songs(q).await)?
            .into_iter()
            .map(|s| {
                Item::Track(Track {
                    video_id: s.video_id.get_raw().to_string(),
                    title: s.title,
                    artists: s.artist,
                    album: s.album.as_ref().map(|a| a.name.clone()),
                    album_id: s.album.as_ref().map(|a| a.id.get_raw().to_string()),
                    duration: parse_duration(&s.duration),
                    thumbnail: best_thumb(&s.thumbnails),
                    liked: None,
                })
            })
            .collect(),
        SearchFilter::Videos => with_yt!(client, yt => yt.search_videos(q).await)?
            .into_iter()
            .filter_map(|v| match v {
                SearchResultVideo::Video {
                    title,
                    channel_name,
                    video_id,
                    length,
                    thumbnails,
                    ..
                } => Some(Item::Track(Track {
                    video_id: video_id.get_raw().to_string(),
                    title,
                    artists: channel_name,
                    album: None,
                    album_id: None,
                    duration: parse_duration(&length),
                    thumbnail: best_thumb(&thumbnails),
                    liked: None,
                })),
                _ => None,
            })
            .collect(),
        SearchFilter::Albums => with_yt!(client, yt => yt.search_albums(q).await)?
            .into_iter()
            .map(|a| {
                Item::Album(AlbumRef {
                    id: a.album_id.get_raw().to_string(),
                    title: a.title,
                    subtitle: format!("{} · {}", a.artist, a.year),
                })
            })
            .collect(),
        SearchFilter::Artists => with_yt!(client, yt => yt.search_artists(q).await)?
            .into_iter()
            .map(|a| {
                Item::Artist(ArtistRef {
                    id: a.browse_id.get_raw().to_string(),
                    name: a.artist,
                    subtitle: a.subscribers.unwrap_or_default(),
                })
            })
            .collect(),
        SearchFilter::Playlists => with_yt!(client, yt => yt.search_playlists(q).await)?
            .into_iter()
            .filter_map(|p| match p {
                SearchResultPlaylist::Featured(p) => Some(Item::Playlist(PlaylistRef {
                    id: p.playlist_id.get_raw().to_string(),
                    title: p.title,
                    subtitle: format!("{} · {}", p.author, p.songs),
                })),
                SearchResultPlaylist::Community(p) => Some(Item::Playlist(PlaylistRef {
                    id: p.playlist_id.get_raw().to_string(),
                    title: p.title,
                    subtitle: format!("{} · {}", p.author, p.views),
                })),
                _ => None,
            })
            .collect(),
    })
}

fn best_thumb(thumbs: &[Thumbnail]) -> Option<String> {
    thumbs.iter().max_by_key(|t| t.width * t.height).map(|t| t.url.clone())
}

fn join_artists(a: &[ytmapi_rs::parse::ParsedSongArtist]) -> String {
    a.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
}

fn like_flag(s: &LikeStatus) -> Option<bool> {
    match s {
        LikeStatus::Liked => Some(true),
        LikeStatus::Indifferent => Some(false),
        LikeStatus::Disliked => Some(false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Hits the real API: `cargo test -- --ignored live_api`.
    #[tokio::test]
    #[ignore]
    async fn live_api() {
        let client = Client::Anon(YtMusic::new_unauthenticated().await.unwrap());
        let songs = search(&client, SearchFilter::Songs, "daft punk get lucky")
            .await
            .unwrap();
        let first = songs.iter().find_map(Item::as_track).expect("a song").clone();
        println!("song: {first:?}");
        for f in [
            SearchFilter::Videos,
            SearchFilter::Albums,
            SearchFilter::Artists,
            SearchFilter::Playlists,
        ] {
            let r = search(&client, f, "daft punk").await.unwrap();
            println!("{f:?}: {} results, first {:?}", r.len(), r.first());
            assert!(!r.is_empty(), "{f:?} empty");
        }
        let album_id = first.album_id.clone().expect("album id");
        let (title, tracks) = load(&client, &View::Album(album_id)).await.unwrap();
        println!("album {title}: {} tracks", tracks.len());
        assert!(!tracks.is_empty());

        let artists = search(&client, SearchFilter::Artists, "daft punk").await.unwrap();
        if let Some(Item::Artist(a)) = artists.first() {
            let (name, items) = load(&client, &View::Artist(a.id.clone())).await.unwrap();
            println!("artist {name}: {} items", items.len());
        }
        let pls = search(&client, SearchFilter::Playlists, "daft punk").await.unwrap();
        if let Some(Item::Playlist(p)) = pls.first() {
            let (_, items) = load(&client, &View::Playlist(p.id.clone())).await.unwrap();
            println!("playlist {} ({}): {} tracks", p.title, p.id, items.len());
            assert!(!items.is_empty());
        }
        match handle(
            &client,
            YtmReq::Radio {
                seed: first.video_id.clone(),
            },
        )
        .await
        .0
        {
            YtmEvent::Radio { tracks, .. } => {
                println!("radio: {} tracks", tracks.len());
                assert!(!tracks.is_empty());
            }
            other => panic!("radio: {other:?}"),
        }
        match handle(
            &client,
            YtmReq::Lyrics {
                video_id: first.video_id.clone(),
            },
        )
        .await
        .0
        {
            YtmEvent::Lyrics { text, .. } => {
                println!("ytm lyrics: {:?}", text.map(|t| t.chars().take(60).collect::<String>()))
            }
            other => panic!("lyrics: {other:?}"),
        }
    }
}

#[cfg(test)]
mod correction_tests {
    use super::{explain, find_correction};

    #[test]
    fn finds_did_you_mean_query() {
        let json: serde_json::Value = serde_json::from_str(
            r#"{"contents":{"tabs":[{"sections":[{"itemSectionRenderer":{"contents":[{"didYouMeanRenderer":
            {"correctedQueryEndpoint":{"searchEndpoint":{"query":"call me when you're sober","params":"x"}}}}]}}]}]}}"#,
        )
        .unwrap();
        assert_eq!(find_correction(&json).as_deref(), Some("call me when you're sober"));
        assert_eq!(find_correction(&serde_json::json!({"a": [1, {"b": null}]})), None);
    }

    #[test]
    fn finds_set_video_id() {
        use super::{bare_playlist_id, find_set_video_id};
        let json = serde_json::json!({"contents": [{"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "aaa", "playlistSetVideoId": "SET_A"}}},
            {"musicResponsiveListItemRenderer": {
            "playlistItemData": {"videoId": "bbb", "playlistSetVideoId": "SET_B"}}}]});
        assert_eq!(find_set_video_id(&json, "bbb").as_deref(), Some("SET_B"));
        assert_eq!(find_set_video_id(&json, "zzz"), None);
        assert_eq!(bare_playlist_id("VLPL123"), "PL123");
        assert_eq!(bare_playlist_id("PL123"), "PL123");
    }

    #[test]
    fn parses_accounts_list_with_names_and_brand_accounts() {
        use super::{AccountInfo, parse_accounts_list};
        let json = serde_json::json!({
            "actions": [{
                "getMultiPageMenuAction": {
                    "menu": {
                        "multiPageMenuRenderer": {
                            "sections": [{
                                "accountSectionListRenderer": {
                                    "contents": [{
                                        "accountItemSectionRenderer": {
                                            "contents": [
                                                {
                                                    "accountItemRenderer": {
                                                        "accountName": {"simpleText": "Main Google"},
                                                        "accountByline": {"simpleText": "main@example.com"},
                                                        "isSelected": false,
                                                        "serviceEndpoint": {
                                                            "selectActiveIdentityEndpoint": {
                                                                "supportedTokens": [
                                                                    {"accountSigninToken": {"signinUrl": "/signin?authuser=0&pageid=none"}}
                                                                ]
                                                            }
                                                        }
                                                    }
                                                },
                                                {
                                                    "accountItemRenderer": {
                                                        "accountName": {"runs": [{"text": "Music Channel"}]},
                                                        "channelHandle": {"simpleText": "@music"},
                                                        "isSelected": true,
                                                        "serviceEndpoint": {
                                                            "selectActiveIdentityEndpoint": {
                                                                "supportedTokens": [
                                                                    {"accountSigninToken": {"signinUrl": "/signin?authuser=1"}},
                                                                    {"pageIdToken": {"pageId": "101234567890123456789"}}
                                                                ]
                                                            }
                                                        }
                                                    }
                                                }
                                            ]
                                        }
                                    }]
                                }
                            }]
                        }
                    }
                }
            }]
        });
        assert_eq!(
            parse_accounts_list(&json, "0"),
            vec![
                AccountInfo {
                    name: "Main Google".into(),
                    subtitle: "main@example.com".into(),
                    auth_user: "0".into(),
                    page_id: None,
                    active: false,
                },
                AccountInfo {
                    name: "Music Channel".into(),
                    subtitle: "@music · Brand Account".into(),
                    auth_user: "1".into(),
                    page_id: Some("101234567890123456789".into()),
                    active: true,
                },
            ]
        );
    }

    #[test]
    fn parses_library_playlists_without_dropping_first_real_playlist() {
        use super::parse_library_playlists_json;
        let json = serde_json::json!({
            "contents": {
                "singleColumnBrowseResultsRenderer": {
                    "tabs": [{
                        "tabRenderer": {
                            "content": {
                                "sectionListRenderer": {
                                    "contents": [{
                                        "itemSectionRenderer": {
                                            "contents": [{
                                                "gridRenderer": {
                                                    "items": [
                                                        {
                                                            "musicTwoRowItemRenderer": {
                                                                "title": {"runs": [{"text": "New playlist"}]},
                                                                "navigationEndpoint": {"browseEndpoint": {"browseId": "FEmusic_new_playlist"}}
                                                            }
                                                        },
                                                        {
                                                            "musicTwoRowItemRenderer": {
                                                                "title": {"runs": [{"text": "Liked Music"}]},
                                                                "navigationEndpoint": {"browseEndpoint": {"browseId": "VLLM"}}
                                                            }
                                                        },
                                                        {
                                                            "musicTwoRowItemRenderer": {
                                                                "title": {"runs": [{"text": "Ruski"}]},
                                                                "subtitle": {"runs": [{"text": "Chr0llo"}, {"text": " • "}, {"text": "2 tracks"}]},
                                                                "navigationEndpoint": {"browseEndpoint": {"browseId": "VLPL123"}}
                                                            }
                                                        }
                                                    ]
                                                }
                                            }]
                                        }
                                    }]
                                }
                            }
                        }
                    }]
                }
            }
        });
        let lists = parse_library_playlists_json(&json);
        assert_eq!(lists.len(), 1);
        assert_eq!(lists[0].id, "VLPL123");
        assert_eq!(lists[0].title, "Ruski");
        assert_eq!(lists[0].subtitle, "Chr0llo • 2 tracks");
    }

    #[test]
    fn reads_logged_in_flag() {
        use super::find_logged_in;
        let out = serde_json::json!({"responseContext": {"serviceTrackingParams": [
            {"service": "GFEEDBACK", "params": [{"key": "logged_in", "value": "0"}]}]}});
        assert_eq!(find_logged_in(&out), Some(false));
        let inn = serde_json::json!({"a": [{"key": "logged_in", "value": "1"}]});
        assert_eq!(find_logged_in(&inn), Some(true));
        assert_eq!(find_logged_in(&serde_json::json!({"key": "other"})), None);
    }

    #[test]
    fn explains_expired_login() {
        let raw =
            "Http error code 401 recieved in response. Message: <You must be signed in to perform this operation.>.";
        let (text, auth) = explain(raw);
        assert!(auth);
        assert!(text.contains("pova login"));
    }

    #[test]
    fn explains_http_errors_readably() {
        let (text, auth) = explain("Http error code 429 recieved in response. Message: <Too many requests>.");
        assert!(!auth);
        assert!(text.starts_with("YouTube Music answered with error 429: Too many requests."));
        assert!(text.contains("wait a minute"));
        let (text, _) = explain("error sending request for url (https://music.youtube.com/...)");
        assert!(text.contains("internet connection"));
        assert_eq!(explain("something odd").0, "something odd");
    }

    #[test]
    fn offline_requests_fail_cleanly_instead_of_hanging() {
        use super::{SearchFilter, View, YtmEvent, YtmReq, offline_event};
        let ev = offline_event(YtmReq::Load {
            token: 7,
            view: View::Search {
                filter: SearchFilter::Songs,
                query: "x".into(),
            },
        });
        assert!(matches!(ev, YtmEvent::Failed { token: Some(7), .. }));
        let ev = offline_event(YtmReq::Suggest { query: "ab".into() });
        assert!(matches!(ev, YtmEvent::Suggestions { items, .. } if items.is_empty()));
        let ev = offline_event(YtmReq::Lyrics { video_id: "v1".into() });
        assert!(matches!(ev, YtmEvent::Lyrics { text: None, .. }));
    }
}

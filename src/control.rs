//! Remote control: a running pova listens on a Unix socket so commands like
//! `pova next` or `pova add "song"` (window-manager keybinds, scripts,
//! status bars) can drive it.
//!
//! Protocol: the client sends one line, `<command>[\t<argument>]`, and gets
//! one line back, starting with `ok` or `error`.

use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt};
use tokio::net::UnixListener;
use tokio::sync::{mpsc, oneshot};

/// Something a client asked for.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    Toggle,
    Play,
    Pause,
    Next,
    Prev,
    Like,
    Dislike,
    /// "+5", "-5" or an absolute "40".
    Volume(String),
    /// "+10", "-10" or an absolute "90" (seconds).
    Seek(String),
    /// Search and add the first song to the end of the queue.
    Add(String),
    /// Search and play the first song after the current one.
    PlayNext(String),
    /// Search and play the first song now.
    PlayNow(String),
    /// Open a YouTube Music, YouTube, Spotify, Tidal, or song.link URL.
    Open(String),
    /// Sleep timer: "30", "track", "+5", "off", "status".
    Sleep(String),
    /// Playback speed: "1.25", "1.0", "+", "-", or "status".
    Speed(String),
    /// Equalizer preset: "bass", "vocal", "flat", "off", "on", or "status".
    Eq(String),
    /// Audio quality tier: "max", "high", "normal", "low", or "status".
    Quality(String),
    /// Toggle / clear A-B section loop.
    AbLoop,
    /// Print local listening stats summary (Wrapped Mini).
    Stats,
    /// Save or remove the current track in the offline cache.
    Offline,
    /// Discord Rich Presence: "on", "off", "toggle", or "status".
    Discord(String),
    /// One line of text about what's playing, from a format string.
    Status(String),
}

/// Default `pova status` format.
pub const STATUS_FORMAT: &str = "{artist} - {title}";

impl Command {
    /// Parse a wire line.
    pub fn parse(line: &str) -> Result<Command, String> {
        let (cmd, arg) = match line.split_once('\t') {
            Some((c, a)) => (c, a.trim().to_string()),
            None => (line, String::new()),
        };
        let need = |what: &str| {
            if arg.is_empty() {
                Err(format!("`{cmd}` needs {what}"))
            } else {
                Ok(arg.clone())
            }
        };
        Ok(match cmd.trim() {
            "toggle" => Command::Toggle,
            "play" => Command::Play,
            "pause" => Command::Pause,
            "next" => Command::Next,
            "prev" => Command::Prev,
            "like" => Command::Like,
            "dislike" => Command::Dislike,
            "volume" => Command::Volume(need("a volume like 40, +5 or -5")?),
            "seek" => Command::Seek(need("seconds like 90, +10 or -10")?),
            "add" => Command::Add(need("a search query")?),
            "play-next" => Command::PlayNext(need("a search query")?),
            "play-now" => Command::PlayNow(need("a search query")?),
            "open" => Command::Open(need("a YouTube, Spotify, Tidal, or song.link URL")?),
            "sleep" => Command::Sleep(if arg.is_empty() { "status".into() } else { arg }),
            "speed" => Command::Speed(if arg.is_empty() { "status".into() } else { arg }),
            "eq" => Command::Eq(if arg.is_empty() { "status".into() } else { arg }),
            "quality" => Command::Quality(if arg.is_empty() { "status".into() } else { arg }),
            "ab-loop" => Command::AbLoop,
            "stats" => Command::Stats,
            "offline" => Command::Offline,
            "discord" => Command::Discord(if arg.is_empty() { "status".into() } else { arg }),
            "status" => Command::Status(if arg.is_empty() { STATUS_FORMAT.into() } else { arg }),
            other => return Err(format!("unknown command `{other}`")),
        })
    }

    /// The wire line for this command.
    pub fn line(&self) -> String {
        match self {
            Command::Toggle => "toggle".into(),
            Command::Play => "play".into(),
            Command::Pause => "pause".into(),
            Command::Next => "next".into(),
            Command::Prev => "prev".into(),
            Command::Like => "like".into(),
            Command::Dislike => "dislike".into(),
            Command::Volume(v) => format!("volume\t{v}"),
            Command::Seek(s) => format!("seek\t{s}"),
            Command::Add(q) => format!("add\t{q}"),
            Command::PlayNext(q) => format!("play-next\t{q}"),
            Command::PlayNow(q) => format!("play-now\t{q}"),
            Command::Open(u) => format!("open\t{u}"),
            Command::Sleep(s) => format!("sleep\t{s}"),
            Command::Speed(s) => format!("speed\t{s}"),
            Command::Eq(s) => format!("eq\t{s}"),
            Command::Quality(s) => format!("quality\t{s}"),
            Command::AbLoop => "ab-loop".into(),
            Command::Stats => "stats".into(),
            Command::Offline => "offline".into(),
            Command::Discord(s) => format!("discord\t{s}"),
            Command::Status(f) => format!("status\t{f}"),
        }
    }
}

/// A command plus where to send the answer.
pub struct Request {
    pub command: Command,
    pub reply: oneshot::Sender<String>,
}

/// "+5" / "-5" (relative) or "40" (absolute).
pub fn parse_amount(s: &str) -> Option<(f64, bool)> {
    let s = s.trim();
    let relative = s.starts_with('+') || s.starts_with('-');
    s.parse::<f64>().ok().filter(|v| v.is_finite()).map(|v| (v, relative))
}

pub fn socket_path(runtime_dir: &Path) -> PathBuf {
    runtime_dir.join("control.sock")
}

/// Take the control socket. None if another pova already owns it (that
/// one stays the controllable instance) or it can't be created.
pub fn bind(path: &Path) -> Option<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).is_ok() {
            tracing::warn!("another pova is already listening on {}", path.display());
            return None;
        }
        let _ = std::fs::remove_file(path); // stale, from a crash
    }
    match UnixListener::bind(path) {
        Ok(l) => {
            // Only this user may drive pova, even if the runtime dir falls
            // back to a shared /tmp.
            use std::os::unix::fs::PermissionsExt;
            if let Err(e) = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)) {
                tracing::warn!("control socket permissions: {e}");
            }
            tracing::info!("control socket: {}", path.display());
            Some(l)
        }
        Err(e) => {
            tracing::warn!("control socket {}: {e}", path.display());
            None
        }
    }
}

/// Answer clients until the app goes away.
pub async fn serve(listener: UnixListener, tx: mpsc::UnboundedSender<Request>) {
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let tx = tx.clone();
        tokio::spawn(async move {
            let (read, mut write) = stream.into_split();
            let mut line = String::new();
            let mut reader = tokio::io::BufReader::new(read);
            let got = tokio::time::timeout(Duration::from_secs(2), reader.read_line(&mut line)).await;
            let answer = match got {
                Ok(Ok(n)) if n > 0 => match Command::parse(line.trim_end_matches(['\r', '\n'])) {
                    Ok(command) => {
                        let (reply, rx) = oneshot::channel();
                        if tx.send(Request { command, reply }).is_err() {
                            "error pova is shutting down".to_string()
                        } else {
                            rx.await.unwrap_or_else(|_| "error no answer".into())
                        }
                    }
                    Err(e) => format!("error {e}"),
                },
                _ => "error empty request".into(),
            };
            let _ = write.write_all(format!("{answer}\n").as_bytes()).await;
        });
    }
}

/// Client side: send one command to the running pova and return its answer.
pub fn send(path: &Path, command: &Command) -> Result<String, String> {
    let mut stream = UnixStream::connect(path)
        .map_err(|_| "pova isn't running (start it in a terminal first, then use these commands)".to_string())?;
    let timeout = Some(Duration::from_secs(5));
    let _ = stream.set_read_timeout(timeout);
    let _ = stream.set_write_timeout(timeout);
    stream
        .write_all(format!("{}\n", command.line()).as_bytes())
        .map_err(|e| format!("couldn't talk to pova: {e}"))?;
    let mut answer = String::new();
    BufReader::new(stream)
        .read_line(&mut answer)
        .map_err(|e| format!("no answer from pova: {e}"))?;
    let answer = answer.trim_end().to_string();
    match answer.strip_prefix("error") {
        Some(rest) => Err(rest.trim().to_string()),
        None => Ok(answer.strip_prefix("ok").unwrap_or(&answer).trim().to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip_through_the_wire_format() {
        for c in [
            Command::Toggle,
            Command::Next,
            Command::Volume("+5".into()),
            Command::Add("seether fine again".into()),
            Command::Open("https://open.spotify.com/track/123".into()),
            Command::Sleep("30".into()),
            Command::Speed("1.25".into()),
            Command::Eq("bass".into()),
            Command::Quality("max".into()),
            Command::AbLoop,
            Command::Stats,
            Command::Offline,
            Command::Discord("on".into()),
            Command::Status("{title}".into()),
        ] {
            assert_eq!(Command::parse(&c.line()), Ok(c));
        }
        assert_eq!(Command::parse("status"), Ok(Command::Status(STATUS_FORMAT.into())));
        assert_eq!(Command::parse("sleep"), Ok(Command::Sleep("status".into())));
        assert!(Command::parse("add").unwrap_err().contains("search query"));
        assert!(Command::parse("open").unwrap_err().contains("URL"));
        assert!(Command::parse("dance").unwrap_err().contains("unknown"));
    }

    #[test]
    fn amounts() {
        assert_eq!(parse_amount("+5"), Some((5.0, true)));
        assert_eq!(parse_amount("-10"), Some((-10.0, true)));
        assert_eq!(parse_amount("40"), Some((40.0, false)));
        assert_eq!(parse_amount("loud"), None);
    }

    #[tokio::test]
    async fn server_answers_client() {
        let dir = std::env::temp_dir().join(format!("pova-control-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = socket_path(&dir);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let listener = bind(&path).expect("bind test socket");
        tokio::spawn(serve(listener, tx));
        // Fake app: answer every request.
        tokio::spawn(async move {
            while let Some(req) = rx.recv().await {
                let _ = req.reply.send(format!("ok got {}", req.command.line()));
            }
        });
        tokio::time::sleep(Duration::from_millis(100)).await;
        let p = path.clone();
        let answer = tokio::task::spawn_blocking(move || send(&p, &Command::Next))
            .await
            .unwrap();
        assert_eq!(answer, Ok("got next".into()));
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

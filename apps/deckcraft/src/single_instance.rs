//! Preferences ▸ Open presentations in tabs: a file the system opens while DeckCraft is running (a
//! double-click in Explorer or another file manager, Open With, several files at once) goes to the
//! running window as new tabs instead of a second window.
//!
//! The first DeckCraft a user starts holds `instance.lock` in the settings folder and listens on a
//! loopback port, which it writes with a random token into `instance.json` (both files are only in
//! that user's settings folder). A later launch that has files and finds the lock taken sends them
//! there as one JSON line, `{"token", "open": [absolute paths]}`. The window answers `{"ok": true}`
//! when tabs are on, and opens them, or `{"ok": false}` when they are off: the launch then opens its
//! own window, as it always has. Whatever goes wrong on the way (no settings folder, a file left by
//! an old run, a timeout) also leaves the launch to open its own window, so the handover can save a
//! window but never lose a file.
//!
//! The lock is an operating-system file lock, which the system releases when the process ends,
//! even in a crash: a stale `instance.json` never makes a launch think a window is running.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const LOCK_FILE: &str = "instance.lock";
const INFO_FILE: &str = "instance.json";
/// A request larger than this is not a list of files.
const MAX_MESSAGE: u64 = 1 << 20;
const MAX_FILES: usize = 256;
const MAX_PATH: usize = 4096;
/// How long either side waits for the other to speak.
const IO_TIMEOUT: Duration = Duration::from_secs(3);
/// How long a launch keeps trying to reach a window that is starting at the same moment (several
/// files opened together start several launches at once; one of them becomes the window).
const PATIENCE: Duration = Duration::from_secs(4);

/// This process is the user's running DeckCraft: it keeps the lock and takes files from later
/// launches.
pub struct Primary {
    /// Held for the life of the process; the system releases it at exit.
    _lock: std::fs::File,
    listener: TcpListener,
    token: String,
}

/// What [`claim`] found.
pub enum Claim {
    /// No other DeckCraft runs: this one is the window later launches hand files to.
    Primary(Primary),
    /// Another DeckCraft runs and holds the lock.
    Taken,
    /// The lock or the listener couldn't be set up; this launch just runs on its own.
    Unavailable,
}

/// Take the lock in `dir` (the settings folder), or find it taken.
pub fn claim(dir: &Path) -> Claim {
    if let Err(e) = std::fs::create_dir_all(dir) {
        log::warn!("{}: {e}; files opened later get their own window", dir.display());
        return Claim::Unavailable;
    }
    let lock = match std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(dir.join(LOCK_FILE)) {
        Ok(f) => f,
        Err(e) => {
            log::warn!("{LOCK_FILE}: {e}; files opened later get their own window");
            return Claim::Unavailable;
        }
    };
    match lock.try_lock() {
        Ok(()) => {}
        Err(std::fs::TryLockError::WouldBlock) => return Claim::Taken,
        Err(std::fs::TryLockError::Error(e)) => {
            log::warn!("{LOCK_FILE}: {e}; files opened later get their own window");
            return Claim::Unavailable;
        }
    }
    let listener = match TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0))) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("no loopback port for files opened later: {e}");
            return Claim::Unavailable;
        }
    };
    let token = token();
    let port = match listener.local_addr() {
        Ok(a) => a.port(),
        Err(e) => {
            log::warn!("no loopback port for files opened later: {e}");
            return Claim::Unavailable;
        }
    };
    if let Err(e) = write_info(dir, port, &token) {
        log::warn!("{INFO_FILE}: {e}; files opened later get their own window");
        return Claim::Unavailable;
    }
    Claim::Primary(Primary { _lock: lock, listener, token })
}

/// `instance.json`, written beside itself and renamed into place, so a launch never reads half.
fn write_info(dir: &Path, port: u16, token: &str) -> std::io::Result<()> {
    let tmp = dir.join(format!("{INFO_FILE}.{}.tmp", std::process::id()));
    std::fs::write(&tmp, json!({"port": port, "token": token, "pid": std::process::id()}).to_string())?;
    std::fs::rename(&tmp, dir.join(INFO_FILE)).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

/// A token another user's process can't guess: it is only in this user's settings folder. The
/// standard library's hasher keys come from the operating system's random source.
fn token() -> String {
    use std::hash::{BuildHasher, Hasher};
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos()).unwrap_or_default();
    (0..2u64)
        .map(|i| {
            let mut h = std::collections::hash_map::RandomState::new().build_hasher();
            h.write_u64(i);
            h.write_u128(nanos);
            h.write_u32(std::process::id());
            format!("{:016x}", h.finish())
        })
        .collect()
}

/// Equal tokens, compared without stopping at the first difference.
fn same_token(a: &str, b: &str) -> bool {
    a.len() == b.len() && a.bytes().zip(b.bytes()).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

impl Primary {
    /// Answer later launches on a thread. `enabled` is the window's tabs preference (it keeps it
    /// current); accepted files go to `files`, and `wake` repaints the window to open them.
    pub fn serve(self, enabled: Arc<AtomicBool>, files: Sender<Vec<String>>, wake: impl Fn() + Send + 'static) {
        let Primary { _lock, listener, token } = self;
        let spawned = std::thread::Builder::new().name("deckcraft-instance".into()).spawn(move || {
            // The lock lives as long as the thread, which lives as long as the process.
            let _lock = _lock;
            for stream in listener.incoming() {
                let Ok(stream) = stream else { continue };
                match answer(&stream, &token, &enabled) {
                    Ok(Some(paths)) => {
                        log::info!("{} file(s) handed over by another launch", paths.len());
                        if files.send(paths).is_err() {
                            return;
                        }
                        wake();
                    }
                    Ok(None) => {}
                    Err(e) => log::debug!("instance request: {e}"),
                }
            }
        });
        if let Err(e) = spawned {
            log::warn!("no thread for files opened later: {e}");
        }
    }
}

/// Read one request and answer it; the files when they were accepted.
fn answer(stream: &TcpStream, token: &str, enabled: &AtomicBool) -> std::io::Result<Option<Vec<String>>> {
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    let mut line = String::new();
    BufReader::new(stream.take(MAX_MESSAGE)).read_line(&mut line)?;
    let (reply, files) = decide(&line, token, enabled.load(Ordering::Relaxed));
    let mut out = stream;
    out.write_all(format!("{reply}\n").as_bytes())?;
    out.flush()?;
    Ok(files)
}

/// The answer to request `line`, and the files to open when it is accepted. A wrong token asks the
/// launch to try again: it may have read the file a previous run left before this one replaced it.
fn decide(line: &str, token: &str, enabled: bool) -> (Value, Option<Vec<String>>) {
    let Ok(req) = serde_json::from_str::<Value>(line) else { return (json!({"ok": false}), None) };
    if !req.get("token").and_then(Value::as_str).is_some_and(|t| same_token(t, token)) {
        return (json!({"ok": false, "retry": true}), None);
    }
    let Some(list) = req.get("open").and_then(Value::as_array).filter(|l| !l.is_empty() && l.len() <= MAX_FILES) else {
        return (json!({"ok": false}), None);
    };
    let files: Option<Vec<String>> =
        list.iter().map(|f| f.as_str().filter(|s| !s.is_empty() && s.len() <= MAX_PATH && !s.contains('\0')).map(str::to_string)).collect();
    match files {
        Some(files) if enabled => (json!({"ok": true}), Some(files)),
        _ => (json!({"ok": false}), None),
    }
}

/// Hand `files` to the running window. True when it took them; false when this launch should
/// open its own window (tabs are off there, or it couldn't be reached).
pub fn hand_over(dir: &Path, files: &[String]) -> bool {
    // The window has another working folder: send paths it can open as they are.
    let paths: Vec<String> = files.iter().map(|f| std::path::absolute(f).map(|p| p.to_string_lossy().into_owned()).unwrap_or_else(|_| f.clone())).collect();
    let deadline = Instant::now() + PATIENCE;
    loop {
        match try_hand_over(dir, &paths) {
            Ok(Some(taken)) => return taken,
            Ok(None) | Err(_) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(100)),
            Ok(None) => return false,
            Err(e) => {
                log::info!("no running window took the files: {e}");
                return false;
            }
        }
    }
}

/// One attempt: `Some(taken)` is the window's answer; `None` asks to try again shortly.
fn try_hand_over(dir: &Path, paths: &[String]) -> std::io::Result<Option<bool>> {
    let mut raw = Vec::new();
    std::fs::File::open(dir.join(INFO_FILE))?.take(4096).read_to_end(&mut raw)?;
    let info: Value = serde_json::from_slice(&raw).map_err(std::io::Error::other)?;
    let port = info.get("port").and_then(Value::as_u64).and_then(|p| u16::try_from(p).ok()).ok_or_else(|| std::io::Error::other("no port"))?;
    let token = info.get("token").and_then(Value::as_str).ok_or_else(|| std::io::Error::other("no token"))?;
    let mut stream = TcpStream::connect_timeout(&SocketAddr::from((Ipv4Addr::LOCALHOST, port)), Duration::from_millis(500))?;
    stream.set_read_timeout(Some(IO_TIMEOUT))?;
    stream.set_write_timeout(Some(IO_TIMEOUT))?;
    stream.write_all(format!("{}\n", json!({"token": token, "open": paths})).as_bytes())?;
    stream.flush()?;
    let mut line = String::new();
    BufReader::new(stream.take(4096)).read_line(&mut line)?;
    let reply: Value = serde_json::from_str(&line).map_err(std::io::Error::other)?;
    if reply.get("retry").and_then(Value::as_bool) == Some(true) {
        return Ok(None);
    }
    Ok(Some(reply.get("ok").and_then(Value::as_bool) == Some(true)))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    fn dir(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("deckcraft-instance-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    /// The request a launch sends is only accepted with the token and with tabs on, and never
    /// holds anything but a short list of plain paths.
    #[test]
    fn requests_are_checked() {
        let ok = |l: &str, on: bool| decide(l, "abc", on);
        let good = r#"{"token":"abc","open":["C:\\decks\\a.pptx","/b.deckcraft"]}"#;
        assert_eq!(ok(good, true), (json!({"ok": true}), Some(vec!["C:\\decks\\a.pptx".to_string(), "/b.deckcraft".to_string()])));
        assert_eq!(ok(good, false), (json!({"ok": false}), None), "tabs off: the launch opens its own window");
        assert_eq!(ok(r#"{"token":"abd","open":["/a.pptx"]}"#, true).0["retry"], json!(true));
        assert_eq!(ok(r#"{"open":["/a.pptx"]}"#, true).0["retry"], json!(true));
        let many = json!({"token": "abc", "open": vec!["/a.pptx"; MAX_FILES + 1]}).to_string();
        let long = json!({"token": "abc", "open": ["x".repeat(MAX_PATH + 1)]}).to_string();
        for bad in ["", "{", "null", "[1]", r#"{"token":"abc"}"#, r#"{"token":"abc","open":[]}"#, r#"{"token":"abc","open":[1]}"#, r#"{"token":"abc","open":["a\u0000b"]}"#, &many, &long] {
            assert_eq!(ok(bad, true).1, None, "{bad}");
        }
    }

    /// The first claim takes the lock; a second one in the same folder finds it taken, hands its
    /// files over when tabs are on and is turned away when they are off.
    #[test]
    fn a_later_launch_hands_its_files_to_the_window() {
        let d = dir("handover");
        let Claim::Primary(primary) = claim(&d) else { panic!("the first launch is the window") };
        assert!(matches!(claim(&d), Claim::Taken), "the lock is held");
        let enabled = Arc::new(AtomicBool::new(false));
        let (tx, rx) = std::sync::mpsc::channel();
        primary.serve(enabled.clone(), tx, || {});
        assert!(!hand_over(&d, &["off.pptx".to_string()]), "tabs off: own window");
        assert!(rx.try_recv().is_err());
        enabled.store(true, Ordering::Relaxed);
        assert!(hand_over(&d, &["one.pptx".to_string(), "two.pptx".to_string()]));
        let got = rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(got.len(), 2);
        assert!(got.iter().all(|p| Path::new(p).is_absolute()), "relative paths are made absolute: {got:?}");
        let _ = std::fs::remove_dir_all(&d);
    }

    /// A stale `instance.json` (a port nothing listens on) makes the launch open its own window
    /// after a short wait instead of hanging or losing the file.
    #[test]
    fn a_stale_info_file_gives_up() {
        let d = dir("stale");
        std::fs::create_dir_all(&d).unwrap();
        let port = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).unwrap().local_addr().unwrap().port();
        std::fs::write(d.join(INFO_FILE), json!({"port": port, "token": "x"}).to_string()).unwrap();
        let t0 = Instant::now();
        assert!(!hand_over(&d, &["a.pptx".to_string()]));
        assert!(t0.elapsed() < PATIENCE + Duration::from_secs(3));
        std::fs::write(d.join(INFO_FILE), b"not json").unwrap();
        assert!(!hand_over(&d, &["a.pptx".to_string()]));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn tokens_differ_and_compare_exactly() {
        assert_ne!(token(), token());
        assert_eq!(token().len(), 32);
        assert!(same_token("abc", "abc") && !same_token("abc", "abd") && !same_token("abc", "ab"));
    }
}

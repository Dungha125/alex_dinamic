//! Now-playing info via Windows Global System Media Transport Controls
//! (works with Spotify, browsers, Media Player, ...).
use std::sync::mpsc::{channel, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};

#[derive(Clone, Default, PartialEq)]
pub struct MediaInfo {
    pub title: String,
    pub artist: String,
    pub app: String,
    pub playing: bool,
}

pub enum MediaCmd {
    PlayPause,
    Next,
    Prev,
    /// Pause or resume the session whose title matches, so the island can control a saved track
    /// while another app is the current session.
    ToggleTitle(String),
}

pub type Shared = Arc<Mutex<Option<MediaInfo>>>;

pub fn spawn(state: Shared, repaint: impl Fn() + Send + 'static) -> Sender<MediaCmd> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        unsafe {
            let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
        }
        let Ok(mgr) = Manager::RequestAsync().and_then(|op| op.join()) else { return };
        loop {
            let mut wait = Duration::from_millis(1500);
            match rx.recv_timeout(wait) {
                Ok(cmd) => {
                    match cmd {
                        MediaCmd::ToggleTitle(title) => {
                            let _ = toggle_title(&mgr, &title);
                        }
                        other => {
                            if let Ok(s) = mgr.GetCurrentSession() {
                                let op = match other {
                                    MediaCmd::PlayPause => s.TryTogglePlayPauseAsync(),
                                    MediaCmd::Next => s.TrySkipNextAsync(),
                                    MediaCmd::Prev => s.TrySkipPreviousAsync(),
                                    MediaCmd::ToggleTitle(_) => unreachable!(),
                                };
                                let _ = op.and_then(|o| o.join());
                            }
                        }
                    }
                    std::thread::sleep(Duration::from_millis(250));
                    wait = Duration::ZERO;
                }
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            let _ = wait;
            let info = read(&mgr);
            let mut cur = state.lock().unwrap();
            if *cur != info {
                *cur = info;
                drop(cur);
                repaint();
            }
        }
    });
    tx
}

fn titles_match(a: &str, b: &str) -> bool {
    let a = a.trim().to_lowercase();
    let b = b.trim().to_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (&a, &b) } else { (&b, &a) };
    short.chars().count() >= 6 && long.contains(short.as_str())
}

fn toggle_title(mgr: &Manager, title: &str) -> bool {
    let Ok(sessions) = mgr.GetSessions() else { return false };
    let Ok(n) = sessions.Size() else { return false };
    for i in 0..n {
        let Ok(s) = sessions.GetAt(i) else { continue };
        let Ok(props) = s.TryGetMediaPropertiesAsync().and_then(|op| op.join()) else { continue };
        let Ok(got) = props.Title() else { continue };
        if titles_match(&got.to_string(), title) {
            let _ = s.TryTogglePlayPauseAsync().and_then(|op| op.join());
            return true;
        }
    }
    false
}

fn read(mgr: &Manager) -> Option<MediaInfo> {
    let s = mgr.GetCurrentSession().ok()?;
    let props = s.TryGetMediaPropertiesAsync().ok()?.join().ok()?;
    let title = props.Title().ok()?.to_string();
    if title.is_empty() {
        return None;
    }
    let playing = s
        .GetPlaybackInfo()
        .and_then(|p| p.PlaybackStatus())
        .map(|st| st == Status::Playing)
        .unwrap_or(false);
    let app = s.SourceAppUserModelId().map(|h| h.to_string()).unwrap_or_default();
    let app = app.rsplit(['\\', '!']).next().unwrap_or("").trim_end_matches(".exe").to_string();
    Some(MediaInfo { title, artist: props.Artist().map(|a| a.to_string()).unwrap_or_default(), app, playing })
}

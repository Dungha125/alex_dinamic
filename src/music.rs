//! YouTube on the island: detect the playing tab, remember its video id, and
//! open that video in a small official player window (Edge or Chrome `--app` embed).
//! Audio is not downloaded.

use std::path::PathBuf;
use std::process::{Command, Stdio};
use windows::Win32::Foundation::{CloseHandle, HWND, LPARAM, TRUE};
use windows::core::BOOL;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Threading::{GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
use windows::Win32::UI::Accessibility::{
    CUIAutomation, IUIAutomation, IUIAutomationElement, IUIAutomationValuePattern, TreeScope_Descendants, UIA_ComboBoxControlTypeId,
    UIA_EditControlTypeId, UIA_ValuePatternId,
};
use windows::Win32::UI::WindowsAndMessaging::{EnumWindows, GetSystemMetrics, GetWindowTextW, IsWindowVisible, SM_CXSCREEN};

pub struct HeartDone {
    pub id: Option<String>,
    pub title: String,
    pub artist: String,
}

pub fn same_title(a: &str, b: &str) -> bool {
    let a = a.trim().to_lowercase();
    let b = b.trim().to_lowercase();
    if a.is_empty() || b.is_empty() {
        return false;
    }
    if a == b {
        return true;
    }
    let (short, long) = if a.len() <= b.len() { (a, b) } else { (b, a) };
    short.chars().count() >= 6 && long.contains(&short)
}

pub fn is_youtube(title: &str, app: &str) -> bool {
    if app.to_lowercase().contains("youtube") {
        return true;
    }
    let title = title.trim();
    if title.chars().count() < 2 {
        return false;
    }
    youtube_windows().into_iter().any(|(_, window)| window_has(&window, title))
}

/// Video id of the YouTube tab that is playing `media_title`, read from the address bar.
pub fn capture_id(media_title: &str) -> Option<String> {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let mut wins = youtube_windows();
    wins.sort_by_key(|(_, title)| !window_has(title, media_title));
    if wins.is_empty() {
        return None;
    }
    unsafe { read_ids(&wins) }
}

pub fn open_player(video_id: &str) -> Result<u32, String> {
    if take_id(video_id).as_deref() != Some(video_id) {
        return Err("Bài hát không hợp lệ".into());
    }
    let exe = browser_exe().ok_or_else(|| "Cần Microsoft Edge hoặc Chrome".to_string())?;
    let dir = profile_dir();
    let (x, y) = player_origin();
    let url = format!("https://www.youtube.com/embed/{video_id}?autoplay=1");
    let child = Command::new(exe)
        .arg(format!("--user-data-dir={}", dir.display()))
        .arg("--autoplay-policy=no-user-gesture-required")
        .arg("--no-first-run")
        .arg("--no-default-browser-check")
        .arg("--disable-sync")
        .arg("--disable-extensions")
        .arg("--window-size=480,300")
        .arg(format!("--window-position={x},{y}"))
        .arg(format!("--app={url}"))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| "Không mở được trình phát".to_string())?;
    Ok(child.id())
}

pub fn stop_pid(pid: u32) {
    if pid == 0 {
        return;
    }
    let _ = Command::new("taskkill")
        .args(["/PID", &pid.to_string(), "/T", "/F"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .output();
}

pub fn pid_alive(pid: u32) -> bool {
    unsafe {
        let Ok(handle) = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) else {
            return false;
        };
        let mut code = 0u32;
        let alive = GetExitCodeProcess(handle, &mut code).is_ok() && code == 259;
        let _ = CloseHandle(handle);
        alive
    }
}

fn window_has(window: &str, title: &str) -> bool {
    let window = window.to_lowercase();
    if !window.contains("youtube") {
        return false;
    }
    let title = title.trim().to_lowercase();
    if title.chars().count() < 2 {
        return false;
    }
    if window.contains(&title) {
        return true;
    }
    let head: String = title.chars().take(24).collect();
    head.chars().count() >= 8 && window.contains(&head)
}

fn youtube_windows() -> Vec<(HWND, String)> {
    let mut out = Vec::new();
    unsafe {
        let _ = EnumWindows(Some(enum_yt), LPARAM(&mut out as *mut Vec<(HWND, String)> as isize));
    }
    out
}

unsafe extern "system" fn enum_yt(hwnd: HWND, lp: LPARAM) -> BOOL {
    unsafe {
        if !IsWindowVisible(hwnd).as_bool() {
            return TRUE;
        }
        let mut buf = [0u16; 512];
        let n = GetWindowTextW(hwnd, &mut buf);
        if n <= 0 {
            return TRUE;
        }
        let title = String::from_utf16_lossy(&buf[..n as usize]);
        if title.to_lowercase().contains("youtube") {
            let out = &mut *(lp.0 as *mut Vec<(HWND, String)>);
            out.push((hwnd, title));
        }
        TRUE
    }
}

unsafe fn read_ids(wins: &[(HWND, String)]) -> Option<String> {
    unsafe {
        let auto: IUIAutomation = CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER).ok()?;
        let cond = auto.CreateTrueCondition().ok()?;
        for (hwnd, _) in wins {
            let Ok(root) = auto.ElementFromHandle(*hwnd) else { continue };
            let Ok(all) = root.FindAll(TreeScope_Descendants, &cond) else { continue };
            let Ok(n) = all.Length() else { continue };
            let n = n.min(2500);
            let mut fallback = None;
            for i in 0..n {
                let Ok(el) = all.GetElement(i) else { continue };
                match id_from_element(&el) {
                    Found::Address(id) => return Some(id),
                    Found::Other(id) if !id.is_empty() && fallback.is_none() => fallback = Some(id),
                    _ => {}
                }
            }
            if fallback.is_some() {
                return fallback;
            }
        }
        None
    }
}

enum Found {
    Address(String),
    Other(String),
}

unsafe fn id_from_element(el: &IUIAutomationElement) -> Found {
    unsafe {
        let name = el.CurrentName().ok().map(|n| n.to_string()).unwrap_or_default();
        let folded = name.to_lowercase();
        let address = folded.contains("address") || folded.contains("địa chỉ") || folded.contains("omnibox");
        let edit = el.CurrentControlType().ok().is_some_and(|k| k == UIA_EditControlTypeId || k == UIA_ComboBoxControlTypeId);
        if !address && !edit {
            return Found::Other(String::new());
        }
        let mut text = name;
        if let Ok(pat) = el.GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId) {
            if let Ok(value) = pat.CurrentValue() {
                let value = value.to_string();
                if !value.is_empty() {
                    text = value;
                }
            }
        }
        match video_id(&text) {
            Some(id) if address => Found::Address(id),
            Some(id) => Found::Other(id),
            None => Found::Other(String::new()),
        }
    }
}

pub fn video_id(text: &str) -> Option<String> {
    let lower = text.to_ascii_lowercase();
    if !(lower.contains("youtube.com") || lower.contains("youtu.be")) {
        return None;
    }
    if let Some(i) = lower.find("youtu.be/") {
        if let Some(id) = take_id(&text[i + "youtu.be/".len()..]) {
            return Some(id);
        }
    }
    for mark in ["/shorts/", "/embed/", "/live/"] {
        if let Some(i) = lower.find(mark) {
            if let Some(id) = take_id(&text[i + mark.len()..]) {
                return Some(id);
            }
        }
    }
    let mut rest = text;
    while let Some(i) = rest.find("v=") {
        let before = rest[..i].chars().next_back();
        let after = &rest[i + 2..];
        if matches!(before, Some('?' | '&' | '#')) {
            if let Some(id) = take_id(after) {
                return Some(id);
            }
        }
        rest = after;
    }
    None
}

fn take_id(s: &str) -> Option<String> {
    let id: String = s.chars().take_while(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-').collect();
    (id.len() == 11).then_some(id)
}

fn profile_dir() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    let dir = base.join("Alex").join("yt-player");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

fn player_origin() -> (i32, i32) {
    let width = unsafe { GetSystemMetrics(SM_CXSCREEN) }.max(480);
    ((width - 480) / 2, 52)
}

fn browser_exe() -> Option<PathBuf> {
    let pf = std::env::var_os("ProgramFiles").map(PathBuf::from);
    let pf86 = std::env::var_os("ProgramFiles(x86)").map(PathBuf::from);
    let local = std::env::var_os("LOCALAPPDATA").map(PathBuf::from);
    let mut candidates = Vec::new();
    for base in [&pf86, &pf, &local] {
        if let Some(base) = base {
            candidates.push(base.join(r"Microsoft\Edge\Application\msedge.exe"));
        }
    }
    for base in [&pf, &pf86, &local] {
        if let Some(base) = base {
            candidates.push(base.join(r"Google\Chrome\Application\chrome.exe"));
        }
    }
    if let Some(local) = &local {
        candidates.push(local.join(r"BraveSoftware\Brave-Browser\Application\brave.exe"));
    }
    candidates.into_iter().find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::video_id;

    #[test]
    fn parses_youtube_urls() {
        assert_eq!(video_id("https://www.youtube.com/watch?v=dQw4w9WgXcQ"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(video_id("https://youtu.be/dQw4w9WgXcQ?t=1"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(video_id("https://www.youtube.com/shorts/dQw4w9WgXcQ"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(video_id("https://music.youtube.com/watch?v=dQw4w9WgXcQ&list=abc"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(video_id("https://www.youtube.com/embed/dQw4w9WgXcQ?autoplay=1"), Some("dQw4w9WgXcQ".into()));
        assert_eq!(video_id("not a url"), None);
    }
}

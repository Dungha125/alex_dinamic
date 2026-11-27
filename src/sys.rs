//! Thin Win32 helpers: window placement, hit-region, tray icon, autostart, misc.
use std::sync::{mpsc::Sender, OnceLock};
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::{CreateRectRgn, SetWindowRgn};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Registry::*;
use windows::Win32::System::SystemInformation::GetLocalTime;
use windows::Win32::System::Threading::CreateMutexW;
use windows::Win32::UI::Shell::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const WINDOW_TITLE: &str = "Alex Island";

pub fn single_instance() -> bool {
    unsafe {
        let _ = CreateMutexW(None, true, w!("Local\\AlexIslandSingleton"));
        GetLastError() != ERROR_ALREADY_EXISTS
    }
}

pub fn find_window() -> Option<HWND> {
    unsafe { FindWindowW(PCWSTR::null(), &HSTRING::from(WINDOW_TITLE)).ok() }
}

/// Puts the window at the top-center of the primary monitor, always on top,
/// hidden from screenshots / screen sharing and from Alt+Tab.
pub fn place_window(hwnd: HWND, w_px: i32, h_px: i32) {
    unsafe {
        let sw = GetSystemMetrics(SM_CXSCREEN);
        let x = (sw - w_px) / 2;
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW.0 as isize);
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, 0, w_px, h_px, SWP_NOACTIVATE | SWP_SHOWWINDOW);
        if std::env::var_os("ALEX_ALLOW_CAPTURE").is_none() {
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        }
    }
}

pub fn ensure_toolwindow(hwnd: HWND) {
    unsafe {
        let ex = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
        if ex & WS_EX_TOOLWINDOW.0 as isize == 0 {
            SetWindowLongPtrW(hwnd, GWL_EXSTYLE, ex | WS_EX_TOOLWINDOW.0 as isize);
        }
    }
}

pub fn keep_topmost(hwnd: HWND) {
    unsafe {
        let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE);
    }
}

/// Only pixels inside this rect receive mouse input / get drawn; the rest of the
/// window is click-through. An empty rect hides the island completely.
pub fn set_region(hwnd: HWND, l: i32, t: i32, r: i32, b: i32) {
    unsafe {
        let rgn = CreateRectRgn(l, t, r, b);
        SetWindowRgn(hwnd, Some(rgn), true);
    }
}

pub fn fullscreen_busy() -> bool {
    unsafe {
        matches!(
            SHQueryUserNotificationState(),
            Ok(QUNS_BUSY) | Ok(QUNS_RUNNING_D3D_FULL_SCREEN) | Ok(QUNS_PRESENTATION_MODE)
        )
    }
}

pub fn shell_open(target: &str) {
    unsafe {
        ShellExecuteW(None, w!("open"), &HSTRING::from(target), PCWSTR::null(), PCWSTR::null(), SW_SHOWNORMAL);
    }
}

pub fn lock_pc() {
    unsafe {
        let _ = windows::Win32::System::Shutdown::LockWorkStation();
    }
}

pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub weekday: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

pub fn now() -> LocalTime {
    let t = unsafe { GetLocalTime() };
    LocalTime {
        year: t.wYear,
        month: t.wMonth,
        day: t.wDay,
        weekday: t.wDayOfWeek,
        hour: t.wHour,
        minute: t.wMinute,
        second: t.wSecond,
    }
}

// ---------------------------------------------------------------- autostart
const RUN_KEY: PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");

pub fn autostart_enabled() -> bool {
    unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Alex"), RRF_RT_REG_SZ, None, None, None) == ERROR_SUCCESS }
}

pub fn set_autostart(on: bool) {
    unsafe {
        if on {
            let exe = std::env::current_exe().unwrap_or_default();
            let v: Vec<u16> = format!("\"{}\"", exe.display()).encode_utf16().chain([0]).collect();
            let _ = RegSetKeyValueW(
                HKEY_CURRENT_USER,
                RUN_KEY,
                w!("Alex"),
                REG_SZ.0,
                Some(v.as_ptr() as _),
                (v.len() * 2) as u32,
            );
        } else {
            let _ = RegDeleteKeyValueW(HKEY_CURRENT_USER, RUN_KEY, w!("Alex"));
        }
    }
}

// ---------------------------------------------------------------- tray icon
pub enum TrayCmd {
    Toggle,
    Quit,
}

struct TrayCtx {
    tx: Sender<TrayCmd>,
    repaint: Box<dyn Fn() + Send + Sync>,
}
static TRAY: OnceLock<TrayCtx> = OnceLock::new();
const WM_TRAY: u32 = WM_APP + 1;

fn make_icon(rgba: &[u8], size: i32) -> HICON {
    let mut bgra = rgba.to_vec();
    for px in bgra.chunks_exact_mut(4) {
        px.swap(0, 2);
    }
    let mask = vec![0u8; (size * size / 8) as usize];
    unsafe { CreateIcon(None, size, size, 1, 32, mask.as_ptr(), bgra.as_ptr()).unwrap_or_default() }
}

fn nid(hwnd: HWND) -> NOTIFYICONDATAW {
    NOTIFYICONDATAW {
        cbSize: std::mem::size_of::<NOTIFYICONDATAW>() as u32,
        hWnd: hwnd,
        uID: 1,
        ..Default::default()
    }
}

unsafe extern "system" fn tray_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_TRAY {
        let ev = (lp.0 & 0xffff) as u32;
        let ctx = TRAY.get().unwrap();
        if ev == WM_LBUTTONUP {
            let _ = ctx.tx.send(TrayCmd::Toggle);
            (ctx.repaint)();
        } else if ev == WM_RBUTTONUP {
            unsafe {
                let menu = CreatePopupMenu().unwrap();
                let auto = autostart_enabled();
                let _ = AppendMenuW(menu, MF_STRING, 1, w!("Ẩn / hiện đảo"));
                let _ = AppendMenuW(menu, MF_STRING | if auto { MF_CHECKED } else { MF_UNCHECKED }, 2, w!("Khởi động cùng Windows"));
                let _ = AppendMenuW(menu, MF_SEPARATOR, 0, PCWSTR::null());
                let _ = AppendMenuW(menu, MF_STRING, 3, w!("Thoát Alex"));
                let mut pt = POINT::default();
                let _ = GetCursorPos(&mut pt);
                let _ = SetForegroundWindow(hwnd);
                let cmd = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, None, hwnd, None).0;
                let _ = DestroyMenu(menu);
                match cmd {
                    1 => {
                        let _ = ctx.tx.send(TrayCmd::Toggle);
                    }
                    2 => set_autostart(!auto),
                    3 => {
                        let _ = Shell_NotifyIconW(NIM_DELETE, &nid(hwnd));
                        let _ = ctx.tx.send(TrayCmd::Quit);
                    }
                    _ => {}
                }
                (ctx.repaint)();
            }
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

pub fn spawn_tray(tx: Sender<TrayCmd>, repaint: impl Fn() + Send + Sync + 'static, icon_rgba: &'static [u8]) {
    let _ = TRAY.set(TrayCtx { tx, repaint: Box::new(repaint) });
    std::thread::spawn(move || unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(tray_proc),
            hInstance: hinst.into(),
            lpszClassName: w!("AlexTray"),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("AlexTray"),
            w!("AlexTray"),
            WINDOW_STYLE(0),
            0,
            0,
            0,
            0,
            Some(HWND_MESSAGE),
            None,
            Some(hinst.into()),
            None,
        ) else {
            return;
        };
        let mut data = nid(hwnd);
        data.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        data.uCallbackMessage = WM_TRAY;
        data.hIcon = make_icon(icon_rgba, 32);
        let tip: Vec<u16> = "Alex".encode_utf16().collect();
        data.szTip[..tip.len()].copy_from_slice(&tip);
        let _ = Shell_NotifyIconW(NIM_ADD, &data);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

// ---------------------------------------------------------------- double-Ctrl
// A low-level keyboard hook that only looks at the Ctrl keys: two clean Ctrl taps
// (press + release with no other key in between, so Ctrl+C never counts) within
// 400 ms fire the callback. Nothing else about keystrokes is read or stored.
static DOUBLE_CTRL: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

struct TapState {
    ctrl_down: bool,
    clean: bool,
    last_tap: u32,
}
static TAP: std::sync::Mutex<TapState> = std::sync::Mutex::new(TapState { ctrl_down: false, clean: false, last_tap: 0 });

unsafe extern "system" fn kb_proc(code: i32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if code == HC_ACTION as i32 {
        let k = unsafe { &*(lp.0 as *const KBDLLHOOKSTRUCT) };
        let is_ctrl = matches!(k.vkCode, 0x11 | 0xA2 | 0xA3); // VK_CONTROL, VK_LCONTROL, VK_RCONTROL
        let down = matches!(wp.0 as u32, WM_KEYDOWN | WM_SYSKEYDOWN);
        let mut fire = false;
        if let Ok(mut s) = TAP.lock() {
            if is_ctrl && down {
                if !s.ctrl_down {
                    s.ctrl_down = true;
                    s.clean = true;
                }
            } else if is_ctrl {
                if s.ctrl_down && s.clean {
                    if s.last_tap != 0 && k.time.wrapping_sub(s.last_tap) <= 400 {
                        fire = true;
                        s.last_tap = 0;
                    } else {
                        s.last_tap = k.time;
                    }
                }
                s.ctrl_down = false;
            } else if down {
                // any other key cancels: Ctrl+C, Ctrl+V, typing between taps...
                s.clean = false;
                s.last_tap = 0;
            }
        }
        if fire {
            if let Some(cb) = DOUBLE_CTRL.get() {
                cb();
            }
        }
    }
    unsafe { CallNextHookEx(None, code, wp, lp) }
}

pub fn spawn_double_ctrl(cb: impl Fn() + Send + Sync + 'static) {
    let _ = DOUBLE_CTRL.set(Box::new(cb));
    std::thread::spawn(|| unsafe {
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let Ok(_hook) = SetWindowsHookExW(WH_KEYBOARD_LL, Some(kb_proc), Some(hinst.into()), 0) else { return };
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    });
}

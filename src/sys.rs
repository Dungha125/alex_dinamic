//! Thin Win32 helpers: window placement, hit-region, tray icon, autostart, misc.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc::Sender, Mutex, OnceLock};
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

// ---------------------------------------------------------------- file drop
// The island window is per-pixel transparent, and Windows will not deliver a file
// drag to a window whose pixels are transparent (the cursor stays "unavailable").
// A color-keyed layered window over the island is invisible but is a real drop
// target. It only claims the hit while the mouse button is already down, so
// ordinary clicks still fall through to the island.
pub struct DropHit {
    pub hovering: bool,
    pub paths: Vec<String>,
}

struct DropBox {
    hover: bool,
    paths: Vec<String>,
    hwnd: isize,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    shown: bool,
}

static DROP_BOX: Mutex<DropBox> = Mutex::new(DropBox {
    hover: false,
    paths: Vec::new(),
    hwnd: 0,
    x: 0,
    y: 0,
    w: 0,
    h: 0,
    shown: false,
});
static DROP_ACCEPT: AtomicBool = AtomicBool::new(false);
static DROP_WAKE: OnceLock<Box<dyn Fn() + Send + Sync>> = OnceLock::new();

pub fn set_drop_wake(cb: impl Fn() + Send + Sync + 'static) {
    let _ = DROP_WAKE.set(Box::new(cb));
}

fn wake_drop() {
    if let Some(cb) = DROP_WAKE.get() {
        cb();
    }
}

fn hdrop_format() -> windows::Win32::System::Com::FORMATETC {
    use windows::Win32::System::Com::{DVASPECT_CONTENT, TYMED_HGLOBAL};
    use windows::Win32::System::Ole::CF_HDROP;
    windows::Win32::System::Com::FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    }
}

fn paths_from(data: &windows::Win32::System::Com::IDataObject) -> Vec<String> {
    use windows::Win32::System::Ole::ReleaseStgMedium;
    use windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
    let fmt = hdrop_format();
    let mut medium = match unsafe { data.GetData(&fmt) } {
        Ok(m) => m,
        Err(_) => return Vec::new(),
    };
    let hdrop = HDROP(unsafe { medium.u.hGlobal.0 });
    let mut out = Vec::new();
    unsafe {
        let n = DragQueryFileW(hdrop, 0xFFFFFFFF, None);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            if got > 0 {
                out.push(String::from_utf16_lossy(&buf[..got.min(buf.len())]));
            }
        }
        ReleaseStgMedium(&mut medium);
    }
    out
}

#[windows::core::implement(windows::Win32::System::Ole::IDropTarget)]
struct IslandDrop;

impl windows::Win32::System::Ole::IDropTarget_Impl for IslandDrop_Impl {
    fn DragEnter(
        &self,
        data: windows::core::Ref<windows::Win32::System::Com::IDataObject>,
        _: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_NONE};
        let ok = data.as_ref().is_some_and(|d| unsafe { d.QueryGetData(&hdrop_format()) }.is_ok());
        DROP_ACCEPT.store(ok, Ordering::Relaxed);
        unsafe {
            if !effect.is_null() {
                *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        if ok {
            if let Ok(mut g) = DROP_BOX.lock() {
                g.hover = true;
            }
            wake_drop();
        }
        Ok(())
    }

    fn DragOver(
        &self,
        _: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_NONE};
        unsafe {
            if !effect.is_null() {
                *effect = if DROP_ACCEPT.load(Ordering::Relaxed) { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        DROP_ACCEPT.store(false, Ordering::Relaxed);
        if let Ok(mut g) = DROP_BOX.lock() {
            g.hover = false;
        }
        wake_drop();
        Ok(())
    }

    fn Drop(
        &self,
        data: windows::core::Ref<windows::Win32::System::Com::IDataObject>,
        _: windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS,
        _: &POINTL,
        effect: *mut windows::Win32::System::Ole::DROPEFFECT,
    ) -> windows::core::Result<()> {
        use windows::Win32::System::Ole::{DROPEFFECT_COPY, DROPEFFECT_NONE};
        let paths = data.as_ref().map(paths_from).unwrap_or_default();
        let ok = !paths.is_empty();
        unsafe {
            if !effect.is_null() {
                *effect = if ok { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
            }
        }
        DROP_ACCEPT.store(false, Ordering::Relaxed);
        if let Ok(mut g) = DROP_BOX.lock() {
            g.hover = false;
            g.paths.extend(paths);
        }
        wake_drop();
        Ok(())
    }
}

unsafe extern "system" fn drop_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    if msg == WM_NCHITTEST {
        let dragging = unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState(windows::Win32::UI::Input::KeyboardAndMouse::VK_LBUTTON.0 as i32) } < 0;
        return LRESULT(if dragging { HTCLIENT as isize } else { HTTRANSPARENT as isize });
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}

fn ensure_drop_window(owner: HWND) -> Option<HWND> {
    use windows::Win32::Graphics::Gdi::CreateSolidBrush;
    use windows::Win32::System::Ole::{OleInitialize, RegisterDragDrop};
    let mut box_ = DROP_BOX.lock().ok()?;
    if box_.hwnd != 0 {
        return Some(HWND(box_.hwnd as *mut _));
    }
    unsafe {
        let _ = OleInitialize(None);
        let hinst = GetModuleHandleW(None).unwrap_or_default();
        let wc = WNDCLASSW {
            lpfnWndProc: Some(drop_proc),
            hInstance: hinst.into(),
            lpszClassName: w!("AlexDrop"),
            hbrBackground: CreateSolidBrush(COLORREF(0x00FF00FF)),
            ..Default::default()
        };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_NOACTIVATE | WS_EX_TOOLWINDOW | WS_EX_TOPMOST,
            w!("AlexDrop"),
            w!("AlexDrop"),
            WS_POPUP,
            0,
            0,
            0,
            0,
            Some(owner),
            None,
            Some(hinst.into()),
            None,
        ) else {
            return None;
        };
        let _ = SetLayeredWindowAttributes(hwnd, COLORREF(0x00FF00FF), 0, LWA_COLORKEY);
        if std::env::var_os("ALEX_ALLOW_CAPTURE").is_none() {
            let _ = SetWindowDisplayAffinity(hwnd, WDA_EXCLUDEFROMCAPTURE);
        }
        let target: windows::Win32::System::Ole::IDropTarget = IslandDrop.into();
        if RegisterDragDrop(hwnd, &target).is_err() {
            let _ = DestroyWindow(hwnd);
            return None;
        }
        box_.hwnd = hwnd.0 as isize;
        Some(hwnd)
    }
}

/// `client` is the island rectangle in the owner window's client pixels.
/// `pad` grows the (invisible) catch area around it. Pass `visible = false` to hide it.
pub fn track_drop_target(owner: HWND, client: (i32, i32, i32, i32), pad: i32, visible: bool) -> DropHit {
    use windows::Win32::Graphics::Gdi::ClientToScreen;
    let Some(hwnd) = ensure_drop_window(owner) else {
        return DropHit { hovering: false, paths: Vec::new() };
    };
    let (prev_shown, prev_x, prev_y, prev_w, prev_h) = {
        let g = DROP_BOX.lock().unwrap_or_else(|e| e.into_inner());
        (g.shown, g.x, g.y, g.w, g.h)
    };
    let (x, y, w, h) = if visible && client.2 > client.0 && client.3 > client.1 {
        let mut pt = POINT { x: client.0, y: client.1 };
        unsafe { let _ = ClientToScreen(owner, &mut pt); }
        let x = pt.x - pad;
        let y = (pt.y - pad).max(0);
        (x, y, (client.2 - client.0) + pad * 2, (client.3 - client.1) + pad * 2)
    } else {
        (prev_x, prev_y, 0, 0)
    };
    let show = visible && w > 0 && h > 0;
    if show != prev_shown || x != prev_x || y != prev_y || w != prev_w || h != prev_h {
        unsafe {
            if show {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            } else {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
    let mut g = DROP_BOX.lock().unwrap_or_else(|e| e.into_inner());
    g.x = x;
    g.y = y;
    g.w = w;
    g.h = h;
    g.shown = show;
    DropHit { hovering: g.hover, paths: std::mem::take(&mut g.paths) }
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

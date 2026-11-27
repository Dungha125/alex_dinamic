use crate::clip::{self, ClipItem, ClipKind};
use crate::logo;
use crate::media::{self, MediaCmd, MediaInfo};
use crate::net::{self, Status};
use crate::share;
use crate::store::{Bookmark, Data, Note};
use crate::sys::{self, TrayCmd};
use crate::{WIN_H, WIN_W};
use eframe::egui::{
    self, pos2, vec2, Align, Align2, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Layout,
    Margin, Pos2, Rect, RichText, Sense, Stroke, StrokeKind, TextureHandle, Ui, UiBuilder,
};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows::Win32::Foundation::HWND;

#[path = "send_ui.rs"]
mod send_ui;
#[path = "docs_ui.rs"]
mod docs_ui;

// ------------------------------------------------------------------ palette
const ISLAND: Color32 = Color32::from_rgb(7, 9, 16);
const CARD: Color32 = Color32::from_rgb(20, 24, 36);
const CARD_HI: Color32 = Color32::from_rgb(30, 36, 54);
const TEXT: Color32 = Color32::from_rgb(236, 240, 250);
const DIM: Color32 = Color32::from_rgb(140, 150, 175);
const ACCENT: Color32 = Color32::from_rgb(45, 160, 255);

const COLLAPSED: (f32, f32) = (196.0, 36.0);
const COLLAPSED_MEDIA: (f32, f32) = (290.0, 36.0);
const TOAST: (f32, f32) = (300.0, 44.0);
const EXPANDED: (f32, f32) = (624.0, 410.0);
const DOT: (f32, f32) = (24.0, 24.0);
const TOP: f32 = 6.0;

fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name("bold".into()))
}
fn reg(size: f32) -> FontId {
    FontId::proportional(size)
}

#[derive(Clone, Copy, PartialEq)]
enum Tab {
    Now,
    Clip,
    Send,
    Notes,
    Marks,
    Tools,
    Garden,
}
const TABS: [(Tab, &str); 7] = [
    (Tab::Now, "Hiện tại"),
    (Tab::Clip, "Clipboard"),
    (Tab::Send, "Gửi"),
    (Tab::Notes, "Ghi chú"),
    (Tab::Marks, "Bookmark"),
    (Tab::Tools, "Công cụ"),
    (Tab::Garden, "Vườn"),
];

#[derive(Default)]
struct Spring {
    x: f32,
    v: f32,
}
impl Spring {
    fn step(&mut self, target: f32, dt: f32) -> bool {
        let k: f32 = 380.0;
        let c = 2.0 * k.sqrt() * 0.74; // slightly under-damped → a little bounce
        for _ in 0..2 {
            let a = k * (target - self.x) - c * self.v;
            self.v += a * dt * 0.5;
            self.x += self.v * dt * 0.5;
        }
        if (target - self.x).abs() < 0.3 && self.v.abs() < 1.0 {
            self.x = target;
            self.v = 0.0;
            false
        } else {
            true
        }
    }
}

struct Toast {
    title: String,
    sub: String,
    until: f64,
}

pub struct Alex {
    hwnd: Option<HWND>,
    region: (i32, i32, i32, i32),
    w: Spring,
    h: Spring,
    expanded: bool,
    pinned_open: bool,
    hover_since: Option<f64>,
    leave_since: Option<f64>,
    hidden: bool,
    busy: Arc<AtomicBool>,
    tab: Tab,
    toast: Option<Toast>,

    data: Data,
    clips: Vec<ClipItem>,
    clip_rx: Receiver<ClipItem>,
    clip_filter_pinned: bool,
    thumbs: HashMap<u64, TextureHandle>,
    cb: Option<arboard::Clipboard>,

    media: media::Shared,
    media_tx: Sender<MediaCmd>,
    tray_rx: Receiver<TrayCmd>,

    note_input: String,
    mark_input: String,
    autostart: bool,
    last_topmost: f64,

    ctx: egui::Context,
    net: net::Shared,
    send_queue: Vec<std::path::PathBuf>,
    send_text: String,
    req_pinned: bool,
    links: Vec<String>,
    link_pinned: bool,
    extra_id: u64,
    doc_queue: Vec<std::path::PathBuf>,
    jobs: crate::docs::Jobs,

    /// idle mode: small faded dot that lets clicks pass through to windows behind
    dormant: bool,
    passthrough: bool,
    last_active: f64,
    hold_until: f64,
    ctrl_flag: Arc<AtomicBool>,
    pdf: Option<crate::pdfview::PdfWindow>,
    update: crate::update::Shared,
    update_seen: Option<String>,

    share_url_edit: String,
    share_in: String,
    account: Arc<Mutex<Option<share::Account>>>,
    show_pw: bool,
    share_setup: bool,
    share: Arc<Mutex<ShareSlot>>,
    /// files dropped on the island should upload to connected phones
    drop_to_phone: bool,
    /// paths accepted in the last few seconds, so one drop is not sent twice
    recent_drops: Vec<(std::path::PathBuf, f64)>,
    file_hover: bool,
}

#[derive(Clone)]
enum ShareView {
    Idle,
    Work { label: String, done: u64, total: u64 },
    Sent { link: String, code: Option<String>, hours: f64 },
    /// sent to the phones connected to this PC
    Phones { hours: f64 },
    Saved { name: String },
    Fail(String),
}

struct ShareSlot {
    view: ShareView,
    /// new account from the server: (id, secret, password)
    registered: Option<(String, String, String)>,
    registering: bool,
    new_password: Option<String>,
    phones: Option<Vec<share::Phone>>,
    /// files that arrived from a phone (names), for toasts
    inbox: Vec<String>,
    /// links sent from a phone (QR scan / pasted)
    urls: Vec<String>,
    /// files to put back in the send queue after a failed upload
    restore: Vec<std::path::PathBuf>,
    announce: bool,
    note: String,
    /// ping succeeded; the UI thread saves settings and closes the setup form
    linked: bool,
}

impl Alex {
    pub fn new(cc: &eframe::CreationContext) -> Self {
        let ctx = cc.egui_ctx.clone();
        let inbox_ctx = ctx.clone();
        setup_fonts(&ctx);
        setup_style(&ctx);

        let r = ctx.clone();
        let clip_rx = clip::spawn(move || r.request_repaint());
        let media_state: media::Shared = Arc::new(Mutex::new(None));
        let r = ctx.clone();
        let media_tx = media::spawn(media_state.clone(), move || r.request_repaint());
        let (tray_tx, tray_rx) = std::sync::mpsc::channel();
        let r = ctx.clone();
        sys::spawn_tray(tray_tx, move || r.request_repaint(), crate::ICON32);

        // fullscreen / presentation watcher
        let busy = Arc::new(AtomicBool::new(false));
        let (b, r) = (busy.clone(), ctx.clone());
        std::thread::spawn(move || loop {
            let now = sys::fullscreen_busy();
            if b.swap(now, Ordering::Relaxed) != now {
                r.request_repaint();
            }
            std::thread::sleep(Duration::from_secs(2));
        });

        let data = Data::load();
        let ctrl_flag = Arc::new(AtomicBool::new(false));
        let (f, r) = (ctrl_flag.clone(), ctx.clone());
        sys::spawn_double_ctrl(move || {
            f.store(true, Ordering::Relaxed);
            r.request_repaint();
        });
        let mut clips = Vec::new();
        let mut next_pin_id = u64::MAX / 2; // ids for restored pins
        for t in &data.pins {
            next_pin_id += 1;
            clips.push(ClipItem {
                id: next_pin_id,
                hash: clip::hash_bytes(t.as_bytes()),
                kind: ClipKind::Text(t.clone()),
                time: Instant::now(),
                pinned: true,
            });
        }
        let (w, h) = COLLAPSED;
        let wake = ctx.clone();
        sys::set_drop_wake(move || wake.request_repaint());
        let share_url_edit = data.settings.share_url.clone();
        let share_setup = share_url_edit.trim().is_empty();
        let st = &data.settings;
        let account = Arc::new(Mutex::new(
            (!st.device_id.is_empty() && !st.device_secret.is_empty() && st.device_server == st.share_url)
                .then(|| share::Account { url: st.share_url.clone(), id: st.device_id.clone(), secret: st.device_secret.clone() }),
        ));
        // ALEX_TAB=<0-5> opens the island pinned on a tab (handy for screenshots/debugging)
        let debug_tab = std::env::var("ALEX_TAB").ok().and_then(|v| v.parse::<usize>().ok()).filter(|i| *i < TABS.len());
        let alex = Alex {
            hwnd: None,
            region: (-1, -1, -1, -1),
            w: Spring { x: w, v: 0.0 },
            h: Spring { x: h, v: 0.0 },
            expanded: debug_tab.is_some(),
            pinned_open: debug_tab.is_some(),
            hover_since: None,
            leave_since: None,
            hidden: false,
            busy,
            tab: debug_tab.map(|i| TABS[i].0).unwrap_or(Tab::Now),
            toast: None,
            data,
            clips,
            clip_rx,
            clip_filter_pinned: false,
            thumbs: HashMap::new(),
            cb: arboard::Clipboard::new().ok(),
            media: media_state,
            media_tx,
            tray_rx,
            note_input: String::new(),
            mark_input: String::new(),
            autostart: sys::autostart_enabled(),
            last_topmost: 0.0,
            net: {
                let r = ctx.clone();
                net::start(move || r.request_repaint())
            },
            ctx,
            send_queue: Vec::new(),
            send_text: String::new(),
            req_pinned: false,
            links: Vec::new(),
            link_pinned: false,
            extra_id: u64::MAX / 4,
            doc_queue: Vec::new(),
            jobs: Default::default(),
            dormant: false,
            passthrough: false,
            last_active: 0.0,
            hold_until: 0.0,
            ctrl_flag,
            // ALEX_PDF=<file> [ALEX_PDF_MODE=view|draw|pages] opens the PDF window at start (debug/screenshots)
            share_url_edit,
            share_in: String::new(),
            account,
            show_pw: false,
            share_setup,
            share: Arc::new(Mutex::new(ShareSlot {
                view: ShareView::Idle,
                announce: false,
                note: String::new(),
                linked: false,
                registered: None,
                registering: false,
                new_password: None,
                phones: None,
                inbox: Vec::new(),
                urls: Vec::new(),
                restore: Vec::new(),
            })),
            update: {
                let r = cc.egui_ctx.clone();
                crate::update::spawn(move || r.request_repaint())
            },
            update_seen: None,
            drop_to_phone: false,
            recent_drops: Vec::new(),
            file_hover: false,
            pdf: std::env::var_os("ALEX_PDF").map(|p| {
                use crate::pdfview::Mode;
                let mode = match std::env::var("ALEX_PDF_MODE").as_deref() {
                    Ok("draw") => Mode::Draw,
                    Ok("pages") => Mode::Pages,
                    _ => Mode::View,
                };
                crate::pdfview::PdfWindow::open(p.into(), mode, &cc.egui_ctx)
            }),
        };
        spawn_inbox(alex.account.clone(), alex.share.clone(), inbox_ctx);
        alex
    }

    fn toast(&mut self, now: f64, title: &str, sub: &str) {
        self.toast = Some(Toast { title: title.into(), sub: sub.into(), until: now + 1.8 });
    }

    fn save_pins(&mut self) {
        self.data.pins = self
            .clips
            .iter()
            .filter(|c| c.pinned)
            .filter_map(|c| if let ClipKind::Text(t) = &c.kind { Some(t.clone()) } else { None })
            .collect();
        self.data.save();
    }

    fn ingest_clips(&mut self, now: f64) {
        while let Ok(mut item) = self.clip_rx.try_recv() {
            if let Some(i) = self.clips.iter().position(|c| c.hash == item.hash) {
                let old = self.clips.remove(i);
                item.pinned = old.pinned;
                self.thumbs.remove(&old.id);
            } else {
                let label = item.label();
                self.toast(now, "Đã sao chép", label);
            }
            self.clips.insert(0, item);
        }
        // limits: 60 entries, 6 images (unpinned evicted first, oldest first)
        let mut imgs = 0;
        let mut n = 0;
        self.clips.retain(|c| {
            if c.pinned {
                return true;
            }
            n += 1;
            if matches!(c.kind, ClipKind::Image { .. }) {
                imgs += 1;
                if imgs > 6 {
                    return false;
                }
            }
            n <= 60
        });
        let ids: Vec<u64> = self.clips.iter().map(|c| c.id).collect();
        self.thumbs.retain(|id, _| ids.contains(id));
    }

    fn copy_item(&mut self, idx: usize, now: f64) {
        let Some(cb) = self.cb.as_mut() else { return };
        let item = &self.clips[idx];
        let ok = match &item.kind {
            ClipKind::Text(t) => cb.set_text(t.clone()).is_ok(),
            ClipKind::Image { w, h, rgba } => cb
                .set_image(arboard::ImageData { width: *w, height: *h, bytes: std::borrow::Cow::Borrowed(rgba) })
                .is_ok(),
            ClipKind::Files(f) => cb.set().file_list(f).is_ok(),
        };
        if ok {
            let label = item.label();
            self.toast(now, "Đã chép lại", label);
        }
    }

    fn share_events(&mut self, now: f64) {
        let (linked, note, announce, restore) = {
            let mut g = self.share.lock().unwrap();
            let restore = std::mem::take(&mut g.restore);
            let linked = g.linked;
            if linked {
                g.linked = false;
            }
            let announce = if g.announce {
                g.announce = false;
                match &g.view {
                    ShareView::Sent { link, .. } => Some(("sent", link.clone())),
                    ShareView::Saved { name } => Some(("saved", name.clone())),
                    ShareView::Phones { .. } => Some(("phones", String::new())),
                    ShareView::Fail(e) => Some(("fail", e.clone())),
                    _ => None,
                }
            } else {
                None
            };
            (linked, g.note.clone(), announce, restore)
        };
        if !restore.is_empty() {
            for p in restore {
                if !self.send_queue.contains(&p) {
                    self.send_queue.push(p);
                }
            }
        }
        if linked {
            self.share_setup = false;
            self.toast(now, "Đã kết nối máy chủ", &note);
            if let Some(t) = &mut self.toast {
                t.until = now + 3.2;
            }
        }
        let (registered, new_pw, inbox, urls) = {
            let mut g = self.share.lock().unwrap();
            (g.registered.take(), g.new_password.take(), std::mem::take(&mut g.inbox), std::mem::take(&mut g.urls))
        };
        if !urls.is_empty() {
            // show the island with the link, even if it was a faded dot
            for u in urls {
                if !self.links.contains(&u) {
                    self.links.insert(0, u);
                }
            }
            self.links.truncate(5);
            self.dormant = false;
            self.hidden = false;
            self.expanded = true;
            self.pinned_open = true;
            self.link_pinned = true;
            self.tab = Tab::Send;
            self.last_active = now;
        } else if self.links.is_empty() && self.link_pinned {
            self.link_pinned = false;
            self.pinned_open = false;
        }
        if let Some((id, secret, pw)) = registered {
            let st = &mut self.data.settings;
            st.device_id = id.clone();
            st.device_secret = secret.clone();
            st.device_password = pw;
            st.device_server = st.share_url.clone();
            *self.account.lock().unwrap() = Some(share::Account { url: st.share_url.clone(), id: id.clone(), secret });
            self.data.save();
            self.toast(now, "Đã tạo tài khoản", &format!("ID {}", share::pretty_id(&id)));
        }
        if let Some(pw) = new_pw {
            self.data.settings.device_password = pw;
            self.data.save();
            self.toast(now, "Đã đổi mật khẩu", "Các điện thoại cần kết nối lại");
        }
        for name in inbox {
            self.toast(now, "Nhận từ điện thoại", &truncate(&name, 28));
            if let Some(t) = &mut self.toast {
                t.until = now + 3.5;
            }
        }
        if self.account.lock().unwrap().is_none() && !self.share_setup && !self.share.lock().unwrap().registering {
            let failed = matches!(self.share.lock().unwrap().view, ShareView::Fail(_));
            if !failed {
                self.share_register();
            }
        }
        match announce {
            Some(("sent", link)) => {
                if let Some(cb) = self.cb.as_mut() {
                    let _ = cb.set_text(link);
                }
                self.toast(now, "Đã chép link", "Dán cho người nhận");
                if let Some(t) = &mut self.toast {
                    t.until = now + 3.2;
                }
            }
            Some(("saved", name)) => self.toast(now, "Đã nhận file", &truncate(&name, 28)),
            Some(("phones", _)) => {
                self.toast(now, "Đã gửi sang điện thoại", "Mở trang Alex trên điện thoại, tab Nhận");
            }
            Some(("fail", e)) => self.toast(now, "Gửi qua Internet", &truncate(&e, 36)),
            _ => {}
        }
        if self.drop_to_phone && !self.share_busy() && !self.send_queue.is_empty() && self.account.lock().unwrap().is_some() {
            self.share_upload(true);
        }
        if self.share_busy() {
            self.ctx.request_repaint_after(Duration::from_millis(200));
        }
    }

    fn share_busy(&self) -> bool {
        matches!(self.share.lock().unwrap().view, ShareView::Work { .. })
    }

    fn share_work(&self, label: &str) {
        let mut g = self.share.lock().unwrap();
        g.view = ShareView::Work { label: label.into(), done: 0, total: 0 };
        g.announce = false;
    }

    fn share_save_check(&mut self) {
        let url = self.share_url_edit.trim().trim_end_matches('/').to_string();
        if url.is_empty() {
            let mut g = self.share.lock().unwrap();
            g.view = ShareView::Fail("Nhập địa chỉ máy chủ".into());
            g.announce = true;
            return;
        }
        if url != self.data.settings.device_server {
            // an account belongs to one server: forget it and register again there
            *self.account.lock().unwrap() = None;
            let st = &mut self.data.settings;
            st.device_id.clear();
            st.device_secret.clear();
            st.device_password.clear();
        }
        self.data.settings.share_url = url;
        self.data.save();
        self.share_setup = false;
        self.share.lock().unwrap().view = ShareView::Idle;
    }

    fn share_register(&mut self) {
        let url = self.data.settings.share_url.clone();
        let name = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "Máy tính".into());
        let slot = self.share.clone();
        let ctx = self.ctx.clone();
        slot.lock().unwrap().registering = true;
        std::thread::spawn(move || {
            let r = share::register(&url, &name);
            let mut g = slot.lock().unwrap();
            g.registering = false;
            match r {
                Ok(acc) => g.registered = Some(acc),
                Err(e) => g.view = ShareView::Fail(format!("Không tạo được tài khoản: {e}")),
            }
            ctx.request_repaint();
        });
    }

    fn share_change_password(&mut self) {
        let Some(acc) = self.account.lock().unwrap().clone() else { return };
        let (slot, ctx) = (self.share.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let r = share::set_password(&acc, None);
            let mut g = slot.lock().unwrap();
            match r {
                Ok(pw) => {
                    g.new_password = Some(pw);
                    g.phones = Some(Vec::new());
                }
                Err(e) => {
                    g.view = ShareView::Fail(e);
                    g.announce = true;
                }
            }
            ctx.request_repaint();
        });
    }

    fn share_load_phones(&self) {
        let Some(acc) = self.account.lock().unwrap().clone() else { return };
        let (slot, ctx) = (self.share.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            if let Ok(list) = share::phones(&acc) {
                slot.lock().unwrap().phones = Some(list);
            }
            ctx.request_repaint();
        });
    }

    fn share_disconnect(&self, sid: String) {
        let Some(acc) = self.account.lock().unwrap().clone() else { return };
        let (slot, ctx) = (self.share.clone(), self.ctx.clone());
        std::thread::spawn(move || {
            let _ = share::disconnect_phone(&acc, &sid);
            if let Ok(list) = share::phones(&acc) {
                slot.lock().unwrap().phones = Some(list);
            }
            ctx.request_repaint();
        });
    }

    fn accept_drop(&mut self, paths: Vec<String>, now: f64) {
        self.recent_drops.retain(|(_, t)| now - *t < 4.0);
        let mut fresh = Vec::new();
        for p in paths {
            let p = std::path::PathBuf::from(p);
            if !p.exists() || fresh.contains(&p) || self.recent_drops.iter().any(|(q, _)| q == &p) {
                continue;
            }
            fresh.push(p);
        }
        if fresh.is_empty() {
            return;
        }
        let for_tools = self.expanded && self.tab == Tab::Tools;
        self.dormant = false;
        self.hidden = false;
        self.expanded = true;
        self.hold_until = now + 6.0;
        self.last_active = now;
        for p in &fresh {
            self.recent_drops.push((p.clone(), now));
        }
        if for_tools {
            for p in fresh {
                if crate::docs::is_supported(&p) && !self.doc_queue.contains(&p) {
                    self.doc_queue.push(p);
                }
            }
            self.toast(now, "Đã nhận file", "Chọn công cụ ở tab Công cụ");
            return;
        }
        for p in fresh {
            if !self.send_queue.contains(&p) {
                self.send_queue.push(p);
            }
        }
        self.tab = Tab::Send;
        self.drop_to_phone = true;
        if self.share_busy() {
            self.toast(now, "Đã nhận file", "Sẽ gửi sang điện thoại ngay sau file đang tải");
            return;
        }
        if self.account.lock().unwrap().is_some() {
            self.toast(now, "Đang gửi sang điện thoại", "");
            self.share_upload(true);
        } else {
            self.toast(now, "Đã nhận file", "Đang kết nối để gửi sang điện thoại");
        }
    }

    fn share_upload(&mut self, to_phones: bool) {
        if self.share_busy() || self.send_queue.is_empty() {
            return;
        }
        let Some(acc) = self.account.lock().unwrap().clone() else { return };
        let paths = std::mem::take(&mut self.send_queue);
        self.drop_to_phone = false;
        let mode = if to_phones {
            share::Mode::Phones
        } else if self.data.settings.share_with_code {
            share::Mode::Code
        } else {
            share::Mode::Link
        };
        self.share_work(if paths.len() == 1 && paths[0].is_file() { "Đang tải lên…" } else { "Đang nén và tải lên…" });
        let slot = self.share.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let slot2 = slot.clone();
            let ctx2 = ctx.clone();
            let res = share::upload(&acc, &paths, mode, move |done, total| {
                if let Ok(mut g) = slot2.lock() {
                    if let ShareView::Work { label, .. } = &g.view {
                        let label = label.clone();
                        g.view = ShareView::Work { label, done, total };
                    }
                }
                ctx2.request_repaint();
            });
            let mut g = slot.lock().unwrap();
            match res {
                Ok(s) if mode == share::Mode::Phones => {
                    g.view = ShareView::Phones { hours: s.hours };
                    g.announce = true;
                }
                Ok(s) => {
                    g.view = ShareView::Sent { link: s.link, code: s.code, hours: s.hours };
                    g.announce = true;
                }
                Err(e) => {
                    g.view = ShareView::Fail(e);
                    g.restore = paths;
                    g.announce = true;
                }
            }
            ctx.request_repaint();
        });
    }

    fn share_receive(&mut self) {
        if self.share_busy() {
            return;
        }
        let input = self.share_in.trim().to_string();
        if input.is_empty() {
            let mut g = self.share.lock().unwrap();
            g.view = ShareView::Fail("Nhập mã 6 số hoặc dán link".into());
            g.announce = true;
            return;
        }
        let url = if let Some((left, _)) = input.split_once('#').filter(|_| input.contains("/s/")) {
            left.rsplit_once("/s/").map(|(s, _)| s.to_string()).unwrap_or_else(|| self.data.settings.share_url.clone())
        } else {
            self.data.settings.share_url.clone()
        };
        self.share_work("Đang tải về…");
        let slot = self.share.clone();
        let ctx = self.ctx.clone();
        std::thread::spawn(move || {
            let dir = net::downloads_dir();
            let slot2 = slot.clone();
            let ctx2 = ctx.clone();
            let res = share::receive(&url, &input, &dir, move |done, total| {
                if let Ok(mut g) = slot2.lock() {
                    g.view = ShareView::Work { label: "Đang tải về…".into(), done, total };
                }
                ctx2.request_repaint();
            });
            let mut g = slot.lock().unwrap();
            match res {
                Ok(p) => {
                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
                    g.view = ShareView::Saved { name };
                    g.announce = true;
                }
                Err(e) => {
                    g.view = ShareView::Fail(e);
                    g.announce = true;
                }
            }
            ctx.request_repaint();
        });
    }

    fn water_rollover(&mut self) {
        let t = sys::now();
        let today = format!("{:04}-{:02}-{:02}", t.year, t.month, t.day);
        let w = &mut self.data.water;
        if w.day != today {
            if !w.day.is_empty() {
                w.streak = if w.count >= w.goal { w.streak + 1 } else { 0 };
            }
            w.count = 0;
            w.day = today;
            self.data.save();
        }
    }
}

fn setup_fonts(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::empty();
    let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
    let mut add = |name: &str, files: &[&str]| -> bool {
        for f in files {
            if let Ok(bytes) = std::fs::read(format!("{dir}{f}")) {
                fonts.font_data.insert(name.into(), Arc::new(FontData::from_owned(bytes)));
                return true;
            }
        }
        false
    };
    add("ui", &["segoeui.ttf", "arial.ttf", "tahoma.ttf"]);
    let has_bold = add("ui-bold", &["seguisb.ttf", "segoeuib.ttf", "arialbd.ttf"]);
    let has_sym = add("sym", &["seguisym.ttf"]);
    let mut chain = vec!["ui".to_string()];
    if has_sym {
        chain.push("sym".into());
    }
    let mut bold_chain = if has_bold { vec!["ui-bold".to_string()] } else { vec![] };
    bold_chain.extend(chain.clone());
    fonts.families.insert(FontFamily::Proportional, chain.clone());
    fonts.families.insert(FontFamily::Monospace, chain);
    fonts.families.insert(FontFamily::Name("bold".into()), bold_chain);
    ctx.set_fonts(fonts);
}

fn setup_style(ctx: &egui::Context) {
    ctx.set_theme(egui::Theme::Dark);
    ctx.all_styles_mut(|s| {
        use egui::TextStyle::*;
        s.visuals = egui::Visuals::dark();
        s.text_styles = [
            (Small, reg(11.0)),
            (Body, reg(13.5)),
            (Button, reg(13.0)),
            (Heading, bold(18.0)),
            (Monospace, reg(13.0)),
        ]
        .into();
        s.spacing.item_spacing = vec2(8.0, 6.0);
        s.spacing.button_padding = vec2(10.0, 5.0);
        s.spacing.scroll.bar_width = 6.0;
        s.spacing.scroll.floating = true;
        let v = &mut s.visuals;
        v.override_text_color = Some(TEXT);
        v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
        v.selection.stroke = Stroke::new(1.0, ACCENT);
        v.extreme_bg_color = CARD;
        v.text_edit_bg_color = Some(CARD);
        let r = CornerRadius::same(8);
        for w in [&mut v.widgets.inactive, &mut v.widgets.hovered, &mut v.widgets.active, &mut v.widgets.noninteractive] {
            w.corner_radius = r;
        }
        v.widgets.inactive.weak_bg_fill = CARD;
        v.widgets.inactive.bg_fill = CARD_HI;
        v.widgets.inactive.bg_stroke = Stroke::new(1.0, Color32::from_white_alpha(28));
        v.widgets.hovered.weak_bg_fill = CARD_HI;
        v.widgets.hovered.bg_fill = CARD_HI;
        v.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT.gamma_multiply(0.5));
        v.widgets.active.weak_bg_fill = ACCENT.gamma_multiply(0.5);
        v.widgets.noninteractive.bg_stroke = Stroke::new(1.0, Color32::from_white_alpha(14));
        v.widgets.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.inactive.fg_stroke = Stroke::new(1.0, TEXT);
        v.widgets.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    });
}

fn ago(t: Instant) -> String {
    let s = t.elapsed().as_secs();
    match s {
        0..=44 => "Vừa xong".into(),
        45..=3599 => format!("{} phút trước", (s + 30) / 60),
        _ => format!("{} giờ trước", s / 3600),
    }
}

fn truncate(s: &str, n: usize) -> String {
    let one: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if one.chars().count() > n {
        one.chars().take(n).collect::<String>() + "…"
    } else {
        one
    }
}

fn file_name(p: &str) -> String {
    std::path::Path::new(p)
        .file_name()
        .map(|f| f.to_string_lossy().into_owned())
        .unwrap_or_else(|| p.to_string())
}

fn card<R>(ui: &mut Ui, add: impl FnOnce(&mut Ui) -> R) -> R {
    egui::Frame::new()
        .fill(CARD)
        .corner_radius(12)
        .inner_margin(Margin::same(12))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui)
        })
        .inner
}

impl eframe::App for Alex {
    fn clear_color(&self, _v: &egui::Visuals) -> [f32; 4] {
        [0.0; 4]
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let (now, dt, ppp) = ctx.input(|i| (i.time, i.stable_dt.min(1.0 / 30.0), i.pixels_per_point));

        if self.hwnd.is_none() {
            self.hwnd = sys::find_window();
            if let Some(h) = self.hwnd {
                sys::place_window(h, (WIN_W * ppp) as i32, (WIN_H * ppp) as i32);
            }
        }
        // separate PDF window (view / draw / reorder); must run before any early return
        if let Some(w) = &mut self.pdf {
            w.show(&ctx);
            if !w.open {
                self.pdf = None;
            }
        }
        // Some apps grab topmost too; re-assert occasionally.
        if now - self.last_topmost > 5.0 {
            self.last_topmost = now;
            if let Some(h) = self.hwnd {
                sys::keep_topmost(h);
            }
        }

        while let Ok(cmd) = self.tray_rx.try_recv() {
            match cmd {
                TrayCmd::Toggle => {
                    self.hidden = !self.hidden;
                    self.dormant = false;
                    self.last_active = now;
                }
                TrayCmd::Quit => {
                    self.data.save();
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
        self.ingest_clips(now);
        self.net_events(now);
        self.share_events(now);
        self.water_rollover();
        let media = self.media.lock().unwrap().clone();

        // ---------------------------------------------------- double Ctrl
        if self.ctrl_flag.swap(false, Ordering::Relaxed) {
            if self.dormant || self.hidden || !self.expanded {
                self.dormant = false;
                self.hidden = false;
                self.expanded = true;
                self.hold_until = now + 4.0; // stay open a moment even if the mouse is elsewhere
                self.last_active = now;
            } else {
                self.expanded = false;
                self.pinned_open = false;
                self.dormant = self.data.settings.auto_dormant;
            }
        }
        if let Some(h) = self.hwnd {
            sys::ensure_toolwindow(h); // winit may rewrite ex-styles when toggling passthrough
        }

        // ---------------------------------------------------- hover logic
        let dragging_files = self.file_hover || ctx.input(|i| !i.raw.hovered_files.is_empty());
        let island_rect = Rect::from_min_size(pos2((WIN_W - self.w.x) / 2.0, TOP), vec2(self.w.x, self.h.x));
        let hovered = dragging_files
            || ctx.input(|i| i.pointer.hover_pos()).is_some_and(|p| island_rect.expand(2.0).contains(p));
        if hovered {
            self.leave_since = None;
            self.hold_until = 0.0;
            let since = *self.hover_since.get_or_insert(now);
            if !self.expanded && now - since >= 0.12 {
                self.expanded = true;
            } else if !self.expanded {
                ctx.request_repaint_after(Duration::from_millis(130));
            }
        } else {
            self.hover_since = None;
            if self.expanded && !self.pinned_open && now < self.hold_until {
                ctx.request_repaint_after(Duration::from_secs_f64(self.hold_until - now + 0.01));
            } else if self.expanded && !self.pinned_open {
                let since = *self.leave_since.get_or_insert(now);
                if now - since >= 0.4 {
                    self.expanded = false;
                    self.leave_since = None;
                } else {
                    ctx.request_repaint_after(Duration::from_millis(420));
                }
            }
        }

        if self.toast.as_ref().is_some_and(|t| now >= t.until) {
            self.toast = None;
        }
        // ---------------------------------------------------- idle → dot
        let st = &self.data.settings;
        if hovered || self.expanded || self.pinned_open || self.toast.is_some() {
            self.last_active = now;
        }
        if !st.auto_dormant {
            self.dormant = false;
        } else if !self.dormant {
            let idle = now - self.last_active;
            if idle >= st.idle_secs as f64 {
                self.dormant = true;
            } else {
                ctx.request_repaint_after(Duration::from_secs_f64(st.idle_secs as f64 - idle + 0.05));
            }
        }
        // ---------------------------------------------------- self-update
        let up = self.update.lock().unwrap().status.clone();
        if let crate::update::Status::Ready { version, .. } = &up {
            if self.update_seen.as_deref() != Some(version.as_str()) {
                self.update_seen = Some(version.clone());
                self.toast(now, &format!("Có bản Alex {version}"), "Cập nhật ở tab Công cụ");
            }
            // install on its own only when nobody has touched the island for a minute
            if self.data.settings.auto_update && !self.expanded && !hovered && now - self.last_active > 60.0 {
                self.data.save();
                if crate::update::install(&self.update).is_ok() {
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            } else if self.data.settings.auto_update {
                ctx.request_repaint_after(Duration::from_secs(20));
            }
        }
        if self.dormant != self.passthrough {
            ctx.send_viewport_cmd(egui::ViewportCommand::MousePassthrough(self.dormant));
            self.passthrough = self.dormant;
            self.hover_since = None;
        }

        let playing = media.as_ref().is_some_and(|m| m.playing);
        let target = if self.dormant && self.toast.is_none() {
            DOT
        } else if self.expanded {
            EXPANDED
        } else if self.toast.is_some() {
            TOAST
        } else if playing {
            COLLAPSED_MEDIA
        } else {
            COLLAPSED
        };
        let anim_w = self.w.step(target.0, dt);
        let anim_h = self.h.step(target.1, dt);
        if anim_w || anim_h {
            ctx.request_repaint();
        }

        // ---------------------------------------------------- hit region
        let hide = self.hidden || self.busy.load(Ordering::Relaxed);
        let rect = Rect::from_min_size(pos2((WIN_W - self.w.x) / 2.0, TOP), vec2(self.w.x, self.h.x));
        let reg = if hide {
            (0, 0, 0, 0)
        } else {
            let r = rect.expand(1.0);
            (
                (r.min.x * ppp).floor() as i32,
                (r.min.y * ppp).floor() as i32,
                (r.max.x * ppp).ceil() as i32,
                (r.max.y * ppp).ceil() as i32,
            )
        };
        if reg != self.region {
            if let Some(h) = self.hwnd {
                sys::set_region(h, reg.0, reg.1, reg.2, reg.3);
                self.region = reg;
            }
        }
        let mut dropped: Vec<String> = ctx.input(|i| {
            i.raw.dropped_files.iter().map(|f| f.path().display().to_string()).filter(|p| !p.is_empty()).collect()
        });
        if let Some(h) = self.hwnd {
            let pad = (32.0 * ppp) as i32;
            let hit = sys::track_drop_target(h, reg, pad, !hide);
            self.file_hover = hit.hovering;
            dropped.extend(hit.paths);
        } else {
            self.file_hover = false;
        }
        if self.file_hover && self.h.x < 120.0 {
            self.tab = Tab::Send;
            self.expanded = true;
            self.dormant = false;
        }
        self.accept_drop(dropped, now);
        if hide {
            return;
        }

        // ---------------------------------------------------- draw island
        let painter = ui.painter().clone();
        let radius = (rect.height() / 2.0).min(24.0);
        let fade = if !self.dormant { 1.0 } else if self.toast.is_some() { 0.85 } else { 0.4 };
        painter.rect_filled(rect, radius, ISLAND.gamma_multiply(fade));
        painter.rect_stroke(rect.shrink(0.5), radius, Stroke::new(1.0, Color32::from_rgba_unmultiplied(70, 130, 255, 70).gamma_multiply(fade)), StrokeKind::Inside);
        if self.file_hover {
            painter.rect_stroke(rect.shrink(1.5), radius, Stroke::new(2.0, logo::C1), StrokeKind::Inside);
        }
        if rect.width() < 60.0 && self.toast.is_none() {
            // the dot: just the mark
            logo::paint_mark(&painter, Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), fade + 0.1);
            return self.schedule(&ctx, now);
        }

        let t_open = ((self.h.x - COLLAPSED.1) / (EXPANDED.1 - COLLAPSED.1)).clamp(0.0, 1.0);
        if self.expanded && t_open > 0.55 {
            let alpha = ((t_open - 0.55) / 0.45).clamp(0.0, 1.0);
            let inner = rect.shrink2(vec2(16.0, 12.0));
            let mut child = ui.new_child(UiBuilder::new().max_rect(inner).layout(Layout::top_down(Align::Min)));
            child.set_clip_rect(rect.shrink(2.0));
            child.multiply_opacity(alpha);
            self.expanded_ui(&mut child, now, media.as_ref());
        } else if !self.expanded || t_open < 0.2 {
            let alpha = if self.expanded { 1.0 - t_open / 0.2 } else { 1.0 };
            self.collapsed_ui(&painter, rect, alpha * fade.max(0.85), media.as_ref(), now);
        }
        self.schedule(&ctx, now);
    }
}

impl Alex {
    fn schedule(&self, ctx: &egui::Context, now: f64) {

        // keep the clock fresh: wake at the next minute boundary (or toast end)
        let t = sys::now();
        let mut next = 60.5 - t.second as f64;
        if let Some(to) = &self.toast {
            next = next.min(to.until - now + 0.01);
        }
        if self.expanded && self.tab == Tab::Clip {
            next = next.min(30.0);
        }
        ctx.request_repaint_after(Duration::from_secs_f64(next.max(0.01)));
    }

    fn collapsed_ui(&self, p: &egui::Painter, rect: Rect, alpha: f32, media: Option<&MediaInfo>, now: f64) {
        let c = |col: Color32| col.gamma_multiply(alpha);
        let cy = rect.center().y;
        let p = p.with_clip_rect(rect.shrink(2.0));
        logo::paint_mark(&p, Rect::from_center_size(pos2(rect.min.x + 22.0, cy), vec2(22.0, 22.0)), alpha);

        if let Some(t) = &self.toast {
            let x = rect.min.x + 42.0;
            p.circle_filled(pos2(x + 8.0, cy), 8.0, c(Color32::from_rgb(40, 200, 120)));
            p.text(pos2(x + 8.0, cy), Align2::CENTER_CENTER, "✓", bold(11.0), c(Color32::BLACK));
            p.text(pos2(x + 24.0, cy), Align2::LEFT_CENTER, &t.title, bold(13.5), c(TEXT));
            p.text(pos2(rect.max.x - 18.0, cy), Align2::RIGHT_CENTER, &t.sub, reg(12.5), c(DIM));
            let _ = now;
            return;
        }

        let tm = sys::now();
        let clock = format!("{:02}:{:02}", tm.hour, tm.minute);
        p.text(pos2(rect.max.x - 16.0, cy), Align2::RIGHT_CENTER, clock, bold(13.5), c(TEXT));

        if let Some(m) = media.filter(|m| m.playing) {
            // static equalizer bars + title
            let x0 = rect.min.x + 44.0;
            for (i, h) in [7.0, 12.0, 9.0].iter().enumerate() {
                let x = x0 + i as f32 * 4.5;
                p.rect_filled(Rect::from_center_size(pos2(x, cy), vec2(2.5, *h)), 1.0, c(logo::grad(i as f32 / 2.0)));
            }
            let title = truncate(&m.title, 26);
            p.text(pos2(x0 + 16.0, cy), Align2::LEFT_CENTER, title, reg(12.5), c(TEXT));
        }
    }

    fn expanded_ui(&mut self, ui: &mut Ui, now: f64, media: Option<&MediaInfo>) {
        // header -------------------------------------------------------
        ui.horizontal(|ui| {
            ui.set_height(28.0);
            let (r, _) = ui.allocate_exact_size(vec2(26.0, 26.0), Sense::hover());
            logo::paint_mark(ui.painter(), r, 1.0);
            let (r, _) = ui.allocate_exact_size(vec2(logo::WORD_W * 0.13 + 6.0, 26.0), Sense::hover());
            logo::paint_word(ui.painter(), pos2(r.min.x + 2.0, r.center().y - 6.5), 13.0, 1.0);
            ui.add_space(2.0);
            ui.spacing_mut().button_padding = vec2(8.0, 4.0);
            ui.spacing_mut().item_spacing.x = 3.0;
            for (tab, name) in TABS {
                let sel = self.tab == tab;
                let txt = RichText::new(name).size(12.5).color(if sel { TEXT } else { DIM });
                let b = egui::Button::new(txt)
                    .fill(if sel { ACCENT.gamma_multiply(0.28) } else { Color32::TRANSPARENT })
                    .stroke(if sel { Stroke::new(1.0, ACCENT.gamma_multiply(0.6)) } else { Stroke::NONE })
                    .corner_radius(14);
                if ui.add(b).clicked() {
                    self.tab = tab;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let pin = egui::Button::new(RichText::new(if self.pinned_open { "📌" } else { "📍" }).size(12.0))
                    .fill(if self.pinned_open { ACCENT.gamma_multiply(0.3) } else { Color32::TRANSPARENT })
                    .corner_radius(14);
                if ui.add(pin).on_hover_text("Giữ đảo mở").clicked() {
                    self.pinned_open = !self.pinned_open;
                }
            });
        });
        ui.add_space(4.0);
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 1.0), Sense::hover());
        ui.painter().rect_filled(r, 0.0, Color32::from_white_alpha(14));
        ui.add_space(6.0);

        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| match self.tab {
            Tab::Now => self.tab_now(ui, media),
            Tab::Clip => self.tab_clip(ui, now),
            Tab::Send => self.tab_send(ui),
            Tab::Notes => self.tab_notes(ui),
            Tab::Marks => self.tab_marks(ui),
            Tab::Tools => self.tab_tools(ui),
            Tab::Garden => self.tab_garden(ui),
        });
    }

    fn tab_now(&mut self, ui: &mut Ui, media: Option<&MediaInfo>) {
        const DAYS: [&str; 7] = ["Chủ nhật", "Thứ Hai", "Thứ Ba", "Thứ Tư", "Thứ Năm", "Thứ Sáu", "Thứ Bảy"];
        let t = sys::now();
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                ui.label(RichText::new(format!("{:02}:{:02}", t.hour, t.minute)).font(bold(46.0)));
                ui.label(
                    RichText::new(format!("{}, {:02}/{:02}/{}", DAYS[t.weekday as usize % 7], t.day, t.month, t.year))
                        .color(DIM),
                );
            });
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(8.0);
                let todo = self.data.notes.iter().filter(|n| !n.done).count();
                let w = &self.data.water;
                for (k, v) in [
                    ("Clipboard", format!("{} mục", self.clips.len())),
                    ("Việc cần làm", format!("{todo} việc")),
                    ("Nước hôm nay", format!("{}/{} cốc", w.count, w.goal)),
                ] {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(k).color(DIM));
                        ui.label(RichText::new(v).font(bold(13.5)));
                    });
                }
            });
        });
        ui.add_space(8.0);
        card(ui, |ui| {
            match media {
                Some(m) => {
                    ui.horizontal(|ui| {
                        let (r, _) = ui.allocate_exact_size(vec2(52.0, 52.0), Sense::hover());
                        let mut mesh = egui::epaint::Mesh::default();
                        mesh.add_colored_rect(r, logo::C2);
                        for (i, v) in mesh.vertices.iter_mut().enumerate() {
                            v.color = logo::grad(i as f32 / 3.0);
                        }
                        ui.painter().add(egui::Shape::mesh(mesh));
                        ui.painter().text(r.center(), Align2::CENTER_CENTER, "♪", bold(24.0), Color32::WHITE);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(truncate(&m.title, 40)).font(bold(15.0)));
                            let sub = [m.artist.as_str(), m.app.as_str()]
                                .into_iter()
                                .filter(|s| !s.is_empty())
                                .collect::<Vec<_>>()
                                .join(" · ");
                            ui.label(RichText::new(truncate(&sub, 50)).color(DIM));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let btn = |s: &str| egui::Button::new(RichText::new(s).size(16.0)).corner_radius(18).min_size(vec2(36.0, 36.0));
                            if ui.add(btn("⏭")).clicked() {
                                let _ = self.media_tx.send(MediaCmd::Next);
                            }
                            if ui.add(btn(if m.playing { "⏸" } else { "▶" })).clicked() {
                                let _ = self.media_tx.send(MediaCmd::PlayPause);
                            }
                            if ui.add(btn("⏮")).clicked() {
                                let _ = self.media_tx.send(MediaCmd::Prev);
                            }
                        });
                    });
                }
                None => {
                    ui.label(RichText::new("Không có nhạc đang phát").color(DIM));
                    ui.label(RichText::new("Mở Spotify, YouTube hay Media Player, đảo sẽ hiện bài hát và nút điều khiển.").small().color(DIM));
                }
            }
        });
        ui.add_space(6.0);
        let next: Vec<String> = self.data.notes.iter().filter(|n| !n.done).take(3).map(|n| n.text.clone()).collect();
        if !next.is_empty() {
            card(ui, |ui| {
                ui.label(RichText::new("Việc tiếp theo").color(DIM).small());
                for n in next {
                    ui.label(format!("•  {}", truncate(&n, 60)));
                }
            });
        }
    }

    fn tab_clip(&mut self, ui: &mut Ui, now: f64) {
        ui.horizontal(|ui| {
            for (pinned, name) in [(false, "Tất cả"), (true, "Đã ghim")] {
                let sel = self.clip_filter_pinned == pinned;
                if ui.selectable_label(sel, name).clicked() {
                    self.clip_filter_pinned = pinned;
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button(RichText::new("Xoá chưa ghim").small()).clicked() {
                    self.clips.retain(|c| c.pinned);
                }
                ui.label(RichText::new("Chỉ lưu trong bộ nhớ").small().color(DIM));
            });
        });
        ui.add_space(4.0);
        let mut copy = None;
        let mut toggle_pin = None;
        let mut remove = None;
        let ctx = ui.ctx().clone();
        let mut shown = 0;
        for i in 0..self.clips.len() {
            let item = &self.clips[i];
            if self.clip_filter_pinned && !item.pinned {
                continue;
            }
            shown += 1;
            if let ClipKind::Image { w, h, rgba } = &item.kind {
                if !self.thumbs.contains_key(&item.id) {
                    let tex = ctx.load_texture(format!("clip{}", item.id), thumbnail(*w, *h, rgba, 200, 90), egui::TextureOptions::LINEAR);
                    self.thumbs.insert(item.id, tex);
                }
            }
            let item = &self.clips[i];
            let resp = egui::Frame::new()
                .fill(CARD)
                .corner_radius(10)
                .inner_margin(Margin::symmetric(12, 8))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(ui.available_width() - 70.0);
                            match &item.kind {
                                ClipKind::Text(t) => {
                                    ui.label(truncate(t, 90));
                                }
                                ClipKind::Image { w, h, .. } => {
                                    if let Some(tex) = self.thumbs.get(&item.id) {
                                        ui.add(egui::Image::new((tex.id(), tex.size_vec2())).corner_radius(6));
                                    }
                                    ui.label(RichText::new(format!("{w} × {h}")).small().color(DIM));
                                }
                                ClipKind::Files(f) => {
                                    let names: Vec<String> = f.iter().take(3).map(|p| file_name(&p.display().to_string())).collect();
                                    let more = if f.len() > 3 { format!(" +{}", f.len() - 3) } else { String::new() };
                                    ui.label(names.join(", ") + &more);
                                }
                            }
                            let sub = if item.pinned { format!("{} · Đã ghim", item.label()) } else { format!("{} · {}", item.label(), ago(item.time)) };
                            ui.label(RichText::new(sub).small().color(DIM));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("✕").on_hover_text("Xoá").clicked() {
                                remove = Some(i);
                            }
                            let pin_txt = RichText::new("★").color(if item.pinned { Color32::from_rgb(255, 200, 60) } else { DIM });
                            if ui.small_button(pin_txt).on_hover_text("Ghim").clicked() {
                                toggle_pin = Some(i);
                            }
                        });
                    });
                })
                .response
                .interact(Sense::click());
            if resp.clicked() {
                copy = Some(i);
            }
            resp.on_hover_text("Bấm để chép lại");
            ui.add_space(2.0);
        }
        if shown == 0 {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(if self.clip_filter_pinned { "Chưa ghim mục nào" } else { "Nhấn Ctrl + C ở bất kỳ đâu, mục sẽ hiện ở đây" }).color(DIM));
            });
        }
        if let Some(i) = toggle_pin {
            self.clips[i].pinned = !self.clips[i].pinned;
            self.save_pins();
        } else if let Some(i) = remove {
            let was_pinned = self.clips.remove(i).pinned;
            if was_pinned {
                self.save_pins();
            }
        } else if let Some(i) = copy {
            self.copy_item(i, now);
        }
    }

    fn tab_notes(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let te = egui::TextEdit::singleline(&mut self.note_input)
                .hint_text("Thêm việc cần nhớ… (Enter)")
                .desired_width(ui.available_width() - 64.0)
                .margin(Margin::symmetric(10, 7));
            let r = ui.add(te);
            let add = ui.button("Thêm").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            if add && !self.note_input.trim().is_empty() {
                self.data.notes.insert(0, Note { text: self.note_input.trim().to_string(), done: false });
                self.note_input.clear();
                self.data.save();
                r.request_focus();
            }
        });
        ui.add_space(6.0);
        let mut changed = false;
        let mut remove = None;
        for (i, n) in self.data.notes.iter_mut().enumerate() {
            egui::Frame::new().fill(CARD).corner_radius(10).inner_margin(Margin::symmetric(10, 6)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    changed |= ui.checkbox(&mut n.done, "").changed();
                    let txt = if n.done { RichText::new(&n.text).strikethrough().color(DIM) } else { RichText::new(&n.text) };
                    ui.add(egui::Label::new(txt).wrap());
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("✕").clicked() {
                            remove = Some(i);
                        }
                    });
                });
            });
            ui.add_space(2.0);
        }
        if let Some(i) = remove {
            self.data.notes.remove(i);
            changed = true;
        }
        if changed {
            self.data.save();
        }
        if self.data.notes.is_empty() {
            ui.add_space(30.0);
            ui.vertical_centered(|ui| ui.label(RichText::new("Chưa có ghi chú nào").color(DIM)));
        } else if self.data.notes.iter().any(|n| n.done) {
            if ui.button(RichText::new("Xoá việc đã xong").small()).clicked() {
                self.data.notes.retain(|n| !n.done);
                self.data.save();
            }
        }
    }

    fn tab_marks(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let te = egui::TextEdit::singleline(&mut self.mark_input)
                .hint_text("Dán đường dẫn file/thư mục… (Enter)")
                .desired_width(ui.available_width() - 64.0)
                .margin(Margin::symmetric(10, 7));
            let r = ui.add(te);
            let add = ui.button("Thêm").clicked() || (r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));
            let p = self.mark_input.trim().trim_matches('"').to_string();
            if add && !p.is_empty() {
                self.data.bookmarks.push(Bookmark { name: file_name(&p), path: p });
                self.mark_input.clear();
                self.data.save();
            }
        });
        ui.label(RichText::new("Mẹo: kéo file hoặc thư mục thả lên đảo để thêm nhanh.").small().color(DIM));
        ui.add_space(6.0);
        let mut remove = None;
        let cols = 2;
        let w = (ui.available_width() - 8.0) / cols as f32;
        let marks = self.data.bookmarks.clone();
        for row in marks.chunks(cols).enumerate() {
            ui.horizontal(|ui| {
                for (j, b) in row.1.iter().enumerate() {
                    let idx = row.0 * cols + j;
                    let is_dir = std::path::Path::new(&b.path).is_dir();
                    let ext = if is_dir {
                        "DIR".to_string()
                    } else {
                        std::path::Path::new(&b.path).extension().map(|e| e.to_string_lossy().to_uppercase()).unwrap_or("FILE".into())
                    };
                    let resp = egui::Frame::new()
                        .fill(CARD)
                        .corner_radius(10)
                        .inner_margin(Margin::symmetric(10, 8))
                        .show(ui, |ui| {
                            ui.set_width(w - 20.0);
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(34.0, 34.0), Sense::hover());
                                ui.painter().rect_filled(r, 8.0, if is_dir { logo::C2.gamma_multiply(0.5) } else { logo::C3.gamma_multiply(0.45) });
                                ui.painter().text(r.center(), Align2::CENTER_CENTER, truncate(&ext, 4), bold(10.0), Color32::WHITE);
                                ui.vertical(|ui| {
                                    ui.set_width(w - 90.0);
                                    ui.label(RichText::new(truncate(&b.name, 22)).font(bold(13.0)));
                                    ui.label(RichText::new(truncate(&b.path, 30)).small().color(DIM));
                                });
                                if ui.small_button("✕").clicked() {
                                    remove = Some(idx);
                                }
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.clicked() {
                        sys::shell_open(&b.path);
                    }
                    resp.on_hover_text(&b.path);
                }
            });
        }
        if let Some(i) = remove {
            self.data.bookmarks.remove(i);
            self.data.save();
        }
    }

    fn tab_tools(&mut self, ui: &mut Ui) {
        self.docs_section(ui);
        let home = std::env::var("USERPROFILE").unwrap_or_default();
        let tools: [(&str, &str, String); 8] = [
            ("Chụp vùng", "PrtScn", "ms-screenclip:".into()),
            ("Quay màn hình", "Snipping Tool", "snippingtool.exe".into()),
            ("Downloads", "Thư mục tải về", format!("{home}\\Downloads")),
            ("Máy tính", "Calculator", "calc.exe".into()),
            ("Task Manager", "Tiến trình", "taskmgr.exe".into()),
            ("Âm thanh", "Cài đặt", "ms-settings:sound".into()),
            ("Thùng rác", "Recycle Bin", "shell:RecycleBinFolder".into()),
            ("Khoá máy", "Win + L", "lock".into()),
        ];
        let w = ((ui.available_width() - 3.0 * 8.0) / 4.0).floor();
        for row in tools.chunks(4) {
            ui.horizontal(|ui| {
                for (i, (name, sub, target)) in row.iter().enumerate() {
                    let resp = egui::Frame::new()
                        .fill(CARD)
                        .corner_radius(12)
                        .inner_margin(Margin::symmetric(10, 12))
                        .show(ui, |ui| {
                            ui.set_width(w - 20.0);
                            ui.set_height(44.0);
                            ui.vertical(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(20.0, 4.0), Sense::hover());
                                ui.painter().rect_filled(r, 2.0, logo::grad(i as f32 / 3.0));
                                ui.label(RichText::new(*name).font(bold(13.5)));
                                ui.label(RichText::new(*sub).small().color(DIM));
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if resp.hovered() {
                        ui.painter().rect_stroke(resp.rect, 12.0, Stroke::new(1.0, ACCENT.gamma_multiply(0.6)), StrokeKind::Inside);
                    }
                    if resp.clicked() {
                        if target == "lock" {
                            sys::lock_pc();
                        } else {
                            sys::shell_open(target);
                        }
                    }
                }
            });
            ui.add_space(4.0);
        }
        ui.add_space(8.0);
        self.update_card(ui);
        card(ui, |ui| {
            if ui.checkbox(&mut self.autostart, "Khởi động Alex cùng Windows").changed() {
                sys::set_autostart(self.autostart);
            }
            ui.horizontal(|ui| {
                let mut changed = ui.checkbox(&mut self.data.settings.auto_dormant, "Tự thu nhỏ thành chấm mờ sau").changed();
                changed |= ui
                    .add_enabled(self.data.settings.auto_dormant, egui::DragValue::new(&mut self.data.settings.idle_secs).range(3..=300).suffix(" giây"))
                    .changed();
                ui.label(RichText::new("không dùng").color(if self.data.settings.auto_dormant { TEXT } else { DIM }));
                if changed {
                    self.data.save();
                }
            });
            ui.label(RichText::new("Khi là chấm mờ, chuột bấm xuyên qua đảo tới cửa sổ phía sau. Nhấn Ctrl hai lần để hiện lại (nhấn Ctrl hai lần khi đảo đang mở để thu lại).").small().color(DIM));
            ui.label(
                RichText::new("Đảo tự ẩn khi xem toàn màn hình / trình chiếu và không lọt vào ảnh chụp hay chia sẻ màn hình. Bấm icon Alex ở khay hệ thống để ẩn/hiện.")
                    .small()
                    .color(DIM),
            );
        });
    }

    fn update_card(&mut self, ui: &mut Ui) {
        use crate::update::Status;
        let status = self.update.lock().unwrap().status.clone();
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("Alex {}", crate::update::CURRENT)).font(bold(14.0)));
                let (text, col) = match &status {
                    Status::Idle => ("Sẽ kiểm tra bản mới sau ít giây".to_string(), DIM),
                    Status::Checking => ("Đang kiểm tra…".to_string(), DIM),
                    Status::UpToDate => ("Đã là bản mới nhất".to_string(), Color32::from_rgb(60, 210, 130)),
                    Status::Downloading { version, pct } => (format!("Đang tải bản {version}… {pct}%"), ACCENT),
                    Status::Ready { version, .. } => (format!("Bản {version} đã sẵn sàng"), ACCENT),
                    Status::Installing => ("Đang cập nhật, Alex sẽ tự mở lại…".to_string(), ACCENT),
                    Status::Error(e) => (format!("Không kiểm tra được: {}", truncate(e, 60)), Color32::from_rgb(230, 120, 90)),
                };
                ui.label(RichText::new(text).small().color(col));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if let Status::Ready { .. } = status {
                        let b = egui::Button::new(RichText::new("Cập nhật ngay").font(bold(13.0)).color(Color32::WHITE)).fill(logo::C2).corner_radius(14);
                        if ui.add(b).clicked() {
                            self.data.save();
                            if crate::update::install(&self.update).is_ok() {
                                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                            }
                        }
                    } else if !matches!(status, Status::Checking | Status::Downloading { .. } | Status::Installing)
                        && ui.small_button("Kiểm tra ngay").clicked()
                    {
                        self.update.lock().unwrap().check_now = true;
                    }
                });
            });
            if let Status::Ready { notes, .. } = &status {
                if !notes.is_empty() {
                    ui.label(RichText::new(truncate(notes, 160)).small().color(DIM));
                }
            }
            if ui.checkbox(&mut self.data.settings.auto_update, "Tự cài bản mới khi không dùng đảo").changed() {
                self.data.save();
            }
        });
        ui.add_space(6.0);
    }

    fn tab_garden(&mut self, ui: &mut Ui) {
        let w = self.data.water.clone();
        let progress = (w.count as f32 / w.goal.max(1) as f32).min(1.0);
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(200.0, 240.0), Sense::hover());
            paint_plant(ui.painter(), rect, progress);
            ui.vertical(|ui| {
                ui.add_space(14.0);
                ui.label(RichText::new("Vườn uống nước").font(bold(18.0)));
                ui.label(RichText::new(format!("{} / {} cốc hôm nay", w.count, w.goal)).color(DIM));
                ui.add_space(6.0);
                // drops
                let (r, _) = ui.allocate_exact_size(vec2(260.0, 22.0), Sense::hover());
                for i in 0..w.goal.min(16) {
                    let c = pos2(r.min.x + 9.0 + i as f32 * 20.0, r.center().y);
                    if i < w.count {
                        ui.painter().circle_filled(c, 7.0, logo::grad(i as f32 / w.goal.max(1) as f32 * 0.5));
                    } else {
                        ui.painter().circle_stroke(c, 7.0, Stroke::new(1.5, Color32::from_white_alpha(40)));
                    }
                }
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    let drink = egui::Button::new(RichText::new("+ Uống một cốc").font(bold(14.0)).color(Color32::WHITE))
                        .fill(logo::C2)
                        .corner_radius(18)
                        .min_size(vec2(150.0, 36.0));
                    if ui.add(drink).clicked() {
                        self.data.water.count += 1;
                        self.data.save();
                    }
                    if ui.add_enabled(w.count > 0, egui::Button::new("−").corner_radius(18).min_size(vec2(36.0, 36.0))).clicked() {
                        self.data.water.count -= 1;
                        self.data.save();
                    }
                });
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Mục tiêu").color(DIM));
                    let mut g = self.data.water.goal;
                    if ui.add(egui::DragValue::new(&mut g).range(1..=16).suffix(" cốc")).changed() {
                        self.data.water.goal = g;
                        self.data.save();
                    }
                });
                ui.label(RichText::new(format!("Chuỗi ngày đủ nước: {} ngày", w.streak)).color(DIM));
                ui.add_space(6.0);
                let msg = match progress {
                    p if p >= 1.0 => "Cây đã nở hoa. Tuyệt lắm!",
                    p if p >= 0.5 => "Cây đang lớn nhanh, cố thêm chút nữa.",
                    p if p > 0.0 => "Mầm đã nhú. Uống thêm nước nhé.",
                    _ => "Uống cốc đầu tiên để gieo hạt.",
                };
                ui.label(RichText::new(msg).small());
            });
        });
    }
}

fn thumbnail(w: usize, h: usize, rgba: &[u8], max_w: usize, max_h: usize) -> egui::ColorImage {
    let s = (max_w as f32 / w as f32).min(max_h as f32 / h as f32).min(1.0);
    let (tw, th) = (((w as f32 * s) as usize).max(1), ((h as f32 * s) as usize).max(1));
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        let sy = (y * h / th).min(h - 1);
        for x in 0..tw {
            let sx = (x * w / tw).min(w - 1);
            let i = (sy * w + sx) * 4;
            out.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    egui::ColorImage::from_rgba_unmultiplied([tw, th], &out)
}

fn paint_plant(p: &egui::Painter, r: Rect, progress: f32) {
    let base = pos2(r.center().x, r.max.y - 60.0);
    // soft glow
    // pot
    let pot = [
        pos2(base.x - 46.0, base.y),
        pos2(base.x + 46.0, base.y),
        pos2(base.x + 34.0, base.y + 52.0),
        pos2(base.x - 34.0, base.y + 52.0),
    ];
    p.add(egui::Shape::convex_polygon(pot.to_vec(), Color32::from_rgb(52, 60, 90), Stroke::NONE));
    p.rect_filled(Rect::from_center_size(pos2(base.x, base.y + 2.0), vec2(104.0, 12.0)), 4.0, Color32::from_rgb(70, 80, 118));
    p.rect_filled(Rect::from_center_size(pos2(base.x, base.y - 4.0), vec2(84.0, 6.0)), 3.0, Color32::from_rgb(60, 42, 30));
    if progress <= 0.0 {
        p.circle_filled(pos2(base.x, base.y - 8.0), 4.0, Color32::from_rgb(150, 110, 70));
        return;
    }
    let green = Color32::from_rgb(60, 210, 130);
    let stem_h = 20.0 + 110.0 * progress;
    let top = pos2(base.x, base.y - 6.0 - stem_h);
    p.line_segment([pos2(base.x, base.y - 6.0), top], Stroke::new(4.0, green));
    let leaves = (progress * 6.0).ceil() as i32;
    for i in 0..leaves {
        let y = base.y - 20.0 - i as f32 * (stem_h - 14.0) / 6.0;
        let side = if i % 2 == 0 { -1.0 } else { 1.0 };
        let size = 9.0 + 3.0 * (1.0 - i as f32 / 6.0);
        let c = pos2(base.x + side * (size + 2.0), y);
        p.add(egui::Shape::ellipse_filled(c, vec2(size + 3.0, size * 0.55), green.gamma_multiply(0.9)));
    }
    if progress >= 1.0 {
        for k in 0..6 {
            let a = k as f32 / 6.0 * std::f32::consts::TAU;
            p.circle_filled(top + vec2(a.cos(), a.sin()) * 11.0, 8.0, logo::grad(k as f32 / 5.0));
        }
        p.circle_filled(top, 7.0, Color32::from_rgb(255, 214, 90));
    } else {
        p.circle_filled(top, 6.0, green);
    }
}

#[allow(dead_code)]
fn _unused(_: Pos2) {}

/// Waits (long-poll) for files a phone sent to this PC, saves them to Downloads\Alex.
/// Follows whatever account is current; idles while there is none.
fn spawn_inbox(account: Arc<Mutex<Option<share::Account>>>, slot: Arc<Mutex<ShareSlot>>, ctx: egui::Context) {
    std::thread::spawn(move || {
        let mut failures: HashMap<String, u32> = HashMap::new();
        loop {
            let acc = account.lock().unwrap().clone();
            let Some(acc) = acc else {
                std::thread::sleep(Duration::from_secs(5));
                continue;
            };
            match share::wait_inbox(&acc, 25) {
                Ok(items) => {
                    for it in items {
                        match share::fetch_item(&acc, &it, &net::downloads_dir(), |_, _| {}) {
                            Ok(p) => {
                                let _ = share::ack(&acc, &it.id);
                                if p.extension().is_some_and(|e| e.eq_ignore_ascii_case("alexurl")) {
                                    let text = std::fs::read_to_string(&p).unwrap_or_default();
                                    let _ = std::fs::remove_file(&p);
                                    let text: String = text.trim().chars().take(4000).collect();
                                    if !text.is_empty() {
                                        slot.lock().unwrap().urls.push(text);
                                    }
                                } else {
                                    let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or(it.label.clone());
                                    slot.lock().unwrap().inbox.push(name);
                                }
                                ctx.request_repaint();
                            }
                            Err(_) => {
                                // give up on a file that keeps failing (e.g. damaged), retry otherwise
                                let n = failures.entry(it.id.clone()).or_insert(0);
                                *n += 1;
                                if *n >= 3 {
                                    let _ = share::ack(&acc, &it.id);
                                }
                                std::thread::sleep(Duration::from_secs(5));
                            }
                        }
                    }
                }
                Err(_) => std::thread::sleep(Duration::from_secs(15)),
            }
        }
    });
}

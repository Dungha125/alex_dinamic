//! PDF window: view & search, draw / highlight / type text, reorder-rotate-delete pages.
//! Pages are rendered by Windows' built-in PDF engine on a worker thread; only a few
//! page textures are kept in memory at once.
use crate::logo;
use crate::pdfedit::{self, Mark, PageEdit};
use eframe::egui::{self, pos2, vec2, Align, Color32, ColorImage, Layout, Pos2, Rect, RichText, Sense, Stroke, TextureHandle, Ui};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver, Sender};

const RENDER_W: u32 = 900;
const MAX_TEX: usize = 6;

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    View,
    Draw,
    Pages,
}

#[derive(Clone, Copy, PartialEq)]
enum Pen {
    Pen,
    Highlight,
    Text,
}

const COLORS: [[u8; 3]; 5] = [[220, 30, 40], [20, 60, 200], [20, 140, 60], [255, 210, 0], [20, 20, 20]];

enum Msg {
    Meta { sizes: Vec<(f32, f32)> },
    Page(u32, ColorImage),
    Texts(Vec<String>),
    Saved(Result<PathBuf, String>),
    Error(String),
}

pub struct PdfWindow {
    pub open: bool,
    path: PathBuf,
    title: String,
    mode: Mode,
    sizes: Vec<(f32, f32)>, // displayed size in points, per source page (0-based)
    tex: HashMap<u32, TextureHandle>,
    pending: HashSet<u32>,
    req: Sender<u32>,
    rx: Receiver<Msg>,
    msg_tx: Sender<Msg>,
    // search
    texts: Option<Vec<String>>,
    query: String,
    results: Vec<(u32, String)>,
    scroll_to: Option<u32>,
    // drawing
    pen: Pen,
    color: usize,
    width: f32,
    text_size: f32,
    marks: BTreeMap<u32, Vec<Mark>>, // key: 1-based source page
    history: Vec<u32>,
    stroke: Option<(u32, Vec<(f32, f32)>)>,
    drag: Option<(u32, (f32, f32))>,
    typing: Option<(u32, (f32, f32), String)>,
    // pages
    order: Vec<(u32, i32)>, // (1-based source page, extra rotation)
    status: String,
    saving: bool,
    saved: Option<PathBuf>,
    visible: Vec<u32>,
    focused: bool,
}

impl PdfWindow {
    pub fn open(path: PathBuf, mode: Mode, ctx: &egui::Context) -> Self {
        let (req_tx, req_rx) = channel::<u32>();
        let (tx, rx) = channel::<Msg>();
        let title = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        // renderer thread (Windows.Data.Pdf)
        {
            let (path, tx, ctx) = (path.clone(), tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                if let Err(e) = render_worker(&path, req_rx, &tx, &ctx) {
                    let _ = tx.send(Msg::Error(e));
                    ctx.request_repaint();
                }
            });
        }
        // text extraction for search
        {
            let (path, tx, ctx) = (path.clone(), tx.clone(), ctx.clone());
            std::thread::spawn(move || {
                let texts = lopdf::Document::load(&path)
                    .map(|d| {
                        let n = d.get_pages().len() as u32;
                        (1..=n).map(|p| d.extract_text(&[p]).unwrap_or_default()).collect()
                    })
                    .unwrap_or_default();
                let _ = tx.send(Msg::Texts(texts));
                ctx.request_repaint();
            });
        }
        PdfWindow {
            open: true,
            path,
            title,
            mode,
            sizes: Vec::new(),
            tex: HashMap::new(),
            pending: HashSet::new(),
            req: req_tx,
            rx,
            msg_tx: tx,
            texts: None,
            query: String::new(),
            results: Vec::new(),
            scroll_to: None,
            pen: Pen::Pen,
            color: 0,
            width: 2.5,
            text_size: 14.0,
            marks: BTreeMap::new(),
            history: Vec::new(),
            stroke: None,
            drag: None,
            typing: None,
            order: Vec::new(),
            status: "Đang mở…".into(),
            saving: false,
            saved: None,
            visible: Vec::new(),
            focused: false,
        }
    }

    fn dirty(&self) -> bool {
        let n = self.sizes.len() as u32;
        self.marks.values().any(|v| !v.is_empty()) || !self.order.iter().copied().eq((1..=n).map(|p| (p, 0)))
    }

    fn texture(&mut self, ctx: &egui::Context, page0: u32) -> Option<TextureHandle> {
        if let Some(t) = self.tex.get(&page0) {
            return Some(t.clone());
        }
        if self.pending.insert(page0) {
            let _ = self.req.send(page0);
        }
        let _ = ctx;
        None
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(m) = self.rx.try_recv() {
            match m {
                Msg::Meta { sizes } => {
                    self.order = (1..=sizes.len() as u32).map(|p| (p, 0)).collect();
                    self.status = format!("{} trang", sizes.len());
                    self.sizes = sizes;
                }
                Msg::Page(p, img) => {
                    self.pending.remove(&p);
                    let t = ctx.load_texture(format!("pdf{p}"), img, egui::TextureOptions::LINEAR);
                    self.tex.insert(p, t);
                    // keep memory bounded: drop textures far from what is on screen
                    if self.tex.len() > MAX_TEX {
                        let centre = self.visible.first().copied().unwrap_or(p) as i64;
                        let mut keys: Vec<u32> = self.tex.keys().copied().collect();
                        keys.sort_by_key(|k| -(*k as i64 - centre).abs());
                        for k in keys.into_iter().take(self.tex.len() - MAX_TEX) {
                            self.tex.remove(&k);
                        }
                    }
                }
                Msg::Texts(t) => {
                    self.texts = Some(t);
                    self.run_search();
                }
                Msg::Saved(r) => {
                    self.saving = false;
                    match r {
                        Ok(p) => {
                            self.status = format!("Đã lưu {}", p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default());
                            self.saved = Some(p);
                        }
                        Err(e) => self.status = format!("Lỗi: {e}"),
                    }
                }
                Msg::Error(e) => self.status = format!("Lỗi: {e}"),
            }
        }
    }

    fn run_search(&mut self) {
        self.results.clear();
        let q = self.query.trim().to_lowercase();
        let Some(texts) = &self.texts else { return };
        if q.is_empty() {
            return;
        }
        for (i, t) in texts.iter().enumerate() {
            let low = t.to_lowercase();
            if let Some(pos) = low.find(&q) {
                let chars: Vec<char> = t.chars().collect();
                let ci = low[..pos].chars().count();
                let a = ci.saturating_sub(30);
                let b = (ci + q.chars().count() + 40).min(chars.len());
                let snip: String = chars[a..b].iter().collect::<String>().split_whitespace().collect::<Vec<_>>().join(" ");
                let hits = low.matches(&q).count();
                self.results.push((i as u32, format!("{}{snip}… ({hits})", if a > 0 { "…" } else { "" })));
            }
        }
    }

    fn save(&mut self) {
        if self.saving {
            return;
        }
        self.saving = true;
        self.status = "Đang lưu…".into();
        let stem = self.path.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        let dir = self.path.parent().map(|p| p.to_path_buf()).unwrap_or_default();
        let mut out = dir.join(format!("{stem} (đã sửa).pdf"));
        let mut i = 1;
        while out.exists() {
            out = dir.join(format!("{stem} (đã sửa {i}).pdf"));
            i += 1;
        }
        let order: Vec<PageEdit> = self.order.iter().map(|(p, r)| PageEdit { src: *p, rotate: *r }).collect();
        let (src, marks, tx) = (self.path.clone(), self.marks.clone(), self.msg_tx.clone());
        std::thread::spawn(move || {
            let r = pdfedit::save(&src, &order, &marks, &out).map(|_| out);
            let _ = tx.send(Msg::Saved(r));
        });
    }

    /// Called every frame from the main app while the window is open.
    pub fn show(&mut self, ctx: &egui::Context) {
        self.poll(ctx);
        let builder = egui::ViewportBuilder::default()
            .with_title(format!("Alex · {}", self.title))
            .with_inner_size([1020.0, 780.0])
            .with_min_inner_size([640.0, 420.0])
            .with_icon(egui::IconData { rgba: crate::ICON32.to_vec(), width: 32, height: 32 });
        ctx.show_viewport_immediate(egui::ViewportId::from_hash_of("alex-pdf"), builder, |ui, _class| {
            if !self.focused {
                self.focused = true;
                ui.ctx().send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            if ui.input(|i| i.viewport().close_requested()) {
                self.open = false;
            }
            self.ui(ui);
        });
        if self.saving || !self.pending.is_empty() || self.texts.is_none() {
            ctx.request_repaint_after(std::time::Duration::from_millis(120));
        }
    }

    fn ui(&mut self, root: &mut Ui) {
        let ctx = root.ctx().clone();
        egui::Panel::top("pdf-top")
            .frame(egui::Frame::new().fill(Color32::from_rgb(12, 15, 24)).inner_margin(egui::Margin::symmetric(12, 8)))
            .show(root, |ui| self.toolbar(ui));
        if self.mode == Mode::View && (!self.results.is_empty() || !self.query.is_empty()) {
            egui::Panel::left("pdf-search")
                .resizable(false)
                .exact_size(260.0)
                .frame(egui::Frame::new().fill(Color32::from_rgb(16, 20, 30)).inner_margin(egui::Margin::same(10)))
                .show(root, |ui| self.search_panel(ui));
        }
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::from_rgb(40, 44, 54)))
            .show(root, |ui| {
                if self.sizes.is_empty() {
                    ui.centered_and_justified(|ui| ui.label(RichText::new(&self.status).color(Color32::LIGHT_GRAY)));
                    return;
                }
                match self.mode {
                    Mode::Pages => self.pages_grid(ui, &ctx),
                    _ => self.pages_scroll(ui, &ctx),
                }
            });
    }

    fn toolbar(&mut self, ui: &mut Ui) {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(vec2(22.0, 22.0), Sense::hover());
            logo::paint_mark(ui.painter(), r, 1.0);
            for (m, name) in [(Mode::View, "Xem & tìm chữ"), (Mode::Draw, "Tô vẽ, gõ chữ"), (Mode::Pages, "Sắp xếp trang")] {
                if ui.selectable_label(self.mode == m, name).clicked() {
                    self.mode = m;
                    self.typing = None;
                }
            }
            ui.separator();
            match self.mode {
                Mode::View => {
                    let r = ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Tìm chữ trong PDF…").desired_width(220.0));
                    if r.changed() {
                        self.run_search();
                    }
                    if self.texts.is_none() {
                        ui.label(RichText::new("đang đọc chữ…").small().weak());
                    }
                }
                Mode::Draw => {
                    for (p, name) in [(Pen::Pen, "Bút"), (Pen::Highlight, "Tô sáng"), (Pen::Text, "Gõ chữ")] {
                        if ui.selectable_label(self.pen == p, name).clicked() {
                            self.pen = p;
                        }
                    }
                    for (i, c) in COLORS.iter().enumerate() {
                        let (r, resp) = ui.allocate_exact_size(vec2(18.0, 18.0), Sense::click());
                        ui.painter().circle_filled(r.center(), 7.0, Color32::from_rgb(c[0], c[1], c[2]));
                        if self.color == i {
                            ui.painter().circle_stroke(r.center(), 9.0, Stroke::new(1.5, Color32::WHITE));
                        }
                        if resp.clicked() {
                            self.color = i;
                        }
                    }
                    if self.pen == Pen::Text {
                        ui.add(egui::DragValue::new(&mut self.text_size).range(6.0..=72.0).suffix(" pt"));
                    } else if self.pen == Pen::Pen {
                        ui.add(egui::DragValue::new(&mut self.width).range(0.5..=12.0).speed(0.1).suffix(" pt"));
                    }
                    if ui.add_enabled(!self.history.is_empty(), egui::Button::new("Hoàn tác")).clicked() {
                        if let Some(p) = self.history.pop() {
                            if let Some(v) = self.marks.get_mut(&p) {
                                v.pop();
                            }
                        }
                    }
                }
                Mode::Pages => {
                    ui.label(RichText::new("Dùng ◀ ▶ để đổi chỗ, ⟳ để xoay, ✕ để bỏ trang").small().weak());
                    if ui.button("Đảo ngược").clicked() {
                        self.order.reverse();
                    }
                    if ui.button("Khôi phục").clicked() {
                        self.order = (1..=self.sizes.len() as u32).map(|p| (p, 0)).collect();
                    }
                }
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let save = egui::Button::new(RichText::new("Lưu bản mới").strong().color(Color32::WHITE)).fill(logo::C2);
                if ui.add_enabled(self.dirty() && !self.saving, save).on_hover_text("Lưu thành file mới cạnh file gốc, không ghi đè").clicked() {
                    self.save();
                }
                if let Some(p) = self.saved.clone() {
                    if ui.button("Mở thư mục").clicked() {
                        use std::os::windows::process::CommandExt;
                        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
                    }
                }
                ui.label(RichText::new(&self.status).small().weak());
            });
        });
    }

    fn search_panel(&mut self, ui: &mut Ui) {
        ui.label(RichText::new(format!("{} trang có kết quả", self.results.len())).strong());
        if self.texts.as_ref().is_some_and(|t| t.iter().all(|s| s.trim().is_empty())) {
            ui.label(RichText::new("PDF này không có lớp chữ (có thể là ảnh quét), nên không tìm được.").small().weak());
        }
        ui.add_space(4.0);
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (p, snip) in self.results.clone() {
                let r = ui.add(egui::Button::new(RichText::new(format!("Trang {}\n{snip}", p + 1)).small()).wrap().min_size(vec2(ui.available_width(), 0.0)));
                if r.clicked() {
                    self.scroll_to = Some(p);
                }
            }
        });
    }

    fn pages_scroll(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut visible = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(12.0);
            let avail = ui.available_width();
            let w = (avail - 40.0).clamp(300.0, 860.0);
            for idx in 0..self.order.len() {
                let (src, _) = self.order[idx];
                let page0 = src - 1;
                let (pw, ph) = self.sizes[page0 as usize];
                let h = w * ph / pw;
                ui.vertical_centered(|ui| {
                    let (rect, resp) = ui.allocate_exact_size(vec2(w, h), Sense::click_and_drag());
                    if self.scroll_to == Some(page0) {
                        ui.scroll_to_rect(rect, Some(Align::TOP));
                        self.scroll_to = None;
                    }
                    if ui.is_rect_visible(rect.expand(h)) {
                        visible.push(page0);
                        let p = ui.painter();
                        p.rect_filled(rect, 2.0, Color32::WHITE);
                        if let Some(t) = self.texture(ctx, page0) {
                            p.image(t.id(), rect, Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
                        } else {
                            p.text(rect.center(), egui::Align2::CENTER_CENTER, format!("Trang {}", src), egui::FontId::proportional(16.0), Color32::GRAY);
                        }
                        self.paint_marks(ui, rect, src, pw);
                        if self.mode == Mode::Draw {
                            self.handle_draw(ui, &resp, rect, src, pw);
                        }
                    }
                    ui.label(RichText::new(format!("{} / {}", idx + 1, self.order.len())).small().color(Color32::LIGHT_GRAY));
                });
                ui.add_space(10.0);
            }
        });
        self.visible = visible;
    }

    fn norm(rect: Rect, p: Pos2) -> (f32, f32) {
        (((p.x - rect.min.x) / rect.width()).clamp(0.0, 1.0), ((p.y - rect.min.y) / rect.height()).clamp(0.0, 1.0))
    }
    fn den(rect: Rect, (u, v): (f32, f32)) -> Pos2 {
        pos2(rect.min.x + u * rect.width(), rect.min.y + v * rect.height())
    }

    fn paint_marks(&self, ui: &Ui, rect: Rect, src: u32, page_w_pts: f32) {
        let scale = rect.width() / page_w_pts;
        let p = ui.painter().with_clip_rect(rect);
        let col = |c: &[u8; 3]| Color32::from_rgb(c[0], c[1], c[2]);
        let mut draw = |m: &Mark| match m {
            Mark::Ink { pts, color, width } => {
                let line: Vec<Pos2> = pts.iter().map(|q| Self::den(rect, *q)).collect();
                p.add(egui::Shape::line(line, Stroke::new(width * scale, col(color))));
            }
            Mark::Highlight { a, b, color } => {
                p.rect_filled(Rect::from_two_pos(Self::den(rect, *a), Self::den(rect, *b)), 0.0, Color32::from_rgba_unmultiplied(color[0], color[1], color[2], 90));
            }
            Mark::Text { pos, text, size, color } => {
                let mut y = Self::den(rect, *pos).y;
                for line in text.lines() {
                    p.text(pos2(Self::den(rect, *pos).x, y), egui::Align2::LEFT_TOP, line, egui::FontId::proportional(size * scale), col(color));
                    y += size * scale * 1.25;
                }
            }
        };
        if let Some(list) = self.marks.get(&src) {
            list.iter().for_each(&mut draw);
        }
        if let Some((sp, pts)) = &self.stroke {
            if *sp == src {
                draw(&Mark::Ink { pts: pts.clone(), color: COLORS[self.color], width: self.width });
            }
        }
        if let (Some((dp, a)), Some(cur)) = (&self.drag, ui.input(|i| i.pointer.interact_pos())) {
            if *dp == src {
                draw(&Mark::Highlight { a: *a, b: Self::norm(rect, cur), color: COLORS[self.color] });
            }
        }
    }

    fn handle_draw(&mut self, ui: &mut Ui, resp: &egui::Response, rect: Rect, src: u32, page_w_pts: f32) {
        let pos = resp.interact_pointer_pos();
        if resp.hovered() {
            ui.ctx().set_cursor_icon(if self.pen == Pen::Text { egui::CursorIcon::Text } else { egui::CursorIcon::Crosshair });
        }
        match self.pen {
            Pen::Pen => {
                if resp.drag_started() {
                    if let Some(p) = pos {
                        self.stroke = Some((src, vec![Self::norm(rect, p)]));
                    }
                } else if resp.dragged() {
                    if let (Some((s, pts)), Some(p)) = (&mut self.stroke, pos) {
                        if *s == src {
                            let n = Self::norm(rect, p);
                            if pts.last().is_none_or(|l| (l.0 - n.0).abs() + (l.1 - n.1).abs() > 0.002) {
                                pts.push(n);
                            }
                        }
                    }
                }
                if resp.drag_stopped() {
                    if let Some((s, pts)) = self.stroke.take() {
                        if pts.len() >= 2 {
                            self.marks.entry(s).or_default().push(Mark::Ink { pts, color: COLORS[self.color], width: self.width });
                            self.history.push(s);
                        }
                    }
                }
            }
            Pen::Highlight => {
                if resp.drag_started() {
                    if let Some(p) = pos {
                        self.drag = Some((src, Self::norm(rect, p)));
                    }
                }
                if resp.drag_stopped() {
                    if let (Some((s, a)), Some(p)) = (self.drag.take(), pos) {
                        let b = Self::norm(rect, p);
                        if (a.0 - b.0).abs() > 0.005 && (a.1 - b.1).abs() > 0.003 {
                            self.marks.entry(s).or_default().push(Mark::Highlight { a, b, color: COLORS[self.color] });
                            self.history.push(s);
                        }
                    }
                }
            }
            Pen::Text => {
                if resp.clicked() {
                    if let Some(p) = pos {
                        self.typing = Some((src, Self::norm(rect, p), String::new()));
                    }
                }
            }
        }
        // inline text box
        let mut commit = None;
        let mut cancel = false;
        if let Some((tp, uv, text)) = &mut self.typing {
            if *tp == src {
                let at = Self::den(rect, *uv);
                let scale = rect.width() / page_w_pts;
                #[allow(deprecated)]
                egui::Area::new(egui::Id::new("pdf-typing")).fixed_pos(at).order(egui::Order::Foreground).show(ui.ctx(), |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        let te = egui::TextEdit::multiline(text)
                            .desired_rows(1)
                            .desired_width(260.0)
                            .font(egui::FontId::proportional((self.text_size * scale).clamp(11.0, 40.0)))
                            .hint_text("Gõ chữ, Ctrl+Enter để đặt");
                        let r = ui.add(te);
                        r.request_focus();
                        ui.horizontal(|ui| {
                            if ui.button("Đặt chữ").clicked() || ui.input(|i| i.key_pressed(egui::Key::Enter) && i.modifiers.ctrl) {
                                commit = Some(text.trim_end().to_string());
                            }
                            if ui.button("Huỷ").clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                                cancel = true;
                            }
                        });
                    });
                });
            }
        }
        if let Some(t) = commit {
            if let Some((tp, uv, _)) = self.typing.take() {
                if !t.is_empty() {
                    self.marks.entry(tp).or_default().push(Mark::Text { pos: uv, text: t, size: self.text_size, color: COLORS[self.color] });
                    self.history.push(tp);
                }
            }
        } else if cancel {
            self.typing = None;
        }
    }

    fn pages_grid(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let mut action: Option<(usize, i32)> = None; // (index, op) op: -1 left, 1 right, 2 rotate, 9 delete
        let mut visible = Vec::new();
        egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
            ui.add_space(12.0);
            let cell = 170.0;
            let cols = ((ui.available_width() - 20.0) / (cell + 14.0)).floor().max(1.0) as usize;
            let n = self.order.len();
            for row in 0..n.div_ceil(cols) {
                ui.horizontal(|ui| {
                    ui.add_space(12.0);
                    for idx in row * cols..((row + 1) * cols).min(n) {
                        let (src, rot) = self.order[idx];
                        let page0 = src - 1;
                        egui::Frame::new().fill(Color32::from_rgb(28, 32, 44)).corner_radius(8).inner_margin(egui::Margin::same(7)).show(ui, |ui| {
                            ui.set_width(cell);
                            ui.vertical_centered(|ui| {
                                let (rect, _) = ui.allocate_exact_size(vec2(cell, cell * 1.2), Sense::hover());
                                if ui.is_rect_visible(rect) {
                                    visible.push(page0);
                                    let (pw, ph) = self.sizes[page0 as usize];
                                    let (dw, dh) = if rot % 180 == 90 { (ph, pw) } else { (pw, ph) };
                                    let s = (rect.width() / dw).min(rect.height() / dh);
                                    let img_rect = Rect::from_center_size(rect.center(), vec2(pw * s, ph * s));
                                    if let Some(t) = self.texture(ctx, page0) {
                                        let img = egui::Image::new((t.id(), img_rect.size())).rotate((rot as f32).to_radians(), vec2(0.5, 0.5));
                                        img.paint_at(ui, img_rect);
                                    } else {
                                        ui.painter().rect_filled(img_rect, 2.0, Color32::from_gray(200));
                                    }
                                }
                                ui.label(RichText::new(format!("{}  ·  trang gốc {}", idx + 1, src)).small().color(Color32::LIGHT_GRAY));
                                ui.horizontal(|ui| {
                                    if ui.small_button("◀").clicked() {
                                        action = Some((idx, -1));
                                    }
                                    if ui.small_button("▶").clicked() {
                                        action = Some((idx, 1));
                                    }
                                    if ui.small_button("⟳").on_hover_text("Xoay 90°").clicked() {
                                        action = Some((idx, 2));
                                    }
                                    if ui.small_button("✕").on_hover_text("Bỏ trang").clicked() {
                                        action = Some((idx, 9));
                                    }
                                });
                            });
                        });
                    }
                });
                ui.add_space(10.0);
            }
        });
        self.visible = visible;
        if let Some((i, op)) = action {
            match op {
                -1 if i > 0 => self.order.swap(i, i - 1),
                1 if i + 1 < self.order.len() => self.order.swap(i, i + 1),
                2 => self.order[i].1 = (self.order[i].1 + 90) % 360,
                9 if self.order.len() > 1 => {
                    self.order.remove(i);
                }
                _ => {}
            }
        }
    }
}

fn render_worker(path: &std::path::Path, req: Receiver<u32>, tx: &Sender<Msg>, ctx: &egui::Context) -> Result<(), String> {
    use windows::core::HSTRING;
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let w = |e: windows::core::Error| e.message().to_string();
    let abs = std::fs::canonicalize(path).map_err(|e| e.to_string())?.display().to_string();
    let abs = abs.strip_prefix(r"\\?\").unwrap_or(&abs).to_string();
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(abs)).map_err(w)?.join().map_err(w)?;
    let doc = PdfDocument::LoadFromFileAsync(&file).map_err(|_| "PDF có mật khẩu hoặc bị hỏng".to_string())?.join().map_err(|_| "PDF có mật khẩu hoặc bị hỏng".to_string())?;
    let n = doc.PageCount().map_err(w)?;
    let mut sizes = Vec::with_capacity(n as usize);
    for i in 0..n {
        let s = doc.GetPage(i).and_then(|p| p.Size()).map_err(w)?;
        // Size is in DIPs (1/96"); keep the aspect, express in points
        sizes.push((s.Width * 0.75, s.Height * 0.75));
    }
    let _ = tx.send(Msg::Meta { sizes });
    ctx.request_repaint();
    while let Ok(p) = req.recv() {
        if p >= n {
            continue;
        }
        let page = doc.GetPage(p).map_err(w)?;
        let opts = PdfPageRenderOptions::new().map_err(w)?;
        opts.SetDestinationWidth(RENDER_W).map_err(w)?;
        opts.SetBitmapEncoderId(BitmapEncoder::PngEncoderId().map_err(w)?).map_err(w)?;
        let stream = InMemoryRandomAccessStream::new().map_err(w)?;
        page.RenderWithOptionsToStreamAsync(&stream, &opts).map_err(w)?.join().map_err(w)?;
        let len = stream.Size().map_err(w)? as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(w)?).map_err(w)?;
        reader.LoadAsync(len).map_err(w)?.join().map_err(w)?;
        let mut buf = vec![0u8; len as usize];
        reader.ReadBytes(&mut buf).map_err(w)?;
        let img = image::load_from_memory(&buf).map_err(|e| e.to_string())?.to_rgba8();
        let ci = ColorImage::from_rgba_unmultiplied([img.width() as usize, img.height() as usize], img.as_raw());
        if tx.send(Msg::Page(p, ci)).is_err() {
            break;
        }
        ctx.request_repaint();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    #[ignore] // ALEX_RENDER=<pdf>
    fn text_for_search() {
        let p = std::env::var("ALEX_RENDER").unwrap();
        let d = lopdf::Document::load(&p).unwrap();
        for n in 1..=d.get_pages().len() as u32 {
            let t = d.extract_text(&[n]).unwrap_or_default();
            println!("page {n}: {:?} | 'doanh thu' found: {}", t.trim(), t.to_lowercase().contains("doanh thu"));
        }
    }
}

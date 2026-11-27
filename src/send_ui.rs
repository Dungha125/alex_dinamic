//! "Gửi" tab: LAN peers, outgoing queue, incoming requests and transfer progress.
use super::*;

impl Alex {
    pub(super) fn net_events(&mut self, now: f64) {
        let (events, has_req) = {
            let mut g = self.net.lock().unwrap();
            (std::mem::take(&mut g.events), !g.requests.is_empty())
        };
        for e in events {
            match e {
                net::Event::Text { from, text } => {
                    self.extra_id += 1;
                    let item = ClipItem {
                        id: self.extra_id,
                        hash: clip::hash_bytes(text.as_bytes()),
                        kind: ClipKind::Text(text),
                        time: Instant::now(),
                        pinned: false,
                    };
                    self.clips.retain(|c| c.hash != item.hash || c.pinned);
                    self.clips.insert(0, item);
                    self.toast(now, "Nhận chữ", &from);
                }
                net::Event::Received { from, label } => self.toast(now, &format!("Đã nhận {}", truncate(&label, 18)), &from),
                net::Event::Sent { to, label } => self.toast(now, &format!("Đã gửi {}", truncate(&label, 18)), &to),
                net::Event::Failed { peer, why } => self.toast(now, &format!("Lỗi với {}", truncate(&peer, 16)), &truncate(&why, 24)),
            }
        }
        // an incoming transfer needs an answer: open the island on the Send tab
        if has_req && !self.req_pinned {
            self.req_pinned = true;
            self.dormant = false; // the user has to click Nhận / Từ chối
            self.expanded = true;
            self.pinned_open = true;
            self.tab = Tab::Send;
        } else if !has_req && self.req_pinned {
            self.req_pinned = false;
            self.pinned_open = false;
        }
    }

    pub(super) fn tab_send(&mut self, ui: &mut Ui) {
        let (me, listening, peers, requests, transfers) = {
            let g = self.net.lock().unwrap();
            let reqs: Vec<(u64, String, String, u64, String)> =
                g.requests.iter().map(|r| (r.id, r.from.clone(), r.label.clone(), r.total, r.code.clone())).collect();
            (g.me.clone(), g.listening, g.live_peers(), reqs, g.transfers.iter().take(6).cloned().collect::<Vec<_>>())
        };

        for (id, from, label, total, code) in requests {
            egui::Frame::new()
                .fill(ACCENT.gamma_multiply(0.18))
                .stroke(Stroke::new(1.0, ACCENT.gamma_multiply(0.7)))
                .corner_radius(12)
                .inner_margin(Margin::same(12))
                .show(ui, |ui| {
                    ui.set_width(ui.available_width());
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            ui.label(RichText::new(format!("{from} muốn gửi cho bạn")).font(bold(14.0)));
                            ui.label(RichText::new(format!("{} · {}", truncate(&label, 40), net::human(total))).color(DIM));
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Mã xác minh").small().color(DIM));
                                ui.label(RichText::new(&code).font(bold(15.0)).color(logo::C1));
                            });
                            ui.label(RichText::new("Chỉ bấm Nhận nếu máy gửi hiện đúng mã này.").small().color(DIM));
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            let ok = egui::Button::new(RichText::new("Nhận").font(bold(13.5)).color(Color32::WHITE))
                                .fill(logo::C2)
                                .corner_radius(16);
                            if ui.add(ok).clicked() {
                                self.net.lock().unwrap().answer(id, true);
                            }
                            if ui.add(egui::Button::new("Từ chối").corner_radius(16)).clicked() {
                                self.net.lock().unwrap().answer(id, false);
                            }
                        });
                    });
                });
            ui.add_space(4.0);
        }

        // what to send ---------------------------------------------------
        card(ui, |ui| {
            if self.send_queue.is_empty() {
                ui.label(RichText::new("Kéo file hoặc thư mục thả lên đảo").font(bold(14.0)));
                ui.label(
                    RichText::new("rồi bấm vào tên máy bên dưới để gửi. Gửi thẳng trong mạng LAN, mã hoá đầu cuối, không qua cloud.")
                        .small()
                        .color(DIM),
                );
            } else {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(format!("Sẵn sàng gửi {} mục", self.send_queue.len())).font(bold(14.0)));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Bỏ chọn").clicked() {
                            self.send_queue.clear();
                        }
                    });
                });
                let names: Vec<String> =
                    self.send_queue.iter().take(4).map(|p| file_name(&p.display().to_string())).collect();
                let more = if self.send_queue.len() > 4 { format!(" +{}", self.send_queue.len() - 4) } else { String::new() };
                ui.label(RichText::new(names.join(", ") + &more).color(DIM));
            }
            ui.add_space(4.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.send_text)
                    .hint_text("…hoặc gõ một đoạn chữ để gửi")
                    .desired_width(ui.available_width())
                    .margin(Margin::symmetric(10, 6)),
            );
        });
        ui.add_space(6.0);

        // peers -----------------------------------------------------------
        ui.horizontal(|ui| {
            ui.label(RichText::new("Máy trong mạng").color(DIM).small());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.label(RichText::new(format!("Máy này: {me}")).color(DIM).small());
            });
        });
        let can_send = !self.send_queue.is_empty() || !self.send_text.trim().is_empty();
        if peers.is_empty() {
            card(ui, |ui| {
                ui.label(RichText::new("Đang tìm máy khác có Alex…").color(DIM));
                let hint = if listening {
                    "Mở Alex trên máy kia (cùng Wi-Fi/LAN). Lần đầu Windows hỏi tường lửa: chọn Mạng riêng (Private)."
                } else {
                    "Không mở được cổng nhận file 47821, có thể đang bị chương trình khác dùng."
                };
                ui.label(RichText::new(hint).small().color(DIM));
            });
        }
        let w = ((ui.available_width() - 2.0 * 8.0) / 3.0).floor();
        for row in peers.chunks(3) {
            ui.horizontal(|ui| {
                for (_, peer) in row {
                    let resp = egui::Frame::new()
                        .fill(CARD)
                        .corner_radius(12)
                        .inner_margin(Margin::symmetric(10, 10))
                        .show(ui, |ui| {
                            ui.set_width(w - 20.0);
                            ui.horizontal(|ui| {
                                let (r, _) = ui.allocate_exact_size(vec2(30.0, 30.0), Sense::hover());
                                ui.painter().rect_filled(r, 8.0, logo::C2.gamma_multiply(0.5));
                                let ch = peer.name.chars().next().unwrap_or('?').to_string();
                                ui.painter().text(r.center(), Align2::CENTER_CENTER, ch, bold(14.0), Color32::WHITE);
                                ui.vertical(|ui| {
                                    ui.label(RichText::new(truncate(&peer.name, 16)).font(bold(13.0)));
                                    ui.label(RichText::new("Cùng mạng").small().color(Color32::from_rgb(60, 210, 130)));
                                });
                            });
                        })
                        .response
                        .interact(Sense::click());
                    if can_send && resp.hovered() {
                        ui.painter().rect_stroke(resp.rect, 12.0, Stroke::new(1.5, ACCENT), StrokeKind::Inside);
                    }
                    if resp.clicked() && can_send {
                        let r = self.ctx.clone();
                        if !self.send_queue.is_empty() {
                            net::send_files(&self.net, peer, std::mem::take(&mut self.send_queue), move || r.request_repaint());
                        } else {
                            let text = std::mem::take(&mut self.send_text);
                            net::send_text(&self.net, peer, text, move || r.request_repaint());
                        }
                    }
                    resp.on_hover_text(if can_send { "Bấm để gửi tới máy này" } else { "Thả file lên đảo hoặc gõ chữ trước" });
                }
            });
        }

        ui.add_space(8.0);
        self.tab_internet(ui);

        // transfers -------------------------------------------------------
        if transfers.is_empty() {
            return;
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("Gần đây").color(DIM).small());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.small_button("Mở thư mục nhận").clicked() {
                    let d = net::downloads_dir();
                    let _ = std::fs::create_dir_all(&d);
                    sys::shell_open(&d.display().to_string());
                }
            });
        });
        for t in transfers {
            egui::Frame::new().fill(CARD).corner_radius(10).inner_margin(Margin::symmetric(12, 8)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                let dir = if t.outgoing { format!("→ {}", t.peer) } else { format!("← {}", t.peer) };
                let status = match t.status {
                    Status::Waiting if !t.code.is_empty() => format!("Chờ nhận · mã {}", t.code),
                    Status::Waiting => "Đang kết nối…".to_string(),
                    Status::Running => format!("{} / {}", net::human(t.done), net::human(t.total)),
                    Status::Done => format!("Xong · {}", net::human(t.total)),
                    Status::Declined => "Bị từ chối".into(),
                    Status::Failed => "Lỗi".into(),
                };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("🔒").small().color(Color32::from_rgb(60, 210, 130))).on_hover_text("Mã hoá đầu cuối (X25519 + ChaCha20-Poly1305)");
                    ui.label(RichText::new(truncate(&t.label, 30)).font(bold(13.0)));
                    ui.label(RichText::new(dir).small().color(DIM));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.label(RichText::new(status).small().color(DIM));
                    });
                });
                let frac = if t.total == 0 { 1.0 } else { t.done as f32 / t.total as f32 };
                let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
                ui.painter().rect_filled(r, 2.0, Color32::from_white_alpha(18));
                let col = match t.status {
                    Status::Failed | Status::Declined => Color32::from_rgb(230, 80, 90),
                    Status::Done => Color32::from_rgb(60, 210, 130),
                    _ => ACCENT,
                };
                let mut fill = r;
                fill.set_width(r.width() * frac.clamp(0.0, 1.0));
                ui.painter().rect_filled(fill, 2.0, col);
            });
            ui.add_space(2.0);
        }
    }

    fn tab_internet(&mut self, ui: &mut Ui) {
        ui.label(RichText::new("Qua Internet").color(DIM).small());
        let view = self.share.lock().unwrap().view.clone();
        let note = self.share.lock().unwrap().note.clone();
        let busy = matches!(view, ShareView::Work { .. });
        card(ui, |ui| {
            if self.share_setup {
                ui.label(RichText::new("Máy chủ của bạn").font(bold(14.0)));
                ui.label(RichText::new("File được mã hoá trước khi rời máy. Nhập địa chỉ và mã truy cập rồi kiểm tra.").small().color(DIM));
                ui.add(
                    egui::TextEdit::singleline(&mut self.share_url_edit)
                        .hint_text("https://api.alex.tailieuptit.com")
                        .desired_width(ui.available_width())
                        .margin(Margin::symmetric(10, 6)),
                );
                ui.add(
                    egui::TextEdit::singleline(&mut self.share_token_edit)
                        .hint_text("Mã truy cập")
                        .password(true)
                        .desired_width(ui.available_width())
                        .margin(Margin::symmetric(10, 6)),
                );
                ui.add_space(2.0);
                ui.add_enabled_ui(!busy, |ui| {
                    let btn = egui::Button::new(RichText::new("Lưu & kiểm tra").font(bold(13.5)).color(Color32::WHITE))
                        .fill(logo::C2)
                        .corner_radius(16);
                    if ui.add(btn).clicked() {
                        self.share_save_check();
                    }
                });
            } else {
                let host = self.data.settings.share_url.trim().trim_start_matches("https://").trim_start_matches("http://");
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Máy chủ").small().color(DIM));
                    ui.label(RichText::new(host).font(bold(13.0)));
                    if !note.is_empty() {
                        ui.label(RichText::new(&note).small().color(DIM));
                    }
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Đổi").clicked() {
                            self.share_url_edit = self.data.settings.share_url.clone();
                            self.share_token_edit = self.data.settings.share_token.clone();
                            self.share_setup = true;
                        }
                    });
                });
                if self.send_queue.is_empty() {
                    ui.label(RichText::new("Thả file hoặc thư mục lên đảo để tải lên lấy link.").small().color(DIM));
                } else {
                    ui.add_enabled_ui(!busy, |ui| {
                        ui.horizontal(|ui| {
                            let btn = egui::Button::new(RichText::new("Tải lên lấy link").font(bold(13.5)).color(Color32::WHITE))
                                .fill(logo::C2)
                                .corner_radius(16);
                            if ui.add(btn).clicked() {
                                self.share_upload();
                            }
                            let mut with_code = self.data.settings.share_with_code;
                            if ui.checkbox(&mut with_code, "Kèm mã 6 số").changed() {
                                self.data.settings.share_with_code = with_code;
                                self.data.save();
                            }
                        });
                    });
                    if self.data.settings.share_with_code {
                        ui.label(
                            RichText::new("Chế độ mã 6 số: máy chủ của bạn giữ được khoá giải mã. Link thường thì khoá chỉ nằm sau dấu #, máy chủ không thấy.")
                                .small()
                                .color(Color32::from_rgb(230, 186, 90)),
                        );
                    }
                }
            }

            if let ShareView::Work { label, done, total } = &view {
                ui.add_space(4.0);
                ui.label(RichText::new(label).small().color(DIM));
                share_bar(ui, *done, *total);
                if *total > 0 {
                    ui.label(RichText::new(format!("{} / {}", net::human(*done), net::human(*total))).small().color(DIM));
                }
            }

            if let ShareView::Sent { link, code, hours } = &view {
                ui.add_space(6.0);
                ui.label(RichText::new("Link (đã chép vào clipboard)").small().color(DIM));
                let mut shown = link.clone();
                ui.add(
                    egui::TextEdit::singleline(&mut shown)
                        .desired_width(ui.available_width() - 72.0)
                        .margin(Margin::symmetric(8, 4))
                        .interactive(false),
                );
                if ui.button("Chép link").clicked() {
                    if let Some(cb) = self.cb.as_mut() {
                        if cb.set_text(link.clone()).is_ok() {
                            self.toast(ui.input(|i| i.time), "Đã chép link", "");
                        }
                    }
                }
                if let Some(code) = code {
                    ui.add_space(4.0);
                    ui.label(RichText::new("Mã 6 số").small().color(DIM));
                    ui.label(RichText::new(code).font(bold(28.0)).color(logo::C1));
                    ui.label(RichText::new("Người nhận nhập mã này trên trang web hoặc trong Alex.").small().color(DIM));
                }
                let exp = if *hours >= 1.0 {
                    format!("Hết hạn sau khoảng {:.0} giờ", hours)
                } else {
                    "Hết hạn trong vòng 1 giờ".into()
                };
                ui.label(RichText::new(exp).small().color(DIM));
            }

            if let ShareView::Fail(e) = &view {
                ui.add_space(4.0);
                ui.label(RichText::new(e).small().color(Color32::from_rgb(230, 80, 90)));
            }

            if !self.share_setup {
                ui.add_space(8.0);
                ui.label(RichText::new("Nhận bằng mã hoặc link").small().color(DIM));
                ui.horizontal(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.share_in)
                            .hint_text("123456 hoặc https://…/s/…#khoá")
                            .desired_width(ui.available_width() - 78.0)
                            .margin(Margin::symmetric(10, 6)),
                    );
                    ui.add_enabled_ui(!busy, |ui| {
                        if ui.button("Nhận").clicked() {
                            self.share_receive();
                        }
                    });
                });
                if let ShareView::Saved { name } = &view {
                    ui.label(RichText::new(format!("Đã lưu {name}")).small().color(Color32::from_rgb(60, 210, 130)));
                    if ui.button("Mở thư mục").clicked() {
                        let d = net::downloads_dir();
                        let _ = std::fs::create_dir_all(&d);
                        sys::shell_open(&d.display().to_string());
                    }
                }
            }
        });
    }
}

fn share_bar(ui: &mut Ui, done: u64, total: u64) {
    let frac = if total == 0 { 0.08 } else { done as f32 / total as f32 };
    let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 4.0), Sense::hover());
    ui.painter().rect_filled(r, 2.0, Color32::from_white_alpha(18));
    let mut fill = r;
    fill.set_width(r.width() * frac.clamp(0.02, 1.0));
    ui.painter().rect_filled(fill, 2.0, ACCENT);
}

//! "Tài liệu" section of the Tools tab: drop files on the island, pick a tool.
use super::*;
use crate::docs::{self, JobStatus};

fn reveal(p: &std::path::Path) {
    use std::os::windows::process::CommandExt;
    if p.is_dir() {
        sys::shell_open(&p.display().to_string());
    } else {
        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", p.display())).spawn();
    }
}

impl Alex {
    pub(super) fn docs_section(&mut self, ui: &mut Ui) {
        card(ui, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new("Tài liệu").font(bold(15.0)));
                ui.label(RichText::new("PDF · Word · Excel · PowerPoint · ảnh").small().color(DIM));
                if !self.doc_queue.is_empty() {
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if ui.small_button("Bỏ chọn").clicked() {
                            self.doc_queue.clear();
                        }
                    });
                }
            });
            if self.doc_queue.is_empty() {
                ui.label(RichText::new("Thả file lên đảo để chuyển, gộp, tách hoặc nén. Xử lý ngay trên máy, không tải lên mạng; kết quả nằm cạnh file gốc.").small().color(DIM));
                return;
            }
            let names: Vec<String> = self.doc_queue.iter().take(4).map(|p| file_name(&p.display().to_string())).collect();
            let more = if self.doc_queue.len() > 4 { format!(" +{}", self.doc_queue.len() - 4) } else { String::new() };
            ui.label(RichText::new(names.join(", ") + &more).color(DIM));
            ui.add_space(4.0);
            let tools = docs::applicable(&self.doc_queue);
            ui.horizontal_wrapped(|ui| {
                for (i, (tool, label)) in tools.iter().enumerate() {
                    let b = egui::Button::new(RichText::new(*label).font(bold(13.0)).color(Color32::WHITE))
                        .fill(logo::grad(i as f32 / tools.len().max(1) as f32).gamma_multiply(0.75))
                        .corner_radius(14);
                    if ui.add(b).clicked() {
                        use crate::pdfview::{Mode, PdfWindow};
                        let mode = match tool {
                            docs::Tool::ViewPdf => Some(Mode::View),
                            docs::Tool::EditPdf => Some(Mode::Draw),
                            docs::Tool::ReorderPdf => Some(Mode::Pages),
                            _ => None,
                        };
                        if let Some(mode) = mode {
                            if let Some(f) = self.doc_queue.iter().find(|p| p.extension().is_some_and(|e| e.eq_ignore_ascii_case("pdf"))) {
                                self.pdf = Some(PdfWindow::open(f.clone(), mode, &self.ctx));
                            }
                        } else {
                            let r = self.ctx.clone();
                            docs::start(*tool, self.doc_queue.clone(), &self.jobs, move || r.request_repaint());
                        }
                        self.doc_queue.clear();
                    }
                }
            });
        });

        let jobs = self.jobs.lock().unwrap().clone();
        for j in jobs.iter().take(4) {
            ui.add_space(2.0);
            egui::Frame::new().fill(CARD).corner_radius(10).inner_margin(Margin::symmetric(12, 7)).show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    let (dot, col) = match j.status {
                        JobStatus::Running => ("●", ACCENT),
                        JobStatus::Done => ("✓", Color32::from_rgb(60, 210, 130)),
                        JobStatus::Failed => ("✕", Color32::from_rgb(230, 80, 90)),
                    };
                    ui.label(RichText::new(dot).color(col));
                    ui.vertical(|ui| {
                        ui.set_width(ui.available_width() - 70.0);
                        ui.label(RichText::new(truncate(&j.title, 46)).font(bold(13.0)));
                        if j.detail.contains('\n') {
                            for line in j.detail.lines() {
                                ui.add(egui::Label::new(RichText::new(line).small().color(DIM)).wrap());
                            }
                        } else {
                            ui.label(RichText::new(truncate(&j.detail, 70)).small().color(DIM));
                        }
                        if j.status == JobStatus::Failed && j.detail.contains("HEIF") && ui.small_button("Mở Microsoft Store").clicked() {
                            sys::shell_open(docs::HEIF_STORE);
                        }
                    });
                    if let Some(out) = &j.output {
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.small_button("Mở").on_hover_text(out.display().to_string()).clicked() {
                                reveal(out);
                            }
                        });
                    }
                });
            });
        }
        if jobs.iter().any(|j| j.status == JobStatus::Running) {
            // a few office conversions take seconds; keep the label honest
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(500));
        }
        ui.add_space(8.0);
    }
}

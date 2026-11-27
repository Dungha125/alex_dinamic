#![windows_subsystem = "windows"]
mod app;
mod clip;
mod docs;
mod logo;
mod media;
mod net;
mod pdfedit;
mod pdfsig;
mod pdfview;
mod secure;
mod share;
mod store;
mod sys;
mod update;

use eframe::egui;

pub const WIN_W: f32 = 660.0;
pub const WIN_H: f32 = 440.0;
pub static ICON32: &[u8] = include_bytes!("../assets/icon32.rgba");
static ICON64: &[u8] = include_bytes!("../assets/icon64.rgba");

fn main() -> eframe::Result {
    if !sys::single_instance() {
        return Ok(());
    }
    let viewport = egui::ViewportBuilder::default()
        .with_title(sys::WINDOW_TITLE)
        .with_inner_size([WIN_W, WIN_H])
        .with_position([-4000.0, -4000.0])
        .with_decorations(false)
        .with_transparent(true)
        .with_resizable(false)
        .with_has_shadow(false)
        .with_always_on_top()
        .with_taskbar(false)
        .with_active(false)
        .with_icon(egui::IconData { rgba: ICON64.to_vec(), width: 64, height: 64 });
    let options = eframe::NativeOptions {
        viewport,
        renderer: eframe::Renderer::Glow,
        multisampling: 0,
        depth_buffer: 0,
        stencil_buffer: 0,
        ..Default::default()
    };
    eframe::run_native("Alex", options, Box::new(|cc| Ok(Box::new(app::Alex::new(cc)))))
}

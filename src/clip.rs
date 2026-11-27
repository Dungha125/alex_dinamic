//! Clipboard watcher. History lives only in memory (never written to disk);
//! content that password managers mark as private is ignored.
use std::path::PathBuf;
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::{Duration, Instant};
use windows::core::w;
use windows::Win32::System::DataExchange::{GetClipboardSequenceNumber, IsClipboardFormatAvailable, RegisterClipboardFormatW};

pub enum ClipKind {
    Text(String),
    Image { w: usize, h: usize, rgba: Arc<Vec<u8>> },
    Files(Vec<PathBuf>),
}

pub struct ClipItem {
    pub id: u64,
    pub kind: ClipKind,
    pub hash: u64,
    pub time: Instant,
    pub pinned: bool,
}

impl ClipItem {
    pub fn label(&self) -> &'static str {
        match self.kind {
            ClipKind::Text(ref t) if t.starts_with("http://") || t.starts_with("https://") => "Link",
            ClipKind::Text(_) => "Văn bản",
            ClipKind::Image { .. } => "Ảnh",
            ClipKind::Files(_) => "Tệp",
        }
    }
}

pub fn hash_bytes(b: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf29ce484222325;
    let step = (b.len() / 4096).max(1);
    for x in b.iter().step_by(step) {
        h = (h ^ *x as u64).wrapping_mul(0x100000001b3);
    }
    h ^ b.len() as u64
}

pub fn spawn(repaint: impl Fn() + Send + 'static) -> Receiver<ClipItem> {
    let (tx, rx) = channel();
    std::thread::spawn(move || {
        let Ok(mut cb) = arboard::Clipboard::new() else { return };
        let excl = unsafe { RegisterClipboardFormatW(w!("ExcludeClipboardContentFromMonitorProcessing")) };
        let mut last = unsafe { GetClipboardSequenceNumber() };
        let mut id = 0u64;
        loop {
            std::thread::sleep(Duration::from_millis(400));
            let seq = unsafe { GetClipboardSequenceNumber() };
            if seq == last {
                continue;
            }
            last = seq;
            std::thread::sleep(Duration::from_millis(60)); // let the source finish writing
            if excl != 0 && unsafe { IsClipboardFormatAvailable(excl) }.is_ok() {
                continue;
            }
            let kind = if let Ok(files) = cb.get().file_list() {
                ClipKind::Files(files)
            } else if let Ok(t) = cb.get_text() {
                if t.trim().is_empty() {
                    continue;
                }
                ClipKind::Text(t)
            } else if let Ok(img) = cb.get_image() {
                if img.width * img.height > 8_000_000 {
                    continue;
                }
                ClipKind::Image { w: img.width, h: img.height, rgba: Arc::new(img.bytes.into_owned()) }
            } else {
                continue;
            };
            let hash = match &kind {
                ClipKind::Text(t) => hash_bytes(t.as_bytes()),
                ClipKind::Image { rgba, .. } => hash_bytes(rgba),
                ClipKind::Files(f) => hash_bytes(format!("{f:?}").as_bytes()),
            };
            id += 1;
            if tx.send(ClipItem { id, kind, hash, time: Instant::now(), pinned: false }).is_err() {
                break;
            }
            repaint();
        }
    });
    rx
}

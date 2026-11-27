//! Small JSON store in %APPDATA%\Alex\data.json for notes, bookmarks, pins, water.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Clone)]
pub struct Note {
    pub text: String,
    pub done: bool,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Bookmark {
    pub name: String,
    pub path: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Water {
    pub goal: u32,
    pub count: u32,
    pub day: String,
    pub streak: u32,
}

impl Default for Water {
    fn default() -> Self {
        Water { goal: 8, count: 0, day: String::new(), streak: 0 }
    }
}

fn default_share_url() -> String {
    "https://api.alex.tailieuptit.com".into()
}
fn default_true() -> bool {
    true
}

#[derive(Serialize, Deserialize, Clone)]
#[serde(default)]
pub struct Settings {
    /// shrink to a faded, click-through dot when idle; double Ctrl brings it back
    pub auto_dormant: bool,
    pub idle_secs: u32,
    /// own share server, e.g. https://api.alex.tailieuptit.com
    #[serde(default = "default_share_url")]
    pub share_url: String,
    /// this PC's account on the share server (9-digit ID, private secret, password for phones)
    pub device_id: String,
    pub device_secret: String,
    pub device_password: String,
    /// the server the account above belongs to
    pub device_server: String,
    /// when true, uploads also leave the decryption key on the server so a 6-digit code is enough
    #[serde(default = "default_true")]
    pub share_with_code: bool,
    /// install downloaded updates automatically while the island is not in use
    #[serde(default = "default_true")]
    pub auto_update: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            auto_dormant: true,
            idle_secs: 8,
            share_url: default_share_url(),
            device_id: String::new(),
            device_secret: String::new(),
            device_password: String::new(),
            device_server: String::new(),
            share_with_code: true,
            auto_update: true,
        }
    }
}

#[derive(Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Data {
    pub settings: Settings,
    pub notes: Vec<Note>,
    pub bookmarks: Vec<Bookmark>,
    pub pins: Vec<String>,
    pub water: Water,
}

fn path() -> PathBuf {
    let base = std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| ".".into());
    base.join("Alex").join("data.json")
}

impl Data {
    pub fn load() -> Self {
        std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let p = path();
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(json) = serde_json::to_vec_pretty(self) {
            let tmp = p.with_extension("tmp");
            if std::fs::write(&tmp, json).is_ok() {
                let _ = std::fs::rename(tmp, p);
            }
        }
    }
}

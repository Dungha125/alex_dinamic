//! Self-update. Reads `latest.json` from the Alex website, and only if the release is
//! signed with the Alex release key (Ed25519, public key embedded below) downloads the
//! installer, checks its SHA-256 and installs it silently (the installer relaunches Alex).
//!
//! latest.json: {"version","url","sha256","size","sig","notes"}
//! sig = Ed25519 over "alex-update|<version>|<sha256 hex>"
use ring::digest::{Context, SHA256};
use ring::signature::{UnparsedPublicKey, ED25519};
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");
const DEFAULT_URL: &str = "https://alex.tailieuptit.com/latest.json";
const PUBKEY: &str = include_str!("../assets/update_pubkey.txt");

#[derive(Clone, PartialEq)]
pub enum Status {
    Idle,
    Checking,
    UpToDate,
    Downloading { version: String, pct: u8 },
    Ready { version: String, notes: String, path: PathBuf },
    Installing,
    Error(String),
}

pub struct State {
    pub status: Status,
    pub check_now: bool,
}

pub type Shared = Arc<Mutex<State>>;

fn url() -> String {
    std::env::var("ALEX_UPDATE_URL").unwrap_or_else(|_| DEFAULT_URL.into())
}

/// "0.10.2" > "0.9.9"
pub fn newer(a: &str, b: &str) -> bool {
    let p = |s: &str| s.trim().trim_start_matches('v').split('.').map(|x| x.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, b) = (p(a), p(b));
    for i in 0..a.len().max(b.len()) {
        let (x, y) = (*a.get(i).unwrap_or(&0), *b.get(i).unwrap_or(&0));
        if x != y {
            return x > y;
        }
    }
    false
}

fn b64u_decode(s: &str) -> Option<Vec<u8>> {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let (mut acc, mut bits, mut out) = (0u32, 0, Vec::new());
    for ch in s.trim().bytes() {
        acc = acc << 6 | T.iter().position(|b| *b == ch)? as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

pub fn verify(version: &str, sha256: &str, sig: &str) -> bool {
    let (Some(pk), Some(sig)) = (b64u_decode(PUBKEY), b64u_decode(sig)) else { return false };
    let msg = format!("alex-update|{version}|{}", sha256.to_ascii_lowercase());
    UnparsedPublicKey::new(&ED25519, pk).verify(msg.as_bytes(), &sig).is_ok()
}

fn set(st: &Shared, s: Status, repaint: &dyn Fn()) {
    st.lock().unwrap().status = s;
    repaint();
}

fn check_once(st: &Shared, repaint: &dyn Fn()) -> Result<(), String> {
    set(st, Status::Checking, repaint);
    let agent = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(60)).build();
    let v: serde_json::Value = agent.get(&url()).call().map_err(|e| e.to_string())?.into_json().map_err(|e| e.to_string())?;
    let version = v["version"].as_str().unwrap_or("").to_string();
    if !newer(&version, CURRENT) {
        set(st, Status::UpToDate, repaint);
        return Ok(());
    }
    let sha = v["sha256"].as_str().unwrap_or("").to_ascii_lowercase();
    let dl = v["url"].as_str().unwrap_or("");
    if !verify(&version, &sha, v["sig"].as_str().unwrap_or("")) {
        return Err("Bản cập nhật không có chữ ký hợp lệ, đã bỏ qua".into());
    }
    if !dl.starts_with("https://") && std::env::var_os("ALEX_UPDATE_URL").is_none() {
        return Err("Địa chỉ tải bản cập nhật không an toàn".into());
    }
    let dir = std::env::temp_dir().join("AlexUpdate");
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join(format!("Alex-Setup-{version}.exe"));
    // already downloaded and intact?
    let have = std::fs::read(&path).ok().map(|b| {
        let mut c = Context::new(&SHA256);
        c.update(&b);
        hex(c.finish().as_ref()) == sha
    });
    if have != Some(true) {
        let resp = agent.get(dl).call().map_err(|e| e.to_string())?;
        let total: u64 = resp.header("Content-Length").and_then(|x| x.parse().ok()).unwrap_or(0);
        let mut rd = resp.into_reader();
        let part = path.with_extension("part");
        let mut f = std::fs::File::create(&part).map_err(|e| e.to_string())?;
        let (mut ctx, mut got, mut buf, mut last) = (Context::new(&SHA256), 0u64, vec![0u8; 256 * 1024], 255u8);
        loop {
            let n = rd.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            ctx.update(&buf[..n]);
            f.write_all(&buf[..n]).map_err(|e| e.to_string())?;
            got += n as u64;
            let pct = if total > 0 { (got * 100 / total) as u8 } else { 0 };
            if pct != last {
                last = pct;
                set(st, Status::Downloading { version: version.clone(), pct }, repaint);
            }
        }
        drop(f);
        if hex(ctx.finish().as_ref()) != sha {
            let _ = std::fs::remove_file(&part);
            return Err("File cập nhật tải về bị sai (SHA-256 không khớp)".into());
        }
        std::fs::rename(&part, &path).map_err(|e| e.to_string())?;
    }
    set(st, Status::Ready { version, notes: v["notes"].as_str().unwrap_or("").to_string(), path }, repaint);
    Ok(())
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Background checker: 15 s after start, then every 6 h (or when `check_now` is set).
pub fn spawn(repaint: impl Fn() + Send + Sync + 'static) -> Shared {
    let st: Shared = Arc::new(Mutex::new(State { status: Status::Idle, check_now: false }));
    let s = st.clone();
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(15));
        loop {
            if !matches!(s.lock().unwrap().status, Status::Ready { .. } | Status::Installing) {
                if let Err(e) = check_once(&s, &repaint) {
                    set(&s, Status::Error(e), &repaint);
                }
            }
            for _ in 0..(6 * 3600) {
                std::thread::sleep(Duration::from_secs(1));
                let mut g = s.lock().unwrap();
                if g.check_now {
                    g.check_now = false;
                    if matches!(g.status, Status::Ready { .. }) {
                        continue;
                    }
                    break;
                }
            }
        }
    });
    st
}

/// Runs the verified installer silently; it closes Alex, installs, and starts the new version.
pub fn install(st: &Shared) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let path = match &st.lock().unwrap().status {
        Status::Ready { path, .. } => path.clone(),
        _ => return Err("Chưa có bản cập nhật".into()),
    };
    std::process::Command::new(&path)
        .args(["/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART", "/CURRENTUSER", "/RELAUNCH=1"])
        .creation_flags(0x0000_0008) // DETACHED_PROCESS: survives Alex being closed by the installer
        .spawn()
        .map_err(|e| e.to_string())?;
    st.lock().unwrap().status = Status::Installing;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn versions() {
        assert!(newer("0.2.0", "0.1.0"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0", "0.99.99"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
        assert!(newer("v0.1.1", "0.1.0"));
    }
    #[test]
    fn rejects_bad_signature() {
        assert!(!verify("9.9.9", "00", "AAAA"));
        assert!(!verify("9.9.9", "00", ""));
    }
}

#[cfg(test)]
mod live {
    use super::*;
    /// ALEX_UPD_BASE=http://localhost:5173/upd with ok/badsig/badsha/older.json + dummy.exe
    #[test]
    #[ignore]
    fn check_against_local_site() {
        let base = std::env::var("ALEX_UPD_BASE").unwrap();
        let _ = std::fs::remove_dir_all(std::env::temp_dir().join("AlexUpdate"));
        for name in ["older.json", "badsig.json", "badsha.json", "ok.json"] {
            std::env::set_var("ALEX_UPDATE_URL", format!("{base}/{name}"));
            let st: Shared = Arc::new(Mutex::new(State { status: Status::Idle, check_now: false }));
            let r = check_once(&st, &|| {});
            let s = match &st.lock().unwrap().status {
                Status::Ready { version, path, .. } => format!("READY {version} {}", path.display()),
                Status::UpToDate => "UP-TO-DATE".into(),
                _ => "other".into(),
            };
            println!("{name:12} -> {:?} / {s}", r);
        }
        let leftovers: Vec<_> = std::fs::read_dir(std::env::temp_dir().join("AlexUpdate")).unwrap().flatten().map(|e| e.file_name()).collect();
        println!("AlexUpdate dir: {leftovers:?}");
    }
}

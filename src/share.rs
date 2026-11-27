//! Internet sharing through the user's own Alex share server (server/alex_share.py).
//!
//! Blob format (same as server/page.html decrypts):
//!   "ALXS1" | frames: [u32 BE length][AES-256-GCM ciphertext + 16-byte tag]
//!   frame 0  = JSON metadata {name, size, mime}, AAD "meta"
//!   frame 1… = file data in 1 MiB pieces, AAD [1] on the last frame, [0] otherwise
//!   nonce    = 4 zero bytes + u64 BE frame index
//! The key lives only in the link's #fragment, unless the sender asks for a
//! 6-digit code — then the key is also left on the server so the code alone works.
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM};
use ring::rand::{SecureRandom, SystemRandom};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::Duration;

const CHUNK: usize = 1 << 20;
const TAG: usize = 16;
const MAGIC: &[u8; 5] = b"ALXS1";

pub struct Shared {
    pub link: String,
    pub code: Option<String>,
    pub hours: f64,
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new()
        .timeout_connect(Duration::from_secs(10))
        .timeout_read(Duration::from_secs(120))
        .timeout_write(Duration::from_secs(120))
        .build()
}

fn base(url: &str) -> String {
    let u = url.trim().trim_end_matches('/');
    if u.starts_with("http://") || u.starts_with("https://") { u.to_string() } else { format!("https://{u}") }
}

fn err_of(e: ureq::Error) -> String {
    match e {
        ureq::Error::Status(code, resp) => {
            let body = resp.into_string().unwrap_or_default();
            serde_json::from_str::<serde_json::Value>(&body)
                .ok()
                .and_then(|v| v["error"].as_str().map(str::to_string))
                .unwrap_or_else(|| format!("Máy chủ trả lỗi {code}"))
        }
        ureq::Error::Transport(t) => format!("Không kết nối được máy chủ: {}", t.kind()),
    }
}

// ------------------------------------------------------------------ base64url
const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

pub fn b64u(data: &[u8]) -> String {
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            s.push(B64[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

pub fn b64u_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let (mut acc, mut bits) = (0u32, 0);
    for ch in s.bytes() {
        let v = B64.iter().position(|b| *b == ch)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    Some(out)
}

fn nonce(i: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&i.to_be_bytes());
    Nonce::assume_unique_for_key(n)
}

fn mime_of(name: &str) -> &'static str {
    match name.rsplit('.').next().unwrap_or("").to_ascii_lowercase().as_str() {
        "jpg" | "jpeg" => "image/jpeg",
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "mp4" => "video/mp4",
        "mp3" => "audio/mpeg",
        "txt" => "text/plain",
        "zip" => "application/zip",
        _ => "application/octet-stream",
    }
}

/// Streams "ALXS1" + encrypted frames without ever holding the whole file.
struct EncReader<R: Read> {
    src: R,
    key: LessSafeKey,
    left: u64,
    idx: u64,
    out: Vec<u8>,
    pos: usize,
    done: bool,
    sent: u64,
    progress: Box<dyn FnMut(u64)>,
}

impl<R: Read> EncReader<R> {
    fn frame(&mut self, mut plain: Vec<u8>, aad: &[u8]) -> std::io::Result<()> {
        self.key
            .seal_in_place_append_tag(nonce(self.idx), Aad::from(aad), &mut plain)
            .map_err(|_| std::io::Error::other("encrypt"))?;
        self.idx += 1;
        self.out.clear();
        self.out.extend_from_slice(&(plain.len() as u32).to_be_bytes());
        self.out.extend_from_slice(&plain);
        self.pos = 0;
        Ok(())
    }

    fn refill(&mut self) -> std::io::Result<()> {
        if self.done {
            self.out.clear();
            self.pos = 0;
            return Ok(());
        }
        let n = (self.left as usize).min(CHUNK);
        let mut buf = vec![0u8; n];
        self.src.read_exact(&mut buf)?;
        self.left -= n as u64;
        let last = self.left == 0;
        self.frame(buf, &[last as u8])?;
        self.done = last;
        Ok(())
    }
}

impl<R: Read> Read for EncReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.out.len() {
            self.refill()?;
            if self.out.is_empty() {
                return Ok(0);
            }
        }
        let n = buf.len().min(self.out.len() - self.pos);
        buf[..n].copy_from_slice(&self.out[self.pos..self.pos + n]);
        self.pos += n;
        self.sent += n as u64;
        (self.progress)(self.sent);
        Ok(n)
    }
}

fn encrypted_len(meta_len: usize, size: u64) -> u64 {
    let frames = size.div_ceil(CHUNK as u64).max(1);
    MAGIC.len() as u64 + (4 + meta_len + TAG) as u64 + frames * (4 + TAG) as u64 + size
}

/// This PC's account on the share server (ID + private device secret).
#[derive(Clone)]
pub struct Account {
    pub url: String,
    pub id: String,
    pub secret: String,
}

impl Account {
    fn auth(&self) -> String {
        format!("Device {}:{}", self.id, self.secret)
    }
    fn api(&self, path: &str) -> String {
        format!("{}{path}", base(&self.url))
    }
}

/// "249791575" -> "249 791 575"
pub fn pretty_id(id: &str) -> String {
    id.as_bytes().chunks(3).map(|c| String::from_utf8_lossy(c).into_owned()).collect::<Vec<_>>().join(" ")
}

/// Registers this PC. Returns (id, secret, password).
pub fn register(url: &str, name: &str) -> Result<(String, String, String), String> {
    let v: serde_json::Value = agent()
        .post(&format!("{}/api/device/register", base(url)))
        .send_json(serde_json::json!({ "name": name }))
        .map_err(err_of)?
        .into_json()
        .map_err(|e| e.to_string())?;
    let s = |k: &str| v[k].as_str().map(str::to_string).ok_or_else(|| "Máy chủ trả dữ liệu lạ (cần máy chủ Alex bản mới)".to_string());
    Ok((s("id")?, s("secret")?, s("password")?))
}

/// Checks the account. Returns the max upload size.
#[allow(dead_code)]
pub fn ping(acc: &Account) -> Result<u64, String> {
    let v: serde_json::Value = agent().get(&acc.api("/api/ping")).set("Authorization", &acc.auth()).call().map_err(err_of)?.into_json().map_err(|e| e.to_string())?;
    if v["auth"].as_str() != Some("device") {
        return Err("Máy chủ không nhận tài khoản của máy này".into());
    }
    Ok(v["max_bytes"].as_u64().unwrap_or(0))
}

/// New password (None = let the server pick one). Every connected phone is signed out.
pub fn set_password(acc: &Account, pw: Option<&str>) -> Result<String, String> {
    let v: serde_json::Value = agent()
        .post(&acc.api("/api/device/password"))
        .set("Authorization", &acc.auth())
        .send_json(serde_json::json!({ "password": pw.unwrap_or("") }))
        .map_err(err_of)?
        .into_json()
        .map_err(|e| e.to_string())?;
    v["password"].as_str().map(str::to_string).ok_or_else(|| "Máy chủ trả dữ liệu lạ".into())
}

pub struct Phone {
    pub sid: String,
    pub name: String,
    pub last: f64,
}

pub fn phones(acc: &Account) -> Result<Vec<Phone>, String> {
    let v: serde_json::Value = agent().get(&acc.api("/api/device/sessions")).set("Authorization", &acc.auth()).call().map_err(err_of)?.into_json().map_err(|e| e.to_string())?;
    Ok(v["sessions"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| Phone {
                    sid: s["sid"].as_str().unwrap_or("").into(),
                    name: s["name"].as_str().unwrap_or("Điện thoại").into(),
                    last: s["last"].as_f64().unwrap_or(0.0),
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn disconnect_phone(acc: &Account, sid: &str) -> Result<(), String> {
    agent().delete(&acc.api(&format!("/api/device/sessions/{sid}"))).set("Authorization", &acc.auth()).call().map_err(err_of)?;
    Ok(())
}

pub struct InboxItem {
    pub id: String,
    pub key: String,
    pub label: String,
    #[allow(dead_code)]
    pub size: u64,
}

/// Waits up to `secs` for files a phone sent to this PC.
pub fn wait_inbox(acc: &Account, secs: u32) -> Result<Vec<InboxItem>, String> {
    let a = ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout_read(Duration::from_secs(secs as u64 + 20)).build();
    let v: serde_json::Value = a
        .get(&acc.api(&format!("/api/inbox?wait={secs}")))
        .set("Authorization", &acc.auth())
        .call()
        .map_err(err_of)?
        .into_json()
        .map_err(|e| e.to_string())?;
    Ok(v["items"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|x| InboxItem {
                    id: x["id"].as_str().unwrap_or("").into(),
                    key: x["key"].as_str().unwrap_or("").into(),
                    label: x["label"].as_str().unwrap_or("file").into(),
                    size: x["size"].as_u64().unwrap_or(0),
                })
                .collect()
        })
        .unwrap_or_default())
}

pub fn ack(acc: &Account, id: &str) -> Result<(), String> {
    agent().post(&acc.api(&format!("/api/inbox/{id}/ack"))).set("Authorization", &acc.auth()).call().map_err(err_of)?;
    Ok(())
}

/// Downloads + decrypts one inbox item into `dir`.
pub fn fetch_item(acc: &Account, item: &InboxItem, dir: &Path, progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    fetch(&base(&acc.url), &item.id, &item.key, dir, progress)
}

#[derive(Clone, Copy, PartialEq)]
pub enum Mode {
    /// key only in the link (#fragment)
    Link,
    /// also a 6-digit code (key kept on the server)
    Code,
    /// to every phone connected to this PC
    Phones,
}

fn collect(path: &Path, base: &Path, out: &mut Vec<(String, PathBuf)>) {
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                collect(&e.path(), base, out);
            }
        }
    } else {
        let rel = path.strip_prefix(base).unwrap_or(path);
        let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
        out.push((rel, path.to_path_buf()));
    }
}

/// Encrypts and uploads. Several files or a folder are zipped first (kept in memory, max 300 MB).
pub fn upload(acc: &Account, paths: &[PathBuf], mode: Mode, mut progress: impl FnMut(u64, u64) + 'static) -> Result<Shared, String> {
    let url = acc.url.as_str();
    let (name, size, src): (String, u64, Box<dyn Read>) = if paths.len() == 1 && paths[0].is_file() {
        let p = &paths[0];
        let size = std::fs::metadata(p).map_err(|e| e.to_string())?.len();
        let name = p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into());
        (name, size, Box::new(std::fs::File::open(p).map_err(|e| e.to_string())?))
    } else {
        let mut files = Vec::new();
        for p in paths {
            collect(p, p.parent().unwrap_or(Path::new("")), &mut files);
        }
        let total: u64 = files.iter().filter_map(|(_, p)| std::fs::metadata(p).ok()).map(|m| m.len()).sum();
        if total > 300 * 1024 * 1024 {
            return Err("Nhiều file/thư mục chỉ gửi được tối đa 300 MB; hãy nén trước hoặc gửi từng file".into());
        }
        let mut data = Vec::new();
        for (rel, p) in &files {
            data.push((rel.clone(), std::fs::read(p).map_err(|e| e.to_string())?));
        }
        let entries: Vec<(&str, &[u8])> = data.iter().map(|(n, d)| (n.as_str(), d.as_slice())).collect();
        let zip = crate::docs::zip_files(&entries);
        let first = paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| "Alex".into());
        let name = if paths.len() == 1 { format!("{first}.zip") } else { format!("{first} và {} mục.zip", paths.len() - 1) };
        (name, zip.len() as u64, Box::new(std::io::Cursor::new(zip)))
    };

    let rng = SystemRandom::new();
    let mut key_bytes = [0u8; 32];
    rng.fill(&mut key_bytes).map_err(|_| "rng")?;
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key_bytes).map_err(|_| "key")?);
    let meta = serde_json::json!({ "name": name, "size": size, "mime": mime_of(&name) }).to_string().into_bytes();
    let total = encrypted_len(meta.len(), size);
    let mut enc = EncReader {
        src,
        key,
        left: size,
        idx: 0,
        out: Vec::new(),
        pos: 0,
        done: false,
        sent: 0,
        progress: Box::new(move |s| progress(s, total)),
    };
    enc.frame(meta, b"meta").map_err(|e| e.to_string())?;
    let mut head = MAGIC.to_vec();
    head.extend_from_slice(&enc.out);
    enc.out = head;
    if size == 0 {
        // empty file: one empty, final data frame
        let first = std::mem::take(&mut enc.out);
        enc.frame(Vec::new(), &[1]).map_err(|e| e.to_string())?;
        let mut all = first;
        all.extend_from_slice(&enc.out);
        enc.out = all;
        enc.done = true;
    }

    let key_b64 = b64u(&key_bytes);
    let mut req = agent()
        .post(&format!("{}/api/upload", base(url)))
        .set("Authorization", &acc.auth())
        .set("Content-Type", "application/octet-stream")
        .set("Content-Length", &total.to_string());
    if mode != Mode::Link {
        req = req.set("X-Alex-Key", &key_b64);
    }
    if mode == Mode::Phones {
        let label: String = name.bytes().map(|b| if b.is_ascii_alphanumeric() || b".-_ ".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
        req = req.set("X-Alex-To", "phones").set("X-Alex-Label", &label);
    }
    let r = req.send(enc).map_err(err_of)?;
    let v: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
    let id = v["id"].as_str().ok_or("Máy chủ trả dữ liệu lạ")?;
    let hours = (v["expires"].as_f64().unwrap_or(0.0)
        - std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0))
        / 3600.0;
    Ok(Shared { link: format!("{}/s/{id}#{key_b64}", base(url)), code: v["code"].as_str().map(str::to_string), hours: hours.max(0.0) })
}

fn safe_name(n: &str) -> String {
    let s: String = n.chars().map(|c| if "\\/:*?\"<>|".contains(c) || c.is_control() { '_' } else { c }).collect();
    let s = s.trim().trim_matches('.').to_string();
    if s.is_empty() { "file".into() } else { s.chars().take(150).collect() }
}

/// Receives by 6-digit code (or by a full link) and saves into `dir`. Returns the saved path.
pub fn receive(url: &str, code_or_link: &str, dir: &Path, progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    let input = code_or_link.trim();
    let (srv, id, key) = if let Some((left, key)) = input.split_once('#').filter(|_| input.contains("/s/")) {
        let (srv, id) = left.rsplit_once("/s/").ok_or("Link không đúng")?;
        (base(srv), id.to_string(), key.to_string())
    } else {
        let digits: String = input.chars().filter(|c| c.is_ascii_digit()).collect();
        if digits.len() != 6 {
            return Err("Nhập mã 6 số hoặc dán link".into());
        }
        let r = agent().get(&format!("{}/api/code/{digits}", base(url))).call().map_err(err_of)?;
        let v: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
        (base(url), v["id"].as_str().unwrap_or("").to_string(), v["key"].as_str().unwrap_or("").to_string())
    };
    fetch(&srv, &id, &key, dir, progress)
}

fn fetch(srv: &str, id: &str, key: &str, dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
    let key_bytes = b64u_decode(key).filter(|k| k.len() == 32).ok_or("Khoá giải mã không đúng")?;
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, &key_bytes).map_err(|_| "key")?);
    let r = agent().get(&format!("{srv}/api/blob/{id}")).call().map_err(err_of)?;
    let total: u64 = r.header("Content-Length").and_then(|v| v.parse().ok()).ok_or("Máy chủ không báo kích thước")?;
    let mut rd = std::io::BufReader::with_capacity(256 * 1024, r.into_reader());
    let mut magic = [0u8; 5];
    rd.read_exact(&mut magic).map_err(|e| e.to_string())?;
    if &magic != MAGIC {
        return Err("File không đúng định dạng Alex".into());
    }
    let mut got = 5u64;
    let mut idx = 0u64;
    let mut out: Option<(std::fs::File, PathBuf, PathBuf)> = None;
    let res = (|| -> Result<PathBuf, String> {
        while got < total {
            let mut lb = [0u8; 4];
            rd.read_exact(&mut lb).map_err(|e| e.to_string())?;
            let len = u32::from_be_bytes(lb) as usize;
            if !(TAG..=CHUNK + TAG + 64 * 1024).contains(&len) {
                return Err("File bị hỏng".into());
            }
            let mut buf = vec![0u8; len];
            rd.read_exact(&mut buf).map_err(|e| e.to_string())?;
            got += 4 + len as u64;
            let last = got == total;
            let aad: Vec<u8> = if idx == 0 { b"meta".to_vec() } else { vec![last as u8] };
            let plain = key.open_in_place(nonce(idx), Aad::from(aad.as_slice()), &mut buf).map_err(|_| "Sai khoá hoặc file đã bị sửa đổi".to_string())?;
            if idx == 0 {
                let meta: serde_json::Value = serde_json::from_slice(plain).map_err(|e| e.to_string())?;
                let name = safe_name(meta["name"].as_str().unwrap_or("file"));
                std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
                let stem = Path::new(&name).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
                let ext = Path::new(&name).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
                let mut dest = dir.join(&name);
                let mut i = 1;
                while dest.exists() {
                    dest = dir.join(format!("{stem} ({i}){ext}"));
                    i += 1;
                }
                let part = dest.with_extension("alexpart");
                out = Some((std::fs::File::create(&part).map_err(|e| e.to_string())?, part, dest));
            } else if let Some((f, _, _)) = out.as_mut() {
                f.write_all(plain).map_err(|e| e.to_string())?;
            }
            idx += 1;
            progress(got, total);
        }
        let (f, part, dest) = out.take().ok_or("File rỗng")?;
        drop(f);
        if idx < 2 {
            let _ = std::fs::remove_file(&part);
            return Err("File không đầy đủ".into());
        }
        if let Err(e) = std::fs::rename(&part, &dest) {
            let _ = std::fs::remove_file(&part);
            return Err(e.to_string());
        }
        Ok(dest)
    })();
    if res.is_err() {
        if let Some((f, part, _)) = out.take() {
            drop(f);
            let _ = std::fs::remove_file(part);
        }
    }
    res
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn b64_roundtrip() {
        for n in 0..40 {
            let d: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(b64u_decode(&b64u(&d)).unwrap(), d);
        }
        assert_eq!(b64u(&[0u8; 32]).len(), 43);
    }

    /// Needs a running server (v2, accounts): ALEX_SHARE_URL
    #[test]
    #[ignore]
    fn upload_and_receive() {
        let url = std::env::var("ALEX_SHARE_URL").unwrap();
        let (id, secret, password) = register(&url, "Máy test").unwrap();
        println!("registered {} / {password}", pretty_id(&id));
        let acc = Account { url: url.clone(), id, secret };
        ping(&acc).unwrap();
        let d = std::env::temp_dir().join("alex-share-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("in/thu muc")).unwrap();
        let big: Vec<u8> = (0..3_500_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(d.join("in/báo giá Q4.bin"), &big).unwrap();
        std::fs::write(d.join("in/thu muc/a.txt"), "xin chào").unwrap();
        std::fs::write(d.join("in/empty.txt"), "").unwrap();

        // code
        let s = upload(&acc, &[d.join("in/báo giá Q4.bin")], Mode::Code, |_, _| {}).unwrap();
        let got = receive(&url, s.code.as_deref().unwrap(), &d.join("out"), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), big);
        assert_eq!(got.file_name().unwrap().to_string_lossy(), "báo giá Q4.bin");
        // link only
        let s2 = upload(&acc, &[d.join("in/báo giá Q4.bin")], Mode::Link, |_, _| {}).unwrap();
        assert!(s2.code.is_none());
        assert_eq!(std::fs::read(receive(&url, &s2.link, &d.join("out"), |_, _| {}).unwrap()).unwrap(), big);
        // empty file + folder
        let s3 = upload(&acc, &[d.join("in/empty.txt")], Mode::Code, |_, _| {}).unwrap();
        assert_eq!(std::fs::read(receive(&url, s3.code.as_deref().unwrap(), &d.join("out"), |_, _| {}).unwrap()).unwrap().len(), 0);
        let s4 = upload(&acc, &[d.join("in/thu muc")], Mode::Code, |_, _| {}).unwrap();
        assert!(receive(&url, s4.code.as_deref().unwrap(), &d.join("out"), |_, _| {}).unwrap().to_string_lossy().ends_with("thu muc.zip"));
        // to phones (outbox): no code, listed for phones
        let s5 = upload(&acc, &[d.join("in/thu muc/a.txt")], Mode::Phones, |_, _| {}).unwrap();
        assert!(s5.code.is_none());
        // wrong key leaves nothing behind
        let bad = s2.link.rsplit_once('#').unwrap().0.to_string() + "#" + &b64u(&[7u8; 32]);
        assert!(receive(&url, &bad, &d.join("out"), |_, _| {}).is_err());
        assert!(!std::fs::read_dir(d.join("out")).unwrap().flatten().any(|e| e.path().extension().is_some_and(|x| x == "alexpart")));
        // bad secret
        let wrong = Account { secret: "sai".into(), ..acc.clone() };
        assert!(ping(&wrong).is_err());
        assert!(upload(&wrong, &[d.join("in/empty.txt")], Mode::Link, |_, _| {}).is_err());
        // password change + phone list
        let pw = set_password(&acc, Some("matkhau-moi")).unwrap();
        assert_eq!(pw, "matkhau-moi");
        assert!(phones(&acc).unwrap().is_empty());
        // inbox: nothing waiting -> returns empty after the wait
        let t = std::time::Instant::now();
        assert!(wait_inbox(&acc, 2).unwrap().is_empty());
        assert!(t.elapsed().as_secs_f32() >= 1.5);
        println!("ACCOUNT {} {} {}", acc.id, acc.secret, pw);
    }
}

#[cfg(test)]
mod inbox_live {
    use super::*;
    /// ALEX_SHARE_URL, ALEX_ACC_ID, ALEX_ACC_SECRET: take what phones sent, then send one file to the phones
    #[test]
    #[ignore]
    fn inbox_roundtrip() {
        let acc = Account { url: std::env::var("ALEX_SHARE_URL").unwrap(), id: std::env::var("ALEX_ACC_ID").unwrap(), secret: std::env::var("ALEX_ACC_SECRET").unwrap() };
        let dir = std::env::temp_dir().join("alex-inbox-test");
        let _ = std::fs::remove_dir_all(&dir);
        let items = wait_inbox(&acc, 5).unwrap();
        println!("inbox: {} item(s)", items.len());
        for it in &items {
            let p = fetch_item(&acc, it, &dir, |_, _| {}).unwrap();
            ack(&acc, &it.id).unwrap();
            println!("FROM PHONE {} = {:?}", p.file_name().unwrap().to_string_lossy(), std::fs::read_to_string(&p).unwrap_or_default());
        }
        assert!(wait_inbox(&acc, 1).unwrap().is_empty(), "acked items must be gone");
        let f = dir.join("Báo cáo từ máy tính.txt");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(&f, "Máy tính gửi sang điện thoại ✓").unwrap();
        let s = upload(&acc, &[f], Mode::Phones, |_, _| {}).unwrap();
        println!("sent to phones, expires in {:.0}h", s.hours);
    }
}

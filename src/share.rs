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

/// Checks the server address and token. Returns the max upload size.
pub fn ping(url: &str, token: &str) -> Result<u64, String> {
    let r = agent().get(&format!("{}/api/ping", base(url))).set("X-Alex-Token", token).call().map_err(err_of)?;
    let v: serde_json::Value = r.into_json().map_err(|e| e.to_string())?;
    if !v["auth"].as_bool().unwrap_or(false) {
        return Err("Máy chủ chạy, nhưng mã truy cập không đúng".into());
    }
    Ok(v["max_bytes"].as_u64().unwrap_or(0))
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
pub fn upload(url: &str, token: &str, paths: &[PathBuf], with_code: bool, mut progress: impl FnMut(u64, u64) + 'static) -> Result<Shared, String> {
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
        .set("X-Alex-Token", token)
        .set("Content-Type", "application/octet-stream")
        .set("Content-Length", &total.to_string());
    if with_code {
        req = req.set("X-Alex-Key", &key_b64);
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
pub fn receive(url: &str, code_or_link: &str, dir: &Path, mut progress: impl FnMut(u64, u64)) -> Result<PathBuf, String> {
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
    let key_bytes = b64u_decode(&key).filter(|k| k.len() == 32).ok_or("Khoá giải mã không đúng")?;
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

    /// Needs a running server: ALEX_SHARE_URL / ALEX_SHARE_TOKEN
    #[test]
    #[ignore]
    fn upload_and_receive() {
        let url = std::env::var("ALEX_SHARE_URL").unwrap();
        let tok = std::env::var("ALEX_SHARE_TOKEN").unwrap();
        ping(&url, &tok).unwrap();
        let d = std::env::temp_dir().join("alex-share-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("in/thu muc")).unwrap();
        let big: Vec<u8> = (0..3_500_000u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(d.join("in/báo giá Q4.bin"), &big).unwrap();
        std::fs::write(d.join("in/thu muc/a.txt"), "xin chào").unwrap();
        std::fs::write(d.join("in/empty.txt"), "").unwrap();

        // single big file, with code
        let s = upload(&url, &tok, &[d.join("in/báo giá Q4.bin")], true, |_, _| {}).unwrap();
        println!("link: {}\ncode: {:?}", s.link, s.code);
        let got = receive(&url, s.code.as_deref().unwrap(), &d.join("out"), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got).unwrap(), big);
        assert_eq!(got.file_name().unwrap().to_string_lossy(), "báo giá Q4.bin");
        // same file via the link (no code)
        let s2 = upload(&url, &tok, &[d.join("in/báo giá Q4.bin")], false, |_, _| {}).unwrap();
        assert!(s2.code.is_none());
        let got2 = receive(&url, &s2.link, &d.join("out"), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got2).unwrap(), big);
        // empty file
        let s3 = upload(&url, &tok, &[d.join("in/empty.txt")], true, |_, _| {}).unwrap();
        let got3 = receive(&url, s3.code.as_deref().unwrap(), &d.join("out"), |_, _| {}).unwrap();
        assert_eq!(std::fs::read(&got3).unwrap().len(), 0);
        // folder → zip
        let s4 = upload(&url, &tok, &[d.join("in/thu muc")], true, |_, _| {}).unwrap();
        let got4 = receive(&url, s4.code.as_deref().unwrap(), &d.join("out"), |_, _| {}).unwrap();
        assert!(got4.to_string_lossy().ends_with("thu muc.zip"));
        // wrong key must fail and leave nothing behind
        let bad = s2.link.rsplit_once('#').unwrap().0.to_string() + "#" + &b64u(&[7u8; 32]);
        assert!(receive(&url, &bad, &d.join("out"), |_, _| {}).is_err());
        assert!(!std::fs::read_dir(d.join("out")).unwrap().flatten().any(|e| e.path().extension().is_some_and(|x| x == "alexpart")));
        // wrong token
        assert!(upload(&url, "sai", &[d.join("in/empty.txt")], false, |_, _| {}).is_err());
        println!("browser link to try: {}", s.link);
    }
}

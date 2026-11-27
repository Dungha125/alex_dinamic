//! LAN send/receive. Peers find each other with a UDP broadcast; files and text
//! go directly machine-to-machine over TCP (no cloud). The receiver must accept
//! every incoming file transfer before any data is written.
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::path::{Component, Path, PathBuf};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use crate::secure;

pub const UDP_PORT: u16 = 47820;
pub const TCP_PORT: u16 = 47821;
const MAGIC: &str = "ALEX2"; // v2 = encrypted transfers

#[derive(Clone)]
pub struct Peer {
    pub name: String,
    pub ip: IpAddr,
    pub port: u16,
    last: Instant,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Status {
    Waiting,
    Running,
    Done,
    Declined,
    Failed,
}

#[derive(Clone)]
pub struct Transfer {
    pub id: u64,
    pub outgoing: bool,
    pub peer: String,
    pub label: String,
    /// 6-digit verification code, identical on both machines
    pub code: String,
    pub done: u64,
    pub total: u64,
    pub status: Status,
    #[allow(dead_code)]
    pub folder: Option<PathBuf>,
}

pub struct Request {
    pub id: u64,
    pub from: String,
    pub label: String,
    pub code: String,
    pub total: u64,
    reply: Sender<bool>,
}

pub enum Event {
    Text { from: String, text: String },
    Received { from: String, label: String },
    Sent { to: String, label: String },
    Failed { peer: String, why: String },
}

#[derive(Default)]
pub struct State {
    pub me: String,
    pub peers: HashMap<String, Peer>,
    pub transfers: Vec<Transfer>,
    pub requests: Vec<Request>,
    pub events: Vec<Event>,
    pub listening: bool,
    next_id: u64,
}

impl State {
    fn id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
    fn upd(&mut self, id: u64, f: impl FnOnce(&mut Transfer)) {
        if let Some(t) = self.transfers.iter_mut().find(|t| t.id == id) {
            f(t);
        }
    }
    pub fn answer(&mut self, id: u64, accept: bool) {
        if let Some(i) = self.requests.iter().position(|r| r.id == id) {
            let r = self.requests.remove(i);
            let _ = r.reply.send(accept);
        }
    }
    pub fn live_peers(&self) -> Vec<(String, Peer)> {
        let mut v: Vec<_> = self
            .peers
            .iter()
            .filter(|(_, p)| p.last.elapsed() < Duration::from_secs(12))
            .map(|(k, p)| (k.clone(), p.clone()))
            .collect();
        v.sort_by(|a, b| a.1.name.cmp(&b.1.name));
        v
    }
}

pub type Shared = Arc<Mutex<State>>;
type Repaint = Arc<dyn Fn() + Send + Sync>;

#[derive(Serialize, Deserialize)]
struct Hello {
    from: String,
    kind: String, // "files" | "text"
    #[serde(default)]
    text: String,
    #[serde(default)]
    count: u64,
    #[serde(default)]
    total: u64,
    #[serde(default)]
    label: String,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    rel: String,
    size: u64,
}

pub fn downloads_dir() -> PathBuf {
    let home = std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(|| ".".into());
    home.join("Downloads").join("Alex")
}

pub fn human(n: u64) -> String {
    let f = n as f64;
    match n {
        0..=1023 => format!("{n} B"),
        1024..=1_048_575 => format!("{:.0} KB", f / 1024.0),
        1_048_576..=1_073_741_823 => format!("{:.1} MB", f / 1_048_576.0),
        _ => format!("{:.2} GB", f / 1_073_741_824.0),
    }
}

pub fn start(repaint: impl Fn() + Send + Sync + 'static) -> Shared {
    let me = std::env::var("COMPUTERNAME").unwrap_or_else(|_| "PC".into());
    let my_id = format!("{:x}", std::process::id() as u64 ^ (Instant::now().elapsed().as_nanos() as u64) ^ rand_seed());
    let state: Shared = Arc::new(Mutex::new(State { me: me.clone(), ..Default::default() }));
    let repaint: Repaint = Arc::new(repaint);

    // TCP listener
    if let Ok(listener) = TcpListener::bind(("0.0.0.0", TCP_PORT)) {
        state.lock().unwrap().listening = true;
        let (st, rp) = (state.clone(), repaint.clone());
        std::thread::spawn(move || {
            for conn in listener.incoming().flatten() {
                let (st, rp) = (st.clone(), rp.clone());
                std::thread::spawn(move || {
                    let peer_ip = conn.peer_addr().map(|a| a.ip().to_string()).unwrap_or_default();
                    if let Err(e) = receive(conn, &st, &rp) {
                        st.lock().unwrap().events.push(Event::Failed { peer: peer_ip, why: e.to_string() });
                        rp();
                    }
                });
            }
        });
    }

    // UDP discovery: announce every 3 s, listen for others
    if let Ok(sock) = UdpSocket::bind(("0.0.0.0", UDP_PORT)) {
        let _ = sock.set_broadcast(true);
        let _ = sock.set_read_timeout(Some(Duration::from_millis(1000)));
        let (st, rp) = (state.clone(), repaint.clone());
        std::thread::spawn(move || {
            let msg = format!("{MAGIC}|{my_id}|{me}|{TCP_PORT}");
            let mut last_announce = Instant::now() - Duration::from_secs(10);
            let mut buf = [0u8; 512];
            loop {
                if last_announce.elapsed() >= Duration::from_secs(3) {
                    last_announce = Instant::now();
                    let _ = sock.send_to(msg.as_bytes(), (Ipv4Addr::BROADCAST, UDP_PORT));
                }
                if let Ok((n, from)) = sock.recv_from(&mut buf) {
                    let s = String::from_utf8_lossy(&buf[..n]);
                    let parts: Vec<&str> = s.splitn(4, '|').collect();
                    if parts.len() == 4 && parts[0] == MAGIC && parts[1] != my_id {
                        let port = parts[3].parse().unwrap_or(TCP_PORT);
                        let mut g = st.lock().unwrap();
                        let new = !g.peers.contains_key(parts[1]);
                        g.peers.insert(
                            parts[1].to_string(),
                            Peer { name: parts[2].chars().take(40).collect(), ip: from.ip(), port, last: Instant::now() },
                        );
                        drop(g);
                        if new {
                            // answer right away so the newcomer sees us quickly
                            let _ = sock.send_to(msg.as_bytes(), from);
                            rp();
                        }
                    }
                }
                // drop stale peers
                let mut g = st.lock().unwrap();
                let before = g.peers.len();
                g.peers.retain(|_, p| p.last.elapsed() < Duration::from_secs(12));
                if g.peers.len() != before {
                    drop(g);
                    rp();
                }
            }
        });
    }
    state
}

fn rand_seed() -> u64 {
    let x = Box::new(0u8);
    (&*x as *const u8 as u64).wrapping_mul(0x9E3779B97F4A7C15)
        ^ std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_nanos() as u64).unwrap_or(0)
}

fn write_line<T: Serialize>(w: &mut impl Write, v: &T) -> std::io::Result<()> {
    let mut s = serde_json::to_string(v)?;
    s.push('\n');
    w.write_all(s.as_bytes())
}

fn read_line<T: for<'a> Deserialize<'a>>(r: &mut impl BufRead) -> std::io::Result<T> {
    let mut s = String::new();
    let mut lim = r.take(64 * 1024);
    lim.read_line(&mut s)?;
    serde_json::from_str(&s).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
}

/// Accepts only plain relative paths (no "..", no drive letters, no root).
fn safe_rel(rel: &str) -> Option<PathBuf> {
    let p = Path::new(rel);
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::Normal(x) => out.push(x),
            _ => return None,
        }
    }
    (!out.as_os_str().is_empty()).then_some(out)
}

/// `dir/first` or, if taken, `dir/first (1)`, `dir/first (2)`, ...
fn unique(dir: &Path, first: &Path) -> PathBuf {
    let mut top = dir.join(first);
    let stem = first.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    let ext = first.extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default();
    let mut i = 1;
    while top.exists() {
        top = dir.join(format!("{stem} ({i}){ext}"));
        i += 1;
    }
    top
}

fn receive(conn: TcpStream, st: &Shared, rp: &Repaint) -> std::io::Result<()> {
    conn.set_read_timeout(Some(Duration::from_secs(30)))?;
    let (sr, mut w, code) = secure::handshake(conn, false)?;
    let mut r = BufReader::with_capacity(256 * 1024, sr);
    let hello: Hello = read_line(&mut r)?;
    let from: String = hello.from.chars().take(40).collect();

    if hello.kind == "text" {
        let text: String = hello.text.chars().take(200_000).collect();
        st.lock().unwrap().events.push(Event::Text { from, text });
        rp();
        w.write_all(&[1])?;
        return w.flush();
    }

    // ask the user first
    let (tx, rx) = channel();
    let id = {
        let mut g = st.lock().unwrap();
        let id = g.id();
        g.requests.push(Request { id, from: from.clone(), label: hello.label.clone(), code: code.clone(), total: hello.total, reply: tx });
        id
    };
    rp();
    let accept = rx.recv_timeout(Duration::from_secs(60)).unwrap_or(false);
    st.lock().unwrap().requests.retain(|q| q.id != id);
    w.write_all(&[accept as u8])?;
    w.flush()?;
    if !accept {
        rp();
        return Ok(());
    }

    let dir = downloads_dir();
    std::fs::create_dir_all(&dir)?;
    st.lock().unwrap().transfers.insert(
        0,
        Transfer {
            id,
            outgoing: false,
            peer: from.clone(),
            label: hello.label.clone(),
            code,
            done: 0,
            total: hello.total,
            status: Status::Running,
            folder: Some(dir.clone()),
        },
    );
    rp();
    let mut done = 0u64;
    let mut tops: HashMap<PathBuf, PathBuf> = HashMap::new();
    let result = (|| -> std::io::Result<()> {
        let mut last_paint = Instant::now();
        let mut buf = vec![0u8; 256 * 1024];
        for _ in 0..hello.count {
            let e: Entry = read_line(&mut r)?;
            let rel = safe_rel(&e.rel).ok_or_else(|| std::io::Error::other("bad path"))?;
            // keep files of one folder together even if the top folder got renamed
            let top = PathBuf::from(rel.components().next().unwrap().as_os_str());
            let dest_top = tops.entry(top.clone()).or_insert_with(|| unique(&dir, &top)).clone();
            let rest = rel.strip_prefix(&top).unwrap_or(Path::new(""));
            let dest = if rest.as_os_str().is_empty() { dest_top } else { dest_top.join(rest) };
            if let Some(p) = dest.parent() {
                std::fs::create_dir_all(p)?;
            }
            let mut f = std::fs::File::create(&dest)?;
            let mut left = e.size;
            while left > 0 {
                let cap = left.min(buf.len() as u64) as usize;
                let n = r.read(&mut buf[..cap])?;
                if n == 0 {
                    return Err(std::io::Error::other("kết nối bị ngắt"));
                }
                f.write_all(&buf[..n])?;
                left -= n as u64;
                done += n as u64;
                if last_paint.elapsed() > Duration::from_millis(120) {
                    last_paint = Instant::now();
                    st.lock().unwrap().upd(id, |t| t.done = done);
                    rp();
                }
            }
        }
        Ok(())
    })();
    if let Err(e) = result {
        // never leave half-written or unauthenticated data behind
        for t in tops.values() {
            let _ = if t.is_dir() { std::fs::remove_dir_all(t) } else { std::fs::remove_file(t) };
        }
        st.lock().unwrap().upd(id, |t| t.status = Status::Failed);
        return Err(e);
    }
    let mut g = st.lock().unwrap();
    g.upd(id, |t| {
        t.done = done;
        t.status = Status::Done
    });
    g.events.push(Event::Received { from, label: hello.label });
    drop(g);
    rp();
    Ok(())
}

fn collect(path: &Path, base: &Path, out: &mut Vec<(PathBuf, String, u64)>) {
    if path.is_dir() {
        if let Ok(rd) = std::fs::read_dir(path) {
            for e in rd.flatten() {
                collect(&e.path(), base, out);
            }
        }
    } else if let Ok(m) = std::fs::metadata(path) {
        let rel = path.strip_prefix(base).unwrap_or(path);
        let rel = rel.components().map(|c| c.as_os_str().to_string_lossy().into_owned()).collect::<Vec<_>>().join("/");
        out.push((path.to_path_buf(), rel, m.len()));
    }
}

pub fn send_text(st: &Shared, peer: &Peer, text: String, repaint: impl Fn() + Send + 'static) {
    let (st, me, addr) = (st.clone(), st.lock().unwrap().me.clone(), SocketAddr::new(peer.ip, peer.port));
    let name = peer.name.clone();
    std::thread::spawn(move || {
        let r = (|| -> std::io::Result<()> {
            let c = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
            c.set_read_timeout(Some(Duration::from_secs(10)))?;
            let (mut r, mut w, _) = secure::handshake(c, true)?;
            write_line(&mut w, &Hello { from: me, kind: "text".into(), text, count: 0, total: 0, label: String::new() })?;
            w.flush()?;
            let mut b = [0u8];
            r.read_exact(&mut b)
        })();
        st.lock().unwrap().events.push(match r {
            Ok(()) => Event::Sent { to: name, label: "Đoạn chữ".into() },
            Err(e) => Event::Failed { peer: name, why: e.to_string() },
        });
        repaint();
    });
}

pub fn send_files(st: &Shared, peer: &Peer, paths: Vec<PathBuf>, repaint: impl Fn() + Send + 'static) {
    let mut files = Vec::new();
    for p in &paths {
        let base = p.parent().unwrap_or(Path::new(""));
        collect(p, base, &mut files);
    }
    if files.is_empty() {
        return;
    }
    let total: u64 = files.iter().map(|f| f.2).sum();
    let label = if paths.len() == 1 {
        paths[0].file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
    } else {
        format!("{} mục", paths.len())
    };
    let (me, id) = {
        let mut g = st.lock().unwrap();
        let id = g.id();
        g.transfers.insert(
            0,
            Transfer { id, outgoing: true, peer: peer.name.clone(), label: label.clone(), code: String::new(), done: 0, total, status: Status::Waiting, folder: None },
        );
        (g.me.clone(), id)
    };
    let (st, addr, name) = (st.clone(), SocketAddr::new(peer.ip, peer.port), peer.name.clone());
    std::thread::spawn(move || {
        let r = (|| -> std::io::Result<bool> {
            let c = TcpStream::connect_timeout(&addr, Duration::from_secs(5))?;
            c.set_read_timeout(Some(Duration::from_secs(70)))?;
            let (mut r, mut w, code) = secure::handshake(c, true)?;
            st.lock().unwrap().upd(id, |t| t.code = code);
            repaint();
            let hello = Hello { from: me, kind: "files".into(), text: String::new(), count: files.len() as u64, total, label: label.clone() };
            write_line(&mut w, &hello)?;
            w.flush()?;
            let mut b = [0u8];
            r.read_exact(&mut b)?;
            if b[0] != 1 {
                return Ok(false);
            }
            st.lock().unwrap().upd(id, |t| t.status = Status::Running);
            repaint();
            let mut done = 0u64;
            let mut last_paint = Instant::now();
            let mut buf = vec![0u8; 256 * 1024];
            for (path, rel, size) in &files {
                write_line(&mut w, &Entry { rel: rel.clone(), size: *size })?;
                let mut f = std::fs::File::open(path)?.take(*size);
                let mut left = *size;
                while left > 0 {
                    let n = f.read(&mut buf)?;
                    if n == 0 {
                        // file shrank while sending: pad so the stream stays in sync
                        buf.fill(0);
                        let pad = left.min(buf.len() as u64) as usize;
                        w.write_all(&buf[..pad])?;
                        left -= pad as u64;
                        continue;
                    }
                    w.write_all(&buf[..n])?;
                    left -= n as u64;
                    done += n as u64;
                    if last_paint.elapsed() > Duration::from_millis(120) {
                        last_paint = Instant::now();
                        st.lock().unwrap().upd(id, |t| t.done = done);
                        repaint();
                    }
                }
            }
            w.flush()?;
            Ok(true)
        })();
        let mut g = st.lock().unwrap();
        match r {
            Ok(true) => {
                g.upd(id, |t| {
                    t.done = t.total;
                    t.status = Status::Done
                });
                g.events.push(Event::Sent { to: name, label });
            }
            Ok(false) => g.upd(id, |t| t.status = Status::Declined),
            Err(e) => {
                g.upd(id, |t| t.status = Status::Failed);
                g.events.push(Event::Failed { peer: name, why: e.to_string() });
            }
        }
        drop(g);
        repaint();
    });
}

//! Encrypted channel for LAN transfers.
//!
//! Handshake: each side sends `AXE1` + a fresh X25519 public key. The shared
//! secret goes through HKDF-SHA256 into one ChaCha20-Poly1305 key per direction
//! and a 6-digit verification code both users can compare (protects against a
//! man-in-the-middle, like Bluetooth pairing). After that every byte travels in
//! authenticated frames: `u32 BE length | ciphertext + 16-byte tag`, with a
//! per-direction counter as nonce. Tampering, replay or reordering → read error.
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, CHACHA20_POLY1305};
use ring::agreement::{agree_ephemeral, EphemeralPrivateKey, UnparsedPublicKey, X25519};
use ring::hkdf::{KeyType, Salt, HKDF_SHA256};
use ring::rand::SystemRandom;
use std::io::{self, Read, Write};
use std::net::TcpStream;

const MAGIC: &[u8; 4] = b"AXE1";
const SALT: &[u8] = b"alex-lan-v1";
const MAX_PLAIN: usize = 64 * 1024;
const TAG: usize = 16;

pub struct SecureReader {
    s: TcpStream,
    key: LessSafeKey,
    ctr: u64,
    buf: Vec<u8>,
    pos: usize,
}

pub struct SecureWriter {
    s: TcpStream,
    key: LessSafeKey,
    ctr: u64,
    buf: Vec<u8>,
}

struct Len(usize);
impl KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn nonce(ctr: u64) -> Nonce {
    let mut n = [0u8; 12];
    n[4..].copy_from_slice(&ctr.to_be_bytes());
    Nonce::assume_unique_for_key(n)
}

fn err(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.to_string())
}

/// Runs the handshake. `initiator` is true on the side that opened the TCP connection.
/// Returns the reader, writer and the 6-digit verification code ("123 456").
pub fn handshake(stream: TcpStream, initiator: bool) -> io::Result<(SecureReader, SecureWriter, String)> {
    let rng = SystemRandom::new();
    let priv_key = EphemeralPrivateKey::generate(&X25519, &rng).map_err(|_| err("keygen"))?;
    let my_pub = priv_key.compute_public_key().map_err(|_| err("keygen"))?;

    let mut s = stream;
    let mut hello = Vec::with_capacity(36);
    hello.extend_from_slice(MAGIC);
    hello.extend_from_slice(my_pub.as_ref());
    s.write_all(&hello)?;
    let mut peer = [0u8; 36];
    s.read_exact(&mut peer)?;
    if &peer[..4] != MAGIC {
        return Err(err("máy kia dùng phiên bản Alex khác"));
    }
    let peer_pub = &peer[4..];

    let (i_pub, r_pub) = if initiator { (my_pub.as_ref(), peer_pub) } else { (peer_pub, my_pub.as_ref()) };
    let mut transcript = Vec::with_capacity(64);
    transcript.extend_from_slice(i_pub);
    transcript.extend_from_slice(r_pub);

    let (k_i2r, k_r2i, sas) = agree_ephemeral(priv_key, &UnparsedPublicKey::new(&X25519, peer_pub), |shared| {
        let prk = Salt::new(HKDF_SHA256, SALT).extract(shared);
        let derive = |label: &[u8], len: usize| -> Vec<u8> {
            let mut out = vec![0u8; len];
            prk.expand(&[label, &transcript], Len(len)).and_then(|okm| okm.fill(&mut out)).expect("hkdf");
            out
        };
        (derive(b"i2r", 32), derive(b"r2i", 32), derive(b"sas", 4))
    })
    .map_err(|_| err("trao đổi khoá thất bại"))?;

    let key = |k: &[u8]| LessSafeKey::new(UnboundKey::new(&CHACHA20_POLY1305, k).expect("key"));
    let (send, recv) = if initiator { (key(&k_i2r), key(&k_r2i)) } else { (key(&k_r2i), key(&k_i2r)) };
    let code = u32::from_be_bytes([sas[0], sas[1], sas[2], sas[3]]) % 1_000_000;
    let code = format!("{:03} {:03}", code / 1000, code % 1000);

    let r = SecureReader { s: s.try_clone()?, key: recv, ctr: 0, buf: Vec::new(), pos: 0 };
    let w = SecureWriter { s, key: send, ctr: 0, buf: Vec::with_capacity(MAX_PLAIN) };
    Ok((r, w, code))
}

impl SecureReader {
    fn next_frame(&mut self) -> io::Result<bool> {
        let mut len = [0u8; 4];
        match self.s.read_exact(&mut len) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(false),
            Err(e) => return Err(e),
        }
        let n = u32::from_be_bytes(len) as usize;
        if !(TAG..=MAX_PLAIN + TAG).contains(&n) {
            return Err(err("khung dữ liệu không hợp lệ"));
        }
        self.buf.resize(n, 0);
        self.s.read_exact(&mut self.buf)?;
        let plain = self
            .key
            .open_in_place(nonce(self.ctr), Aad::empty(), &mut self.buf)
            .map_err(|_| err("dữ liệu bị sửa đổi hoặc sai khoá"))?
            .len();
        self.ctr += 1;
        self.buf.truncate(plain);
        self.pos = 0;
        Ok(true)
    }
}

impl Read for SecureReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.buf.len() {
            if !self.next_frame()? {
                return Ok(0);
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

impl SecureWriter {
    fn emit(&mut self, take: usize) -> io::Result<()> {
        let mut frame: Vec<u8> = self.buf.drain(..take).collect();
        self.key
            .seal_in_place_append_tag(nonce(self.ctr), Aad::empty(), &mut frame)
            .map_err(|_| err("mã hoá thất bại"))?;
        self.ctr += 1;
        self.s.write_all(&(frame.len() as u32).to_be_bytes())?;
        self.s.write_all(&frame)
    }
}

impl Write for SecureWriter {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        self.buf.extend_from_slice(data);
        while self.buf.len() >= MAX_PLAIN {
            self.emit(MAX_PLAIN)?;
        }
        Ok(data.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if !self.buf.is_empty() {
            let n = self.buf.len();
            self.emit(n)?;
        }
        self.s.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    #[test]
    fn roundtrip() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let (c, _) = l.accept().unwrap();
            let (mut r, mut w, code) = handshake(c, false).unwrap();
            let mut got = Vec::new();
            r.read_to_end(&mut got).unwrap_or(0);
            w.write_all(b"ok").unwrap();
            w.flush().unwrap();
            (got, code)
        });
        let c = TcpStream::connect(addr).unwrap();
        let (mut r, mut w, code) = handshake(c.try_clone().unwrap(), true).unwrap();
        let data: Vec<u8> = (0..200_000u32).map(|i| (i % 251) as u8).collect();
        w.write_all(&data).unwrap();
        w.flush().unwrap();
        c.shutdown(std::net::Shutdown::Write).unwrap();
        let (got, code2) = t.join().unwrap();
        assert_eq!(got, data);
        assert_eq!(code, code2);
        let mut ok = [0u8; 2];
        r.read_exact(&mut ok).unwrap();
        assert_eq!(&ok, b"ok");
    }
}

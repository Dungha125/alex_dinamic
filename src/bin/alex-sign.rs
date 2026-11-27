//! Release signing tool for Alex updates (Ed25519, via ring).
//!   alex-sign keygen <key.pk8>        create a key pair, print the public key (base64url)
//!   alex-sign sign <key.pk8> <text>   print the signature of <text> (base64url)
//!   alex-sign pubkey <key.pk8>        print the public key again
//! Keep key.pk8 private and backed up: without it no update can be published.
use ring::rand::SystemRandom;
use ring::signature::{Ed25519KeyPair, KeyPair};

fn b64u(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut s = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..=c.len() {
            s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
        }
    }
    s
}

fn load(path: &str) -> Ed25519KeyPair {
    let bytes = std::fs::read(path).unwrap_or_else(|e| fail(&format!("cannot read {path}: {e}")));
    Ed25519KeyPair::from_pkcs8(&bytes).unwrap_or_else(|_| fail("not an Ed25519 PKCS#8 key"))
}

fn fail(msg: &str) -> ! {
    eprintln!("alex-sign: {msg}");
    std::process::exit(1)
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["keygen", path] => {
            if std::path::Path::new(path).exists() {
                fail("key file already exists; refusing to overwrite it");
            }
            let pk8 = Ed25519KeyPair::generate_pkcs8(&SystemRandom::new()).unwrap_or_else(|_| fail("keygen failed"));
            if let Some(dir) = std::path::Path::new(path).parent() {
                let _ = std::fs::create_dir_all(dir);
            }
            std::fs::write(path, pk8.as_ref()).unwrap_or_else(|e| fail(&e.to_string()));
            println!("{}", b64u(load(path).public_key().as_ref()));
        }
        ["pubkey", path] => println!("{}", b64u(load(path).public_key().as_ref())),
        ["sign", path, text] => println!("{}", b64u(load(path).sign(text.as_bytes()).as_ref())),
        _ => fail("usage: alex-sign keygen|pubkey <key.pk8> | sign <key.pk8> <text>"),
    }
}

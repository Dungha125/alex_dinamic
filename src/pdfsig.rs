//! Digital-signature check for PDFs, using Windows CryptoAPI (no extra crates).
//! For each signature dictionary: the bytes named by /ByteRange are verified
//! against the detached PKCS#7 / CAdES blob in /Contents; then the signer's
//! certificate chain is checked against the Windows trust store.
use std::path::Path;
use windows::Win32::Security::Cryptography::*;

pub struct SigReport {
    pub signer: String,
    pub when: String,
    pub reason: String,
    pub intact: bool,
    pub covers_all: bool,
    pub trusted: Option<bool>,
    pub note: String,
}

fn int(o: &lopdf::Object) -> Option<usize> {
    o.as_i64().ok().map(|v| v.max(0) as usize)
}

fn text(d: &lopdf::Dictionary, key: &[u8]) -> String {
    d.get(key)
        .ok()
        .and_then(|o| o.as_str().ok())
        .map(|b| {
            if b.starts_with(&[0xFE, 0xFF]) {
                let u: Vec<u16> = b[2..].chunks_exact(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                String::from_utf16_lossy(&u)
            } else {
                String::from_utf8_lossy(b).into_owned()
            }
        })
        .unwrap_or_default()
}

/// "D:20261127093000+07'00'" → "27/11/2026 09:30"
fn pdf_date(s: &str) -> String {
    let d: String = s.trim_start_matches("D:").chars().take(12).collect();
    if d.len() >= 12 && d.chars().all(|c| c.is_ascii_digit()) {
        format!("{}/{}/{} {}:{}", &d[6..8], &d[4..6], &d[0..4], &d[8..10], &d[10..12])
    } else {
        s.to_string()
    }
}

/// DER length of a SEQUENCE so the zero padding after the blob is dropped.
fn der_len(b: &[u8]) -> usize {
    if b.len() < 2 || b[0] != 0x30 {
        return b.len();
    }
    let l = b[1] as usize;
    if l < 0x80 {
        return (2 + l).min(b.len());
    }
    let n = l & 0x7f;
    if n == 0 || n > 4 || b.len() < 2 + n {
        return b.len();
    }
    let mut len = 0usize;
    for i in 0..n {
        len = len << 8 | b[2 + i] as usize;
    }
    (2 + n + len).min(b.len())
}

pub fn verify(path: &Path) -> Result<Vec<SigReport>, String> {
    let bytes = std::fs::read(path).map_err(|e| e.to_string())?;
    let doc = lopdf::Document::load_mem(&bytes).map_err(|e| format!("Không đọc được PDF: {e}"))?;
    let mut out = Vec::new();
    for obj in doc.objects.values() {
        let Ok(d) = obj.as_dict() else { continue };
        let (Ok(br), Ok(contents)) = (d.get(b"ByteRange"), d.get(b"Contents")) else { continue };
        let Ok(br) = br.as_array() else { continue };
        let r: Vec<usize> = br.iter().filter_map(int).collect();
        let Ok(blob) = contents.as_str() else { continue };
        let mut rep = SigReport {
            signer: text(d, b"Name"),
            when: pdf_date(&text(d, b"M")),
            reason: text(d, b"Reason"),
            intact: false,
            covers_all: false,
            trusted: None,
            note: String::new(),
        };
        let sub = d.get(b"SubFilter").ok().and_then(|o| o.as_name().ok()).map(|n| String::from_utf8_lossy(n).into_owned()).unwrap_or_default();
        if r.len() != 4 || r[0] + r[1] > bytes.len() || r[2] + r[3] > bytes.len() {
            rep.note = "ByteRange không hợp lệ".into();
            out.push(rep);
            continue;
        }
        if sub == "adbe.pkcs7.sha1" || sub == "adbe.x509.rsa_sha1" {
            rep.note = format!("Kiểu chữ ký cũ {sub} chưa hỗ trợ kiểm tra");
            out.push(rep);
            continue;
        }
        rep.covers_all = r[2] + r[3] == bytes.len();
        let p7 = &blob[..der_len(blob)];
        let part1 = &bytes[r[0]..r[0] + r[1]];
        let part2 = &bytes[r[2]..r[2] + r[3]];
        unsafe {
            let para = CRYPT_VERIFY_MESSAGE_PARA {
                cbSize: std::mem::size_of::<CRYPT_VERIFY_MESSAGE_PARA>() as u32,
                dwMsgAndCertEncodingType: (PKCS_7_ASN_ENCODING.0 | X509_ASN_ENCODING.0),
                ..Default::default()
            };
            let ptrs = [part1.as_ptr(), part2.as_ptr()];
            let lens = [part1.len() as u32, part2.len() as u32];
            let mut cert: *mut CERT_CONTEXT = std::ptr::null_mut();
            let ok = CryptVerifyDetachedMessageSignature(&para, 0, p7, 2, ptrs.as_ptr(), lens.as_ptr(), Some(&mut cert));
            rep.intact = ok.is_ok();
            if !cert.is_null() {
                let mut buf = [0u16; 256];
                let n = CertGetNameStringW(cert, CERT_NAME_SIMPLE_DISPLAY_TYPE, 0, None, Some(&mut buf));
                if n > 1 {
                    rep.signer = String::from_utf16_lossy(&buf[..n as usize - 1]);
                }
                let chain_para = CERT_CHAIN_PARA { cbSize: std::mem::size_of::<CERT_CHAIN_PARA>() as u32, ..Default::default() };
                let mut chain: *mut CERT_CHAIN_CONTEXT = std::ptr::null_mut();
                if CertGetCertificateChain(None, cert, None, Some((*cert).hCertStore), &chain_para, 0, None, &mut chain).is_ok() && !chain.is_null() {
                    let status = (*chain).TrustStatus.dwErrorStatus;
                    rep.trusted = Some(status == 0);
                    if status & CERT_TRUST_IS_NOT_TIME_VALID != 0 {
                        rep.note = "chứng thư hiện đã hết hạn (có thể còn hạn lúc ký)".into();
                    } else if status & (CERT_TRUST_IS_UNTRUSTED_ROOT | CERT_TRUST_IS_PARTIAL_CHAIN) != 0 {
                        rep.note = "chứng thư không thuộc nhà cung cấp được Windows tin cậy (tự ký?)".into();
                    } else if status & CERT_TRUST_IS_REVOKED != 0 {
                        rep.note = "chứng thư đã bị thu hồi".into();
                    }
                    CertFreeCertificateChain(chain);
                }
                let _ = CertFreeCertificateContext(Some(cert));
            }
            if !rep.intact && rep.note.is_empty() {
                rep.note = "nội dung đã bị sửa sau khi ký, hoặc chữ ký hỏng".into();
            }
        }
        out.push(rep);
    }
    Ok(out)
}

pub fn summary(reports: &[SigReport]) -> String {
    if reports.is_empty() {
        return "PDF này không có chữ ký số".into();
    }
    let mut lines = Vec::new();
    for (i, r) in reports.iter().enumerate() {
        let state = match (r.intact, r.trusted) {
            (true, Some(true)) if r.covers_all => "✓ Hợp lệ",
            (true, Some(true)) => "✓ Hợp lệ (có nội dung thêm sau khi ký)",
            (true, _) if r.covers_all => "! Nội dung nguyên vẹn, chưa xác thực người ký",
            (true, _) => "! Nguyên vẹn đến lúc ký, có nội dung thêm sau",
            (false, _) => "✕ Không hợp lệ",
        };
        let mut l = format!("{}. {state} · {}", i + 1, if r.signer.is_empty() { "không rõ người ký" } else { &r.signer });
        if !r.when.is_empty() {
            l += &format!(" · {}", r.when);
        }
        if !r.reason.is_empty() {
            l += &format!(" · {}", r.reason);
        }
        if !r.note.is_empty() {
            l += &format!(" ({})", r.note);
        }
        lines.push(l);
    }
    lines.join("\n")
}

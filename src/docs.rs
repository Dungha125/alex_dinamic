//! Office tools that run entirely on this PC (nothing is uploaded).
//! Results are written next to the original file.
//!
//! * Word / Excel / PowerPoint → PDF, merge Word, merge Excel: Microsoft Office via COM
//!   (LibreOffice as fallback for → PDF).
//! * Merge / split / compress PDF: `lopdf`.
//! * Images → PDF: `pdf-writer`.  PDF → PNG: Windows' built-in PDF renderer (Windows.Data.Pdf).
//! * Compress / convert images: `image`.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Clone, Copy, PartialEq)]
pub enum Tool {
    ToPdf,
    ImagesToPdf,
    MergePdf,
    SplitPdf,
    CompressPdf,
    PdfToImages,
    CompressImages,
    ToPng,
    ToJpg,
    MergeWord,
    MergeExcel,
    PdfToWord,
    HeicToJpg,
    VerifySig,
    /// these three open the PDF window instead of running a job
    ViewPdf,
    EditPdf,
    ReorderPdf,
}

#[derive(Clone, Copy, PartialEq)]
pub enum JobStatus {
    Running,
    Done,
    Failed,
}

#[derive(Clone)]
pub struct Job {
    id: u64,
    pub title: String,
    pub status: JobStatus,
    pub detail: String,
    pub output: Option<PathBuf>,
}

pub type Jobs = Arc<Mutex<Vec<Job>>>;
type Out = Result<(PathBuf, String), String>;

#[derive(PartialEq, Clone, Copy)]
enum Kind {
    Word,
    Excel,
    Ppt,
    Pdf,
    Image,
    Heic,
    Other,
}

fn kind(p: &Path) -> Kind {
    let e = p.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
    match e.as_str() {
        "doc" | "docx" | "docm" | "rtf" | "odt" => Kind::Word,
        "xls" | "xlsx" | "xlsm" | "csv" | "ods" => Kind::Excel,
        "ppt" | "pptx" | "pptm" | "odp" => Kind::Ppt,
        "pdf" => Kind::Pdf,
        "jpg" | "jpeg" | "png" | "bmp" | "gif" | "webp" | "tif" | "tiff" => Kind::Image,
        "heic" | "heif" | "hif" => Kind::Heic,
        _ => Kind::Other,
    }
}

pub fn is_supported(p: &Path) -> bool {
    p.is_file() && kind(p) != Kind::Other
}

/// Tools that make sense for the dropped files, in display order.
pub fn applicable(files: &[PathBuf]) -> Vec<(Tool, &'static str)> {
    let count = |k: Kind| files.iter().filter(|f| kind(f) == k).count();
    let (w, x, p, pdf, img) = (count(Kind::Word), count(Kind::Excel), count(Kind::Ppt), count(Kind::Pdf), count(Kind::Image));
    let heic = count(Kind::Heic);
    let mut v = Vec::new();
    if heic >= 1 {
        v.push((Tool::HeicToJpg, "HEIC sang JPG"));
    }
    if w + x + p > 0 {
        v.push((Tool::ToPdf, "Sang PDF"));
    }
    if pdf == 1 {
        v.push((Tool::ViewPdf, "Xem, tìm chữ"));
        v.push((Tool::EditPdf, "Tô vẽ, gõ chữ"));
        v.push((Tool::ReorderPdf, "Sắp xếp trang"));
    }
    if pdf >= 2 {
        v.push((Tool::MergePdf, "Gộp PDF"));
    }
    if pdf >= 1 {
        v.push((Tool::SplitPdf, "Tách trang"));
        v.push((Tool::PdfToImages, "PDF sang ảnh"));
        v.push((Tool::PdfToWord, "PDF sang Word"));
        v.push((Tool::CompressPdf, "Nén PDF"));
        v.push((Tool::VerifySig, "Kiểm tra chữ ký số"));
    }
    if img >= 1 {
        v.push((Tool::ImagesToPdf, "Ảnh sang PDF"));
        v.push((Tool::CompressImages, "Nén ảnh"));
        v.push((Tool::ToPng, "Sang PNG"));
        v.push((Tool::ToJpg, "Sang JPG"));
    }
    if w >= 2 {
        v.push((Tool::MergeWord, "Gộp Word"));
    }
    if x >= 2 {
        v.push((Tool::MergeExcel, "Gộp Excel"));
    }
    v
}

pub fn start(tool: Tool, files: Vec<PathBuf>, jobs: &Jobs, repaint: impl Fn() + Send + Sync + 'static) {
    let pick = |k: &[Kind]| files.iter().filter(|f| k.contains(&kind(f))).cloned().collect::<Vec<_>>();
    // one job per file for per-file tools, one job for combining tools
    let mut work: Vec<(String, Box<dyn FnOnce() -> Out + Send>)> = Vec::new();
    let name = |p: &Path| p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    match tool {
        Tool::ToPdf => {
            for f in pick(&[Kind::Word, Kind::Excel, Kind::Ppt]) {
                work.push((format!("{} → PDF", name(&f)), Box::new(move || office_to_pdf(&f))));
            }
        }
        Tool::SplitPdf => {
            for f in pick(&[Kind::Pdf]) {
                work.push((format!("Tách {}", name(&f)), Box::new(move || split_pdf(&f))));
            }
        }
        Tool::CompressPdf => {
            for f in pick(&[Kind::Pdf]) {
                work.push((format!("Nén {}", name(&f)), Box::new(move || compress_pdf(&f))));
            }
        }
        Tool::PdfToImages => {
            for f in pick(&[Kind::Pdf]) {
                work.push((format!("{} → ảnh", name(&f)), Box::new(move || pdf_to_images(&f))));
            }
        }
        Tool::MergePdf => {
            let f = pick(&[Kind::Pdf]);
            work.push((format!("Gộp {} PDF", f.len()), Box::new(move || merge_pdfs(&f))));
        }
        Tool::ImagesToPdf => {
            let f = pick(&[Kind::Image]);
            work.push((format!("{} ảnh → PDF", f.len()), Box::new(move || images_to_pdf(&f))));
        }
        Tool::CompressImages | Tool::ToPng | Tool::ToJpg => {
            let f = pick(&[Kind::Image]);
            let title = match tool {
                Tool::CompressImages => format!("Nén {} ảnh", f.len()),
                Tool::ToPng => format!("{} ảnh → PNG", f.len()),
                _ => format!("{} ảnh → JPG", f.len()),
            };
            work.push((title, Box::new(move || convert_images(&f, tool))));
        }
        Tool::MergeWord => {
            let f = pick(&[Kind::Word]);
            work.push((format!("Gộp {} file Word", f.len()), Box::new(move || merge_office(&f, "mergeword", "docx"))));
        }
        Tool::PdfToWord => {
            for f in pick(&[Kind::Pdf]) {
                work.push((format!("{} → Word", name(&f)), Box::new(move || pdf_to_word(&f))));
            }
        }
        Tool::HeicToJpg => {
            let f = pick(&[Kind::Heic]);
            work.push((format!("{} ảnh HEIC → JPG", f.len()), Box::new(move || heic_to_jpg(&f))));
        }
        Tool::VerifySig => {
            for f in pick(&[Kind::Pdf]) {
                work.push((format!("Chữ ký số · {}", name(&f)), Box::new(move || {
                    let r = crate::pdfsig::verify(&f)?;
                    Ok((f.clone(), crate::pdfsig::summary(&r)))
                })));
            }
        }
        Tool::ViewPdf | Tool::EditPdf | Tool::ReorderPdf => {}
        Tool::MergeExcel => {
            let f = pick(&[Kind::Excel]);
            work.push((format!("Gộp {} file Excel", f.len()), Box::new(move || merge_office(&f, "mergeexcel", "xlsx"))));
        }
    }
    let repaint = Arc::new(repaint);
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    for (title, f) in work {
        let id = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        {
            let mut g = jobs.lock().unwrap();
            g.insert(0, Job { id, title, status: JobStatus::Running, detail: "Đang xử lý…".into(), output: None });
            g.truncate(8);
        }
        let (jobs, repaint) = (jobs.clone(), repaint.clone());
        repaint();
        std::thread::spawn(move || {
            let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)).unwrap_or_else(|_| Err("lỗi không xác định".into()));
            let mut g = jobs.lock().unwrap();
            if let Some(j) = g.iter_mut().find(|j| j.id == id) {
                match r {
                    Ok((out, detail)) => {
                        j.status = JobStatus::Done;
                        j.detail = detail;
                        j.output = Some(out);
                    }
                    Err(e) => {
                        j.status = JobStatus::Failed;
                        j.detail = e;
                    }
                }
            }
            drop(g);
            repaint();
        });
    }
}

// ------------------------------------------------------------------ helpers
fn stem(p: &Path) -> String {
    p.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_else(|| "file".into())
}

fn dir_of(p: &Path) -> PathBuf {
    p.parent().map(Path::to_path_buf).unwrap_or_else(|| ".".into())
}

/// `dir/name.ext`, or `dir/name (1).ext`, `dir/name (2).ext`, ... if taken.
fn unique(dir: &Path, name: &str, ext: &str) -> PathBuf {
    let dot = if ext.is_empty() { String::new() } else { format!(".{ext}") };
    let mut p = dir.join(format!("{name}{dot}"));
    let mut i = 1;
    while p.exists() {
        p = dir.join(format!("{name} ({i}){dot}"));
        i += 1;
    }
    p
}

fn size(p: &Path) -> u64 {
    std::fs::metadata(p).map(|m| m.len()).unwrap_or(0)
}

fn human(n: u64) -> String {
    crate::net::human(n)
}

fn e2s(e: impl std::fmt::Display) -> String {
    e.to_string()
}

// ------------------------------------------------------------------ Office (COM)
const PS_SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$kind = $env:ALEX_KIND; $out = $env:ALEX_OUT; $ins = $env:ALEX_IN -split '\|'
$exe = @{ word='WINWORD'; mergeword='WINWORD'; excel='EXCEL'; mergeexcel='EXCEL'; ppt='POWERPNT' }[$kind]
$before = @(Get-Process -Name $exe -ErrorAction SilentlyContinue | ForEach-Object Id)
function Save-Pid { Get-Process -Name $exe -ErrorAction SilentlyContinue | Where-Object { $before -notcontains $_.Id } | Select-Object -First 1 | ForEach-Object { Set-Content -Path $env:ALEX_PIDFILE -Value $_.Id } }
switch ($kind) {
  'word' {
    $a = New-Object -ComObject Word.Application; Save-Pid; $a.Visible = $false; $a.DisplayAlerts = 0
    try { $d = $a.Documents.Open($ins[0], $false, $true, $false); $d.ExportAsFixedFormat($out, 17); $d.Close(0) } finally { $a.Quit() }
  }
  'excel' {
    $a = New-Object -ComObject Excel.Application; Save-Pid; $a.Visible = $false; $a.DisplayAlerts = $false
    try { $w = $a.Workbooks.Open($ins[0], 0, $true); $w.ExportAsFixedFormat(0, $out); $w.Close($false) } finally { $a.Quit() }
  }
  'ppt' {
    $a = New-Object -ComObject PowerPoint.Application; Save-Pid
    try { $p = $a.Presentations.Open($ins[0], -1, 0, 0); $p.SaveAs($out, 32); $p.Close() } finally { $a.Quit() }
  }
  'mergeword' {
    $a = New-Object -ComObject Word.Application; $a.Visible = $false; $a.DisplayAlerts = 0
    try {
      Save-Pid; $d = $a.Documents.Add(); $first = $true
      foreach ($f in $ins) {
        $r = $d.Content; $r.Collapse(0)
        if (-not $first) { $r.InsertBreak(7); $r = $d.Content; $r.Collapse(0) }
        $r.InsertFile($f); $first = $false
      }
      $d.SaveAs2($out, 16); $d.Close(0)
    } finally { $a.Quit() }
  }
  'mergeexcel' {
    $a = New-Object -ComObject Excel.Application; $a.Visible = $false; $a.DisplayAlerts = $false
    try {
      Save-Pid; $dst = $a.Workbooks.Add(); $blank = $dst.Worksheets.Item(1)
      foreach ($f in $ins) {
        $w = $a.Workbooks.Open($f, 0, $true)
        foreach ($s in $w.Worksheets) { $s.Copy([Type]::Missing, $dst.Worksheets.Item($dst.Worksheets.Count)) }
        $w.Close($false)
      }
      $blank.Delete(); $dst.SaveAs($out, 51); $dst.Close($false)
    } finally { $a.Quit() }
  }
}
"#;

fn b64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            if i <= c.len() {
                s.push(T[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                s.push('=');
            }
        }
    }
    s
}

/// Runs the COM script in a hidden PowerShell. Paths go through environment
/// variables, so no quoting/injection issues with odd file names.
fn run_office(kind: &str, inputs: &[PathBuf], out: &Path) -> Result<(), String> {
    use std::os::windows::process::CommandExt;
    let utf16: Vec<u8> = PS_SCRIPT.encode_utf16().flat_map(|u| u.to_le_bytes()).collect();
    let joined = inputs.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join("|");
    static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
    let pidfile = std::env::temp_dir().join(format!(
        "alex-office-{}-{}.pid",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
    ));
    let _ = std::fs::remove_file(&pidfile);
    let o = std::process::Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-EncodedCommand", &b64(&utf16)])
        .env("ALEX_KIND", kind)
        .env("ALEX_IN", joined)
        .env("ALEX_OUT", out)
        .env("ALEX_PIDFILE", &pidfile)
        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(e2s)?;
    // never hang forever on a hidden Office dialog
    let start = std::time::Instant::now();
    let mut child = o;
    let status = loop {
        if let Some(st) = child.try_wait().map_err(e2s)? {
            break st;
        }
        // ALEX_OFFICE_TIMEOUT (seconds) is only for tests/diagnostics
        let limit = std::env::var("ALEX_OFFICE_TIMEOUT").ok().and_then(|v| v.parse().ok()).unwrap_or(240u64);
        if start.elapsed() > std::time::Duration::from_secs(limit) {
            let _ = child.kill();
            // close only the hidden Office instance this run started
            if let Some(pid) = std::fs::read_to_string(&pidfile).ok().and_then(|s| s.trim().parse::<u32>().ok()) {
                use std::os::windows::process::CommandExt;
                let _ = std::process::Command::new("taskkill")
                    .args(["/PID", &pid.to_string(), "/F"])
                    .creation_flags(0x0800_0000)
                    .status();
            }
            let _ = std::fs::remove_file(&pidfile);
            return Err(format!("Office không phản hồi sau {} giây", limit));
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    };
    let mut err_bytes = Vec::new();
    if let Some(mut e) = child.stderr.take() {
        use std::io::Read;
        let _ = e.read_to_end(&mut err_bytes);
    }
    let _ = std::fs::remove_file(&pidfile);
    let o = std::process::Output { status, stdout: Vec::new(), stderr: err_bytes };
    if o.status.success() && out.exists() {
        Ok(())
    } else {
        let err = String::from_utf8_lossy(&o.stderr);
        let line = err.lines().find(|l| !l.trim().is_empty()).unwrap_or("Office không chạy được").trim().to_string();
        Err(line.chars().take(120).collect())
    }
}

fn soffice() -> Option<PathBuf> {
    ["C:\\Program Files\\LibreOffice\\program\\soffice.exe", "C:\\Program Files (x86)\\LibreOffice\\program\\soffice.exe"]
        .iter()
        .map(PathBuf::from)
        .find(|p| p.exists())
}

fn office_to_pdf(f: &Path) -> Out {
    let out = unique(&dir_of(f), &stem(f), "pdf");
    let k = match kind(f) {
        Kind::Word => "word",
        Kind::Excel => "excel",
        _ => "ppt",
    };
    let com = run_office(k, &[f.to_path_buf()], &out);
    if com.is_ok() {
        return Ok((out.clone(), format!("{} · cạnh file gốc", human(size(&out)))));
    }
    if let Some(so) = soffice() {
        use std::os::windows::process::CommandExt;
        let tmp = std::env::temp_dir().join(format!("alex-lo-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&tmp);
        let st = std::process::Command::new(so)
            .args(["--headless", "--convert-to", "pdf", "--outdir"])
            .arg(&tmp)
            .arg(f)
            .creation_flags(0x0800_0000)
            .status()
            .map_err(e2s)?;
        let produced = tmp.join(format!("{}.pdf", stem(f)));
        if st.success() && produced.exists() {
            std::fs::copy(&produced, &out).map_err(e2s)?;
            let _ = std::fs::remove_dir_all(&tmp);
            return Ok((out.clone(), format!("{} · LibreOffice", human(size(&out)))));
        }
    }
    Err(format!("Cần Microsoft Office hoặc LibreOffice. {}", com.unwrap_err()))
}

fn merge_office(files: &[PathBuf], kind: &str, ext: &str) -> Out {
    let first = files.first().ok_or("không có file")?;
    let out = unique(&dir_of(first), &format!("Gộp - {}", stem(first)), ext);
    run_office(kind, files, &out).map_err(|e| format!("Cần Microsoft Office. {e}"))?;
    Ok((out.clone(), format!("{} file · {}", files.len(), human(size(&out)))))
}

// ------------------------------------------------------------------ PDF (lopdf)
fn load_pdf(p: &Path) -> Result<lopdf::Document, String> {
    let d = lopdf::Document::load(p).map_err(|e| format!("Không đọc được {}: {e}", p.display()))?;
    if d.is_encrypted() {
        return Err("PDF có mật khẩu, chưa hỗ trợ".into());
    }
    Ok(d)
}

fn merge_pdfs(files: &[PathBuf]) -> Out {
    use lopdf::{Dictionary, Document, Object, ObjectId};
    let first = files.first().ok_or("không có file")?;
    let mut max_id = 1;
    let mut pages: Vec<(ObjectId, Dictionary)> = Vec::new();
    let mut objects = std::collections::BTreeMap::new();
    for f in files {
        let mut doc = load_pdf(f)?;
        doc.renumber_objects_with(max_id);
        max_id = doc.max_id + 1;
        for (_, pid) in doc.get_pages() {
            // pages inherit these from their parents; copy them down before re-parenting
            let mut dict = doc.get_dictionary(pid).map_err(e2s)?.clone();
            for key in [b"Resources".as_slice(), b"MediaBox", b"CropBox", b"Rotate"] {
                if dict.has(key) {
                    continue;
                }
                let mut parent = dict.get(b"Parent").and_then(Object::as_reference).ok();
                while let Some(pr) = parent {
                    let Ok(pd) = doc.get_dictionary(pr) else { break };
                    if let Ok(v) = pd.get(key) {
                        dict.set(key, v.clone());
                        break;
                    }
                    parent = pd.get(b"Parent").and_then(Object::as_reference).ok();
                }
            }
            pages.push((pid, dict));
        }
        objects.extend(doc.objects);
    }
    let mut out_doc = Document::with_version("1.7");
    for (id, obj) in objects {
        match obj.type_name().unwrap_or(b"") {
            b"Catalog" | b"Pages" | b"Page" | b"Outlines" | b"Outline" => {}
            _ => {
                out_doc.objects.insert(id, obj);
            }
        }
    }
    let pages_id = out_doc.new_object_id();
    let count = pages.len() as i64;
    let mut kids = Vec::new();
    for (pid, mut dict) in pages {
        dict.set("Parent", pages_id);
        out_doc.objects.insert(pid, Object::Dictionary(dict));
        kids.push(Object::Reference(pid));
    }
    let mut pd = Dictionary::new();
    pd.set("Type", Object::Name(b"Pages".to_vec()));
    pd.set("Kids", kids);
    pd.set("Count", count);
    out_doc.objects.insert(pages_id, Object::Dictionary(pd));
    let mut cat = Dictionary::new();
    cat.set("Type", Object::Name(b"Catalog".to_vec()));
    cat.set("Pages", pages_id);
    let cat_id = out_doc.add_object(cat);
    out_doc.trailer.set("Root", cat_id);
    out_doc.renumber_objects();
    out_doc.prune_objects();
    out_doc.compress();
    let out = unique(&dir_of(first), &format!("Gộp - {}", stem(first)), "pdf");
    out_doc.save(&out).map_err(e2s)?;
    Ok((out.clone(), format!("{} file · {count} trang · {}", files.len(), human(size(&out)))))
}

fn split_pdf(f: &Path) -> Out {
    let doc = load_pdf(f)?;
    let n = doc.get_pages().len() as u32;
    if n < 2 {
        return Err("PDF chỉ có 1 trang".into());
    }
    let dir = unique(&dir_of(f), &format!("{} (tách trang)", stem(f)), "");
    std::fs::create_dir_all(&dir).map_err(e2s)?;
    let width = n.to_string().len();
    for page in 1..=n {
        let mut d = doc.clone();
        let others: Vec<u32> = (1..=n).filter(|p| *p != page).collect();
        d.delete_pages(&others);
        d.prune_objects();
        d.compress();
        d.save(dir.join(format!("{} - trang {:0width$}.pdf", stem(f), page))).map_err(e2s)?;
    }
    Ok((dir, format!("{n} file, mỗi file 1 trang")))
}

fn compress_pdf(f: &Path) -> Out {
    let before = size(f);
    let mut d = load_pdf(f)?;
    d.delete_zero_length_streams();
    d.prune_objects();
    d.compress();
    let out = unique(&dir_of(f), &format!("{} (nén)", stem(f)), "pdf");
    d.save(&out).map_err(e2s)?;
    let after = size(&out);
    if after >= before {
        let _ = std::fs::remove_file(&out);
        return Err("File đã tối ưu, không giảm thêm được".into());
    }
    Ok((out, format!("{} → {} (−{}%)", human(before), human(after), 100 - after * 100 / before.max(1))))
}

// ------------------------------------------------------------------ PDF → PNG (Windows.Data.Pdf)
fn pdf_to_images(f: &Path) -> Out {
    use windows::core::HSTRING;
    use windows::Data::Pdf::{PdfDocument, PdfPageRenderOptions};
    use windows::Graphics::Imaging::BitmapEncoder;
    use windows::Storage::StorageFile;
    use windows::Storage::Streams::{DataReader, InMemoryRandomAccessStream};
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let abs = std::fs::canonicalize(f).map_err(e2s)?;
    let abs = abs.display().to_string();
    let abs = abs.strip_prefix(r"\\?\").unwrap_or(&abs).to_string();
    let w = |e: windows::core::Error| e.message().to_string();
    let file = StorageFile::GetFileFromPathAsync(&HSTRING::from(abs)).map_err(w)?.join().map_err(w)?;
    let doc = PdfDocument::LoadFromFileAsync(&file).map_err(w)?.join().map_err(w)?;
    if doc.IsPasswordProtected().unwrap_or(false) {
        return Err("PDF có mật khẩu, chưa hỗ trợ".into());
    }
    let n = doc.PageCount().map_err(w)?;
    let dir = unique(&dir_of(f), &format!("{} (ảnh)", stem(f)), "");
    std::fs::create_dir_all(&dir).map_err(e2s)?;
    let width = n.to_string().len();
    for i in 0..n {
        let page = doc.GetPage(i).map_err(w)?;
        let sz = page.Size().map_err(w)?;
        let opts = PdfPageRenderOptions::new().map_err(w)?;
        // ~150 DPI (PDF points are 1/72")
        opts.SetDestinationWidth((sz.Width * 150.0 / 72.0) as u32).map_err(w)?;
        opts.SetBitmapEncoderId(BitmapEncoder::PngEncoderId().map_err(w)?).map_err(w)?;
        let stream = InMemoryRandomAccessStream::new().map_err(w)?;
        page.RenderWithOptionsToStreamAsync(&stream, &opts).map_err(w)?.join().map_err(w)?;
        let len = stream.Size().map_err(w)? as u32;
        let reader = DataReader::CreateDataReader(&stream.GetInputStreamAt(0).map_err(w)?).map_err(w)?;
        reader.LoadAsync(len).map_err(w)?.join().map_err(w)?;
        let mut buf = vec![0u8; len as usize];
        reader.ReadBytes(&mut buf).map_err(w)?;
        std::fs::write(dir.join(format!("{} - trang {:0width$}.png", stem(f), i + 1)), buf).map_err(e2s)?;
    }
    Ok((dir, format!("{n} ảnh PNG (~150 DPI)")))
}

// ------------------------------------------------------------------ images
/// Number of colour components of a baseline/progressive JPEG (1 gray, 3 RGB, 4 CMYK).
fn jpeg_components(b: &[u8]) -> Option<u8> {
    let mut i = 2;
    while i + 9 < b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if matches!(m, 0xC0 | 0xC1 | 0xC2) {
            return Some(b[i + 9]);
        }
        i += 2 + len;
    }
    None
}

fn images_to_pdf(files: &[PathBuf]) -> Out {
    use pdf_writer::{Content, Filter, Finish, Name, Pdf, Rect, Ref};
    let first = files.first().ok_or("không có ảnh")?;
    let mut pdf = Pdf::new();
    let catalog = Ref::new(1);
    let tree = Ref::new(2);
    let mut next = 3;
    let mut alloc = || {
        next += 1;
        Ref::new(next - 1)
    };
    let mut kids = Vec::new();
    for f in files {
        let bytes = std::fs::read(f).map_err(e2s)?;
        let (page_id, img_id, content_id) = (alloc(), alloc(), alloc());
        let is_jpeg = bytes.starts_with(&[0xFF, 0xD8]);
        let comps = if is_jpeg { jpeg_components(&bytes) } else { None };
        let (w, h);
        let mut smask = None;
        if let (true, Some(c @ (1 | 3))) = (is_jpeg, comps) {
            // embed the JPEG as-is: no quality loss, tiny file
            let (iw, ih) = image::image_dimensions(f).map_err(e2s)?;
            (w, h) = (iw, ih);
            let mut x = pdf.image_xobject(img_id, &bytes);
            x.filter(Filter::DctDecode);
            x.width(w as i32).height(h as i32).bits_per_component(8);
            if c == 1 {
                x.color_space().device_gray();
            } else {
                x.color_space().device_rgb();
            }
            x.finish();
        } else {
            let img = image::load_from_memory(&bytes).map_err(|e| format!("{}: {e}", f.display()))?;
            (w, h) = (img.width(), img.height());
            let rgba = img.to_rgba8();
            let has_alpha = rgba.pixels().any(|p| p[3] < 255);
            let rgb: Vec<u8> = rgba.pixels().flat_map(|p| [p[0], p[1], p[2]]).collect();
            let data = miniz_oxide::deflate::compress_to_vec_zlib(&rgb, 6);
            if has_alpha {
                let a: Vec<u8> = rgba.pixels().map(|p| p[3]).collect();
                let sid = alloc();
                let ad = miniz_oxide::deflate::compress_to_vec_zlib(&a, 6);
                let mut s = pdf.image_xobject(sid, &ad);
                s.filter(Filter::FlateDecode);
                s.width(w as i32).height(h as i32).bits_per_component(8);
                s.color_space().device_gray();
                s.finish();
                smask = Some(sid);
            }
            let mut x = pdf.image_xobject(img_id, &data);
            x.filter(Filter::FlateDecode);
            x.width(w as i32).height(h as i32).bits_per_component(8);
            x.color_space().device_rgb();
            if let Some(s) = smask {
                x.s_mask(s);
            }
            x.finish();
        }
        // A4, orientation follows the image, 24 pt margin, image fitted & centred
        let (pw, ph) = if w > h { (842.0, 595.0) } else { (595.0, 842.0) };
        let m = 24.0;
        let s = ((pw - 2.0 * m) / w as f32).min((ph - 2.0 * m) / h as f32);
        let (dw, dh) = (w as f32 * s, h as f32 * s);
        let mut page = pdf.page(page_id);
        page.media_box(Rect::new(0.0, 0.0, pw, ph)).parent(tree).contents(content_id);
        page.resources().x_objects().pair(Name(b"Im"), img_id);
        page.finish();
        let mut c = Content::new();
        c.save_state();
        c.transform([dw, 0.0, 0.0, dh, (pw - dw) / 2.0, (ph - dh) / 2.0]);
        c.x_object(Name(b"Im"));
        c.restore_state();
        pdf.stream(content_id, &c.finish());
        kids.push(page_id);
    }
    pdf.catalog(catalog).pages(tree);
    pdf.pages(tree).kids(kids.iter().copied()).count(kids.len() as i32);
    let name = if files.len() == 1 { stem(first) } else { format!("{} và {} ảnh khác", stem(first), files.len() - 1) };
    let out = unique(&dir_of(first), &name, "pdf");
    std::fs::write(&out, pdf.finish()).map_err(e2s)?;
    Ok((out.clone(), format!("{} trang A4 · {}", files.len(), human(size(&out)))))
}

fn convert_images(files: &[PathBuf], tool: Tool) -> Out {
    use image::codecs::jpeg::JpegEncoder;
    use image::codecs::png::{CompressionType, FilterType, PngEncoder};
    use image::ImageEncoder;
    let (mut before, mut after) = (0u64, 0u64);
    let mut last = None;
    for f in files {
        let img = image::open(f).map_err(|e| format!("{}: {e}", f.display()))?;
        let has_alpha = img.color().has_alpha() && img.to_rgba8().pixels().any(|p| p[3] < 255);
        let mut img = img;
        let ext = match tool {
            Tool::ToPng => "png",
            Tool::ToJpg => "jpg",
            _ => {
                // compress: cap the long side at 2560 px
                if img.width().max(img.height()) > 2560 {
                    img = img.resize(2560, 2560, image::imageops::FilterType::Lanczos3);
                }
                if has_alpha { "png" } else { "jpg" }
            }
        };
        let suffix = if tool == Tool::CompressImages { " (nén)" } else { "" };
        let out = unique(&dir_of(f), &format!("{}{suffix}", stem(f)), ext);
        let file = std::io::BufWriter::new(std::fs::File::create(&out).map_err(e2s)?);
        if ext == "png" {
            let rgba = img.to_rgba8();
            PngEncoder::new_with_quality(file, CompressionType::Best, FilterType::Adaptive)
                .write_image(&rgba, rgba.width(), rgba.height(), image::ExtendedColorType::Rgba8)
                .map_err(e2s)?;
        } else {
            // flatten transparency on white
            let rgba = img.to_rgba8();
            let rgb: Vec<u8> = rgba
                .pixels()
                .flat_map(|p| {
                    let a = p[3] as u32;
                    [0, 1, 2].map(|i| ((p[i] as u32 * a + 255 * (255 - a)) / 255) as u8)
                })
                .collect();
            let q = if tool == Tool::CompressImages { 75 } else { 90 };
            JpegEncoder::new_with_quality(file, q)
                .write_image(&rgb, rgba.width(), rgba.height(), image::ExtendedColorType::Rgb8)
                .map_err(e2s)?;
        }
        before += size(f);
        after += size(&out);
        last = Some(out);
    }
    let out = last.ok_or("không có ảnh")?;
    let detail = if tool == Tool::CompressImages {
        let pct = if before > 0 { 100 - (after * 100 / before).min(100) } else { 0 };
        format!("{} → {} (−{pct}%)", human(before), human(after))
    } else {
        format!("{} ảnh · cạnh file gốc", files.len())
    };
    Ok((out, detail))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp() -> PathBuf {
        let d = std::env::temp_dir().join("alex-docs-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn pages(p: &Path) -> usize {
        lopdf::Document::load(p).unwrap().get_pages().len()
    }

    #[test]
    fn pdf_and_image_pipeline() {
        let d = tmp();
        // sample images: opaque JPEG, transparent PNG
        let jpg = d.join("anh 1.jpg");
        let png = d.join("logo.png");
        image::RgbImage::from_fn(800, 600, |x, y| image::Rgb([(x / 4) as u8, (y / 3) as u8, 160])).save(&jpg).unwrap();
        image::RgbaImage::from_fn(300, 300, |x, y| image::Rgba([0, 120, 255, if (x + y) % 60 < 30 { 255 } else { 0 }])).save(&png).unwrap();

        let (one, _) = images_to_pdf(&[jpg.clone(), png.clone()]).unwrap();
        assert_eq!(pages(&one), 2);
        let (two, _) = images_to_pdf(&[jpg.clone()]).unwrap();
        assert_eq!(pages(&two), 1);

        let (merged, detail) = merge_pdfs(&[one.clone(), two.clone()]).unwrap();
        println!("merge: {detail}");
        assert_eq!(pages(&merged), 3);

        let (split_dir, _) = split_pdf(&merged).unwrap();
        let parts: Vec<_> = std::fs::read_dir(&split_dir).unwrap().flatten().collect();
        assert_eq!(parts.len(), 3);
        for p in &parts {
            assert_eq!(pages(&p.path()), 1);
        }

        match compress_pdf(&merged) {
            Ok((_, d)) => println!("compress: {d}"),
            Err(e) => println!("compress: {e}"),
        }

        let (img_dir, _) = pdf_to_images(&merged).unwrap();
        let pngs: Vec<_> = std::fs::read_dir(&img_dir).unwrap().flatten().collect();
        assert_eq!(pngs.len(), 3);
        let (w, _) = image::image_dimensions(pngs[0].path()).unwrap();
        assert!(w > 1000, "rendered width {w}");

        let (_, c) = convert_images(&[jpg.clone(), png.clone()], Tool::CompressImages).unwrap();
        println!("compress images: {c}");
        convert_images(&[png.clone()], Tool::ToJpg).unwrap();
        convert_images(&[jpg.clone()], Tool::ToPng).unwrap();
        assert!(d.join("logo.jpg").exists() && d.join("anh 1.png").exists());
        println!("outputs in {}", d.display());
    }

    #[test]
    #[ignore] // needs Microsoft Office; run with: cargo test -- --ignored
    fn office_pipeline() {
        let d = std::env::temp_dir().join("alex-office-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let rtf = |n: &str, t: &str| {
            let p = d.join(n);
            std::fs::write(&p, format!(r"{{\rtf1\ansi {{\b {t}}}\par Dong hai.\par}}")).unwrap();
            p
        };
        let csv = |n: &str, t: &str| {
            let p = d.join(n);
            std::fs::write(&p, t).unwrap();
            p
        };
        let a = rtf("hop dong 'A'.rtf", "Hop dong A");
        let b = rtf("bao cao B.rtf", "Bao cao B");
        let x = csv("doanh thu.csv", "Thang,Doanh thu\n10,120\n11,150\n");
        let y = csv("chi phi.csv", "Thang,Chi phi\n10,80\n11,95\n");
        let (p, dd) = office_to_pdf(&a).unwrap();
        println!("word->pdf {dd}");
        assert!(pages(&p) >= 1);
        let (p, dd) = office_to_pdf(&x).unwrap();
        println!("excel->pdf {dd}");
        assert!(pages(&p) >= 1);
        let (p, dd) = merge_office(&[a, b], "mergeword", "docx").unwrap();
        println!("merge word {dd} {}", p.display());
        let (p, dd) = merge_office(&[x, y], "mergeexcel", "xlsx").unwrap();
        println!("merge excel {dd} {}", p.display());
    }
}


// ------------------------------------------------------------------ PDF → Word, HEIC → JPG
/// PDF → Word without Office: the text of every page goes into a real .docx
/// (one paragraph per line, a page break between pages). Fast and never hangs;
/// complex layout (tables, columns, images) is not reproduced.
fn pdf_to_word(f: &Path) -> Out {
    let doc = load_pdf(f)?;
    let n = doc.get_pages().len() as u32;
    let pages: Vec<String> = (1..=n).map(|p| doc.extract_text(&[p]).unwrap_or_default()).collect();
    if pages.iter().all(|t| t.trim().is_empty()) {
        return Err("PDF này không có lớp chữ (có thể là ảnh quét), chưa chuyển được sang Word".into());
    }
    let out = unique(&dir_of(f), &stem(f), "docx");
    std::fs::write(&out, build_docx(&pages)).map_err(e2s)?;
    Ok((out.clone(), format!("{n} trang · {} · giữ chữ, không giữ bố cục phức tạp", human(size(&out)))))
}

fn xml_escape(s: &str) -> String {
    s.chars()
        .filter(|c| !c.is_control() || *c == '\t')
        .map(|c| match c {
            '&' => "&amp;".to_string(),
            '<' => "&lt;".to_string(),
            '>' => "&gt;".to_string(),
            '"' => "&quot;".to_string(),
            c => c.to_string(),
        })
        .collect()
}

fn build_docx(pages: &[String]) -> Vec<u8> {
    let mut body = String::new();
    for (i, page) in pages.iter().enumerate() {
        if i > 0 {
            body += r#"<w:p><w:r><w:br w:type="page"/></w:r></w:p>"#;
        }
        for line in page.lines() {
            let t = line.trim_end();
            if t.is_empty() {
                body += "<w:p/>";
            } else {
                body += &format!(r#"<w:p><w:r><w:t xml:space="preserve">{}</w:t></w:r></w:p>"#, xml_escape(t));
            }
        }
    }
    let document = format!(
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body>{body}<w:sectPr><w:pgSz w:w="11906" w:h="16838"/><w:pgMar w:top="1134" w:right="1134" w:bottom="1134" w:left="1134" w:header="709" w:footer="709" w:gutter="0"/></w:sectPr></w:body></w:document>"#
    );
    let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
    let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;
    zip(&[
        ("[Content_Types].xml", content_types.as_bytes()),
        ("_rels/.rels", rels.as_bytes()),
        ("word/document.xml", document.as_bytes()),
    ])
}

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for b in data {
        c ^= *b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

/// Minimal ZIP writer (deflate), enough for an Office Open XML package.
fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let comp = miniz_oxide::deflate::compress_to_vec(data, 6);
        let crc = crc32(data);
        let offset = out.len() as u32;
        let header = |sig: u32, central: bool| {
            let mut h = Vec::new();
            h.extend_from_slice(&sig.to_le_bytes());
            if central {
                h.extend_from_slice(&20u16.to_le_bytes()); // made by
            }
            h.extend_from_slice(&20u16.to_le_bytes()); // needed
            h.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
            h.extend_from_slice(&8u16.to_le_bytes()); // deflate
            h.extend_from_slice(&0u16.to_le_bytes()); // time
            h.extend_from_slice(&0x21u16.to_le_bytes()); // date 1980-01-01
            h.extend_from_slice(&crc.to_le_bytes());
            h.extend_from_slice(&(comp.len() as u32).to_le_bytes());
            h.extend_from_slice(&(data.len() as u32).to_le_bytes());
            h.extend_from_slice(&(name.len() as u16).to_le_bytes());
            h.extend_from_slice(&0u16.to_le_bytes()); // extra
            if central {
                h.extend_from_slice(&[0u8; 6]); // comment len, disk start, internal attrs
                h.extend_from_slice(&0u32.to_le_bytes()); // external attrs
                h.extend_from_slice(&offset.to_le_bytes());
            }
            h.extend_from_slice(name.as_bytes());
            h
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(&comp);
        central.extend(header(0x0201_4b50, true));
    }
    let cd_offset = out.len() as u32;
    let cd_len = central.len() as u32;
    out.extend(central);
    out.extend_from_slice(&0x0605_4b50u32.to_le_bytes());
    out.extend_from_slice(&[0u8; 4]);
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&(entries.len() as u16).to_le_bytes());
    out.extend_from_slice(&cd_len.to_le_bytes());
    out.extend_from_slice(&cd_offset.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

pub const HEIF_STORE: &str = "ms-windows-store://pdp/?ProductId=9PMMSR1CGPWG";

/// Decodes with Windows' own HEIF codec (colour-managed to sRGB, EXIF rotation applied)
/// and writes a JPEG at quality 0.95. Keeps the original file's modified time.
fn heic_to_jpg(files: &[PathBuf]) -> Out {
    use windows::core::HSTRING;
    use windows::Foundation::PropertyValue;
    use windows::Graphics::Imaging::*;
    use windows::Storage::{FileAccessMode, StorageFile};
    unsafe {
        let _ = windows::Win32::System::Com::CoInitializeEx(None, windows::Win32::System::Com::COINIT_MULTITHREADED);
    }
    let w = |e: windows::core::Error| e.message().to_string();
    let open = |p: &Path| -> Result<StorageFile, String> {
        let abs = std::fs::canonicalize(p).map_err(e2s)?.display().to_string();
        let abs = abs.strip_prefix(r"\\?\").unwrap_or(&abs).to_string();
        StorageFile::GetFileFromPathAsync(&HSTRING::from(abs)).map_err(w)?.join().map_err(w)
    };
    let (mut before, mut after, mut last) = (0u64, 0u64, None);
    for f in files {
        let src = open(f)?.OpenAsync(FileAccessMode::Read).map_err(w)?.join().map_err(w)?;
        let decoder = match BitmapDecoder::CreateAsync(&src).and_then(|op| op.join()) {
            Ok(d) => d,
            Err(_) => {
                return Err("Windows chưa đọc được HEIC: cài “HEIF Image Extensions” (miễn phí) từ Microsoft Store rồi thử lại".into())
            }
        };
        let bmp = decoder
            .GetSoftwareBitmapTransformedAsync(
                BitmapPixelFormat::Bgra8,
                BitmapAlphaMode::Ignore,
                &BitmapTransform::new().map_err(w)?,
                ExifOrientationMode::RespectExifOrientation,
                ColorManagementMode::ColorManageToSRgb,
            )
            .map_err(w)?
            .join()
            .map_err(|e| format!("Không giải mã được ảnh: {}", e.message()))?;
        let out = unique(&dir_of(f), &stem(f), "jpg");
        std::fs::File::create(&out).map_err(e2s)?;
        let dst = open(&out)?.OpenAsync(FileAccessMode::ReadWrite).map_err(w)?.join().map_err(w)?;
        let opts = BitmapPropertySet::new().map_err(w)?;
        let q = BitmapTypedValue::Create(&PropertyValue::CreateSingle(0.95).map_err(w)?, windows::Foundation::PropertyType::Single).map_err(w)?;
        opts.Insert(&HSTRING::from("ImageQuality"), &q).map_err(w)?;
        let enc = BitmapEncoder::CreateWithEncodingOptionsAsync(
            BitmapEncoder::JpegEncoderId().map_err(w)?,
            &dst,
            &opts,
        )
        .map_err(w)?
        .join()
        .map_err(w)?;
        enc.SetSoftwareBitmap(&bmp).map_err(w)?;
        enc.FlushAsync().map_err(w)?.join().map_err(w)?;
        // release every WinRT handle on the output before touching its timestamp
        drop(enc);
        if let Ok(c) = windows::core::Interface::cast::<windows::Foundation::IClosable>(&dst) {
            let _ = c.Close();
        }
        drop(dst);
        drop(src);
        if let Ok(t) = std::fs::metadata(f).and_then(|m| m.modified()) {
            match std::fs::OpenOptions::new().write(true).open(&out) {
                Ok(fh) => {
                    let _ = fh.set_modified(t);
                }
                Err(e) => eprintln!("keep date failed: {e}"),
            }
        }
        before += size(f);
        after += size(&out);
        last = Some(out);
    }
    let out = last.ok_or("không có ảnh")?;
    Ok((out, format!("{} ảnh · JPG chất lượng 95% · {} → {}", files.len(), human(before), human(after))))
}

#[cfg(test)]
mod tests2 {
    use super::*;
    fn sample(n: &str) -> PathBuf {
        PathBuf::from(std::env::var("ALEX_SAMPLE").expect("ALEX_SAMPLE")).join(n)
    }

    #[test]
    #[ignore] // needs ALEX_SAMPLE with signed.pdf / tampered.pdf
    fn signatures() {
        let ok = crate::pdfsig::verify(&sample("signed.pdf")).unwrap();
        println!("signed:\n{}", crate::pdfsig::summary(&ok));
        assert_eq!(ok.len(), 1);
        assert!(ok[0].intact && ok[0].covers_all);
        assert_eq!(ok[0].trusted, Some(false));
        let bad = crate::pdfsig::verify(&sample("tampered.pdf")).unwrap();
        println!("tampered:\n{}", crate::pdfsig::summary(&bad));
        assert!(!bad[0].intact);
    }

    #[test]
    #[ignore] // needs ALEX_SAMPLE/test.heic (and Windows' HEIF codec)
    fn heic() {
        let d = std::env::temp_dir().join("alex-heic-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let f = d.join("test.heic");
        std::fs::copy(sample("test.heic"), &f).unwrap();
        match heic_to_jpg(&[f]) {
            Ok((out, detail)) => {
                println!("heic: {detail}");
                let (w, h) = image::image_dimensions(&out).unwrap();
                assert_eq!((w, h), (1600, 1200));
            }
            Err(e) => println!("heic error: {e}"),
        }
    }

    #[test]
    #[ignore] // needs Word + ALEX_SAMPLE/signed.pdf
    fn pdf_word() {
        let d = std::env::temp_dir().join("alex-pdfword-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let name = std::env::var("ALEX_PDFWORD_SAMPLE").unwrap_or_else(|_| "signed.pdf".into());
        let f = d.join("hop dong.pdf");
        std::fs::copy(sample(&name), &f).unwrap();
        let t = std::time::Instant::now();
        let r = pdf_to_word(&f);
        println!("pdf->word in {:?}: {:?}", t.elapsed(), r.as_ref().map(|x| &x.1));
        assert!(r.is_ok());
    }
}

#[cfg(test)]
pub fn test_images_pdf(files: &[PathBuf]) -> Vec<u8> {
    let (p, _) = images_to_pdf(files).unwrap();
    std::fs::read(p).unwrap()
}

#[cfg(test)]
mod render_check {
    #[test]
    #[ignore] // ALEX_RENDER=<pdf> renders it to PNGs next to it
    fn render() {
        let p = std::path::PathBuf::from(std::env::var("ALEX_RENDER").unwrap());
        println!("{:?}", super::pdf_to_images(&p).map(|x| x.0));
    }
}

#[cfg(test)]
mod word_opens {
    #[test]
    #[ignore] // ALEX_DOCX=<file.docx>: Word must be able to open it (exports to PDF)
    fn word_can_open() {
        let p = std::path::PathBuf::from(std::env::var("ALEX_DOCX").unwrap());
        let r = super::office_to_pdf(&p);
        println!("word open+export: {:?}", r.as_ref().map(|x| &x.1));
        assert!(r.is_ok());
    }
}

/// Zip several files in memory (used when sharing a folder or many files).
pub fn zip_files(entries: &[(&str, &[u8])]) -> Vec<u8> {
    zip(entries)
}

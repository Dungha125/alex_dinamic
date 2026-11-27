//! Writes an edited copy of a PDF: page order / rotation / deletion, plus pen
//! strokes, highlights and typed text drawn as real PDF vector content.
//! Typed text embeds Segoe UI (Type0 / Identity-H) so Vietnamese renders
//! everywhere and stays selectable/searchable (ToUnicode map included).
use lopdf::{Dictionary, Document, Object, ObjectId, Stream};
use std::collections::BTreeMap;
use std::path::Path;

/// Coordinates are normalised to the page *as displayed* (0..1, top-left origin,
/// existing /Rotate already applied), so they match what the viewer shows.
#[derive(Clone)]
pub enum Mark {
    Ink { pts: Vec<(f32, f32)>, color: [u8; 3], width: f32 },
    Highlight { a: (f32, f32), b: (f32, f32), color: [u8; 3] },
    Text { pos: (f32, f32), text: String, size: f32, color: [u8; 3] },
}

pub struct PageEdit {
    /// 1-based page number in the source file
    pub src: u32,
    /// extra clockwise rotation in degrees (0/90/180/270)
    pub rotate: i32,
}

fn inherited(doc: &Document, page: ObjectId, key: &[u8]) -> Option<Object> {
    let mut cur = Some(page);
    while let Some(id) = cur {
        let d = doc.get_dictionary(id).ok()?;
        if let Ok(v) = d.get(key) {
            return Some(v.clone());
        }
        cur = d.get(b"Parent").and_then(Object::as_reference).ok();
    }
    None
}

fn resolve(doc: &Document, o: Object) -> Object {
    match o {
        Object::Reference(id) => doc.get_object(id).cloned().unwrap_or(Object::Null),
        x => x,
    }
}

fn num(o: &Object) -> f32 {
    match o {
        Object::Integer(i) => *i as f32,
        Object::Real(r) => *r,
        _ => 0.0,
    }
}

/// Visible box (CropBox, else MediaBox) as (x0, y0, x1, y1).
fn page_box(doc: &Document, page: ObjectId) -> (f32, f32, f32, f32) {
    let b = inherited(doc, page, b"CropBox").or_else(|| inherited(doc, page, b"MediaBox"));
    let v: Vec<f32> = b
        .map(|o| resolve(doc, o))
        .and_then(|o| o.as_array().ok().cloned())
        .map(|a| a.iter().map(|x| num(&resolve(doc, x.clone()))).collect())
        .unwrap_or_default();
    if v.len() == 4 {
        (v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3]))
    } else {
        (0.0, 0.0, 595.0, 842.0)
    }
}

fn page_rotation(doc: &Document, page: ObjectId) -> i32 {
    inherited(doc, page, b"Rotate").map(|o| num(&resolve(doc, o)) as i32).unwrap_or(0).rem_euclid(360) / 90 * 90
}

struct Map {
    b: (f32, f32, f32, f32),
    rot: i32,
}

impl Map {
    /// displayed normalised (u, v) → PDF user space
    fn pt(&self, u: f32, v: f32) -> (f32, f32) {
        let (x0, y0, x1, y1) = self.b;
        let (w, h) = (x1 - x0, y1 - y0);
        match self.rot {
            90 => (x0 + v * w, y0 + u * h),
            180 => (x1 - u * w, y0 + v * h),
            270 => (x1 - v * w, y1 - u * h),
            _ => (x0 + u * w, y1 - v * h),
        }
    }
    fn disp_h(&self) -> f32 {
        let (x0, y0, x1, y1) = self.b;
        if self.rot % 180 == 90 { x1 - x0 } else { y1 - y0 }
    }
    /// text matrix rotation so text reads upright on the displayed page
    fn tm(&self) -> [f32; 4] {
        match self.rot {
            90 => [0.0, 1.0, -1.0, 0.0],
            180 => [-1.0, 0.0, 0.0, -1.0],
            270 => [0.0, -1.0, 1.0, 0.0],
            _ => [1.0, 0.0, 0.0, 1.0],
        }
    }
}

struct Font {
    data: Vec<u8>,
    units: f32,
    ascent: f32,
}

impl Font {
    fn load() -> Result<Self, String> {
        let dir = std::env::var("WINDIR").unwrap_or_else(|_| "C:\\Windows".into()) + "\\Fonts\\";
        let data = ["segoeui.ttf", "arial.ttf", "tahoma.ttf"]
            .iter()
            .find_map(|f| std::fs::read(format!("{dir}{f}")).ok())
            .ok_or("không tìm thấy font Segoe UI/Arial")?;
        let face = ttf_parser::Face::parse(&data, 0).map_err(|e| e.to_string())?;
        let units = face.units_per_em() as f32;
        let ascent = face.ascender() as f32 / units;
        Ok(Font { data, units, ascent })
    }
}

fn f(x: f32) -> String {
    let s = format!("{x:.3}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s.is_empty() || s == "-0" { "0".into() } else { s.to_string() }
}

fn rgb(c: [u8; 3]) -> String {
    format!("{} {} {}", f(c[0] as f32 / 255.0), f(c[1] as f32 / 255.0), f(c[2] as f32 / 255.0))
}

pub fn save(src: &Path, order: &[PageEdit], marks: &BTreeMap<u32, Vec<Mark>>, out: &Path) -> Result<(), String> {
    let mut doc = Document::load(src).map_err(|e| format!("Không đọc được PDF: {e}"))?;
    if doc.is_encrypted() {
        return Err("PDF có mật khẩu, chưa hỗ trợ".into());
    }
    if order.is_empty() {
        return Err("Không còn trang nào để lưu".into());
    }
    let pages = doc.get_pages();

    // ---------------------------------------------------------------- font (only if text is used)
    let any_text = marks.values().flatten().any(|m| matches!(m, Mark::Text { .. }));
    let mut font_ref = None;
    let mut glyphs: BTreeMap<u16, (char, u16)> = BTreeMap::new(); // gid -> (char, advance)
    let font = if any_text { Some(Font::load()?) } else { None };
    let face = font.as_ref().map(|fo| ttf_parser::Face::parse(&fo.data, 0).unwrap());
    let encode = |text: &str, glyphs: &mut BTreeMap<u16, (char, u16)>| -> String {
        let face = face.as_ref().unwrap();
        let mut hex = String::new();
        for c in text.chars() {
            let gid = face.glyph_index(c).map(|g| g.0).unwrap_or(0);
            let adv = face.glyph_hor_advance(ttf_parser::GlyphId(gid)).unwrap_or(0);
            glyphs.entry(gid).or_insert((c, adv));
            hex += &format!("{gid:04X}");
        }
        hex
    };

    // ---------------------------------------------------------------- drawings per source page
    let mut page_streams: Vec<(ObjectId, Vec<u8>, bool, bool)> = Vec::new();
    for (&src_page, list) in marks {
        if list.is_empty() {
            continue;
        }
        let Some(&pid) = pages.get(&src_page) else { continue };
        let map = Map { b: page_box(&doc, pid), rot: page_rotation(&doc, pid) };
        let mut c = String::from("Q q\n");
        let (mut uses_font, mut uses_gs) = (false, false);
        for m in list {
            match m {
                Mark::Ink { pts, color, width } if pts.len() >= 2 => {
                    c += &format!("{} RG {} w 1 J 1 j\n", rgb(*color), f(*width));
                    for (i, (u, v)) in pts.iter().enumerate() {
                        let (x, y) = map.pt(*u, *v);
                        c += &format!("{} {} {}\n", f(x), f(y), if i == 0 { "m" } else { "l" });
                    }
                    c += "S\n";
                }
                Mark::Highlight { a, b, color } => {
                    uses_gs = true;
                    let corners = [(a.0, a.1), (b.0, a.1), (b.0, b.1), (a.0, b.1)];
                    c += &format!("q /AlexHL gs {} rg\n", rgb(*color));
                    for (i, (u, v)) in corners.iter().enumerate() {
                        let (x, y) = map.pt(*u, *v);
                        c += &format!("{} {} {}\n", f(x), f(y), if i == 0 { "m" } else { "l" });
                    }
                    c += "h f Q\n";
                }
                Mark::Text { pos, text, size, color } => {
                    let Some(fo) = &font else { continue };
                    uses_font = true;
                    let line_h = size * 1.25;
                    for (i, line) in text.lines().enumerate() {
                        let dv = (fo.ascent * size + i as f32 * line_h) / map.disp_h();
                        let (x, y) = map.pt(pos.0, pos.1 + dv);
                        let [a, b, cc, d] = map.tm();
                        let hex = encode(line, &mut glyphs);
                        c += &format!(
                            "BT /AlexF {} Tf {} rg {} {} {} {} {} {} Tm <{hex}> Tj ET\n",
                            f(*size),
                            rgb(*color),
                            f(a),
                            f(b),
                            f(cc),
                            f(d),
                            f(x),
                            f(y)
                        );
                    }
                }
                _ => {}
            }
        }
        c += "Q\n";
        page_streams.push((pid, c.into_bytes(), uses_font, uses_gs));
    }

    if let (Some(fo), true) = (&font, !glyphs.is_empty()) {
        let face = face.as_ref().unwrap();
        let k = 1000.0 / fo.units;
        let mut ff = Stream::new(Dictionary::from_iter(vec![("Length1", Object::Integer(fo.data.len() as i64))]), fo.data.clone());
        let _ = ff.compress();
        let ff_id = doc.add_object(ff);
        let bb = face.global_bounding_box();
        let desc = Dictionary::from_iter(vec![
            ("Type", Object::Name(b"FontDescriptor".to_vec())),
            ("FontName", Object::Name(b"AlexSegoeUI".to_vec())),
            ("Flags", Object::Integer(32)),
            (
                "FontBBox",
                Object::Array(vec![
                    (bb.x_min as f32 * k).into(),
                    (bb.y_min as f32 * k).into(),
                    (bb.x_max as f32 * k).into(),
                    (bb.y_max as f32 * k).into(),
                ]),
            ),
            ("ItalicAngle", Object::Integer(0)),
            ("Ascent", Object::Real(face.ascender() as f32 * k)),
            ("Descent", Object::Real(face.descender() as f32 * k)),
            ("CapHeight", Object::Real(face.capital_height().unwrap_or(face.ascender()) as f32 * k)),
            ("StemV", Object::Integer(80)),
            ("FontFile2", Object::Reference(ff_id)),
        ]);
        let desc_id = doc.add_object(desc);
        let mut w = Vec::new();
        for (gid, (_, adv)) in &glyphs {
            w.push(Object::Integer(*gid as i64));
            w.push(Object::Array(vec![Object::Real(*adv as f32 * k)]));
        }
        let cid = Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"CIDFontType2".to_vec())),
            ("BaseFont", Object::Name(b"AlexSegoeUI".to_vec())),
            (
                "CIDSystemInfo",
                Object::Dictionary(Dictionary::from_iter(vec![
                    ("Registry", Object::string_literal("Adobe")),
                    ("Ordering", Object::string_literal("Identity")),
                    ("Supplement", Object::Integer(0)),
                ])),
            ),
            ("FontDescriptor", Object::Reference(desc_id)),
            ("DW", Object::Integer(1000)),
            ("W", Object::Array(w)),
            ("CIDToGIDMap", Object::Name(b"Identity".to_vec())),
        ]);
        let cid_id = doc.add_object(cid);
        // ToUnicode so copy/search of the added text works
        let mut cmap = String::from(
            "/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n",
        );
        let entries: Vec<_> = glyphs.iter().collect();
        for chunk in entries.chunks(100) {
            cmap += &format!("{} beginbfchar\n", chunk.len());
            for (gid, (ch, _)) in chunk {
                let mut u = [0u16; 2];
                let hex: String = ch.encode_utf16(&mut u).iter().map(|x| format!("{x:04X}")).collect();
                cmap += &format!("<{gid:04X}> <{hex}>\n");
            }
            cmap += "endbfchar\n";
        }
        cmap += "endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
        let mut tu = Stream::new(Dictionary::new(), cmap.into_bytes());
        let _ = tu.compress();
        let tu_id = doc.add_object(tu);
        let type0 = Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Font".to_vec())),
            ("Subtype", Object::Name(b"Type0".to_vec())),
            ("BaseFont", Object::Name(b"AlexSegoeUI".to_vec())),
            ("Encoding", Object::Name(b"Identity-H".to_vec())),
            ("DescendantFonts", Object::Array(vec![Object::Reference(cid_id)])),
            ("ToUnicode", Object::Reference(tu_id)),
        ]);
        font_ref = Some(doc.add_object(type0));
    }
    let gs_ref = doc.add_object(Dictionary::from_iter(vec![
        ("Type", Object::Name(b"ExtGState".to_vec())),
        ("ca", Object::Real(0.35)),
        ("CA", Object::Real(0.35)),
    ]));

    for (pid, content, uses_font, uses_gs) in page_streams {
        // own, direct Resources dict on the page (may have been inherited/indirect)
        let res = inherited(&doc, pid, b"Resources").map(|o| resolve(&doc, o));
        let mut res = res.and_then(|o| o.as_dict().ok().cloned()).unwrap_or_default();
        let add = |cat: &[u8], name: &[u8], r: ObjectId, res: &mut Dictionary| {
            let mut sub = res.get(cat).ok().cloned().map(|o| resolve(&doc, o)).and_then(|o| o.as_dict().ok().cloned()).unwrap_or_default();
            sub.set(name.to_vec(), Object::Reference(r));
            res.set(cat.to_vec(), Object::Dictionary(sub));
        };
        if uses_font {
            if let Some(fr) = font_ref {
                add(b"Font", b"AlexF", fr, &mut res);
            }
        }
        if uses_gs {
            add(b"ExtGState", b"AlexHL", gs_ref, &mut res);
        }
        // wrap original content in q … Q so its graphics state can't leak into ours
        let pre = doc.add_object(Stream::new(Dictionary::new(), b"q\n".to_vec()));
        let post = doc.add_object(Stream::new(Dictionary::new(), content));
        let old: Vec<Object> = match doc.get_dictionary(pid).ok().and_then(|d| d.get(b"Contents").ok().cloned()) {
            Some(Object::Array(a)) => a,
            Some(Object::Reference(r)) => match doc.get_object(r) {
                Ok(Object::Array(a)) => a.clone(),
                _ => vec![Object::Reference(r)],
            },
            _ => vec![],
        };
        let mut contents = vec![Object::Reference(pre)];
        contents.extend(old);
        contents.push(Object::Reference(post));
        let d = doc.get_object_mut(pid).and_then(Object::as_dict_mut).map_err(|e| e.to_string())?;
        d.set("Resources", Object::Dictionary(res));
        d.set("Contents", Object::Array(contents));
    }

    // ---------------------------------------------------------------- new page order
    let pages_id = doc.new_object_id();
    let mut kids = Vec::new();
    let mut used = std::collections::HashSet::new();
    for e in order {
        let Some(&pid) = pages.get(&e.src) else { continue };
        let mut d = doc.get_dictionary(pid).map_err(|e| e.to_string())?.clone();
        for key in [b"Resources".as_slice(), b"MediaBox", b"CropBox"] {
            if !d.has(key) {
                if let Some(v) = inherited(&doc, pid, key) {
                    d.set(key.to_vec(), v);
                }
            }
        }
        let rot = (page_rotation(&doc, pid) + e.rotate).rem_euclid(360);
        d.set("Rotate", Object::Integer(rot as i64));
        d.set("Parent", Object::Reference(pages_id));
        // the same source page may appear twice: give each use its own object
        let id = if used.insert(pid) {
            doc.objects.insert(pid, Object::Dictionary(d));
            pid
        } else {
            doc.add_object(d)
        };
        kids.push(Object::Reference(id));
    }
    let count = kids.len() as i64;
    doc.objects.insert(
        pages_id,
        Object::Dictionary(Dictionary::from_iter(vec![
            ("Type", Object::Name(b"Pages".to_vec())),
            ("Kids", Object::Array(kids)),
            ("Count", Object::Integer(count)),
        ])),
    );
    let root = doc.trailer.get(b"Root").and_then(Object::as_reference).map_err(|e| e.to_string())?;
    let cat = doc.get_object_mut(root).and_then(Object::as_dict_mut).map_err(|e| e.to_string())?;
    cat.set("Pages", Object::Reference(pages_id));
    if !order.iter().map(|e| e.src).eq(1..=pages.len() as u32) {
        // outlines/page labels would point at the old order
        cat.remove(b"Outlines");
        cat.remove(b"PageLabels");
    }
    doc.prune_objects();
    doc.compress();
    doc.save(out).map_err(|e| e.to_string())?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reorder_rotate_draw_text() {
        let d = std::env::temp_dir().join("alex-pdfedit-test");
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        // 3-page source made with our own image→pdf path
        let mut srcs = Vec::new();
        for i in 0..3u8 {
            let p = d.join(format!("p{i}.png"));
            image::RgbImage::from_pixel(400, 560, image::Rgb([240, 240 - i * 60, 255])).save(&p).unwrap();
            srcs.push(p);
        }
        let src = d.join("src.pdf");
        std::fs::write(&src, crate::docs::test_images_pdf(&srcs)).unwrap();

        let mut marks = BTreeMap::new();
        marks.insert(
            1,
            vec![
                Mark::Ink { pts: vec![(0.1, 0.1), (0.5, 0.3), (0.9, 0.1)], color: [220, 30, 30], width: 3.0 },
                Mark::Highlight { a: (0.1, 0.5), b: (0.9, 0.6), color: [255, 220, 0] },
                Mark::Text { pos: (0.1, 0.7), text: "Đã duyệt – Nguyễn Văn Ấn".into(), size: 20.0, color: [20, 40, 160] },
            ],
        );
        let order = [PageEdit { src: 3, rotate: 0 }, PageEdit { src: 1, rotate: 90 }];
        let out = d.join("out.pdf");
        save(&src, &order, &marks, &out).unwrap();
        let doc = Document::load(&out).unwrap();
        let pages = doc.get_pages();
        assert_eq!(pages.len(), 2);
        let p2 = doc.get_dictionary(pages[&2]).unwrap();
        assert_eq!(p2.get(b"Rotate").unwrap().as_i64().unwrap(), 90);
        let text = doc.extract_text(&[2]).unwrap_or_default();
        println!("extracted page 2 text: {text:?}");
        println!("output: {}", out.display());
    }
}
